//! Build workflow reads (GRE-1123): `GET /plans/{id}/workflow`, the ranked
//! task feed `GET /api/v1/tasks` and MCP `get_build_workflow`.
//!
//! Storage reads every Build's `workflow::Facts` in one deferred transaction,
//! the same facts `context::capture` resolves for Build reads. The client then
//! adds the inputs other owners compute (`WorkflowInputs`: due date and stock
//! shortfall), resolves each Build with `workflow::resolve` and, for the task
//! feed, orders the results. Every consumer goes through `WorkflowClient::read`,
//! so the feed is a view over the projection, not a second ranking engine.

use super::{Credential, context, graph, workflow};
use crate::{Envelope, Shared, WriterOwner, auth};
use anyhow::{Result, anyhow, ensure};
use pp_domain::PositiveId;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use workflow::{ActionKind, Facts, NextAction, Stock, Workspace};

/// Open Builds the task feed reads. More answers `Failure::TooManyBuilds`
/// instead of a partial ranking.
pub const MAX_BUILDS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    TooManyBuilds,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "More than {MAX_BUILDS} open Builds")
    }
}

impl std::error::Error for Failure {}

/// What other owners know about a Build. Storage has no due-date field until
/// order fields land (slice 7), and stock shortfall belongs to the Spoolman
/// computation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildInputs {
    pub due_date: Option<time::Date>,
    pub stock: Stock,
}

/// Supplies `BuildInputs` after the storage read. Called outside the writer
/// thread, once per Build in the read.
pub trait WorkflowInputs: Send + Sync {
    fn inputs(&self, tenant: &str, build: i64) -> BuildInputs;
}

/// No due dates and stock not ready: the inputs until their owners exist.
pub struct PendingInputs;

impl WorkflowInputs for PendingInputs {
    fn inputs(&self, _tenant: &str, _build: i64) -> BuildInputs {
        BuildInputs::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// One Build. `None` names no Build: the read still checks the
    /// credential, then answers `Outcome::MissingBuild`.
    Build(Option<PositiveId>),
    /// Every open Build's next action, most urgent first.
    Tasks,
}

/// One entry of the task feed: a Build's next action and why it ranks where
/// it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct Task {
    pub plan_id: i64,
    pub build_name: String,
    /// `YYYY-MM-DD`, when the Build has one.
    pub due_date: Option<String>,
    /// The next action is blocked on the user; these rank first.
    pub attention: bool,
    pub rank_reason: String,
    pub next_action: NextAction,
}

/// The body of `GET /api/v1/tasks`. Builds whose every unit is checked off
/// have no task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct TaskFeed {
    pub tasks: Vec<Task>,
}

#[derive(Debug)]
pub enum Outcome {
    Workspace(Box<Workspace>),
    Tasks(TaskFeed),
    MissingBuild,
}

#[derive(Clone)]
pub struct WorkflowClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
    repos: PathBuf,
    inputs: Arc<dyn WorkflowInputs>,
}

pub(crate) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    repos: PathBuf,
    request: Request,
    cancelled: Arc<AtomicBool>,
}

/// The caller's tenant and the facts of each Build read, in id order. `None`
/// when the requested Build does not exist.
pub(crate) type Read = Option<(String, Vec<Facts>)>;

impl WriterOwner {
    pub fn build_workflow(
        &self,
        policy: auth::AuthPolicy,
        inputs: Arc<dyn WorkflowInputs>,
    ) -> Result<WorkflowClient> {
        auth::validate_policy(policy)?;
        Ok(WorkflowClient {
            shared: self.client.shared.clone(),
            policy,
            repos: self
                .lease
                .as_ref()
                .ok_or_else(|| anyhow!("Storage owner is closed"))?
                .data_dir()
                .join("repos"),
            inputs,
        })
    }
}

