//! Build history read (`GET /plans/{id}/history`): one paged, newest-first
//! list of a Build's accepted revisions, snapshots, checkoff bursts, sends and
//! assistant decisions, read by `pp_storage::read_model::history`.
//!
//! Query: `limit` (1 to 200, default 50) and `cursor` (the previous page's
//! `next_cursor`). The route writes nothing. Admission is `accepted::Gate`.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, build_id, failure, internal, ok_json, read_error},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use pp_storage::read_model::history::{
    Cursor, DEFAULT_LIMIT, Failure, HistoryClient, MAX_LIMIT, Outcome, Request,
};

#[derive(Clone)]
struct App {
    gate: Gate,
    client: HistoryClient,
}

/// Mounts `GET /plans/{id}/history` at `/` and `/api/v2` when
/// `PP_DESKTOP_REBUILD` was exactly `1` or `true`; otherwise nothing.
/// Admission is the Accepted Plan reads' (`AcceptedHttpConfig`).
pub fn history_router(config: AcceptedHttpConfig, client: HistoryClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new().route("/plans/{id}/history", get(history));
    gate.guard(routes.clone().nest("/api/v2", routes))
        .with_state(App { gate, client })
}

/// Reads `limit` and `cursor`, URL-decoded as `jobs.rs` decodes its query.
/// Other parameters are ignored; a repeated or malformed one is refused.
fn page_query(query: Option<&str>) -> Result<(Option<Cursor>, u16), &'static str> {
    let (mut cursor, mut limit) = (None, None);
    let url = reqwest::Url::parse(&format!("http://localhost/?{}", query.unwrap_or_default()))
        .map_err(|_| "Request is invalid")?;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "cursor" if cursor.is_none() => {
                cursor = Some(Cursor::parse(&value).ok_or("Invalid history cursor")?);
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
    let request = Request {
        build: build_id(&id),
        cursor,
        limit,
    };
    let client = app.client;
    let read = app
        .gate
        .read(presented, move |credential, cancelled, wait| {
            client.read(credential, request, cancelled, wait)
        })
        .await;
    match read {
        Some(Ok(Outcome::Page(page))) => ok_json(&page),
        Some(Ok(Outcome::MissingBuild)) => failure(StatusCode::NOT_FOUND, "Profile not found"),
        Some(Err(error)) if error.downcast_ref::<Failure>().is_some() => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Build history exceeds the read limit",
        ),
        Some(Err(error)) => read_error(&error),
        None => internal(),
    }
}

#[cfg(test)]
mod tests {
    use super::page_query;

    #[test]
    fn build_history_query_is_decoded_before_validation() {
        assert_eq!(page_query(Some("limit=%32")).unwrap().1, 2);
        assert_eq!(page_query(Some("%6cimit=3")).unwrap().1, 3);
        assert_eq!(page_query(None).unwrap().1, 50);
        for query in ["limit=1&%6cimit=2", "limit=%30", "limit=+5"] {
            assert_eq!(
                page_query(Some(query)),
                Err("Invalid history limit"),
                "{query}"
            );
        }
        assert_eq!(
            page_query(Some("cursor=%00")),
            Err("Invalid history cursor")
        );
    }
}
