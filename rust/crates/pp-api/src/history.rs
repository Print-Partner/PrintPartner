//! Build history read (`GET /plans/{id}/history`): one paged, newest-first
//! list of a Build's accepted revisions, snapshots, checkoff bursts, sends and
//! assistant decisions, read by `pp_storage::read_model::history`.
//!
//! Query: `limit` (1 to 200, default 50) and `cursor` (the previous page's
//! `next_cursor`). The route writes nothing. Admission is `accepted::Gate`.

use crate::{
    accepted::{AcceptedHttpConfig, Cancellation, Gate, failure, json_response, read_error},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use pp_storage::{
    build_graph::PositiveId,
    read_model::history::{
        Cursor, DEFAULT_LIMIT, Failure, HistoryClient, MAX_LIMIT, Outcome, Request,
    },
};
use std::sync::{Arc, atomic::AtomicBool};

/// History admits requests exactly as the Accepted Plan reads do.
pub type HistoryHttpConfig = AcceptedHttpConfig;

#[derive(Clone)]
struct App {
    gate: Gate,
    client: HistoryClient,
}

/// Mounts `GET /plans/{id}/history` at `/` and `/api/v2` when
/// `PP_DESKTOP_REBUILD` was exactly `1` or `true`; otherwise nothing.
pub fn history_router(config: HistoryHttpConfig, client: HistoryClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new().route("/plans/{id}/history", get(history));
    gate.guard(routes.clone().nest("/api/v2", routes))
        .with_state(App { gate, client })
}

/// Reads `limit` and `cursor`. Other parameters are ignored; a repeated or
/// malformed one is refused.
fn page_query(query: Option<&str>) -> Result<(Option<Cursor>, u16), &'static str> {
    let (mut cursor, mut limit) = (None, None);
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "cursor" if cursor.is_none() => {
                cursor = Some(Cursor::parse(value).ok_or("Invalid history cursor")?);
            }
            "limit" if limit.is_none() => {
                limit = Some(
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                        .ok_or("Invalid history limit")?,
                );
            }
            "cursor" => return Err("Invalid history cursor"),
            "limit" => return Err("Invalid history limit"),
            _ => {}
        }
    }
    Ok((cursor, limit.unwrap_or(DEFAULT_LIMIT)))
}

async fn history(
    State(app): State<App>,
    Path(id): Path<String>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
) -> Response {
    let (cursor, limit) = match page_query(uri.query()) {
        Ok(page) => page,
        Err(detail) => return failure(StatusCode::BAD_REQUEST, detail),
    };
    // Only a decimal positive safe integer can name a Build.
    let Some(build) = (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .then(|| id.parse().ok().and_then(|id| PositiveId::new(id).ok()))
        .flatten()
    else {
        return failure(StatusCode::NOT_FOUND, "Profile not found");
    };
    let request = Request {
        build,
        cursor,
        limit,
    };
    let stop = Cancellation(Arc::new(AtomicBool::new(false)));
    let (client, wait, cancelled) = (app.client, app.gate.wait(), stop.0.clone());
    let read = tokio::task::spawn_blocking(move || {
        client.read(presented.into_credential(), request, cancelled, wait)
    })
    .await;
    drop(stop);
    match read {
        Ok(Ok(Outcome::Page(page))) => match serde_json::to_value(&page) {
            Ok(body) => json_response(StatusCode::OK, &body, false),
            Err(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        },
        Ok(Ok(Outcome::MissingBuild)) => failure(StatusCode::NOT_FOUND, "Profile not found"),
        Ok(Err(error)) if error.downcast_ref::<Failure>().is_some() => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Build history exceeds the read limit",
        ),
        Ok(Err(error)) => read_error(&error),
        Err(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}
