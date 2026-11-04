mod context;
pub(crate) mod graph;
#[cfg(feature = "desktop_rebuild")]
mod json_body;
pub use context::{CapturedContext, PlateRef, RevisionRef, UnitHistory};
#[cfg(feature = "desktop_rebuild")]
pub use json_body::{AcceptedBatchJsonBody, AcceptedReadJsonBody, AcceptedSnapshotJsonBody};
#[cfg(feature = "desktop_rebuild")]
pub mod stock;
pub mod views;
pub mod workflow;

use crate::{
    Envelope, Shared, WriterOwner,
    auth::{self, Secret},
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub use graph::{Artifact, Input, Part, Profile, Provenance, Snapshot, Unit};

pub enum Credential {
    Session(Secret),
    ApiKey {
        routed_tenant: String,
        secret: Secret,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptedRead {
    Ready {
        snapshot: Box<Snapshot>,
    },
    Empty {
        #[serde(skip)]
        profile: Profile,
    },
    Missing,
    CompatibilityDirty,
    Uninitialized,
    IntegrityFailure {
        code: String,
        message: String,
    },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRead {
    pub profile_id: i64,
    pub accepted: AcceptedRead,
    pub context: Option<CapturedContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_error: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Batch {
    pub builds: Vec<BuildRead>,
    /// Spoolman stock inputs, read only by [`ReadClient::read_stock`].
    #[cfg(feature = "desktop_rebuild")]
    #[serde(skip)]
    pub stock: Option<stock::StockInputs>,
}

#[derive(Clone)]
pub struct ReadClient {
    shared: Arc<Shared>,
    repos: PathBuf,
    policy: auth::AuthPolicy,
}
pub(super) struct Command {
    credential: Credential,
    target: Target,
    repos: PathBuf,
    policy: auth::AuthPolicy,
}
enum Target {
    Profiles(Vec<i64>),
    /// The Plan that owns this tenant's projection Part, resolved in the same
    /// transaction as the read. No Part reads no Plan.
    Part(i64),
    /// One Plan plus its Spoolman stock inputs, in the same transaction.
    #[cfg(feature = "desktop_rebuild")]
    Stock(i64),
}

impl WriterOwner {
    pub fn accepted_reads(&self) -> Result<ReadClient> {
        self.accepted_reads_with_policy(auth::AuthPolicy {
            registration: auth::RegistrationPolicy::Open,
            session_tenant: auth::SessionTenantPolicy::AccountTenant,
            first_user: auth::FirstUserTenant::NewUser,
        })
    }
    pub fn accepted_reads_with_policy(&self, policy: auth::AuthPolicy) -> Result<ReadClient> {
        auth::validate_policy(policy)?;
        Ok(ReadClient {
            policy,
            shared: self.client.shared.clone(),
            repos: self
                .lease
                .as_ref()
                .ok_or_else(|| anyhow!("Storage owner is closed"))?
                .data_dir()
                .join("repos"),
        })
    }
}
impl ReadClient {
    pub fn read(
        &self,
        credential: Credential,
        profile_ids: &[i64],
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Batch> {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for &id in profile_ids {
            ensure!(
                id > 0 && id <= 9_007_199_254_740_991,
                "Accepted Plan Progress profile IDs must be positive safe integers"
            );
            if seen.insert(id) {
                ids.push(id);
            }
            ensure!(
                ids.len() <= 64,
                "Accepted Plan Progress batches contain at most 64 Plans"
            );
        }
        self.submit(credential, Target::Profiles(ids), cancelled, wait)
    }

    /// Reads the Accepted Plan that owns projection Part `part_id`. The batch
    /// is empty when the caller's tenant has no such Part.
    pub fn read_part(
        &self,
        credential: Credential,
        part_id: i64,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Batch> {
        ensure!(
            part_id > 0 && part_id <= 9_007_199_254_740_991,
            "Accepted Part IDs must be positive safe integers"
        );
        self.submit(credential, Target::Part(part_id), cancelled, wait)
    }

    /// Reads one Accepted Plan and, when the Plan exists, its Spoolman stock
    /// inputs in the same transaction. Reads only; never reserves stock.
    #[cfg(feature = "desktop_rebuild")]
    pub fn read_stock(
        &self,
        credential: Credential,
        profile_id: i64,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Batch> {
        ensure!(
            profile_id > 0 && profile_id <= 9_007_199_254_740_991,
            "Accepted Plan IDs must be positive safe integers"
        );
        self.submit(credential, Target::Stock(profile_id), cancelled, wait)
    }

    fn submit(
        &self,
        credential: Credential,
        target: Target,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Batch> {
        let deadline = Instant::now() + wait;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!("Writer admission poisoned"))?;
        loop {
            ensure!(!queue.closed, "Storage stopped");
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Cancelled before admission"
            );
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, "Writer queue full");
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!("Writer admission poisoned"))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::Read {
            command: Command {
                credential,
                target,
                repos: self.repos.clone(),
                policy: self.policy,
            },
            reply,
        });
        self.shared.changed.notify_all();
        drop(queue);
        receiver
            .recv()
            .map_err(|_| anyhow!("Storage stopped without read result"))?
    }
}
pub(super) fn execute(connection: &mut Connection, command: Command) -> Result<Batch> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let tenant = match command.credential {
        Credential::Session(secret) => auth::read_session_tenant(&tx, &secret, command.policy)?,
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => auth::read_key_tenant(&tx, &routed_tenant, &secret)?,
    };
    #[cfg(feature = "desktop_rebuild")]
    let stock_profile = match command.target {
        Target::Stock(id) => Some(id),
        _ => None,
    };
    let profile_ids = match command.target {
        Target::Profiles(ids) => ids,
        #[cfg(feature = "desktop_rebuild")]
        Target::Stock(id) => vec![id],
        Target::Part(part) => tx
            .query_row(
                "SELECT profile_id FROM parts WHERE tenant_id=?1 AND id=?2",
                rusqlite::params![tenant, part],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .into_iter()
            .collect(),
    };
    let mut builds = Vec::new();
    let mut budget = graph::Budget::default();
    for profile_id in profile_ids {
        let accepted = match graph::read(&tx, &tenant, profile_id, &command.repos, &mut budget) {
            Ok(read) => read,
            Err(error) => match error.downcast_ref::<graph::Integrity>() {
                Some(error) => AcceptedRead::IntegrityFailure {
                    code: error.code.to_owned(),
                    message: error.message.clone(),
                },
                None => return Err(error),
            },
        };
        let (context, context_error) =
            match context::capture(&tx, &tenant, profile_id, &accepted, &mut budget) {
                Ok(value) => (value, None),
                Err(error) => {
                    #[cfg(feature = "desktop_rebuild")]
                    if matches!(
                        error.downcast_ref::<crate::build_graph::Failure>(),
                        Some(crate::build_graph::Failure::BudgetExhausted { .. })
                    ) {
                        return Err(error);
                    }
                    (None, Some(error.to_string()))
                }
            };
        builds.push(BuildRead {
            profile_id,
            accepted,
            context,
            context_error,
        });
    }
    #[cfg(feature = "desktop_rebuild")]
    let stock = match stock_profile {
        Some(id)
            if builds
                .iter()
                .any(|b| !matches!(b.accepted, AcceptedRead::Missing)) =>
        {
            Some(stock::read_inputs(&tx, &tenant, id)?)
        }
        _ => None,
    };
    tx.commit()?;
    Ok(Batch {
        builds,
        #[cfg(feature = "desktop_rebuild")]
        stock,
    })
}

#[cfg(feature = "desktop_rebuild")]
pub(crate) fn reusable_draft_base(
    tx: &rusqlite::Transaction<'_>,
    tenant: &str,
    profile: i64,
    repos: &std::path::Path,
    preparation: &mut crate::working_drafts::observation::PreparationBudget<'_>,
) -> Result<bool> {
    preparation.check()?;
    let mut budget = graph::Budget::default();
    if !context::draft_freshness_current(tx, tenant, profile, &mut budget)? {
        preparation.check()?;
        return Ok(false);
    }
    let accepted = graph::read(tx, tenant, profile, repos, &mut budget)?;
    preparation.check()?;
    Ok(matches!(accepted, AcceptedRead::Ready { .. }))
}

/// Reads the Accepted Plan and its Build context after a committed Save. The
/// snapshot must be at least `minimum_version`, the receipt's Plan version.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn capture_published(
    tx: &rusqlite::Transaction<'_>,
    tenant: &str,
    profile: i64,
    repos: &std::path::Path,
    minimum_version: u64,
) -> Result<(Box<Snapshot>, CapturedContext)> {
    let mut budget = graph::Budget::default();
    let accepted = graph::read(tx, tenant, profile, repos, &mut budget)?;
    let context = context::capture(tx, tenant, profile, &accepted, &mut budget)?
        .ok_or_else(|| anyhow!("Published Build context missing"))?;
    let AcceptedRead::Ready { snapshot } = accepted else {
        return Err(anyhow!("Published accepted authority unavailable"));
    };
    ensure!(
        u64::try_from(snapshot.plan_version).is_ok_and(|version| version >= minimum_version),
        "Accepted capture predates Save receipt"
    );
    Ok((snapshot, context))
}

#[cfg(all(test, feature = "desktop_rebuild"))]
#[path = "read_model/preparation_tests.rs"]
mod preparation_tests;

#[cfg(all(test, feature = "desktop_rebuild"))]
#[path = "read_model/capture_published_tests.rs"]
mod capture_published_tests;
