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

use crate::accepted::{AcceptedHttpConfig, Gate, build_id, failure, internal, ok_json, read_error};
use crate::loopback::PresentedCredential;
use axum::{Extension, Router, extract, http::StatusCode, response::Response, routing::get};
use pp_storage::read_model::build_workflow::{Failure, Outcome, Request, WorkflowClient};

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
    extract::State(app): extract::State<App>,
    extract::Path(id): extract::Path<String>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read(app, presented, Request::Build(build_id(&id))).await
}

async fn tasks(
    extract::State(app): extract::State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read(app, presented, Request::Tasks).await
}

async fn read(
    App { gate, client }: App,
    presented: PresentedCredential,
    request: Request,
) -> Response {
    let outcome = gate.read(presented, move |credential, stop, wait| {
        client.read(credential, request, stop, wait)
    });
    match outcome.await {
        Some(Ok(Outcome::Workspace(workspace))) => ok_json(&workspace),
        Some(Ok(Outcome::Tasks(feed))) => ok_json(&feed),
        Some(Ok(Outcome::MissingBuild)) => failure(StatusCode::NOT_FOUND, "Profile not found"),
        Some(Err(error)) if error.downcast_ref::<Failure>().is_some() => {
            failure(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
        Some(Err(error)) => read_error(&error),
        None => internal(),
    }
}
