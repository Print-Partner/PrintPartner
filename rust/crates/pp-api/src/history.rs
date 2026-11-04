//! Build history read (`GET /plans/{id}/history`): one paged, newest-first
//! list of a Build's accepted revisions, snapshots, checkoff bursts, sends and
//! assistant decisions, read by `pp_storage::read_model::history`.
//!
//! Query: `limit` (1 to 200, default 50) and `cursor` (the previous page's
//! `next_cursor`). The route writes nothing.

use crate::{
    accepted::{failure, read_error},
    loopback::{
        LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
        require_api_key_tenant,
    },
};
use anyhow::Result;
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use pp_storage::{
    build_graph::PositiveId,
    read_model::history::{
        Cursor, DEFAULT_LIMIT, Failure, HistoryClient, MAX_LIMIT, Outcome, Request,
    },
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
pub struct HistoryHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    wait: Duration,
}

impl HistoryHttpConfig {
    /// `flag` is the process `PP_DESKTOP_REBUILD` value captured at startup.
    pub fn from_startup(origin: &str, api_key_tenant: &str, flag: Option<&str>) -> Result<Self> {
        let listener = LoopbackOrigin::parse(origin)?;
        require_api_key_tenant(api_key_tenant)?;
        Ok(Self {
            origin: listener.origin,
            host: listener.host,
            api_key_tenant: api_key_tenant.into(),
            enabled: rebuild_flag_enabled(flag),
            wait: Duration::from_millis(100),
        })
    }
}

#[derive(Clone)]
struct App {
    config: HistoryHttpConfig,
    client: HistoryClient,
    admission: Arc<tokio::sync::Semaphore>,
}

/// Mounts `GET /plans/{id}/history` at `/` and `/api/v2` when
/// `PP_DESKTOP_REBUILD` was exactly `1` or `true`; otherwise nothing.
pub fn history_router(config: HistoryHttpConfig, client: HistoryClient) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let app = App {
        config,
        client,
        admission: Arc::new(tokio::sync::Semaphore::new(64)),
    };
    let routes = Router::new().route("/plans/{id}/history", get(history));
    Router::new()
        .merge(routes.clone())
        .nest("/api/v2", routes)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

async fn guard(
    State(app): State<App>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Ok(_permit) = app.admission.try_acquire() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service temporarily unavailable",
        );
    };
    if request.uri().to_string().len() > 16_384 {
        return failure(StatusCode::URI_TOO_LONG, "Request target too long");
    }
    if let Err((status, detail)) = admit(
        &app.config.origin,
        &app.config.host,
        &app.config.api_key_tenant,
        &request,
    ) {
        return failure(status, detail);
    }
    let Some(presented) = principal(request.headers(), &app.config.api_key_tenant) else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    request.extensions_mut().insert(presented);
    next.run(request).await
}

struct Cancellation(Arc<AtomicBool>);
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
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
    request: axum::extract::Request,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let (cursor, limit) = match page_query(request.uri().query()) {
        Ok(page) => page,
        Err(detail) => return failure(StatusCode::BAD_REQUEST, detail),
    };
    // Only a decimal positive safe integer can name a Build.
    let build = id
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| id.parse::<u64>().ok())
        .flatten()
        .and_then(|id| PositiveId::new(id).ok());
    let Some(build) = build else {
        return failure(StatusCode::NOT_FOUND, "Profile not found");
    };
    let client = app.client.clone();
    let wait = app.config.wait;
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Cancellation(cancelled.clone());
    let read = tokio::task::spawn_blocking(move || {
        client.read(
            presented.into_credential(),
            Request {
                build,
                cursor,
                limit,
            },
            cancelled,
            wait,
        )
    })
    .await;
    drop(flag);
    match read {
        Ok(Ok(Outcome::Page(page))) => match serde_json::to_vec(&page) {
            Ok(body) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CACHE_CONTROL, "no-store")
                .body(axum::body::Body::from(body))
                .unwrap_or_else(|_| {
                    failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
                }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use pp_storage::{
        Limits, WriterOwner,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    };
    use serde_json::Value;

    struct Server {
        _root: crate::scratch::Scratch,
        owner: Option<WriterOwner>,
        origin: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
            if let Some(owner) = self.owner.take() {
                owner.shutdown().unwrap();
            }
        }
    }

    async fn serve(flag: Option<&str>) -> Server {
        let root = crate::scratch::Scratch::new("build-history-http");
        std::fs::write(
            root.join("print-partner.db"),
            include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        rusqlite::Connection::open(root.join("print-partner.db"))
            .unwrap()
            .execute_batch(
                "INSERT INTO plan_decisions (tenant_id,profile_id,created_at,actor,kind,summary)
                 VALUES ('default',1,'2026-10-03T09:00:00.000Z','assistant','choice','Picked PETG');",
            )
            .unwrap();
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner
            .build_history(AuthPolicy {
                registration: RegistrationPolicy::Open,
                session_tenant: SessionTenantPolicy::AccountTenant,
                first_user: FirstUserTenant::NewUser,
            })
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = history_router(
            HistoryHttpConfig::from_startup(&origin, "default", flag).unwrap(),
            client,
        );
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Server {
            _root: root,
            owner: Some(owner),
            origin,
            task,
        }
    }

    async fn get(server: &Server, path: &str, session: bool) -> (u16, Value) {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("{}{path}", server.origin));
        if session {
            request = request.header("cookie", "pp_session=read-fixture-secret");
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_history_http_pages_with_cursor_on_both_mounts() {
        let server = serve(Some("1")).await;
        let (status, first) = get(&server, "/plans/1/history?limit=1", true).await;
        assert_eq!(status, 200, "{first}");
        assert_eq!(first["plan_id"], 1);
        assert_eq!(first["entries"][0]["type"], "decision");
        assert_eq!(first["entries"][0]["actor"], "assistant");
        assert_eq!(first["entries"][0]["summary"], "Picked PETG");
        let cursor = first["next_cursor"].as_str().unwrap();
        let (status, second) = get(
            &server,
            &format!("/api/v2/plans/1/history?limit=1&cursor={cursor}"),
            true,
        )
        .await;
        assert_eq!(status, 200, "{second}");
        assert_eq!(second["entries"][0]["id"], "revision:1");
        assert_eq!(second["next_cursor"], Value::Null);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_history_http_refusals() {
        let server = serve(Some("true")).await;
        for (path, session, status, detail) in [
            ("/plans/1/history", false, 401, "Authentication required"),
            ("/plans/999/history", true, 404, "Profile not found"),
            ("/plans/0x1/history", true, 404, "Profile not found"),
            (
                "/plans/1/history?limit=0",
                true,
                400,
                "Invalid history limit",
            ),
            (
                "/plans/1/history?limit=201",
                true,
                400,
                "Invalid history limit",
            ),
            (
                "/plans/1/history?limit=1&limit=2",
                true,
                400,
                "Invalid history limit",
            ),
            (
                "/plans/1/history?cursor=bogus",
                true,
                400,
                "Invalid history cursor",
            ),
        ] {
            let (actual, body) = get(&server, path, session).await;
            assert_eq!(
                (actual, body["detail"].as_str()),
                (status, Some(detail)),
                "{path}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_history_http_mounts_nothing_with_flag_off() {
        for flag in [None, Some("0"), Some("yes")] {
            let server = serve(flag).await;
            let (status, _) = get(&server, "/plans/1/history", true).await;
            assert_eq!(status, 404, "{flag:?}");
        }
    }
}
