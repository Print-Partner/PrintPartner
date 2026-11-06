//! Managed accepted Plates (GRE-1184, part of GRE-419) over
//! `pp_storage::plates`: `GET /plans/{id}/plates` and the Plate-revision
//! edits that move, pin, unplace and transfer a Required unit or restore the
//! revision an arrange replaced. One request is one storage transaction.
//!
//! Not mounted yet. Initialize and arrange still run on Node and write the
//! same Plate tables, so these routes join `read_router` together with them
//! (GRE-419) and every alias switches as one operation.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, internal, ok_json, read_error},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    routing::{get, patch, post},
};
use pp_storage::plates::{PlatesClient, Request, UnitEdit};
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
        .route("/plans/{id}/plates/restore", post(restore))
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

async fn restore(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(profile): Path<String>,
    body: Bytes,
) -> Response {
    let body = json(&body);
    run(app, presented, Request::Restore { profile, body }).await
}

async fn unit(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path((profile, plate, token)): Path<(String, String, String)>,
    body: Bytes,
    edit: UnitEdit,
) -> Response {
    let request = Request::Unit {
        edit,
        profile,
        plate,
        token,
        body: json(&body),
    };
    run(app, presented, request).await
}

/// The parsed body. Storage answers anything but a JSON object as Node's
/// parsers do: 400 `invalid_request`.
fn json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

async fn run(app: App, presented: PresentedCredential, request: Request) -> Response {
    let client = app.client;
    let applied = app
        .gate
        .read(presented, move |credential, cancelled, wait| {
            client.apply(credential, request, &cancelled, wait)
        })
        .await;
    let response = match applied {
        Some(Ok(response)) => response,
        Some(Err(error)) => return read_error(&error),
        None => return internal(),
    };
    match (StatusCode::from_u16(response.status), response.body) {
        (Ok(StatusCode::OK), body) => ok_json(&body),
        (Ok(status), Value::Object(mut fields)) => {
            let detail = fields.remove("detail");
            let code = fields.remove("code");
            match (
                detail.as_ref().and_then(Value::as_str),
                code.as_ref().and_then(Value::as_str),
            ) {
                (Some(detail), Some(code)) => crate::problem::build(
                    crate::problem::problem(status, detail, Some(code)),
                    fields,
                ),
                _ => internal(),
            }
        }
        _ => internal(),
    }
}
