use super::*;
use PrinterHostState::*;
use pp_gateway::PrinterHostState;
use std::sync::{Arc, Mutex};

type Snapshot = (Vec<(String, PrinterHostStatus)>, u64);

/// A real broadcast receiver behind a start snapshot.
struct Channel {
    transitions: broadcast::Receiver<Transition>,
    snapshot: Option<Snapshot>,
}

impl Statuses for Channel {
    fn resync(&mut self) -> Snapshot {
        self.snapshot.take().expect("one snapshot, at start")
    }

    async fn next(&mut self) -> Result<Transition, RecvError> {
        self.transitions.recv().await
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
        let (filename, progress) = (status.filename.clone(), status.progress);
        Some(HostReport {
            state,
            filename,
            progress,
            cancelled: status.cancelled,
        })
    }
}

/// Records each write as `host state file`, plus ` cancelled` and
/// ` failed` when they apply; the first `failures` writes fail, and write
/// `cancel_at` cancels the observer.
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
        let cancel = if report.cancelled { " cancelled" } else { "" };
        let failed = if landed { "" } else { " failed" };
        let mut calls = self.calls.lock().unwrap();
        calls.push(format!("{id} {:?} {file}{cancel}{failed}", report.state));
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

fn at(seq: u64, status: PrinterHostStatus) -> Transition {
    Transition {
        seq,
        id: "moon".into(),
        status,
    }
}

/// Transitions numbered from 1.
fn numbered(statuses: Vec<PrinterHostStatus>) -> Vec<Transition> {
    (1..)
        .zip(statuses)
        .map(|(seq, status)| at(seq, status))
        .collect()
}

/// Sends `events` into a broadcast channel holding `capacity`, runs the
/// observer from `snapshot` for two simulated minutes, and returns its
/// writes joined with `; `.
async fn writes(snapshot: Snapshot, events: Vec<Transition>, capacity: usize, log: Log) -> String {
    let (sender, transitions) = broadcast::channel(capacity);
    for event in events {
        sender.send(event).unwrap();
    }
    let cancelled = log
        .cancel_at
        .as_ref()
        .map_or_else(CancellationToken::new, |c| c.1.clone());
    let channel = Channel {
        transitions,
        snapshot: Some(snapshot),
    };
    let observing = tokio::spawn(observe_statuses(channel, log.clone(), cancelled.clone()));
    tokio::time::sleep(Duration::from_secs(120)).await;
    cancelled.cancel();
    observing.await.unwrap();
    drop(sender);
    log.calls.lock().unwrap().join("; ")
}

fn failing(failures: usize) -> Log {
    Log {
        failures: Arc::new(Mutex::new(failures)),
        ..Log::default()
    }
}

fn at_rest() -> Snapshot {
    (Vec::new(), 0)
}

#[tokio::test(start_paused = true)]
async fn gre_1897_the_start_snapshot_skips_transitions_it_already_holds() {
    let snapshot = (vec![("moon".into(), status(Complete, "a"))], 2);
    let events = numbered(vec![
        status(Printing, "a"),
        status(Complete, "a"),
        status(Idle, "a"),
    ]);
    let calls = writes(snapshot, events, 16, Log::default()).await;
    assert_eq!(calls, "moon Complete a; moon Idle a");
}

#[tokio::test(start_paused = true)]
async fn gre_1897_a_lagged_reader_still_lands_the_retained_finish() {
    // Five changes into a buffer of four: the first is lost, and the finish
    // of a print last seen at 42% is still buffered behind the newer idle.
    let seen = |progress| PrinterHostStatus {
        progress: Some(progress),
        ..status(Printing, "a")
    };
    let events = numbered(vec![
        seen(10.0),
        seen(20.0),
        seen(42.0),
        status(Complete, "a"),
        status(Idle, "a"),
    ]);
    let calls = writes(at_rest(), events, 4, Log::default()).await;
    assert_eq!(
        calls,
        "moon Printing a; moon Printing a; moon Complete a; moon Idle a"
    );
}

#[tokio::test(start_paused = true)]
async fn gre_1897_a_failed_write_retries_the_finish_with_no_new_status() {
    // The printer keeps reporting the same complete, which is no transition.
    let calls = writes(
        at_rest(),
        numbered(vec![status(Complete, "a")]),
        16,
        failing(2),
    )
    .await;
    assert_eq!(
        calls,
        "moon Complete a failed; moon Complete a failed; moon Complete a"
    );
}

#[tokio::test(start_paused = true)]
async fn gre_1897_later_statuses_wait_behind_a_failed_one_and_keep_its_cancel() {
    let events = vec![
        status(Complete, "a"),
        status(Printing, "b"),
        status(Printing, "b"),
    ];
    let calls = writes(at_rest(), numbered(events), 16, failing(1)).await;
    assert_eq!(
        calls,
        "moon Complete a failed; moon Complete a; moon Printing b"
    );

    // An explicit cancel that fails to write is not merged into the plain
    // idle that follows it.
    let cancel = PrinterHostStatus {
        cancelled: true,
        ..status(Idle, "a")
    };
    let calls = writes(
        at_rest(),
        numbered(vec![cancel, status(Idle, "a")]),
        16,
        failing(1),
    )
    .await;
    assert_eq!(
        calls,
        "moon Idle a cancelled failed; moon Idle a cancelled; moon Idle a"
    );
}

#[tokio::test(start_paused = true)]
async fn gre_1897_a_long_backlog_drops_progress_and_never_the_finish() {
    let mut events = vec![status(Complete, "a")];
    for step in 0..40 {
        events.push(status(if step % 2 == 0 { Printing } else { Paused }, "b"));
    }
    let calls = writes(at_rest(), numbered(events), 64, failing(1)).await;
    let calls: Vec<&str> = calls.split("; ").collect();
    assert_eq!(calls[..2], ["moon Complete a failed", "moon Complete a"]);
    assert_eq!(calls.len(), 2 + KEPT - 1, "{calls:?}");
    assert_eq!(calls.last(), Some(&"moon Paused b"));
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
    let calls = writes((hosts, 3), Vec::new(), 16, log).await;
    assert_eq!(calls, "a Complete a");
}
