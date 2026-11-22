//! Host status to Printer Checkoff (GRE-1897), the Rust-only runtime's port
//! of Node's `POST /printer-checkoff/reconcile`, run by the watchers instead
//! of a page poll. Every answered host status that differs from the last one
//! reconciled for that host goes to storage's [`ReconcileClient`]: a finished
//! print makes its `watching` link `awaiting_verify`, so Today offers to
//! confirm the bed, and an error or a cancel makes it `host_failed`. The first
//! status of each host is reconciled too, so a print that finished while the
//! app was closed is still confirmable. A changed temperature alone writes
//! nothing. A failed write is retried on that host's next status change.
//! Nothing here checks a unit off, dispatches a send, or polls a printer.
use crate::printer_status::observe;
use pp_gateway::{PrinterHostStatus, PrinterObservations};
use pp_storage::{
    checkoff_progress::reconcile::{HostReport, ReconcileClient},
    read_model::farm::Observation,
};
use std::{collections::HashMap, time::Duration};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

const WRITE_WAIT: Duration = Duration::from_secs(5);

/// Runs until the watchers stop or `cancelled` fires.
pub(crate) async fn run(
    client: ReconcileClient,
    observations: PrinterObservations,
    cancelled: CancellationToken,
) {
    let mut transitions = observations.transitions();
    let mut reconciled = HashMap::new();
    let mut resync = true;
    loop {
        if resync {
            resync = false;
            reconciled.clear();
            for (id, status) in observations.all().iter() {
                apply(&client, &observations, &mut reconciled, id, status).await;
            }
        }
        let transition = tokio::select! {
            () = cancelled.cancelled() => break,
            transition = transitions.recv() => transition,
        };
        match transition {
            Ok(transition) => {
                let (id, status) = (&transition.id, &transition.status);
                apply(&client, &observations, &mut reconciled, id, status).await;
            }
            // A missed change may hide a finish: reconcile every host again.
            Err(RecvError::Lagged(_)) => resync = true,
            Err(RecvError::Closed) => break,
        }
    }
}

/// Reconciles host `id` against `status` unless it repeats the last report
/// reconciled for that host.
async fn apply(
    client: &ReconcileClient,
    observations: &PrinterObservations,
    reconciled: &mut HashMap<String, HostReport>,
    id: &str,
    status: &PrinterHostStatus,
) {
    let Some(report) = report(observations, id, status) else {
        return;
    };
    if reconciled.get(id) == Some(&report) {
        return;
    }
    let (writer, host, sent) = (client.clone(), id.to_owned(), report.clone());
    let written = tokio::task::spawn_blocking(move || writer.reconcile(&host, sent, WRITE_WAIT))
        .await
        .is_ok_and(|result| result.is_ok());
    if written {
        reconciled.insert(id.to_owned(), report);
    } else {
        reconciled.remove(id);
    }
}

/// The status as the farm read shows it, or `None` for a host the watchers
/// never reached: the "Awaiting status" placeholder, HTTPS and redirecting
/// hosts, and Bambu while its feature is off.
pub(crate) fn report(
    observations: &PrinterObservations,
    id: &str,
    status: &PrinterHostStatus,
) -> Option<HostReport> {
    match observe(observations, id, Some(status.clone())) {
        Observation::Seen {
            state,
            progress_percent,
            filename,
            ..
        } => Some(HostReport {
            state,
            filename,
            progress: progress_percent.map(f64::from),
        }),
        _ => None,
    }
}
