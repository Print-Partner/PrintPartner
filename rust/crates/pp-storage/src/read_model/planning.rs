//! Build planning (`GET /plans/{id}/build-planning`) and assistant decisions
//! (`GET /plans/{id}/decisions`), read as Node's `routes/plans.ts` reads them,
//! plus the MCP `get_plan_decisions` and `list_plan_snapshots` reads (GRE-441).
//!
//! One deferred transaction reads, for the caller's tenant: the Build row,
//! then either the `build_planning.v1.<id>` setting, every Source in
//! `projects` and the verified Plan draft the brief names, or the Build's
//! latest 200 `plan_decisions` rows. Hydration, which hashes the synced
//! structural and overlay Source trees, runs on the calling thread after
//! the transaction, not on the writer. Nothing is written.

mod brief;

pub use brief::*;

use super::Credential;
use crate::{Envelope, Shared, WriterOwner, auth, working_drafts::source_reads::SourceReads};
use anyhow::{Result, ensure};
use pp_domain::PositiveId;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// Node's `listPlanDecisions` default limit.
const DECISION_LIMIT: i64 = 200;

/// One `plan_decisions` row as Node's `mapPlanDecision` returns it.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanDecision {
    pub id: i64,
    pub plan_id: i64,
    pub created_at: String,
    pub actor: String,
    pub kind: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub action_type: Option<String>,
    /// `params_json` when it holds a JSON object, else `{}`.
    pub params: Map<String, Value>,
    pub label: String,
    pub summary: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub rationale: Option<String>,
    /// `null` when `result_json` is empty, else as `params`.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<Map<String, Value>>")
    )]
    pub result: Option<Map<String, Value>>,
}

/// `GET /plans/{id}/decisions` body: the latest 200, oldest first.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanDecisionList {
    pub decisions: Vec<PlanDecision>,
}

/// One `plan_snapshots` row as Node's `mapPlanSnapshotSummary` returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanSnapshotSummary {
    pub id: i64,
    pub plan_id: i64,
    pub name: String,
    pub created_at: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// Node refuses an invalid `draft_id` after it hydrates a stored brief
    /// and before it derives the phase.
    BuildPlanning {
        draft_id_valid: bool,
    },
    Decisions,
    /// Node's `listPlanDecisions(planId, limit)`: the latest `limit` (at
    /// least one), oldest first. MCP `get_plan_decisions` reads this.
    LatestDecisions {
        limit: i64,
    },
    /// Node's `listPlanSnapshots`: every snapshot summary, oldest first.
    Snapshots,
    /// The guards Node's `propose_apply_plan_draft` runs before Accept.
    AcceptGuard {
        draft_id: PositiveId,
    },
}

/// Whether MCP may accept a draft, as Node's `confirm_apply` decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptGuard {
    NoBrief,
    /// The brief selects another draft, or none.
    NotSelected,
    /// `resolvedSourcePathExclusions` blockers.
    SourceChoices(Vec<String>),
    Clear,
}

#[derive(Debug, Clone)]
pub struct Request {
    /// `None` names no Build: the read still checks the credential, then
    /// answers `MissingBuild`.
    pub build: Option<PositiveId>,
    pub query: Query,
}

#[derive(Debug)]
pub enum Outcome {
    BuildPlanning(Box<BuildPlanningResponse>),
    Decisions(PlanDecisionList),
    Snapshots(Vec<PlanSnapshotSummary>),
    AcceptGuard(AcceptGuard),
    /// A brief exists, hydrated, and the request's `draft_id` is invalid.
    InvalidDraftId,
    MissingBuild,
}

pub(crate) enum Facts {
    MissingBuild,
    NoBrief,
    Brief {
        brief: Box<BuildPlanningBrief>,
        sources: Vec<SourceRow>,
        /// The verified draft the stored brief names: used, and its failure
        /// raised, only when hydration keeps that draft.
        draft: Result<Option<(String, Option<i64>)>>,
    },
    Decisions(Vec<PlanDecision>),
    Snapshots(Vec<PlanSnapshotSummary>),
}

