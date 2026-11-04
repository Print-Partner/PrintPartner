use super::http::{App, bytes_response, execute, failure, parse_build_id, run, unknown_build};
use crate::loopback::PresentedCredential;
use axum::{
    body::to_bytes,
    extract::{Path, State},
    http::{Method, StatusCode},
    response::Response,
};
use pp_storage::build_graph::{BuildCommand, ManifestOptionsCommand, ManifestOptionsOutcome};
use serde_json::json;

/// Node / docs allow 8 MiB for kit-manifest PUT; Build create stays at the
/// lower shared default.
const KIT_BODY_LIMIT: usize = 8 * 1024 * 1024;

/// `GET`/`HEAD` `/plans/{id}/kit-manifest` — storage `ReadKit` through the Build router.
pub(super) async fn read_kit(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let Some(build) = parse_build_id(&id) else {
        return unknown_build(app, request).await;
    };
    execute(
        app,
        request,
        BuildCommand::Manifest(ManifestOptionsCommand::ReadKit { build }),
    )
    .await
}

/// `PUT` `/plans/{id}/kit-manifest` — storage `SaveKit` through the Build router.
pub(super) async fn save_kit(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let Some(build) = parse_build_id(&id) else {
        return unknown_build(app, request).await;
    };
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let path = request.uri().path().to_owned();
    let bytes = match to_bytes(request.into_body(), KIT_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => return failure(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"),
    };
    run(
        app,
        presented,
        Method::PUT,
        path,
        super::http::SummaryContract::Accepted,
        BuildCommand::Manifest(ManifestOptionsCommand::SaveKit {
            build,
            request: bytes.to_vec(),
        }),
    )
    .await
}

/// `GET`/`HEAD` `/plans/{id}/plan-manifest-builder` — storage `ReadBuilder`.
pub(super) async fn read_builder(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let Some(build) = parse_build_id(&id) else {
        return unknown_build(app, request).await;
    };
    execute(
        app,
        request,
        BuildCommand::Manifest(ManifestOptionsCommand::ReadBuilder { build }),
    )
    .await
}

pub(super) fn present(method: &Method, outcome: ManifestOptionsOutcome) -> Response {
    let head = *method == Method::HEAD;
    match outcome {
        success @ (ManifestOptionsOutcome::Kit { .. }
        | ManifestOptionsOutcome::Saved { .. }
        | ManifestOptionsOutcome::Builder(_)) => match success.into_success_body() {
            Ok(body) => bytes_response(StatusCode::OK, body.into_bytes(), head),
            Err(_) => failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Build service unavailable",
            ),
        },
        ManifestOptionsOutcome::MissingBuild => failure(StatusCode::NOT_FOUND, "Profile not found"),
        // A detail that is not valid UTF-8 JSON (a lone surrogate) gets a fixed one.
        ManifestOptionsOutcome::InvalidInput { detail } => {
            let bytes = detail.into_body().into_bytes();
            let parsed = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
            let detail = parsed.as_ref().and_then(|body| body["detail"].as_str());
            failure(
                StatusCode::BAD_REQUEST,
                detail.unwrap_or("Request is invalid"),
            )
        }
        ManifestOptionsOutcome::StaleObservation => crate::problem::coded(
            StatusCode::CONFLICT,
            "Build inputs changed during manifest observation",
            "stale_observation",
        ),
        // `detail` is a closed `ReadFailure` variant name, never a path.
        ManifestOptionsOutcome::ObservationFailed { detail } => {
            let problem = crate::problem::problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "Manifest source observation failed",
                Some("source_observation_failed"),
            );
            let mut reason = serde_json::Map::new();
            reason.insert("reason".into(), json!(detail));
            crate::problem::build(problem, reason)
        }
    }
}
