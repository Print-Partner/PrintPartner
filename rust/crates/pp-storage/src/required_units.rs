pub(crate) mod model;
pub(crate) mod reconcile;
pub(crate) mod storage;
#[cfg(test)]
mod tests;
pub(crate) mod view;

use crate::{Envelope, Shared, WriterOwner, auth, manifest_text::JsText, read_model::Credential};
use anyhow::{Result, anyhow, ensure};
use pp_domain::PositiveId;
use rusqlite::Connection;
use serde::Deserialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
pub use view::{
    AcceptedReference, Change, ConflictView, Diff, DomainCode, DraftBase, DraftIdentity,
    DraftListResponse, Outcome, PartView, ReconciliationView, Workspace,
};

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Decision {
    SelectExactPredecessor {
        target_draft_part_id: PositiveId,
        predecessor_revision_part_id: PositiveId,
    },
    AcceptPriorCompletion {
        target_draft_part_id: PositiveId,
        predecessor_revision_part_id: PositiveId,
    },
    Replace {
        target_draft_part_id: PositiveId,
    },
}

impl Decision {
    pub(crate) fn target(&self) -> PositiveId {
        match self {
            Self::SelectExactPredecessor {
                target_draft_part_id,
                ..
            }
            | Self::AcceptPriorCompletion {
                target_draft_part_id,
                ..
            }
            | Self::Replace {
                target_draft_part_id,
            } => *target_draft_part_id,
        }
    }
}

/// The JSON body of a reconcile request, with Node's snake-case field names.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RequestWire")]
pub struct ReconciliationRequest {
    expected_snapshot_digest: String,
    decisions: Vec<Decision>,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct RequestWire {
    #[cfg_attr(feature = "codegen", schemars(with = "pp_domain::Digest"))]
    expected_snapshot_digest: String,
    #[cfg_attr(feature = "codegen", schemars(length(max = 10000)))]
    decisions: Vec<Decision>,
}

/// The wire schema is [`RequestWire`]'s. JSON Schema cannot express the
/// one-decision-per-Part rule; the server refuses it.
#[cfg(feature = "codegen")]
impl schemars::JsonSchema for ReconciliationRequest {
    fn schema_name() -> String {
        "ReconciliationRequest".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        <RequestWire as schemars::JsonSchema>::json_schema(generator)
    }
}

impl TryFrom<RequestWire> for ReconciliationRequest {
    type Error = &'static str;
    fn try_from(wire: RequestWire) -> Result<Self, Self::Error> {
        let digest = &wire.expected_snapshot_digest;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Expected Plan draft snapshot digest must be a SHA-256 hex digest");
        }
        if wire.decisions.len() > 10_000 {
            return Err("A reconcile request allows at most 10000 decisions");
        }
        let mut targets = std::collections::HashSet::new();
        if !wire
            .decisions
            .iter()
            .all(|d| targets.insert(d.target().get()))
        {
            return Err("A Plan draft Part may be reconciled only once per request");
        }
        Ok(Self {
            expected_snapshot_digest: wire.expected_snapshot_digest,
            decisions: wire.decisions,
        })
    }
}

pub struct ReconciliationCommand {
    profile_id: PositiveId,
    draft_id: PositiveId,
    request: ReconciliationRequest,
    idempotency_key: String,
    credential: Credential,
}

impl ReconciliationCommand {
    /// Trims the key as JavaScript `String.prototype.trim` does and requires
    /// 1 to 160 UTF-16 units, as the Node route does.
    pub fn new(
        profile_id: PositiveId,
        draft_id: PositiveId,
        request: ReconciliationRequest,
        idempotency_key: &str,
        credential: Credential,
    ) -> Result<Self> {
        let key = JsText::scalar(idempotency_key).trim_ecmascript();
        ensure!(
            !key.is_empty() && key.units().len() <= 160,
            "Invalid idempotency key"
        );
        Ok(Self {
            profile_id,
            draft_id,
            request,
            idempotency_key: key
                .as_scalar()
                .ok_or_else(|| anyhow!("Invalid idempotency key"))?,
            credential,
        })
    }
}

pub use Failure as AdmissionFailure;

#[derive(Debug)]
pub enum Failure {
    Cancelled,
    QueueFull,
    Stopped,
    OutcomeUnknown,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Failure {}

#[derive(Clone)]
pub struct ReconciliationClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}

pub(super) struct Command {
    input: ReconciliationCommand,
    policy: auth::AuthPolicy,
    cancelled: Arc<AtomicBool>,
}

impl WriterOwner {
    pub fn required_units_with_policy(
        &self,
        policy: auth::AuthPolicy,
    ) -> Result<ReconciliationClient> {
        auth::validate_policy(policy)?;
        Ok(ReconciliationClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}

impl ReconciliationClient {
    pub(crate) fn new(shared: Arc<Shared>, policy: auth::AuthPolicy) -> Self {
        Self { shared, policy }
    }

    pub fn reconcile(
        &self,
        input: ReconciliationCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Outcome> {
        let deadline = Instant::now() + wait;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!(Failure::Stopped))?;
        loop {
            ensure!(!queue.closed, Failure::Stopped);
            ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, Failure::QueueFull);
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!(Failure::Stopped))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::RequiredUnits {
            command: Command {
                input,
                policy: self.policy,
                cancelled,
            },
            reply,
        });
        self.shared.changed.notify_all();
        drop(queue);
        receiver
            .recv()
            .map_err(|_| anyhow!(Failure::OutcomeUnknown))?
            .map_err(|error| {
                if error.downcast_ref::<auth::AuthFailure>().is_some()
                    || error.downcast_ref::<Failure>().is_some()
                {
                    error
                } else {
                    error.context("Plan draft update failed")
                }
            })
    }
}

pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Outcome> {
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    let outcome = reconcile::run(&tx, &command.input, command.policy, &mut || {
        ensure!(
            !command.cancelled.load(Ordering::Acquire),
            Failure::Cancelled
        );
        Ok(())
    })?;
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    tx.commit()?;
    Ok(outcome)
}
