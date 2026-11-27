use super::{App, build_id, managed, problem};
use crate::{
    NetworkAdmission,
    accepted::read_error,
    loopback::PresentedCredential,
    problem::{Problem, ToProblem},
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{Extensions, StatusCode, header},
    response::{IntoResponse, Response},
};
use pp_services::{
    exports::three_mf::MAX_OBJECTS,
    native_slicers::{Artifact, Handoff, HandoffFailure},
};
use pp_storage::read_model::Credential;
use serde::Deserialize;
use std::sync::{Arc, atomic::Ordering};

fn local_session(extensions: &Extensions) -> Result<Credential, &'static str> {
    if extensions.get::<NetworkAdmission>().is_some() {
        return Err("Local desktop only");
    }
    match extensions.get::<PresentedCredential>() {
        Some(presented @ PresentedCredential::Session(_)) => {
            Ok(presented.clone().into_credential())
        }
        _ => Err("Desktop session required"),
    }
}

pub(super) async fn picker(
    State(app): State<App>,
    Extension(adapter): Extension<Arc<dyn Handoff>>,
    extensions: Extensions,
) -> Response {
    let credential = match local_session(&extensions) {
        Ok(credential) => credential,
        Err(detail) => return problem::response(StatusCode::FORBIDDEN, detail),
    };
    match app
        .gate
        .blocking(move |cancelled, wait| {
            app.client.read(credential, &[], &cancelled, wait)?;
            Ok(adapter.picker())
        })
        .await
    {
        Ok(Ok(picker)) => no_store(picker),
        Ok(Err(error)) => read_error(&error),
        Err(_) => problem::refused(&managed::Failure::Encode),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    plate_revision_id: u64,
    plate_ordinal: u32,
    instance_id: Option<String>,
}

pub(super) async fn deliver(
    State(app): State<App>,
    Extension(adapter): Extension<Arc<dyn Handoff>>,
    Path(id): Path<String>,
    extensions: Extensions,
    Json(selection): Json<Selection>,
) -> Response {
    let credential = match local_session(&extensions) {
        Ok(credential) => credential,
        Err(detail) => return problem::response(StatusCode::FORBIDDEN, detail),
    };
    let safe_id = |id: &str| build_id(id).and_then(|id| i64::try_from(id.get()).ok());
    let Some(revision) = safe_id(&selection.plate_revision_id.to_string()) else {
        return problem::refused(&managed::Failure::RevisionRequired);
    };
    let result = app
        .gate
        .blocking(move |cancelled, wait| {
            let work = || -> Result<_, managed::Failure> {
                let captured = app
                    .client
                    .read_managed_export(
                        credential,
                        safe_id(&id),
                        revision,
                        MAX_OBJECTS,
                        &cancelled,
                        wait,
                    )
                    .map_err(managed::Failure::Capture)?;
                let _permit = app
                    .managed_render
                    .try_acquire()
                    .map_err(|_| managed::Failure::Busy)?;
                let ordinal = selection.plate_ordinal;
                let bytes = managed::render(&app.client, captured, Some(ordinal), &cancelled)?;
                if cancelled.load(Ordering::Acquire) {
                    return Err(managed::Failure::Cancelled);
                }
                let artifact = Artifact {
                    plate_revision_id: revision,
                    plate_ordinal: ordinal,
                    bytes,
                };
                Ok(adapter.deliver(selection.instance_id.as_deref(), artifact))
            };
            work()
        })
        .await;
    match result {
        Ok(Ok(Ok(delivery))) => no_store(delivery),
        Ok(Ok(Err(failure))) => problem::refused(&DeliveryRefusal(failure)),
        Ok(Err(failure)) => failure.response(),
        Err(_) => problem::refused(&managed::Failure::Encode),
    }
}

fn no_store(body: impl serde::Serialize) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

struct DeliveryRefusal(HandoffFailure);

impl ToProblem for DeliveryRefusal {
    fn to_problem(&self) -> Problem {
        let (status, detail, code) = match self.0 {
            HandoffFailure::UnknownInstance => (
                StatusCode::NOT_FOUND,
                "Slicer instance not found",
                "slicer_instance_not_found",
            ),
            HandoffFailure::NoDefault => (
                StatusCode::CONFLICT,
                "Choose a local slicer instance",
                "slicer_default_unavailable",
            ),
            HandoffFailure::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Local slicer inbox is unavailable",
                "slicer_instance_unavailable",
            ),
            HandoffFailure::ArtifactConflict => (
                StatusCode::CONFLICT,
                "The slicer inbox already contains a different artifact",
                "slicer_artifact_conflict",
            ),
        };
        problem::problem(status, detail, Some(code))
    }
}