/// `id`, `name`, `local_path`, `last_synced_at`, `last_commit_sha`.
pub(crate) type SourceRow = (i64, String, Option<String>, Option<String>, Option<String>);

pub(crate) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    request: Request,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct PlanningClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
    sources: Arc<SourceReads>,
}

impl WriterOwner {
    /// Reads Sources through `sources`, whose filesystem policy should be
    /// Node's for `policy` (`SourceFilesystemPolicy::for_auth`).
    pub fn build_planning(
        &self,
        policy: auth::AuthPolicy,
        sources: SourceReads,
    ) -> Result<PlanningClient> {
        auth::validate_policy(policy)?;
        ensure!(self.lease.is_some(), "Storage owner is closed");
        Ok(PlanningClient {
            shared: self.client.shared.clone(),
            policy,
            sources: Arc::new(sources),
        })
    }
}

impl PlanningClient {
    pub fn read(
        &self,
        credential: Credential,
        request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Outcome> {
        let refuse_draft_id = request.query
            == Query::BuildPlanning {
                draft_id_valid: false,
            };
        let guard = match request.query {
            Query::AcceptGuard { draft_id } => Some(draft_id),
            _ => None,
        };
        let flag = cancelled.clone();
        let facts = super::submit_read(&self.shared, &flag, wait, |reply| Envelope::Planning {
            command: Command {
                credential,
                policy: self.policy,
                request,
                cancelled,
            },
            reply,
        })?;
        if guard.is_some() && !matches!(facts, Facts::Brief { .. }) {
            return Ok(match facts {
                Facts::MissingBuild => Outcome::MissingBuild,
                _ => Outcome::AcceptGuard(AcceptGuard::NoBrief),
            });
        }
        Ok(match facts {
            Facts::MissingBuild => Outcome::MissingBuild,
            Facts::NoBrief => {
                Outcome::BuildPlanning(Box::new(BuildPlanningResponse { planning: None }))
            }
            Facts::Decisions(decisions) => Outcome::Decisions(PlanDecisionList { decisions }),
            Facts::Snapshots(snapshots) => Outcome::Snapshots(snapshots),
            Facts::Brief {
                brief,
                sources,
                draft,
            } => {
                let sources: HashMap<i64, PlanningSource> = sources
                    .into_iter()
                    .map(|(id, name, local_path, synced_at, sha)| {
                        let source = PlanningSource::resolve(
                            &self.sources,
                            id,
                            name,
                            local_path.as_deref(),
                            (synced_at, sha),
                        );
                        (id, source)
                    })
                    .collect();
                let brief = hydrate(*brief, &sources)?;
                if let Some(draft_id) = guard {
                    return Ok(Outcome::AcceptGuard(accept_guard(
                        &brief, &sources, draft_id,
                    )));
                }
                if refuse_draft_id {
                    return Ok(Outcome::InvalidDraftId);
                }
                let groups: std::collections::HashSet<&str> = brief
                    .differences
                    .iter()
                    .map(|d| d.group_id.as_str())
                    .collect();
                let planning = BuildPlanning {
                    planning_phase: match brief.draft_id {
                        Some(_) => phase(brief.draft_id, draft?.as_ref())?,
                        None => BuildPlanningPhase::Preparing,
                    },
                    readiness: readiness(&brief),
                    grouped_difference_count: groups.len(),
                    difference_count: brief.differences.len(),
                    brief,
                };
                Outcome::BuildPlanning(Box::new(BuildPlanningResponse {
                    planning: Some(planning),
                }))
            }
        })
    }
}

/// Node's `confirm_apply` checks of a hydrated brief: hydration clears
/// `draft_id` when the Sources, pinned revisions or resolutions moved.
fn accept_guard(
    brief: &BuildPlanningBrief,
    sources: &HashMap<i64, PlanningSource>,
    draft_id: PositiveId,
) -> AcceptGuard {
    if brief.draft_id.and_then(|id| u64::try_from(id).ok()) != Some(draft_id.get()) {
        return AcceptGuard::NotSelected;
    }
    match source_choice_blockers(brief, sources) {
        blockers if blockers.is_empty() => AcceptGuard::Clear,
        blockers => AcceptGuard::SourceChoices(blockers),
    }
}

pub(crate) fn execute(connection: &mut Connection, command: Command) -> Result<Facts> {
    let (tx, found) = super::owned_build(
        connection,
        &command.cancelled,
        &command.credential,
        command.policy,
        command.request.build,
    )?;
    let facts = match (found, command.request.query) {
        (None, _) => Facts::MissingBuild,
        (Some((tenant, build)), Query::Decisions) => {
            Facts::Decisions(decisions(&tx, &tenant, build, DECISION_LIMIT)?)
        }
        (Some((tenant, build)), Query::LatestDecisions { limit }) => {
            Facts::Decisions(decisions(&tx, &tenant, build, limit.max(1))?)
        }
        (Some((tenant, build)), Query::Snapshots) => {
            Facts::Snapshots(snapshots(&tx, &tenant, build)?)
        }
        (Some((tenant, build)), Query::BuildPlanning { .. } | Query::AcceptGuard { .. }) => {
            planning(&tx, &tenant, build)?
        }
    };
    tx.commit()?;
    Ok(facts)
}

fn planning(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Facts> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, format!("build_planning.v1.{build}")],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Ok(Facts::NoBrief);
    };
    let brief = BuildPlanningBrief::parse(&raw, build)?;
    let mut statement = tx.prepare(
        "SELECT id,name,local_path,last_synced_at,last_commit_sha FROM projects WHERE tenant_id=?1",
    )?;
    let sources = statement
        .query_map([tenant], |row| SourceRow::try_from(row))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Node's `getPlanDraft`, which verifies the stored snapshot digest.
    let draft = brief.draft_id.map_or(Ok(None), |id| {
        crate::required_units::storage::draft(tx, tenant, build, id).map(|draft| {
            draft.map(|draft| {
                let state = draft.header["state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                (state, draft.header["consumedRevisionId"].as_i64())
            })
        })
    });
    Ok(Facts::Brief {
        brief: Box::new(brief),
        sources,
        draft,
    })
}

