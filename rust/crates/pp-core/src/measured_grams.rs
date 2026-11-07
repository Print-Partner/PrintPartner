//! Measured filament at the end of a print (GRE-1165, part c).
//!
//! An observer on the status watchers' ordered transitions. When a Moonraker
//! host goes to `complete` straight from `printing` or `paused` (an outage in
//! between does not count as a new state), it asks the printer once for the
//! filament length the print used and hands it to storage, which converts it
//! with the send's grams per millimetre for the newest watching Checkoff link
//! of that host whose file is the finished one. A `complete` the watchers did
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
use std::{collections::HashMap, time::Duration};
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
                    record(&settings, &reads, &grams, &transition.id, &filename).await;
                }
            }
            // A missed change may hide a finish; its estimate stays.
            Err(RecvError::Lagged(_)) => active.clear(),
            Err(RecvError::Closed) => break,
        }
    }
}

/// The finished file's name when `transition` ends a print this process saw
/// running. `active` keeps each host's last answered state.
fn finished(
    active: &mut HashMap<String, PrinterHostState>,
    transition: &Transition,
) -> Option<String> {
    let state = transition.status.state.clone();
    if matches!(state, PrinterHostState::Offline | PrinterHostState::Unknown) {
        return None;
    }
    let before = active.insert(transition.id.clone(), state.clone());
    let was_running = matches!(
        before,
        Some(PrinterHostState::Printing | PrinterHostState::Paused)
    );
    (state == PrinterHostState::Complete && was_running)
        .then(|| transition.status.filename.clone())
        .flatten()
        .filter(|filename| !filename.trim().is_empty())
}

/// Reads host `integration_id`'s filament length once and records it for
/// `filename`'s watching link. A failed read or write leaves the estimate.
pub(crate) async fn record(
    settings: &SettingsClient,
    reads: &HostReads,
    grams: &PrintGramsClient,
    integration_id: &str,
    filename: &str,
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
    let measured_mm = reads
        .filament_used(&target)
        .await
        .filter(|mm| mm.is_finite() && *mm > 0.0)?;
    let (grams, id, filename) = (
        grams.clone(),
        integration_id.to_owned(),
        filename.to_owned(),
    );
    tokio::task::spawn_blocking(move || grams.measured(&id, &filename, measured_mm, WRITE_WAIT))
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
        assert_eq!(step("a", Complete, Some("a.gcode")), Some("a.gcode".into()));
        assert_eq!(
            step("a", Complete, Some("a.gcode")),
            None,
            "same state again"
        );
        assert_eq!(step("b", Paused, Some("b.gcode")), None);
        assert_eq!(step("b", Complete, Some(" ")), None, "no filename");
        assert_eq!(step("c", Printing, Some("c.gcode")), None);
        assert_eq!(step("c", Error, Some("c.gcode")), None);
        assert_eq!(
            step("c", Complete, Some("c.gcode")),
            None,
            "error is not running"
        );
        assert_eq!(step("d", Paused, Some("d.gcode")), None);
        assert_eq!(step("d", Complete, Some("d.gcode")), Some("d.gcode".into()));
    }
}
