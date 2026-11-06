//! `GET /plans/{id}/variant-dimensions` and `POST /plans/{id}/variant-selection`
//! through the Build router (GRE-1221).

use super::http::{
    App, bytes_response, execute, failure, parse_build_id, run, unknown_build, unreadable,
};
use crate::loopback::PresentedCredential;
use axum::{
    body::to_bytes,
    extract::{Path, State},
    http::{Method, StatusCode},
    response::Response,
};
use pp_storage::build_graph::{
    BuildCommand, ManifestOptionsOutcome, VariantCommand, VariantOutcome,
};

pub(super) async fn read(
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
        BuildCommand::Variants(VariantCommand::Read { build }),
    )
    .await
}

pub(super) async fn select(
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
    let limit = app.body_limit();
    let Ok(bytes) = to_bytes(request.into_body(), limit).await else {
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
    };
    let command = VariantCommand::Select {
        build,
        request: bytes.to_vec(),
    };
    run(
        app,
        presented,
        Method::POST,
        path,
        super::http::SummaryContract::Accepted,
        BuildCommand::Variants(command),
    )
    .await
}

pub(super) fn present(method: &Method, outcome: VariantOutcome) -> Response {
    match outcome {
        VariantOutcome::Body(body) => bytes_response(StatusCode::OK, body, *method == Method::HEAD),
        VariantOutcome::MissingBuild => failure(StatusCode::NOT_FOUND, "Profile not found"),
        VariantOutcome::Unreadable => unreadable(),
        VariantOutcome::Refused(detail) => failure(StatusCode::BAD_REQUEST, &detail),
        VariantOutcome::ObservationFailed { detail } => super::manifest_options::present(
            method,
            ManifestOptionsOutcome::ObservationFailed { detail },
        ),
    }
}
