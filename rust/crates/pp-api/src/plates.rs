//! Managed accepted Plates (GRE-419) over `pp_storage::plates`:
//! `GET /plans/{id}/plates`, initialize, arrange, and the Plate-revision edits
//! that move, pin, unplace and transfer a Required unit or restore the
//! revision an arrange replaced. One request is one storage transaction.
//!
//! `pp_core::rebuild_api::read_router` mounts every route and alias together,
//! so no Plate write runs on Node and Rust at once.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, storage_answer},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    routing::{get, patch, post},
};
use pp_storage::plates::{PlatesClient, Request, UnitEdit};

/// Node's `MAX_JSON_BODY_BYTES`, and `MAX_BULK_JSON_BODY_BYTES` for initialize
/// (`services/upload-limits.ts`).
const BODY_LIMIT: usize = 1024 * 1024;
const BULK_BODY_LIMIT: usize = 8 * 1024 * 1024;
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
    let path = "/plans/{id}/plates/{plate}/units/{token}";
    let routes = Router::new()
        .route("/plans/{id}/plates", get(read))
        .route(
            "/plans/{id}/plates/restore",
            post(|s, c, p, b| plan(s, c, p, b, Write::Restore)),
        )
        .route(
            "/plans/{id}/plates/arrange",
            post(|s, c, p, b| plan(s, c, p, b, Write::Arrange)),
        )
        .route(
            "/plans/{id}/plates/initialize",
            post(|s, c, p, b| plan(s, c, p, b, Write::Initialize)),
        )
        .route(path, patch(|s, c, p, b| unit(s, c, p, b, UnitEdit::Move)))
        .route(
            &format!("{path}/pin"),
            patch(|s, c, p, b| unit(s, c, p, b, UnitEdit::Pin)),
        )
        .route(
            &format!("{path}/unplace"),
            post(|s, c, p, b| unit(s, c, p, b, UnitEdit::Unplace)),
        )
        .route(
            &format!("{path}/transfer"),
            post(|s, c, p, b| unit(s, c, p, b, UnitEdit::Transfer)),
        );
    gate.guard(routes.clone().nest("/api/v2", routes))
        .with_state(App { gate, client })
}

async fn read(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(profile): Path<String>,
) -> Response {
    run(app, presented, Request::Read { profile }).await
}

/// A write on the whole Plan's Plates.
#[derive(Clone, Copy)]
enum Write {
    Restore,
    Arrange,
    Initialize,
}

async fn plan(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(profile): Path<String>,
    body: Body,
    write: Write,
) -> Response {
    let limit = match write {
        Write::Initialize => BULK_BODY_LIMIT,
        Write::Restore | Write::Arrange => BODY_LIMIT,
    };
    let Some(body) = json(body, limit).await else {
        return too_large();
    };
    let request = match write {
        Write::Restore => Request::Restore { profile, body },
        Write::Arrange => Request::Arrange { profile, body },
        Write::Initialize => Request::Initialize { profile, body },
    };
    run(app, presented, request).await
}

async fn unit(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path((profile, plate, token)): Path<(String, String, String)>,
    body: Body,
    edit: UnitEdit,
) -> Response {
    let Some(body) = json(body, BODY_LIMIT).await else {
        return too_large();
    };
    let request = Request::Unit {
        edit,
        profile,
        plate,
        token,
        body,
    };
    run(app, presented, request).await
}

/// The parsed body, or `None` past Node's body limit. Storage answers
/// anything but a JSON object as Node's parsers do: 400 `invalid_request`.
async fn json(body: Body, limit: usize) -> Option<Value> {
    let bytes = to_bytes(body, limit).await.ok()?;
    Some(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn too_large() -> Response {
    crate::problem::response(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large")
}

async fn run(
    App { gate, client }: App,
    presented: PresentedCredential,
    request: Request,
) -> Response {
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
