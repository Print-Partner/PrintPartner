//! Build workflow routes (GRE-1123), read by
//! `pp_storage::read_model::build_workflow`:
//!
//! - `GET /plans/{id}/workflow` at `/`, `/api/v1` and `/api/v2`: one Build's
//!   stages and next action.
//! - `GET /api/v1/tasks` and `/api/v2/tasks`: every open Build's next action,
//!   most urgent first.
//!
//! Both write nothing. Admission is `accepted::Gate`. MCP `get_build_workflow`
//! reads the same client.

use crate::{
    accepted::{
        AcceptedHttpConfig, Cancellation, Gate, build_id, failure, json_response, read_error,
    },
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    routing::get,
};
use pp_storage::read_model::build_workflow::{Failure, Outcome, Request, WorkflowClient};
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Clone)]
struct App {
    gate: Gate,
    client: WorkflowClient,
}

/// Mounts the workflow and task routes when `PP_DESKTOP_REBUILD` was exactly
/// `1` or `true`; otherwise nothing.
pub fn workflow_router(config: AcceptedHttpConfig, client: WorkflowClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let plans = Router::new().route("/plans/{id}/workflow", get(workflow));
    let api = plans.clone().route("/tasks", get(tasks));
    gate.guard(
        Router::new()
            .merge(plans)
            .nest("/api/v1", api.clone())
            .nest("/api/v2", api),
    )
    .with_state(App { gate, client })
}

async fn workflow(
    State(app): State<App>,
    Path(id): Path<String>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read(app, presented, Request::Build(build_id(&id))).await
}

async fn tasks(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read(app, presented, Request::Tasks).await
}

async fn read(app: App, presented: PresentedCredential, request: Request) -> Response {
    let stop = Cancellation(Arc::new(AtomicBool::new(false)));
    let (client, wait, cancelled) = (app.client, app.gate.wait(), stop.0.clone());
    let read = tokio::task::spawn_blocking(move || {
        client.read(presented.into_credential(), request, cancelled, wait)
    })
    .await;
    drop(stop);
    let body = match read {
        Ok(Ok(Outcome::Workspace(workspace))) => serde_json::to_value(workspace),
        Ok(Ok(Outcome::Tasks(feed))) => serde_json::to_value(feed),
        Ok(Ok(Outcome::MissingBuild)) => {
            return failure(StatusCode::NOT_FOUND, "Profile not found");
        }
        Ok(Err(error)) if error.downcast_ref::<Failure>().is_some() => {
            return failure(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
        }
        Ok(Err(error)) => return read_error(&error),
        Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    };
    match body {
        Ok(body) => json_response(StatusCode::OK, &body, false),
        Err(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}
