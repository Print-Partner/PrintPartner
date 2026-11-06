//! Print-file assignment routes (GRE-1220), at the paths Node's
//! `routes/printer-checkoff.ts` serves at the root and under `/api/v1`:
//!
//! - `POST /printer-checkoff/file-assignments/preview` answers what the file
//!   is and which Required units it suggests, and writes nothing.
//! - `POST /printer-checkoff/file-assignments` records the print as a new
//!   printer Checkoff link through storage's create path.
//!
//! Storage parses the body and resolves the printer in Node's order, then the
//! route reads the file. A `snapshot_token` is redeemed from the
//! [`SnapshotStore`] the files/content route captures into; a token for
//! another Build, printer, path or file name, an expired one, or one whose
//! printer host changed is refused. After a successful assignment the token
//! is spent. Until this runtime serves the upload route and reads a printer's
//! file itself, an `upload_token` is refused as Node refuses an unknown one,
//! and a `remote_path` on a Moonraker or PrusaLink host without a snapshot
//! answers 501 rather than being recorded unread.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, ok_json},
    loopback::PresentedCredential,
    printer_host::{Windows, throttled},
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::Response,
    routing::post,
};
use pp_storage::checkoff_progress::{
    AssignedLink, Assignment, Body, FileRequest, FileRoute, ProgressClient, Request,
    print_file::{Inspected, SNAPSHOT_EXPIRED, SnapshotStore},
};
use serde_json::{Value, json};
use std::sync::Arc;

/// Node's `rateLimit` and JSON body limit for both routes.
const LIMIT: u32 = 20;
const BODY_LIMIT: usize = 1024 * 1024;
const UPLOAD_GONE: &str = "That upload is no longer available; upload the print file again";
const UNPORTED: &str = "Reading a print file off a printer is not available in this runtime yet; reopen the file from its preview";

#[derive(Clone)]
struct App {
    gate: Gate,
    client: ProgressClient,
    snapshots: Arc<SnapshotStore>,
    windows: Arc<Windows>,
}

/// Mounts both routes at `/` and `/api/v1`, each mount with its own rate
/// windows as Node registers them, when `PP_DESKTOP_REBUILD` was exactly `1`
/// or `true`; otherwise nothing.
pub fn file_assignments_router(
    config: AcceptedHttpConfig,
    client: ProgressClient,
    snapshots: Arc<SnapshotStore>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let mount = || {
        Router::new()
            .route("/printer-checkoff/file-assignments/preview", post(preview))
            .route("/printer-checkoff/file-assignments", post(assign))
            .layer(DefaultBodyLimit::max(BODY_LIMIT))
            .with_state(App {
                gate: gate.clone(),
                client: client.clone(),
                snapshots: snapshots.clone(),
                windows: Arc::default(),
            })
    };
    gate.guard(Router::new().merge(mount()).nest("/api/v1", mount()))
}

/// What reading the request's file found.
enum Inspection {
    Inspected(Inspected),
    Refused(&'static str),
    /// Nothing about the file is known, and nothing may be assumed.
    Unreadable,
    /// Node would read the file off the printer; this runtime cannot yet.
    Unported,
}

fn inspect(snapshots: &SnapshotStore, parsed: &FileRequest) -> Inspection {
    if let (Some(token), Some(scope)) = (&parsed.snapshot_token, parsed.scope()) {
        return snapshots
            .read(token, &scope, parsed.printer.source.as_deref())
            .map_or(Inspection::Refused(SNAPSHOT_EXPIRED), Inspection::Inspected);
    }
    if parsed.remote_path.is_some() {
        return if parsed.printer.host_files {
            Inspection::Unported
        } else {
            Inspection::Unreadable
        };
    }
    if parsed.upload_token.is_some() {
        return Inspection::Refused(UPLOAD_GONE);
    }
    Inspection::Unreadable
}

fn conflict(detail: &str) -> Response {
    failure(StatusCode::CONFLICT, detail)
}

/// Storage's answer: a parsed request, or the response that ends this one.
fn answer(
    response: pp_storage::checkoff_progress::Response,
) -> Result<Box<FileRequest>, Box<Response>> {
    match response.body {
        Body::FileRequest(parsed) if response.status == 200 => Ok(parsed),
        body => Err(Box::new(crate::checkoff::present(
            pp_storage::checkoff_progress::Response {
                status: response.status,
                body,
            },
        ))),
    }
}

async fn storage(
    app: &App,
    presented: PresentedCredential,
    request: Request,
) -> Result<pp_storage::checkoff_progress::Response, Box<Response>> {
    let client = app.client.clone();
    match app
        .gate
        .read(presented, move |credential, cancelled, wait| {
            client.apply(credential, request, &cancelled, wait)
        })
        .await
    {
        Some(Ok(response)) => Ok(response),
        Some(Err(error)) => Err(Box::new(crate::accepted::read_error(&error))),
        None => Err(Box::new(crate::accepted::internal())),
    }
}

async fn parse(
    app: &App,
    presented: PresentedCredential,
    body: &[u8],
    route: FileRoute,
) -> Result<Box<FileRequest>, Box<Response>> {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return Err(Box::new(failure(
            StatusCode::BAD_REQUEST,
            "Request body must be a JSON object",
        )));
    };
    answer(storage(app, presented, Request::FileRequest { body, route }).await?)
}

