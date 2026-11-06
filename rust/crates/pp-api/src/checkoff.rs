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
//! The farm log (GRE-1126) shares the router: `GET /farm-log` pages the
//! fleet's logged bursts, undos and sends, and `POST /farm-log/{id}/undo`
//! undoes a logged burst through the same compensating burst undo.
//!
//! Unattributed prints (GRE-1182) share it too:
//! `POST /printer-checkoff/unattributed/{id}/claim` attributes a print to a
//! Build and creates its Checkoff link, and `/dismiss` drops it from Match
//! review. Storage owns both writes; there is no Node fallback.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, internal, ok_json, read_error},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use pp_storage::checkoff_progress::{Body, ProgressClient, Request};
use serde_json::Value;

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
    let applied = app
        .gate
        .read(presented, move |credential, cancelled, wait| {
            client.apply(credential, request, &cancelled, wait)
        })
        .await;
    match applied {
        Some(Ok(response)) => match (StatusCode::from_u16(response.status), response.body) {
            (Ok(StatusCode::OK), body) => ok_json(&body),
            (Ok(status), Body::Failure { detail, code }) => crate::problem::build(
                crate::problem::problem(status, &detail, code.as_deref()),
                serde_json::Map::new(),
            ),
            _ => internal(),
        },
        Some(Err(error)) => read_error(&error),
        None => internal(),
    }
}
