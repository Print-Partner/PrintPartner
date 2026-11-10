//! Measured filament at the end of a print (GRE-1165, part c).
//!
//! An observer on the status watchers' ordered transitions. When a Moonraker
//! or PrusaLink host goes to `complete` straight from `printing` or `paused` (an outage in
//! between does not count as a new state), it asks the printer once for the
//! finished print's file name and filament length. The length counts only
//! when that same read still reports the print finished and, when the status
//! named a file, names the same file, so a slow observer never records the
//! next print's usage. A status without a file name still counts: PrusaLink's
//! `/api/v1/status` job carries no `file`, so its name comes from that
//! finished read alone. Storage converts it
//! with the send's grams per millimetre for the newest `watching`,
//! `awaiting_verify` or `verified` Checkoff link
//! of that host whose file is the finished one and that existed when the
//! finish was seen. A link is measured once; a later finish of the same file
//! name leaves its measurement. A `complete` the watchers did
//! not see begin, such as one already showing at startup, records nothing, so
//! the slicer estimate stays (Alfala, 2026-11-07). Nothing here changes
//! Checkoff state, dispatches a send, or retries a read.
use crate::printer_host::{Host, host};
use pp_gateway::{PrinterHostState, PrinterObservations, Transition, printer_host::HostReads};
use pp_storage::{
    SettingsClient,
    print_grams::{Measurement, PrintGramsClient},
    printer_hosts,
};
use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

const READER_WAIT: Duration = Duration::from_millis(500);
const WRITE_WAIT: Duration = Duration::from_secs(5);

/// Runs until the watchers stop or `cancelled` fires.
pub(crate) async fn observe(
    settings: SettingsClient,
    reads: HostReads,
    grams: PrintGramsClient,
    observations: PrinterObservations,
    cancelled: CancellationToken,
) {
    let mut transitions = observations.transitions();
    let mut active = HashMap::new();
    loop {
        let transition = tokio::select! {
            () = cancelled.cancelled() => break,
            transition = transitions.recv() => transition,
        };
        match transition {
            Ok(transition) => {
                if let Some(filename) = finished(&mut active, &transition) {
                    let (id, at) = (&transition.id, SystemTime::now());
                    record(&settings, &reads, &grams, id, filename.as_deref(), at).await;
                }
            }
            // A missed change may hide a finish; its estimate stays.
            Err(RecvError::Lagged(_)) => active.clear(),
            Err(RecvError::Closed) => break,
        }
    }
}

/// `Some` when `transition` ends a print this process saw running, holding
/// the file name the status reported, if any. `active` keeps each host's last
/// answered state.
fn finished(
    active: &mut HashMap<String, PrinterHostState>,
    transition: &Transition,
) -> Option<Option<String>> {
    let state = transition.status.state.clone();
    if matches!(state, PrinterHostState::Offline | PrinterHostState::Unknown) {
        return None;
    }
    let before = active.insert(transition.id.clone(), state.clone());
    let was_running = matches!(
        before,
        Some(PrinterHostState::Printing | PrinterHostState::Paused)
    );
    (state == PrinterHostState::Complete && was_running).then(|| {
        transition
            .status
            .filename
            .clone()
            .filter(|filename| !filename.trim().is_empty())
    })
}

/// Reads host `integration_id`'s finished print once and records its
/// filament length for the open link of the file that read names, as
/// long as the link existed when the print was seen finishing at
/// `finished_at`. When the status named `filename`, the read must name the
/// same file. A failed read or write, a printer no longer reporting a
/// finished print (or `filename`), or no positive length leaves the estimate.
pub(crate) async fn record(
    settings: &SettingsClient,
    reads: &HostReads,
    grams: &PrintGramsClient,
    integration_id: &str,
    filename: Option<&str>,
    finished_at: SystemTime,
) -> Option<Measurement> {
    let (client, id) = (settings.clone(), integration_id.to_owned());
    let stored =
        tokio::task::spawn_blocking(move || printer_hosts::integration(&client, READER_WAIT, &id))
            .await
            .ok()?
            .ok()??;
    let Some(Host::Read(target)) = host(stored) else {
        return None;
    };
    let (reported, measured_mm) = reads.finished_filament(&target).await?;
    let base = |name: &str| {
        let name = name.trim().replace('\\', "/");
        name.rsplit('/').next().unwrap_or_default().to_lowercase()
    };
    let other_file = filename.is_some_and(|filename| base(&reported) != base(filename));
    if base(&reported).is_empty() || other_file || !(measured_mm.is_finite() && measured_mm > 0.0) {
        return None;
    }
    let (grams, id, filename) = (grams.clone(), integration_id.to_owned(), reported);
    tokio::task::spawn_blocking(move || {
        grams.measured(&id, &filename, measured_mm, finished_at, WRITE_WAIT)
    })
    .await
    .ok()?
    .ok()?
}

#[cfg(test)]
mod tests {
    use super::*;
    use pp_gateway::PrinterHostStatus;

    fn change(id: &str, state: PrinterHostState, filename: Option<&str>) -> Transition {
        Transition {
            seq: 0,
            id: id.into(),
            status: PrinterHostStatus {
                state,
                filename: filename.map(str::to_owned),
                ..PrinterHostStatus::unknown("")
            },
        }
    }

    #[test]
    fn gre_1165_only_a_print_seen_running_finishes() {
        use PrinterHostState::*;
        let mut active = HashMap::new();
        let mut step = |id, state, filename| finished(&mut active, &change(id, state, filename));
        assert_eq!(
            step("a", Complete, Some("old.gcode")),
            None,
            "complete at startup"
        );
        assert_eq!(step("a", Printing, Some("a.gcode")), None);
        assert_eq!(step("a", Offline, None), None);
        assert_eq!(
            step("a", Complete, Some("a.gcode")),
            Some(Some("a.gcode".into()))
        );
        assert_eq!(
            step("a", Complete, Some("a.gcode")),
            None,
            "same state again"
        );
        assert_eq!(step("b", Paused, Some("b.gcode")), None);
        assert_eq!(
            step("b", Complete, Some(" ")),
            Some(None),
            "a blank name still finishes; the finished read names the file"
        );
        assert_eq!(step("e", Printing, None), None);
        assert_eq!(
            step("e", Complete, None),
            Some(None),
            "PrusaLink's status job has no file"
        );
        assert_eq!(step("c", Printing, Some("c.gcode")), None);
        assert_eq!(step("c", Error, Some("c.gcode")), None);
        assert_eq!(
            step("c", Complete, Some("c.gcode")),
            None,
            "error is not running"
        );
        assert_eq!(step("d", Paused, Some("d.gcode")), None);
        assert_eq!(
            step("d", Complete, Some("d.gcode")),
            Some(Some("d.gcode".into()))
        );
    }
}
