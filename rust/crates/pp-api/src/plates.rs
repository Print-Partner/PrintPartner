//! The accepted Plate workspace read (GRE-1189, part of GRE-419) over
//! `pp_storage::plates`: `GET /plans/{id}/plates`.
//!
//! Not mounted yet. Initialize, arrange and the Plate edits still run on Node
//! and write the Plate tables, so these routes join `read_router` together
//! with them (GRE-419) and every alias switches as one operation.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, storage_answer},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    extract::{Path, State},
    response::Response,
    routing::get,
};
use pp_storage::plates::{PlatesClient, Request};
use serde_json::Value;

#[derive(Clone)]
struct App {
    gate: Gate,
    client: PlatesClient,
}

/// The Plate routes at `/` and `/api/v2` when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`; otherwise nothing.
pub fn plates_router(config: AcceptedHttpConfig, client: PlatesClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new().route("/plans/{id}/plates", get(read));
    gate.guard(routes.clone().nest("/api/v2", routes))
        .with_state(App { gate, client })
}

async fn read(
    State(App { gate, client }): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(profile): Path<String>,
) -> Response {
    let request = Request::Read { profile };
    gate.respond(
        presented,
        move |credential, cancelled, wait| client.apply(credential, request, &cancelled, wait),
        present,
    )
    .await
}

/// Node's `{ detail, code }` failure body, with any other fields kept.
fn present(response: pp_storage::plates::Response) -> Response {
    storage_answer(response.status, response.body, |status, body| {
        let Value::Object(mut fields) = body else {
            return None;
        };
        let detail = fields.remove("detail")?;
        let code = fields.remove("code")?;
        Some(crate::problem::build(
            crate::problem::problem(status, detail.as_str()?, Some(code.as_str()?)),
            fields,
        ))
    })
}
