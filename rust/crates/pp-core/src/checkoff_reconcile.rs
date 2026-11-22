//! Host status to Printer Checkoff (GRE-1897): the Rust-only runtime's port of
//! Node's `POST /printer-checkoff/reconcile`, run by the watchers instead of a
//! page poll. Each answered host status that differs from the last one written
//! for that host goes to storage's [`ReconcileClient`]. At start it subscribes,
//! reconciles one snapshot, and skips the buffered transitions that snapshot
//! holds. After falling behind it keeps reading from the oldest change still
//! buffered, so retained changes land in order. A failed write keeps that host's statuses in order and
//! retries them with a capped backoff, because a printer that keeps reporting
//! the same `complete` sends no new transition. Storage checks recorded
//! finishes when a `complete` is forwarded after a reconnect or restart. See
//! `docs/farm-reads.md#checkoff-reconcile`. Nothing here checks a unit off,
//! dispatches a send, or polls a printer.
use crate::printer_status::observe;
use pp_gateway::{PrinterHostStatus, PrinterObservations, Transition};
use pp_storage::{
    checkoff_progress::reconcile::{HostReport, ReconcileClient},
    read_model::farm::{HostState, Observation},
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
/// Statuses kept per host while its writes fail, before older progress
/// reports start to go. A finish, an error, an idle or a cancel never goes.
const KEPT: usize = 32;

/// Where the observer reads host statuses.
pub(crate) trait Statuses {
    /// Every host's newest status and the `seq` of the last transition that
    /// includes, read once at start after subscribing.
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
    /// Queues `report` for host `id` unless it repeats the newest one. A
    /// progress report replaces the queued one before it for the same state
    /// and file, keeping the higher progress. Past [`KEPT`], the oldest
    /// progress report with a newer one for its file in the same run (before
    /// any finish, idle or error) goes and hands on its progress, else the
    /// oldest offline or unknown spell; nothing else is ever dropped.
    fn push(&mut self, id: &str, mut report: HostReport) {
        let newest = self.queued.get(id).and_then(VecDeque::back);
        if newest.or_else(|| self.written.get(id)) == Some(&report) {
            return;
        }
        let queue = self.queued.entry(id.to_owned()).or_default();
        if let Some(back) = queue.back()
            && running(back)
            && (&back.state, &back.filename) == (&report.state, &report.filename)
        {
            report.progress = higher(back.progress, report.progress);
            queue.pop_back();
        }
        queue.push_back(report);
        if queue.len() > KEPT {
            // The later progress report of the same run that supersedes `i`.
            let successor = |i: usize| {
                let old: &HostReport = &queue[i];
                let same_run = |(_, new): &(usize, &HostReport)| running(new) || quiet(new);
                let newer =
                    |(_, new): &(usize, &HostReport)| running(new) && new.filename == old.filename;
                let later = queue.iter().enumerate().skip(i + 1).take_while(same_run);
                running(old)
                    .then(|| later.filter(newer).map(|(j, _)| j).next())
                    .flatten()
            };
            let superseded = (0..queue.len()).find_map(|i| successor(i).map(|j| (i, Some(j))));
            let evicted = superseded.or_else(|| queue.iter().position(quiet).map(|i| (i, None)));
            if let Some((index, successor)) = evicted {
                if let Some(j) = successor {
                    queue[j].progress = higher(queue[index].progress, queue[j].progress);
                }
                queue.remove(index);
            }
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

/// A progress report: the host is printing or paused.
fn running(report: &HostReport) -> bool {
    matches!(report.state, HostState::Printing | HostState::Paused)
}

/// An unanswered spell, which neither ends a run nor changes a link.
fn quiet(report: &HostReport) -> bool {
    matches!(report.state, HostState::Offline | HostState::Unknown)
}

fn higher(old: Option<f64>, new: Option<f64>) -> Option<f64> {
    match (old, new) {
        (Some(old), Some(new)) => Some(old.max(new)),
        (old, new) => new.or(old),
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
    let (snapshot, mut mark) = statuses.resync();
    for (id, status) in snapshot {
        if cancelled.is_cancelled() {
            return;
        }
        if let Some(report) = statuses.report(&id, &status) {
            hosts.push(&id, report);
            hosts.flush(&id, &writer).await;
        }
    }
    loop {
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
                mark = transition.seq;
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
            // A change the start snapshot holds, or a lag: the receiver goes
            // on from the oldest change still buffered, so every retained
            // change, a finish included, lands in order.
            Some(Ok(_) | Err(RecvError::Lagged(_))) => {}
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
