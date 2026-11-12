//! `GET /sources/github-branches?url=` and `GET /sources/github-tags?url=`
//! (GRE-429 PR 1): a repository's branches and tags, read with the stored
//! GitHub token. `pp-core` supplies the transport through [`GithubConnector`].

use crate::admin::{AdminHttpConfig, Gate, response};
use crate::loopback::PresentedCredential;
use axum::{
    Extension, Router,
    extract::{Query, State},
    http::StatusCode,
    response::Response,
    routing::get,
};
use pp_services::sync::{self, GithubPort, SyncError};
use pp_storage::{
    app_settings::{AppSettingsClient, Key, Outcome},
    read_model::ReadClient,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, sync::atomic::AtomicBool, time::Duration};

/// The most one listing may take, all pages included.
const LISTING_DEADLINE: Duration = Duration::from_secs(60);

/// Hands out a [`GithubPort`] that sends `token`, when there is one.
pub trait GithubConnector: Clone + Send + Sync + 'static {
    type Port: GithubPort + Send + Sync;
    fn port(&self, token: Option<String>) -> Self::Port;
}

#[derive(Clone)]
struct App<C> {
    settings: AppSettingsClient,
    wait: Duration,
    github: C,
}

#[derive(Deserialize)]
struct RepoQuery {
    url: Option<String>,
}

/// Mounts both routes at `/` and `/api/v1` when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`; otherwise nothing. The admin gate proves the
/// credential first and then releases its admission slot, so a slow GitHub
/// answer does not hold it.
pub fn github_router<C: GithubConnector>(
    config: AdminHttpConfig,
    reads: ReadClient,
    settings: AppSettingsClient,
    github: C,
) -> Router {
    let wait = config.wait;
    let Some(gate) = Gate::new(config, reads) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/sources/github-branches", get(branches::<C>))
        .route("/sources/github-tags", get(tags::<C>));
    gate.authentication_only()
        .guard(Router::new().merge(routes.clone()).nest("/api/v1", routes))
        .with_state(App {
            settings,
            wait,
            github,
        })
}

async fn branches<C: GithubConnector>(
    State(app): State<App<C>>,
    Extension(presented): Extension<PresentedCredential>,
    Query(query): Query<RepoQuery>,
) -> Response {
    let (port, url) = match connect(&app, presented, query).await {
        Ok(ready) => ready,
        Err(refused) => return *refused,
    };
    answer(sync::list_branches(&port, &url)).await
}

async fn tags<C: GithubConnector>(
    State(app): State<App<C>>,
    Extension(presented): Extension<PresentedCredential>,
    Query(query): Query<RepoQuery>,
) -> Response {
    let (port, url) = match connect(&app, presented, query).await {
        Ok(ready) => ready,
        Err(refused) => return *refused,
    };
    answer(sync::list_tags(&port, &url)).await
}

/// The `url` query value and a port carrying the stored token.
async fn connect<C: GithubConnector>(
    app: &App<C>,
    presented: PresentedCredential,
    query: RepoQuery,
) -> Result<(C::Port, String), Box<Response>> {
    let url = query.url.unwrap_or_default();
    if url.trim().is_empty() {
        return Err(Box::new(crate::problem::response(
            StatusCode::BAD_REQUEST,
            "url query parameter is required",
        )));
    }
    let client = app.settings.clone();
    let wait = app.wait;
    let outcome = tokio::task::spawn_blocking(move || {
        client.read(
            presented.into_credential(),
            Key::GithubPat,
            &AtomicBool::new(false),
            wait,
        )
    })
    .await;
    let token = match crate::settings::settled(outcome)? {
        Outcome::Value(token) => token,
        _ => {
            return Err(Box::new(crate::problem::response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
            )));
        }
    };
    Ok((app.github.port(token), url))
}

/// The listing as JSON, or its failure as a problem. A listing past
/// [`LISTING_DEADLINE`] is 502.
async fn answer<T: Serialize>(listing: impl Future<Output = Result<T, SyncError>>) -> Response {
    match tokio::time::timeout(LISTING_DEADLINE, listing).await {
        Ok(Ok(listing)) => match serde_json::to_string(&listing) {
            Ok(body) => response(StatusCode::OK, "application/json", body),
            Err(_) => {
                crate::problem::response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
        },
        Ok(Err(error)) => github_error(&error),
        Err(_) => crate::problem::response(StatusCode::BAD_GATEWAY, "GitHub timed out"),
    }
}

/// A URL that is not a GitHub repository is 400. GitHub's own 404 is 404.
/// Every other GitHub failure, including no answer, is 502; a refused token
/// and a rate limit carry a `code` the UI can act on. The detail is fixed
/// text: GitHub's reply never reaches the client.
fn github_error(error: &SyncError) -> Response {
    let port = match error {
        SyncError::Refused(_) => {
            return crate::problem::response(
                StatusCode::BAD_REQUEST,
                "Invalid GitHub repository URL",
            );
        }
        SyncError::Port(port) => port,
    };
    match port.status {
        Some(404) => crate::problem::response(StatusCode::NOT_FOUND, "GitHub repository not found"),
        Some(401) => crate::problem::coded(
            StatusCode::BAD_GATEWAY,
            "GitHub refused the saved token",
            "github_token_rejected",
        ),
        Some(403 | 429) => crate::problem::coded(
            StatusCode::BAD_GATEWAY,
            "GitHub refused the request; it may be rate limited",
            "github_refused",
        ),
        _ => crate::problem::response(StatusCode::BAD_GATEWAY, "GitHub could not answer"),
    }
}