/// Node's `parseJsonObject`: a JSON object, else empty.
fn object(raw: Option<&str>) -> Map<String, Value> {
    match raw.map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Object(object))) => object,
        _ => Map::new(),
    }
}

fn snapshots(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Vec<PlanSnapshotSummary>> {
    let mut statement = tx.prepare(
        "SELECT id,profile_id,name,created_at,source FROM plan_snapshots \
         WHERE tenant_id=?1 AND profile_id=?2 ORDER BY created_at, id",
    )?;
    let rows = statement.query_map(params![tenant, build], |row| {
        Ok(PlanSnapshotSummary {
            id: row.get(0)?,
            plan_id: row.get(1)?,
            name: row.get(2)?,
            created_at: row.get(3)?,
            source: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn decisions(
    tx: &Transaction<'_>,
    tenant: &str,
    build: i64,
    limit: i64,
) -> Result<Vec<PlanDecision>> {
    let mut statement = tx.prepare(
        "SELECT id,profile_id,created_at,actor,kind,action_type,params_json,label,summary,rationale,result_json \
         FROM (SELECT * FROM plan_decisions WHERE tenant_id=?1 AND profile_id=?2 \
         ORDER BY created_at DESC, id DESC LIMIT ?3) ORDER BY created_at, id",
    )?;
    let rows = statement.query_map(params![tenant, build, limit], |row| {
        let result: Option<String> = row.get(10)?;
        Ok(PlanDecision {
            id: row.get(0)?,
            plan_id: row.get(1)?,
            created_at: row.get(2)?,
            actor: row.get(3)?,
            kind: row.get(4)?,
            action_type: row.get(5)?,
            params: object(row.get::<_, Option<String>>(6)?.as_deref()),
            label: row.get(7)?,
            summary: row.get(8)?,
            rationale: row.get(9)?,
            result: result
                .filter(|raw| !raw.is_empty())
                .map(|raw| object(Some(&raw))),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
