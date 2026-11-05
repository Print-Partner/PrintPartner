//! Build planning and assistant decisions reads:
//! `GET /plans/{id}/build-planning` and `GET /plans/{id}/decisions`, read by
//! `pp_storage::read_model::planning`. Both write nothing. Admission is
//! `accepted::Gate`; the id is read with Node's `Number()`.

use crate::accepted::{AcceptedHttpConfig, Gate, failure, json_response, node_id, read_error};
use crate::loopback::PresentedCredential;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use pp_storage::build_graph::PositiveId;
use pp_storage::read_model::planning::{Outcome, PlanningClient, Query, Request};

#[derive(Clone)]
struct App {
    gate: Gate,
    client: PlanningClient,
}

/// Mounts both reads at `/`, `/api/v1` and `/api/v2`, as Node does, when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`; otherwise nothing.
pub fn planning_router(config: AcceptedHttpConfig, client: PlanningClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/plans/{id}/build-planning", get(build_planning))
        .route("/plans/{id}/decisions", get(decisions));
    gate.guard(
        routes
            .clone()
            .nest("/api/v1", routes.clone())
            .nest("/api/v2", routes),
    )
    .with_state(App { gate, client })
}

/// Node checks `draft_id` only once a brief exists, and ignores it after:
/// `Ok(())` when it is absent or one positive safe integer.
fn draft_query(query: Option<&str>) -> Result<(), ()> {
    let url = reqwest::Url::parse(&format!("http://localhost/?{}", query.unwrap_or_default()))
        .map_err(|_| ())?;
    let values: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key == "draft_id")
        .map(|(_, value)| value)
        .collect();
    match values.as_slice() {
        [] => Ok(()),
        [value] => node_id(value).map(|_| ()).ok_or(()),
        _ => Err(()),
    }
}

async fn build_planning(
    State(app): State<App>,
    Path(id): Path<String>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
) -> Response {
    let query = Query::BuildPlanning {
        draft_id_valid: draft_query(uri.query()).is_ok(),
    };
    let read = read(app, &id, presented, query).await;
    respond(read, |outcome| match outcome {
        Outcome::InvalidDraftId => Some(failure(
            StatusCode::BAD_REQUEST,
            "draft_id must be a positive integer",
        )),
        Outcome::BuildPlanning(body) => Some(serialize(&*body)),
        _ => None,
    })
}

async fn decisions(
    State(app): State<App>,
    Path(id): Path<String>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    let read = read(app, &id, presented, Query::Decisions).await;
    respond(read, |outcome| match outcome {
        Outcome::Decisions(body) => Some(serialize(&body)),
        _ => None,
    })
}

/// `ok` answers the outcome it expects; any other outcome is a missing Build.
fn respond(
    read: Option<anyhow::Result<Outcome>>,
    ok: impl FnOnce(Outcome) -> Option<Response>,
) -> Response {
    match read {
        Some(Ok(outcome)) => {
            ok(outcome).unwrap_or_else(|| failure(StatusCode::NOT_FOUND, "Profile not found"))
        }
        Some(Err(error)) => read_error(&error),
        None => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}

fn serialize(body: &impl serde::Serialize) -> Response {
    match serde_json::to_value(body) {
        Ok(body) => json_response(StatusCode::OK, &body, false),
        Err(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}

/// Any id still authenticates; one that is not a positive safe integer then
/// answers not found.
async fn read(
    app: App,
    id: &str,
    presented: PresentedCredential,
    query: Query,
) -> Option<anyhow::Result<Outcome>> {
    let request = Request {
        build: node_id(id).and_then(|id| PositiveId::new(id.unsigned_abs()).ok()),
        query,
    };
    let client = app.client;
    app.gate
        .blocking(move |cancelled, wait| {
            client.read(presented.into_credential(), request, cancelled, wait)
        })
        .await
        .ok()
}

#[cfg(test)]
mod tests {
    use super::draft_query;

    #[test]
    fn build_planning_read_draft_query_follows_node_number() {
        for ok in [
            None,
            Some(""),
            Some("draft_id=3"),
            Some("draft_id=%33"),
            Some("draft_id=0x2"),
            Some("other=1"),
        ] {
            assert_eq!(draft_query(ok), Ok(()), "{ok:?}");
        }
        for bad in [
            "draft_id=",
            "draft_id=0",
            "draft_id=-1",
            "draft_id=1.5",
            "draft_id=x",
            "draft_id=1&draft_id=2",
        ] {
            assert_eq!(draft_query(Some(bad)), Err(()), "{bad}");
        }
    }
}
