//! Checkoff bursts (`POST /printer-checkoff/verify-batch` and
//! `POST /printer-checkoff/verify-batch/undo`) over
//! `pp_storage::checkoff_progress`. One request is one writer transaction.
//!
//! The handler parses JSON and hands the body to storage, which owns the
//! idempotency receipt, every check and the undo. No printer, webhook or
//! Spoolman call happens here or inside the transaction. A response lost after
//! admission is reconciled by repeating the request with the same
//! `idempotency_key`, which returns the stored receipt. Admission is
//! `accepted::Gate`: loopback, exact Host and Origin, and a session or API key.
//!
//! Node's accepted Progress import (`POST /plans/{id}/progress/import`)
//! shares the router and the same writer transaction rules.
//!
//! The farm log (GRE-1126) shares the router: `GET /farm-log` pages the
//! fleet's logged bursts, undos and sends, and `POST /farm-log/{id}/undo`
//! undoes a logged burst through the same compensating burst undo.
//!
//! Manual Checkoff bursts (GRE-1191) share it: `POST /plans/{id}/checkoff/bursts`
//! sets printed and assembled state on a Build's Required units as one burst
//! with no printer link, and `/bursts/undo` reverses it.
//!
//! Unattributed prints (GRE-1182) share it too:
//! `POST /printer-checkoff/unattributed/{id}/claim` attributes a print to a
//! Build and creates its Checkoff link, and `/dismiss` drops it from Match
//! review. Storage owns both writes; there is no Node fallback.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, storage_answer},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::DefaultBodyLimit,
    extract::{Path, RawQuery, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use pp_storage::checkoff_progress::{Body, Import, ProgressClient, Request};
use serde_json::Value;

/// Node's `MAX_BULK_JSON_BODY_BYTES` for the import route.
const IMPORT_BODY_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone)]
struct App {
    gate: Gate,
    client: ProgressClient,
}

/// Mounts the burst routes at `/` and `/api/v2` when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`; otherwise nothing.
pub fn checkoff_router(config: AcceptedHttpConfig, client: ProgressClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/printer-checkoff/verify-batch", post(verify_batch))
        .route("/printer-checkoff/verify-batch/undo", post(undo_batch))
        .route("/farm-log", get(farm_log))
        .route("/farm-log/{id}/undo", post(undo_farm_log_entry))
        .route(
            "/plans/{id}/progress/import",
            post(import).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)),
        )
        .route("/plans/{id}/checkoff/bursts", post(manual_burst::<false>))
        .route(
            "/plans/{id}/checkoff/bursts/undo",
            post(manual_burst::<true>),
        )
        .route(
            "/printer-checkoff/unattributed/{id}/claim",
            post(claim_unattributed),
        )
        .route(
            "/printer-checkoff/unattributed/{id}/dismiss",
            post(dismiss_unattributed),
        );
    gate.guard(routes.clone().nest("/api/v2", routes))
        .with_state(App { gate, client })
}

async fn verify_batch(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    apply(app, presented, &body, |body| Request::VerifyBatch { body }).await
}

async fn undo_batch(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    apply(app, presented, &body, |body| Request::UndoBatch { body }).await
}

/// Node's `POST /plans/:id/progress/import`: sets each listed Part's printed
/// count against the Accepted Plan the caller observed. A body for another
/// Plan is 404 `progress_target_not_found`, as Node answers it.
async fn import(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let profile = id.parse::<i64>().ok().filter(|id| *id > 0);
    let body = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    let parsed = profile.map(|profile| (profile, Import::parse(body)));
    let import = match parsed {
        Some((profile, Ok(import))) if import.expected.profile_id == profile => import,
        Some((_, Ok(_))) => {
            return crate::problem::build(
                crate::problem::problem(
                    StatusCode::NOT_FOUND,
                    "Plan or Part not found",
                    Some("progress_target_not_found"),
                ),
                serde_json::Map::new(),
            );
        }
        Some((_, Err(response))) => return present(response),
        None => {
            return crate::problem::build(
                crate::problem::problem(
                    StatusCode::BAD_REQUEST,
                    "Request is invalid",
                    Some("invalid_request"),
                ),
                serde_json::Map::new(),
            );
        }
    };
    run(app, presented, Request::Import(import)).await
}

/// 404 `Plan not found` for a Build id path segment that is not a positive
/// integer.
fn plan_not_found() -> Response {
    crate::problem::build(
        crate::problem::problem(StatusCode::NOT_FOUND, "Plan not found", None),
        serde_json::Map::new(),
    )
}

/// `POST /plans/{id}/checkoff/bursts`, or its `/undo` with `UNDO`.
async fn manual_burst<const UNDO: bool>(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Some(profile_id) = id.parse::<i64>().ok().filter(|id| *id > 0) else {
        return plan_not_found();
    };
    apply(app, presented, &body, |body| {
        if UNDO {
            Request::UndoManualBurst { profile_id, body }
        } else {
            Request::ManualBurst { profile_id, body }
        }
    })
    .await
}

/// `cursor` and `limit`, URL-decoded as `history.rs` decodes its query.
/// Other parameters are ignored; a repeated one is refused. Storage validates
/// the values.
async fn farm_log(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    RawQuery(query): RawQuery,
) -> Response {
    let Ok(url) = reqwest::Url::parse(&format!("http://localhost/?{}", query.unwrap_or_default()))
    else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    let (mut cursor, mut limit) = (None, None);
    for (key, value) in url.query_pairs() {
        let slot = match &*key {
            "cursor" => &mut cursor,
            "limit" => &mut limit,
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            return failure(StatusCode::BAD_REQUEST, &format!("Repeated {key}"));
        }
    }
    run(app, presented, Request::FarmLog { cursor, limit }).await
}

async fn undo_farm_log_entry(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    run(app, presented, Request::UndoFarmLogEntry { id }).await
}

async fn claim_unattributed(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    apply(app, presented, &body, |body| Request::ClaimUnattributed {
        id,
        body,
    })
    .await
}

async fn dismiss_unattributed(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    run(app, presented, Request::DismissUnattributed { id }).await
}

async fn apply(
    app: App,
    presented: PresentedCredential,
    body: &[u8],
    request: impl FnOnce(Value) -> Request,
) -> Response {
    let Ok(body @ Value::Object(_)) = serde_json::from_slice::<Value>(body) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "Request body must be a JSON object",
        );
    };
    run(app, presented, request(body)).await
}

async fn run(app: App, presented: PresentedCredential, request: Request) -> Response {
    let client = app.client;
    app.gate
        .respond(
            presented,
            move |credential, cancelled, wait| client.apply(credential, request, &cancelled, wait),
            present,
        )
        .await
}

fn present(response: pp_storage::checkoff_progress::Response) -> Response {
    storage_answer(response.status, response.body, |status, body| match body {
        Body::Failure { detail, code } => Some(crate::problem::build(
            crate::problem::problem(status, &detail, code.as_deref()),
            serde_json::Map::new(),
        )),
        _ => None,
    })
}
