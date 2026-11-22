//! Host status to Printer Checkoff (GRE-1897): the Rust-only runtime's port of
//! Node's `POST /printer-checkoff/reconcile`, run by the watchers instead of a
//! page poll. Each answered host status that differs from the last one written
//! for that host goes to storage's [`ReconcileClient`]. At start and after
//! falling behind, it resubscribes, reconciles one snapshot, and skips the
//! buffered transitions that snapshot holds, so an older status never lands
//! after a newer one. A failed write keeps that host's statuses in order and
//! retries them with a capped backoff, because a printer that keeps reporting
//! the same `complete` sends no new transition. See
//! `docs/farm-reads.md#checkoff-reconcile`. Nothing here checks a unit off,
//! dispatches a send, or polls a printer.
use crate::printer_status::observe;
use pp_gateway::{PrinterHostStatus, PrinterObservations, Transition};
use pp_storage::{
    checkoff_progress::reconcile::{HostReport, ReconcileClient},
    read_model::farm::Observation,
};
use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_util::sync::CancellationToken;

const WRITE_WAIT: Duration = Duration::from_secs(5);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// Statuses kept per host while its writes fail. A run of one state and file
/// keeps only its newest, so this holds many prints' worth of changes.
const KEPT: usize = 32;

/// Where the observer reads host statuses.
pub(crate) trait Statuses {
    /// Subscribes afresh, then returns every host's newest status and the
    /// `seq` of the last transition that includes.
    fn resync(&mut self) -> (Vec<(String, PrinterHostStatus)>, u64);
    async fn next(&mut self) -> Result<Transition, RecvError>;
    /// The status as a reconcile reads it, or `None` for a host the
    /// watchers never reached.
    fn report(&self, id: &str, status: &PrinterHostStatus) -> Option<HostReport>;
}

/// Where the observer writes. `false` means the write did not land.
pub(crate) trait Reconcile {
    async fn reconcile(&self, id: &str, report: HostReport) -> bool;
}

/// Runs until the watchers stop or `cancelled` fires.
pub(crate) async fn run(
    client: ReconcileClient,
    observations: PrinterObservations,
    cancelled: CancellationToken,
) {
    let transitions = observations.transitions();
    let statuses = Watched {
        observations,
        transitions,
    };
    observe_statuses(statuses, Storage(client), cancelled).await;
}

/// Each host's statuses not yet written, oldest first, and the last one
/// written.
#[derive(Default)]
struct Hosts {
    queued: HashMap<String, VecDeque<HostReport>>,
    written: HashMap<String, HostReport>,
}

impl Hosts {
    /// Queues `report` for host `id` unless it repeats the newest one.
    fn push(&mut self, id: &str, report: HostReport) {
        let newest = self.queued.get(id).and_then(VecDeque::back);
        if newest.or_else(|| self.written.get(id)) == Some(&report) {
            return;
        }
        let queue = self.queued.entry(id.to_owned()).or_default();
        let same =
            |back: &HostReport| back.state == report.state && back.filename == report.filename;
        if queue.back().is_some_and(same) {
            queue.pop_back();
        }
        queue.push_back(report);
        if queue.len() > KEPT {
            queue.pop_front();
        }
    }

    /// Writes host `id`'s queue in order and stops at the first failure.
    /// `true` when the queue is empty.
    async fn flush(&mut self, id: &str, writer: &impl Reconcile) -> bool {
        let Some(queue) = self.queued.get_mut(id) else {
            return true;
        };
        while let Some(report) = queue.front().cloned() {
            if !writer.reconcile(id, report.clone()).await {
                return false;
            }
            queue.pop_front();
            self.written.insert(id.to_owned(), report);
        }
        self.queued.remove(id);
        true
    }
}

pub(crate) async fn observe_statuses(
    mut statuses: impl Statuses,
    writer: impl Reconcile,
    cancelled: CancellationToken,
) {
    let mut hosts = Hosts::default();
    let mut retry = FIRST_RETRY;
    let mut retry_at: Option<tokio::time::Instant> = None;
    let mut resync = true;
    let mut mark = 0;
    loop {
        if resync {
            resync = false;
            let (snapshot, seen) = statuses.resync();
            mark = seen;
            for (id, status) in snapshot {
                if cancelled.is_cancelled() {
                    return;
                }
                if let Some(report) = statuses.report(&id, &status) {
                    hosts.push(&id, report);
                    hosts.flush(&id, &writer).await;
                }
            }
        }
        // A fixed deadline, so a stream of new statuses never postpones it.
        if hosts.queued.is_empty() {
            (retry, retry_at) = (FIRST_RETRY, None);
        } else if retry_at.is_none() {
            retry_at = Some(tokio::time::Instant::now() + retry);
        }
        let due = retry_at.unwrap_or_else(tokio::time::Instant::now);
        let next = tokio::select! {
            () = cancelled.cancelled() => return,
            () = tokio::time::sleep_until(due), if retry_at.is_some() => None,
            next = statuses.next() => Some(next),
        };
        match next {
            None => {
                let waiting: Vec<String> = hosts.queued.keys().cloned().collect();
                for id in waiting {
                    if cancelled.is_cancelled() {
                        return;
                    }
                    hosts.flush(&id, &writer).await;
                }
                retry = (retry * 2).min(LAST_RETRY);
                retry_at = None;
            }
            Some(Ok(transition)) if transition.seq > mark => {
                let id = transition.id;
                if let Some(report) = statuses.report(&id, &transition.status) {
                    // A host still retrying waits for its turn, in order.
                    let behind = hosts.queued.contains_key(&id);
                    hosts.push(&id, report);
                    if !behind {
                        hosts.flush(&id, &writer).await;
                    }
                }
            }
            Some(Ok(_)) => {}
            Some(Err(RecvError::Lagged(_))) => resync = true,
            Some(Err(RecvError::Closed)) => return,
        }
    }
}

struct Watched {
    observations: PrinterObservations,
    transitions: broadcast::Receiver<Transition>,
}

impl Statuses for Watched {
    fn resync(&mut self) -> (Vec<(String, PrinterHostStatus)>, u64) {
        self.transitions = self.observations.transitions();
        let (map, mark) = self.observations.snapshot();
        (
            map.iter().map(|(id, s)| (id.clone(), s.clone())).collect(),
            mark,
        )
    }

    async fn next(&mut self) -> Result<Transition, RecvError> {
        self.transitions.recv().await
    }

    /// The status as the farm read shows it. `None` for the "Awaiting
    /// status" placeholder, HTTPS and redirecting hosts, and Bambu while its
    /// feature is off.
    fn report(&self, id: &str, status: &PrinterHostStatus) -> Option<HostReport> {
        match observe(&self.observations, id, Some(status.clone())) {
            Observation::Seen {
                state,
                progress_percent,
                filename,
                ..
            } => Some(HostReport {
                state,
                filename,
                progress: progress_percent.map(f64::from),
                cancelled: status.cancelled,
            }),
            _ => None,
        }
    }
}

struct Storage(ReconcileClient);

impl Reconcile for Storage {
    async fn reconcile(&self, id: &str, report: HostReport) -> bool {
        let (client, id) = (self.0.clone(), id.to_owned());
        tokio::task::spawn_blocking(move || client.reconcile(&id, report, WRITE_WAIT))
            .await
            .is_ok_and(|written| written.is_ok())
    }
}

#[cfg(test)]
mod tests;
