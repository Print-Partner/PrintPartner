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

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, internal, ok_json, read_error},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router, body::Bytes, extract::State, http::StatusCode, response::Response,
    routing::post,
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
        .route("/printer-checkoff/verify-batch/undo", post(undo_batch));
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
    let request = request(body);
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
            (Ok(status), Body::Failure { detail, .. }) => failure(status, &detail),
            _ => internal(),
        },
        Some(Err(error)) => read_error(&error),
        None => internal(),
    }
}
