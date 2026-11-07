//! Node's `POST /plans/{id}/decisions` through the Build router (GRE-1243).
//! `GET /plans/{id}/decisions` stays in the planning router.

use super::http::{
    App, Received, bytes_response, failure, missing_build, number_build_id, run, unreadable,
};
use axum::{
    extract::{Path, State},
    http::{Method, StatusCode},
    response::Response,
};
use pp_storage::build_graph::{BuildCommand, DecisionOutcome, variants::json_body_refused};

/// Fastify refuses an empty or poisoned JSON body before the route runs; the
/// route reads the id with `Number()` before anything in the body. A
/// `text/plain` body reaches Node as a string, whose fields are all missing, so
/// it appends the defaults, as `{}` does.
pub(super) async fn append(
    State(app): State<App>,
    Path(id): Path<String>,
    received: Received,
) -> Response {
    if received.json && (received.bytes.is_empty() || json_body_refused(&received.bytes)) {
        return unreadable();
    }
    let Some(build) = number_build_id(&id) else {
        return missing_build(app, received.presented).await;
    };
    let command = BuildCommand::AppendDecision {
        build,
        body: if received.text {
            b"{}".to_vec()
        } else {
            received.bytes.to_vec()
        },
    };
    run(
        app,
        received.presented,
        Method::POST,
        received.path,
        received.contract,
        command,
    )
    .await
}

pub(super) fn present(outcome: DecisionOutcome) -> Response {
    match outcome {
        DecisionOutcome::Created(body) => bytes_response(StatusCode::OK, body, false),
        DecisionOutcome::Refused(detail) => failure(StatusCode::BAD_REQUEST, &detail),
        DecisionOutcome::Unreadable => unreadable(),
    }
}