impl WorkflowClient {
    pub fn read(
        &self,
        credential: Credential,
        request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Outcome> {
        let flag = cancelled.clone();
        let read = super::submit_read(&self.shared, &flag, wait, |reply| Envelope::Workflow {
            command: Command {
                credential,
                policy: self.policy,
                repos: self.repos.clone(),
                request,
                cancelled,
            },
            reply,
        })?;
        let Some((tenant, facts)) = read else {
            return Ok(Outcome::MissingBuild);
        };
        let mut resolved = Vec::with_capacity(facts.len());
        for mut facts in facts {
            let inputs = self.inputs.inputs(&tenant, facts.build.id);
            facts.stock = inputs.stock;
            resolved.push((workflow::resolve(&facts)?, inputs.due_date));
        }
        Ok(match request {
            Request::Build(_) => Outcome::Workspace(Box::new(
                resolved
                    .pop()
                    .ok_or_else(|| anyhow!("Build workflow missing"))?
                    .0,
            )),
            Request::Tasks => Outcome::Tasks(rank(resolved)),
        })
    }
}

/// Orders open Builds: work blocked on the user first, then the earliest due
/// date, then Builds without one, then Build id. Completed Builds drop out.
pub fn rank(resolved: Vec<(Workspace, Option<time::Date>)>) -> TaskFeed {
    let mut ranked: Vec<_> = resolved
        .into_iter()
        .filter(|(workspace, _)| workspace.next_action.kind != ActionKind::ViewCompletedBuild)
        .map(|(workspace, due)| {
            let attention = workspace.next_action.kind.needs_attention();
            let rank_reason = match (attention, due) {
                (true, Some(due)) => format!("Needs attention now. Due {due}."),
                (true, None) => "Needs attention now.".to_owned(),
                (false, Some(due)) => format!("Due {due}."),
                (false, None) => "No due date.".to_owned(),
            };
            let task = Task {
                plan_id: workspace.build.id,
                build_name: workspace.build.name,
                due_date: due.map(|due| due.to_string()),
                attention,
                rank_reason,
                next_action: workspace.next_action,
            };
            ((!attention, due.is_none(), due, task.plan_id), task)
        })
        .collect();
    ranked.sort_by_key(|(key, _)| *key);
    TaskFeed {
        tasks: ranked.into_iter().map(|(_, task)| task).collect(),
    }
}

pub(crate) fn execute(connection: &mut Connection, command: Command) -> Result<Read> {
    let cancelled = || command.cancelled.load(Ordering::Acquire);
    ensure!(!cancelled(), "Cancelled before admission");
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let tenant = match &command.credential {
        Credential::Session(secret) => auth::read_session_tenant(&tx, secret, command.policy)?,
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => auth::read_key_tenant(&tx, routed_tenant, secret)?,
    };
    let ids = match command.request {
        Request::Build(None) => Vec::new(),
        Request::Build(Some(build)) => tx
            .query_row(
                "SELECT id FROM build_profiles WHERE tenant_id=?1 AND id=?2",
                params![tenant, i64::try_from(build.get())?],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .into_iter()
            .collect(),
        Request::Tasks => {
            let mut statement = tx.prepare(
                "SELECT id FROM build_profiles WHERE tenant_id=?1 AND archived_at IS NULL ORDER BY id LIMIT ?2",
            )?;
            let ids = statement
                .query_map(params![tenant, MAX_BUILDS as i64 + 1], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            ensure!(ids.len() <= MAX_BUILDS, Failure::TooManyBuilds);
            ids
        }
    };
    if ids.is_empty() && matches!(command.request, Request::Build(_)) {
        tx.commit()?;
        return Ok(None);
    }
    let mut facts = Vec::with_capacity(ids.len());
    for id in ids {
        ensure!(!cancelled(), "Cancelled before admission");
        let mut budget = graph::Budget::default();
        let accepted = super::accepted_read(&tx, &tenant, id, &command.repos, &mut budget)?;
        let captured = context::capture(&tx, &tenant, id, &accepted, &mut budget)?
            .ok_or_else(|| anyhow!("Build context missing"))?;
        facts.push(captured.facts);
    }
    tx.commit()?;
    Ok(Some((tenant, facts)))
}
