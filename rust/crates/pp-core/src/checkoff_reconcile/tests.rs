use super::*;
use PrinterHostState::*;
use pp_gateway::PrinterHostState;
use pp_storage::read_model::farm::HostState;
use std::sync::{Arc, Mutex};

type Snapshot = (Vec<(String, PrinterHostStatus)>, u64);

/// Replays scripted snapshots and transitions, then waits forever.
struct Script {
    snapshots: VecDeque<Snapshot>,
    events: VecDeque<Result<Transition, RecvError>>,
}

impl Statuses for Script {
    fn resync(&mut self) -> Snapshot {
        self.snapshots.pop_front().unwrap_or_default()
    }

    async fn next(&mut self) -> Result<Transition, RecvError> {
        match self.events.pop_front() {
            Some(event) => event,
            None => std::future::pending().await,
        }
    }

    fn report(&self, _: &str, status: &PrinterHostStatus) -> Option<HostReport> {
        let state = match status.state {
            Idle => HostState::Idle,
            Printing => HostState::Printing,
            Paused => HostState::Paused,
            Complete => HostState::Complete,
            Error => HostState::Error,
            Offline => HostState::Offline,
            Unknown => HostState::Unknown,
        };
        let filename = status.filename.clone();
        let cancelled = status.cancelled;
        Some(HostReport {
            state,
            filename,
            progress: None,
            cancelled,
        })
    }
}

/// Records each write as `host state file`, with ` failed` when it did not
/// land; the first `failures` writes fail, and write `cancel_at` cancels.
#[derive(Clone, Default)]
struct Log {
    calls: Arc<Mutex<Vec<String>>>,
    failures: Arc<Mutex<usize>>,
    cancel_at: Option<(usize, CancellationToken)>,
}

impl Reconcile for Log {
    async fn reconcile(&self, id: &str, report: HostReport) -> bool {
        let mut failures = self.failures.lock().unwrap();
        let landed = *failures == 0;
        *failures = failures.saturating_sub(1);
        let file = report.filename.unwrap_or_default();
        let failed = if landed { "" } else { " failed" };
        let mut calls = self.calls.lock().unwrap();
        calls.push(format!("{id} {:?} {file}{failed}", report.state));
        if let Some((at, token)) = &self.cancel_at
            && calls.len() == *at
        {
            token.cancel();
        }
        landed
    }
}

fn status(state: PrinterHostState, filename: &str) -> PrinterHostStatus {
    let filename = Some(filename.to_owned());
    PrinterHostStatus {
        state,
        filename,
        ..PrinterHostStatus::unknown("")
    }
}

fn at(seq: u64, state: PrinterHostState, filename: &str) -> Result<Transition, RecvError> {
    let status = status(state, filename);
    Ok(Transition {
        seq,
        id: "moon".into(),
        status,
    })
}

fn host(state: PrinterHostState, filename: &str) -> Vec<(String, PrinterHostStatus)> {
    vec![("moon".into(), status(state, filename))]
}

/// Runs the observer over the script for two simulated minutes and returns
/// its writes, joined with `; `.
async fn writes(
    snapshots: Vec<Snapshot>,
    events: Vec<Result<Transition, RecvError>>,
    log: Log,
) -> String {
    let cancelled = log
        .cancel_at
        .as_ref()
        .map_or_else(CancellationToken::new, |c| c.1.clone());
    let script = Script {
        snapshots: snapshots.into(),
        events: events.into(),
    };
    let observing = tokio::spawn(observe_statuses(script, log.clone(), cancelled.clone()));
    tokio::time::sleep(Duration::from_secs(120)).await;
    cancelled.cancel();
    observing.await.unwrap();
    log.calls.lock().unwrap().join("; ")
}

fn failing(failures: usize) -> Log {
    Log {
        failures: Arc::new(Mutex::new(failures)),
        ..Log::default()
    }
}

#[tokio::test(start_paused = true)]
async fn gre_1897_the_start_snapshot_skips_transitions_it_already_holds() {
    let events = vec![at(4, Printing, "a"), at(5, Complete, "a"), at(6, Idle, "a")];
    let calls = writes(vec![(host(Complete, "a"), 5)], events, Log::default()).await;
    assert_eq!(calls, "moon Complete a; moon Idle a");
}

#[tokio::test(start_paused = true)]
async fn gre_1897_a_lag_resyncs_and_never_replays_an_older_status() {
    let snapshots = vec![(Vec::new(), 0), (host(Idle, "a"), 9)];
    let events = vec![
        at(1, Printing, "a"),
        Err(RecvError::Lagged(3)),
        at(7, Complete, "a"),
        at(9, Idle, "a"),
        at(10, Printing, "b"),
    ];
    let calls = writes(snapshots, events, Log::default()).await;
    // The buffered finish at seq 7 is older than the snapshot's idle.
    assert_eq!(calls, "moon Printing a; moon Idle a; moon Printing b");
}

#[tokio::test(start_paused = true)]
async fn gre_1897_a_failed_write_retries_the_finish_with_no_new_status() {
    // The printer keeps reporting the same complete, which is no transition.
    let calls = writes(vec![], vec![at(1, Complete, "a")], failing(2)).await;
    assert_eq!(
        calls,
        "moon Complete a failed; moon Complete a failed; moon Complete a"
    );
}

#[tokio::test(start_paused = true)]
async fn gre_1897_a_later_status_waits_behind_a_failed_one() {
    let events = vec![
        at(1, Complete, "a"),
        at(2, Printing, "b"),
        at(3, Printing, "b"),
    ];
    let calls = writes(vec![], events, failing(1)).await;
    assert_eq!(
        calls,
        "moon Complete a failed; moon Complete a; moon Printing b"
    );
}

#[tokio::test(start_paused = true)]
async fn gre_1897_shutdown_stops_between_hosts() {
    let hosts = ["a", "b", "c"]
        .map(|id| (id.to_owned(), status(Complete, "a")))
        .to_vec();
    let log = Log {
        cancel_at: Some((1, CancellationToken::new())),
        ..Log::default()
    };
    let calls = writes(vec![(hosts, 3)], vec![], log).await;
    assert_eq!(calls, "a Complete a");
}