async fn preview(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    if let Some(after) = app.windows.consume("preview", LIMIT) {
        return throttled(after);
    }
    let mut parsed = match parse(&app, presented, &body, FileRoute::Preview).await {
        Ok(parsed) => parsed,
        Err(response) => return *response,
    };
    let mut answer = match parsed.suggestion.take() {
        Some(Value::Object(suggestion)) => suggestion,
        _ => return crate::accepted::internal(),
    };
    match inspect(&app.snapshots, &parsed) {
        Inspection::Refused(detail) => return conflict(detail),
        Inspection::Unported => return failure(StatusCode::NOT_IMPLEMENTED, UNPORTED),
        Inspection::Unreadable => {
            answer.insert("inspected".into(), json!(false));
        }
        Inspection::Inspected(inspected) => {
            let classification = inspected.classification;
            answer.insert("inspected".into(), json!(true));
            answer.insert("classification".into(), json!(classification));
            answer.insert("print_ready".into(), json!(classification.is_print_ready()));
            answer.insert("next_action".into(), json!(classification.next_action()));
        }
    }
    ok_json(&answer)
}

async fn assign(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    if let Some(after) = app.windows.consume("assign", LIMIT) {
        return throttled(after);
    }
    let parsed = match parse(&app, presented.clone(), &body, FileRoute::Assign).await {
        Ok(parsed) => parsed,
        Err(response) => return *response,
    };
    let Some(input) = parsed.assign.clone() else {
        return crate::accepted::internal();
    };
    // A redeemed snapshot is held for this assignment alone, so a concurrent
    // one cannot record the same file, and returns if this one fails.
    let reservation = match (&parsed.snapshot_token, parsed.scope()) {
        (Some(token), Some(scope)) => {
            match app
                .snapshots
                .reserve(token, &scope, parsed.printer.source.as_deref())
            {
                Some(reservation) => Some(reservation),
                None => return conflict(SNAPSHOT_EXPIRED),
            }
        }
        _ => None,
    };
    let inspection = match reservation.as_ref().and_then(|held| held.inspected()) {
        Some(held) => Inspection::Inspected(held.clone()),
        None => inspect(&app.snapshots, &parsed),
    };
    // The bytes decide. A file recorded as already printed is evidence of
    // what was made, so only a file PrintPartner might still print must be
    // print-ready.
    let inspected = match inspection {
        Inspection::Refused(detail) => return conflict(detail),
        Inspection::Unported => return failure(StatusCode::NOT_IMPLEMENTED, UNPORTED),
        Inspection::Unreadable => None,
        Inspection::Inspected(inspected) => {
            if !input.completed && !inspected.classification.is_print_ready() {
                return conflict(inspected.classification.next_action());
            }
            Some(inspected)
        }
    };
    if inspected.is_none() && parsed.filename.to_lowercase().ends_with(".3mf") {
        return conflict(
            "PrintPartner has to read a 3MF to tell whether it holds printer instructions; upload the file so PrintPartner can read it",
        );
    }
    let assignment = Assignment {
        profile_id: parsed.profile_id,
        plan_revision_id: input.plan_revision_id,
        object_names: parsed.object_names.clone(),
        units: input.units,
        mappings: input.mappings,
        retain_unmatched: input.retain_unmatched,
        completed: input.completed,
        link: AssignedLink {
            integration_id: parsed.printer.integration_id.clone(),
            printer_id: parsed.printer.printer_id.clone(),
            host_name: parsed.printer.printer_name.clone(),
            filename: parsed.filename.clone(),
            remote_path: parsed.remote_path.clone(),
            classification: inspected
                .as_ref()
                .map(|inspected| json!(inspected.classification)),
            remote_identity: inspected.map(|inspected| inspected.identity),
        },
        snapshot_source: reservation.as_ref().and(parsed.printer.source.clone()),
    };
    let response = match storage(&app, presented, Request::AssignFile(Box::new(assignment))).await {
        Ok(response) => response,
        Err(response) => return *response,
    };
    if response.status == 200
        && let Some(held) = reservation
    {
        held.spend();
    }
    crate::checkoff::present(response)
}
