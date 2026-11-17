//! Sends each pending Spoolman deduction once (GRE-1738).
//!
//! Started and stopped with the printer send worker. A committed Checkoff
//! verify burst wakes it; with nothing to send it looks again after
//! [`RECHECK`]. Before each claim it reads the Spoolman Module again. While
//! the Module is off, or its setting cannot be read, the row stays `pending`
//! and is sent once the Module is on. Applied, unknown and failed rows are
//! not sent again. A lost reply or a restart while a row is `sending` stays
//! unknown until reconcile.

use crate::spoolman::{SpoolmanInventory, UseResult};
use anyhow::{Result, anyhow, bail};
use pp_storage::{
    WriterOwner,
    auth::{AuthPolicy, SessionTenantPolicy},
    read_model::stock::SpoolmanIntegration,
    spoolman_deductions::{
        DeductionError, ModuleClaim, Outcome, SendWork, SpoolmanDeductionsClient,
    },
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

/// How long an idle worker waits without a burst's wake. The same backstop
/// the printer send worker uses.
const RECHECK: Duration = Duration::from_secs(30);
const STORAGE_WAIT: Duration = Duration::from_secs(5);

pub struct DeductionWorker {
    client: SpoolmanDeductionsClient,
    inventory: SpoolmanInventory,
    wake: Arc<Notify>,
}

enum Turn {
    /// Nothing to send, or every pending row is held by a Module that is off.
    Wait,
    /// One row was claimed, or it was no longer pending. Look for the next.
    Again,
}

impl DeductionWorker {
    /// Registers the burst wake on `owner`. `policy` must be the
    /// single-account policy the desktop runtime serves.
    pub fn new(
        owner: &WriterOwner,
        policy: AuthPolicy,
        inventory: SpoolmanInventory,
    ) -> Result<Self> {
        if !matches!(
            policy.session_tenant,
            SessionTenantPolicy::SingleAccountDefault
        ) {
            bail!("Spoolman deductions send only for the single-account installation");
        }
        let client = owner.spoolman_deductions(policy)?;
        let wake = Arc::new(Notify::new());
        let notify = Arc::clone(&wake);
        client.on_burst(Arc::new(move || notify.notify_one()));
        Ok(Self {
            client,
            inventory,
            wake,
        })
    }

    /// Wakes an idle worker. Stopping uses the same notify.
    pub fn waker(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Sends pending rows until `cancelled` is set. An idle worker waits for
    /// a burst's wake or [`RECHECK`].
    pub fn spawn(self, cancelled: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while !cancelled.load(Ordering::Acquire) {
                match self.turn(&cancelled).await {
                    Ok(Turn::Wait) => {
                        if cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        let _ = tokio::time::timeout(RECHECK, self.wake.notified()).await;
                    }
                    Ok(Turn::Again) => {}
                    Err(error) => {
                        tracing::warn!(error = %error, "spoolman deduction send stopped");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
            }
        })
    }

    /// Claims and sends at most one pending row. `Ok(false)` means nothing
    /// was sendable.
    pub async fn run_next(&self) -> Result<bool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        Ok(matches!(self.turn(&cancelled).await?, Turn::Again))
    }

    async fn turn(&self, cancelled: &Arc<AtomicBool>) -> Result<Turn> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(Turn::Wait);
        }
        let work = self.next(Arc::clone(cancelled)).await?;
        if cancelled.load(Ordering::Acquire) {
            return Ok(Turn::Wait);
        }
        match work {
            SendWork::Idle | SendWork::Held => Ok(Turn::Wait),
            SendWork::Missing { id } => {
                self.fail_without_sending(id, Outcome::IntegrationMissing, cancelled)
                    .await
            }
            SendWork::Disabled { id } => {
                self.fail_without_sending(id, Outcome::IntegrationDisabled, cancelled)
                    .await
            }
            SendWork::Ready {
                id,
                spool_id,
                integration,
                ..
            } => {
                let before = self.inventory.remaining_grams(&integration, spool_id).await;
                if cancelled.load(Ordering::Acquire) {
                    return Ok(Turn::Wait);
                }
                self.send(id, &integration, before, cancelled).await
            }
        }
    }

    async fn fail_without_sending(
        &self,
        id: i64,
        end: Outcome,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Turn> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(Turn::Wait);
        }
        match self.claim(id, None, cancelled).await? {
            ModuleClaim::Held => Ok(Turn::Wait),
            ModuleClaim::Gone => Ok(Turn::Again),
            ModuleClaim::Claimed(claimed) => {
                self.record(claimed.id, end).await?;
                Ok(Turn::Again)
            }
        }
    }

    async fn send(
        &self,
        id: i64,
        integration: &SpoolmanIntegration,
        grams_before: Option<f64>,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Turn> {
        match self.claim(id, grams_before, cancelled).await? {
            ModuleClaim::Held => Ok(Turn::Wait),
            ModuleClaim::Gone => Ok(Turn::Again),
            ModuleClaim::Claimed(claimed) => {
                // The claim has committed. Finish this send even if stopping.
                // An abort here leaves the row `sending`; the next open marks
                // it unknown and nothing is sent again.
                let end = stored(
                    self.inventory
                        .use_weight(integration, claimed.spool_id, claimed.grams)
                        .await,
                );
                self.record(claimed.id, end).await?;
                Ok(Turn::Again)
            }
        }
    }

    async fn next(&self, cancelled: Arc<AtomicBool>) -> Result<SendWork> {
        let client = self.client.clone();
        spawn_storage(move || client.next_send(&cancelled, STORAGE_WAIT)).await
    }

    async fn claim(
        &self,
        id: i64,
        grams_before: Option<f64>,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<ModuleClaim> {
        let client = self.client.clone();
        let cancelled = Arc::clone(cancelled);
        spawn_storage(move || client.claim_for_send(id, grams_before, &cancelled, STORAGE_WAIT))
            .await
    }

    async fn record(&self, id: i64, end: Outcome) -> Result<bool> {
        let client = self.client.clone();
        let cancelled = AtomicBool::new(false);
        let finished =
            spawn_storage(move || client.finish_for_send(id, end, &cancelled, STORAGE_WAIT))
                .await?;
        if finished {
            match end {
                Outcome::Applied => {
                    tracing::debug!(deduction_id = id, "spoolman deduction applied");
                }
                other => tracing::warn!(
                    deduction_id = id,
                    outcome = ?other,
                    "spoolman deduction not applied"
                ),
            }
        } else {
            tracing::warn!(
                deduction_id = id,
                "spoolman deduction was no longer sending"
            );
        }
        Ok(finished)
    }
}

fn stored(result: UseResult) -> Outcome {
    match result {
        UseResult::Applied => Outcome::Applied,
        UseResult::Unreachable => Outcome::Unreachable,
        UseResult::ReplyLost => Outcome::ReplyLost,
        UseResult::Refused(status) if (100..=599).contains(&status) => Outcome::Refused(status),
        UseResult::Refused(_) => Outcome::Unreachable,
    }
}

async fn spawn_storage<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, DeductionError> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| anyhow!("spoolman deduction storage task failed: {error}"))?
        .map_err(Into::into)
}
