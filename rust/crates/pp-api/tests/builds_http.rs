use pp_storage::{Limits, WriterOwner};
use reqwest::{Client, Method};
use serde_json::Value;
#[cfg(feature = "desktop_rebuild")]
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;
#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;

/// Returns the directory first so that, on panic, `owner` drops before the
/// scratch directory is removed.
fn open() -> (Scratch, Arc<Mutex<WriterOwner>>) {
    let directory = Scratch::new("pp-builds-http-");
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    (directory, Arc::new(Mutex::new(owner)))
}

fn files(dir: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                out.push((
                    entry
                        .path()
                        .strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    entry.metadata().unwrap().len(),
                ));
            }
        }
    }
    out.sort();
    out
}

fn profiles(directory: &Path) -> Vec<(String, i64, String)> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let mut statement = db
        .prepare("SELECT tenant_id, id, name FROM build_profiles ORDER BY tenant_id, id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

struct Server {
    origin: String,
    client: Client,
    serving: Option<pp_api::serving::Serving>,
}

impl Server {
    async fn spawn(listener: tokio::net::TcpListener, router: axum::Router) -> Self {
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let serving = pp_api::serving::Serving::start(listener, router);
        Self {
            origin,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .unwrap(),
            serving: Some(serving),
        }
    }

    async fn send(&self, method: Method, path: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        self.send_body(method, path, headers, None).await
    }

    async fn send_json(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> (u16, Value) {
        self.send_body(method, path, headers, Some(body)).await
    }

    async fn send_body(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> (u16, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", &self.origin);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.unwrap();
        (
            response.status().as_u16(),
            pp_api::problem::without_envelope(response.json().await.unwrap_or(Value::Null)),
        )
    }

    async fn stop(&mut self) {
        if let Some(mut serving) = self.serving.take() {
            serving.stop().await.unwrap();
        }
    }
}

async fn bind() -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()
}

fn shutdown(owner: Arc<Mutex<WriterOwner>>, directory: Scratch) {
    let owner = Arc::try_unwrap(owner).unwrap_or_else(|_| panic!("router still holds storage"));
    owner.into_inner().unwrap().shutdown().unwrap();
    drop(directory);
}

const READ_PATHS: &[&str] = &[
    "/plans",
    "/plans/1",
    "/api/v1/plans",
    "/api/v1/plans/1",
    "/api/v2/plans",
    "/api/v2/plans/1",
];

/// Without `desktop_rebuild`, Plan build HTTP is not linked into pp-api.
/// Linear keeps these names so `cargo test -p pp-api` (no features) still discovers them.
#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_reads_flag_off_are_404() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    for path in READ_PATHS {
        for method in [Method::GET, Method::HEAD] {
            let (status, body) = server.send(method, path, &[]).await;
            assert_eq!(status, 404, "{path}");
            assert!(body.get("profiles").is_none() && body.get("id").is_none());
        }
    }
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_reads_are_authenticated_and_tenant_scoped() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server.send(Method::GET, "/plans", &[]).await;
    assert_eq!(status, 404);
    assert!(body.get("profiles").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_create_matches_node_contract() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server
        .send_json(
            Method::POST,
            "/plans",
            &[],
            &serde_json::json!({"name":"X"}),
        )
        .await;
    assert_eq!(status, 404);
    assert!(body.get("id").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_delete_refusal_preserves_plan() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server.send(Method::DELETE, "/plans/1", &[]).await;
    assert_eq!(status, 404);
    assert!(body.get("id").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_layer_read_preserves_stale_reasons() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server.send(Method::GET, "/plans/1/layers", &[]).await;
    assert_eq!(status, 404);
    assert!(body.get("layers").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_touch_uses_existing_command() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server.send(Method::POST, "/plans/1/touch", &[]).await;
    assert_eq!(status, 404);
    assert!(body.get("id").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_layer_mutations_are_atomic() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server
        .send_json(
            Method::PUT,
            "/plans/1/layers/base",
            &[],
            &serde_json::json!({"project_id": 1}),
        )
        .await;
    assert_eq!(status, 404);
    assert!(body.get("layers").is_none());
    server.stop().await;
    assert_eq!(
        (files(&directory), profiles(&directory)),
        (snapshot, before)
    );
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn build_layer_wrong_owner_has_no_effects() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server
        .send_json(
            Method::POST,
            "/plans/1/layers",
            &[],
            &serde_json::json!({"project_id": 1}),
        )
        .await;
    assert_eq!(status, 404);
    assert!(body.get("layers").is_none());
    server.stop().await;
    assert_eq!(
        (files(&directory), profiles(&directory)),
        (snapshot, before)
    );
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
fn policy() -> pp_storage::auth::AuthPolicy {
    use pp_storage::auth::*;
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        first_user: FirstUserTenant::NewUser,
        session_tenant: SessionTenantPolicy::AccountTenant,
    }
}

#[cfg(feature = "desktop_rebuild")]
async fn register(server: &Server, email: &str) -> String {
    let registered = server
        .client
        .post(format!("{}/auth/register", server.origin))
        .header("Origin", &server.origin)
        .json(&json!({"email": email, "password": "password-1234"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200, "{email}");
    registered.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[cfg(feature = "desktop_rebuild")]
fn assert_progress_shape(body: &Value, prefix: &str) {
    // Node's /api/v1 keeps the legacy unit counts; `/` and /api/v2 send
    // `accepted_progress`.
    if prefix == "/api/v1" {
        assert!(body.get("accepted_progress").is_none(), "{prefix} {body}");
        assert_eq!(
            (&body["remaining_units"], &body["total_units"]),
            (&json!(0), &json!(0))
        );
    } else {
        assert!(body["accepted_progress"].is_object(), "{prefix} {body}");
        assert!(body.get("remaining_units").is_none(), "{prefix} {body}");
    }
}

#[cfg(feature = "desktop_rebuild")]
fn assert_summary_shape(body: &Value, prefix: &str, id: i64, name: &str) {
    assert_eq!(body["id"], id);
    assert_eq!(body["name"], name);
    assert!(body.get("order_number").is_some());
    assert!(body.get("special_request").is_some());
    assert_eq!(body["part_count"], 0);
    assert_progress_shape(body, prefix);
    assert_eq!(body["build_stale"], false);
    assert!(body["freshness"].is_object());
    assert!(body.get("archived_at").is_some());
    assert!(body.get("last_used_at").is_some());
    assert!(body.get("layers").is_none());
}

/// Real `enabled=false` path: `build_router` with a non-opt-in `PP_DESKTOP_REBUILD`.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_reads_flag_off_are_404() {
    use pp_api::builds::{BuildHttpConfig, build_router};
    let (directory, owner) = open();
    let before = profiles(&directory);
    let snapshot = files(&directory);
    for flag in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("TRUE"),
        Some(" true "),
    ] {
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let config = BuildHttpConfig::from_startup(&origin, "default", flag).unwrap();
        let router = build_router(
            config,
            owner
                .lock()
                .unwrap()
                .build_graph_with_policy(policy())
                .unwrap(),
        );
        let mut server = Server::spawn(bound, router).await;
        for path in READ_PATHS {
            for method in [Method::GET, Method::HEAD] {
                let (status, body) = server.send(method, path, &[]).await;
                assert_eq!(status, 404, "{flag:?} {path}");
                assert!(body.get("profiles").is_none() && body.get("id").is_none());
            }
        }
        server.stop().await;
    }
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_reads_are_authenticated_and_tenant_scoped() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        builds::{BuildHttpConfig, build_router},
    };
    use pp_storage::{
        auth::Secret,
        build_graph::{BuildCommand, BuildName, BuildOutcome},
        read_model::Credential,
    };
    use std::{sync::atomic::AtomicBool, time::Duration};

    let (directory, owner) = open();
    let chosen = policy();
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let auth = owner.lock().unwrap().auth_with_policy(chosen).unwrap();
    let mut bootstrap = Server::spawn(
        bound,
        auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            auth,
            ProviderClient::new(None, None).unwrap(),
            ResetMailer::disabled(),
        ),
    )
    .await;
    let cookie_a = register(&bootstrap, "builds-a@example.test").await;
    let cookie_b = register(&bootstrap, "builds-b@example.test").await;
    let token_a = cookie_a.strip_prefix("pp_session=").unwrap().to_owned();
    let token_b = cookie_b.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let create = |token: &str, name: &str| {
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(chosen)
            .unwrap();
        match client
            .execute(
                Credential::Session(Secret::new(token.into())),
                BuildCommand::Create {
                    name: BuildName::parse(name.into()).unwrap(),
                    base_source: None,
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
            )
            .unwrap()
        {
            BuildOutcome::Created { profile, .. } => profile.id,
            other => panic!("{other:?}"),
        }
    };
    let id_a = create(&token_a, "Tenant A Plan");
    let id_b = create(&token_b, "Tenant B Plan");
    let tenant_a = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id_a)
        .map(|row| row.0)
        .expect("tenant A");

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let (auth, graph) = {
        let locked = owner.lock().unwrap();
        (
            locked.auth_with_policy(chosen).unwrap(),
            locked.build_graph_with_policy(chosen).unwrap(),
        )
    };
    let mut server = Server::spawn(
        bound,
        build_router(
            BuildHttpConfig::from_startup(&origin, &tenant_a, Some("1")).unwrap(),
            graph,
        )
        .merge(auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            auth,
            ProviderClient::new(None, None).unwrap(),
            ResetMailer::disabled(),
        )),
    )
    .await;

    let before = profiles(&directory);
    let snapshot = files(&directory);
    let (status, body) = server.send(Method::GET, "/plans", &[]).await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (401, Some("Authentication required"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    type Refusal<'a> = (&'a str, &'a [(&'a str, &'a str)], u16, &'a str);
    let refusals: &[Refusal<'_>] = &[
        (
            "/plans",
            &[("Cookie", cookie_a.as_str()), ("Host", "evil.test")],
            403,
            "Invalid request host",
        ),
        (
            "/plans",
            &[
                ("Cookie", cookie_a.as_str()),
                ("Origin", "https://evil.test"),
            ],
            403,
            "Invalid request origin",
        ),
        (
            "/plans",
            &[
                ("Cookie", cookie_a.as_str()),
                ("sec-fetch-site", "cross-site"),
            ],
            403,
            "Invalid request origin",
        ),
        (
            "/plans",
            &[
                ("Cookie", cookie_a.as_str()),
                ("x-forwarded-for", "1.2.3.4"),
            ],
            403,
            "Invalid request host",
        ),
        (
            "/plans",
            &[
                ("Cookie", cookie_a.as_str()),
                ("Authorization", "Bearer also"),
            ],
            401,
            "Authentication required",
        ),
    ];
    for &(path, headers, status, detail) in refusals {
        let before_files = files(&directory);
        let before_rows = profiles(&directory);
        let (got_status, body) = server.send(Method::GET, path, headers).await;
        assert_eq!(
            (got_status, body["detail"].as_str()),
            (status, Some(detail)),
            "{path} {headers:?}"
        );
        assert_eq!(files(&directory), before_files, "refusal mutated files");
        assert_eq!(profiles(&directory), before_rows, "refusal mutated rows");
    }

    let (status, list_a) = server
        .send(Method::GET, "/plans", &[("Cookie", cookie_a.as_str())])
        .await;
    assert_eq!(status, 200);
    assert_eq!(list_a["profiles"].as_array().unwrap().len(), 1);
    assert_summary_shape(&list_a["profiles"][0], "", id_a, "Tenant A Plan");

    let (status, list_b) = server
        .send(Method::GET, "/plans", &[("Cookie", cookie_b.as_str())])
        .await;
    assert_eq!(status, 200);
    assert_eq!(list_b["profiles"].as_array().unwrap().len(), 1);
    assert_summary_shape(&list_b["profiles"][0], "", id_b, "Tenant B Plan");

    for prefix in ["", "/api/v1", "/api/v2"] {
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/plans/{id_a}"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 200, "{prefix}");
        assert_summary_shape(&body, prefix, id_a, "Tenant A Plan");
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/plans/{id_b}"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(
            (status, body["detail"].as_str()),
            (404, Some("Profile not found")),
            "{prefix}"
        );
        let (status, _) = server
            .send(
                Method::HEAD,
                &format!("{prefix}/plans"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 200, "HEAD list {prefix}");
        let (status, _) = server
            .send(
                Method::HEAD,
                &format!("{prefix}/plans/{id_a}"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 200, "HEAD detail {prefix}");
        let (status, body) = server
            .send(
                Method::HEAD,
                &format!("{prefix}/plans/{id_b}"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 404, "HEAD cross-tenant {prefix}");
        assert!(body.get("id").is_none());
    }

    let (status, body) = server
        .send(
            Method::GET,
            "/plans/9007199254740991",
            &[("Cookie", cookie_a.as_str())],
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Profile not found"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    let key = server
        .client
        .post(format!("{}/settings/api-keys", server.origin))
        .header("Origin", &server.origin)
        .header("Cookie", &cookie_a)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(key.status(), 201);
    let key: Value = pp_api::problem::without_envelope(key.json().await.unwrap());
    let bearer = format!("Bearer {}", key["key"].as_str().unwrap());
    let (status, body) = server
        .send(Method::GET, "/plans", &[("Authorization", bearer.as_str())])
        .await;
    assert_eq!(status, 200);
    assert_eq!(body, list_a);
    let (status, body) = server
        .send(
            Method::GET,
            &format!("/plans/{id_b}"),
            &[("Authorization", bearer.as_str())],
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Profile not found"))
    );
    assert_eq!(profiles(&directory), before);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
async fn spawn_builds_with_auth(
    owner: &Arc<Mutex<WriterOwner>>,
    tenant: &str,
) -> (Server, String, String) {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        builds::{BuildHttpConfig, build_router},
    };
    let chosen = policy();
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let auth = owner.lock().unwrap().auth_with_policy(chosen).unwrap();
    let mut bootstrap = Server::spawn(
        bound,
        auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            auth,
            ProviderClient::new(None, None).unwrap(),
            ResetMailer::disabled(),
        ),
    )
    .await;
    let cookie_a = register(&bootstrap, "builds-create-a@example.test").await;
    let cookie_b = register(&bootstrap, "builds-create-b@example.test").await;
    bootstrap.stop().await;

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let (auth, graph) = {
        let locked = owner.lock().unwrap();
        (
            locked.auth_with_policy(chosen).unwrap(),
            locked.build_graph_with_policy(chosen).unwrap(),
        )
    };
    let server = Server::spawn(
        bound,
        build_router(
            BuildHttpConfig::from_startup(&origin, tenant, Some("1")).unwrap(),
            graph,
        )
        .merge(auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            auth,
            ProviderClient::new(None, None).unwrap(),
            ResetMailer::disabled(),
        )),
    )
    .await;
    (server, cookie_a, cookie_b)
}

#[cfg(feature = "desktop_rebuild")]
fn assert_created_shape(body: &Value, prefix: &str, name: &str) {
    assert_eq!(body["name"], name);
    assert!(body["id"].as_i64().unwrap() > 0);
    assert!(body.get("order_number").is_some());
    assert!(body.get("special_request").is_some());
    assert_eq!(body["part_count"], 0);
    assert_progress_shape(body, prefix);
    assert_eq!(body["build_stale"], false);
    assert!(body["freshness"].is_object());
    assert!(body.get("archived_at").is_some());
    assert!(body.get("last_used_at").is_some());
    assert!(body["layers"].as_array().is_some());
}

/// A create or layer body that is not a JSON object cannot be read at all, so
/// it is a malformed request; Node's 400 detail is kept.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn problem_response_unreadable_build_bodies_are_malformed() {
    let (directory, owner) = open();
    let (mut server, cookie, _) = spawn_builds_with_auth(&owner, "default").await;
    for (path, body, kind) in [
        ("/plans", "{", "malformed-request"),
        ("/plans", "[]", "malformed-request"),
        ("/plans/1/layers", "{", "malformed-request"),
        ("/plans", r#"{"name":7}"#, "validation"),
    ] {
        let response = server
            .client
            .post(format!("{}{path}", server.origin))
            .header("Origin", &server.origin)
            .header("Cookie", cookie.as_str())
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{path} {body}");
        let problem: Value = response.json().await.unwrap();
        let expected = format!("urn:print-partner:problem:{kind}");
        assert_eq!(problem["type"], expected, "{path} {body}");
        assert_eq!(problem["detail"], "Request is invalid", "{path} {body}");
    }
    server.stop().await;
    shutdown(owner, directory);
}

/// POST /plans matches Node: summary fields plus `layers`, duplicate/base refusals.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_create_matches_node_contract() {
    let (directory, owner) = open();
    let (mut server, cookie_a, _) = spawn_builds_with_auth(&owner, "default").await;

    let (status, created) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"  Kit  "}),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    assert_created_shape(&created, "", "Kit");
    assert_eq!(created["layers"].as_array().unwrap().len(), 0);

    for prefix in ["", "/api/v1", "/api/v2"] {
        let name = format!("Alias{prefix}");
        let (status, body) = server
            .send_json(
                Method::POST,
                &format!("{prefix}/plans"),
                &[("Cookie", cookie_a.as_str())],
                &json!({"name": name}),
            )
            .await;
        assert_eq!(status, 200, "{prefix}");
        assert_created_shape(&body, prefix, &name);
        assert_eq!(body["layers"].as_array().unwrap().len(), 0);
    }

    let before = profiles(&directory);
    let snapshot = files(&directory);
    let (status, body) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"Kit"}),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Profile already exists: Kit"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    let (status, body) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"MissingBase","base_project_id":9_007_199_254_740_991u64}),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Project not found"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    let (status, body) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"   "}),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Profile name is required"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    let (status, body) = server
        .send_json(Method::POST, "/plans", &[], &json!({"name":"Anon"}))
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (401, Some("Authentication required"))
    );
    assert_eq!(profiles(&directory), before);

    server.stop().await;
    shutdown(owner, directory);
}

/// DELETE refusals leave Plan rows and files unchanged; success returns 204.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_delete_refusal_preserves_plan() {
    let (directory, owner) = open();
    let (mut server, cookie_a, cookie_b) = spawn_builds_with_auth(&owner, "default").await;

    let (status, created) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"KeepMe"}),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_i64().unwrap();

    let before = profiles(&directory);
    let snapshot = files(&directory);
    let path = format!("/plans/{id}");
    let missing = format!("/plans/{}", id + 1_000_000);

    type Refusal<'a> = (&'a str, &'a [(&'a str, &'a str)], u16, Option<&'a str>);
    let refusals: &[Refusal<'_>] = &[
        (path.as_str(), &[], 401, Some("Authentication required")),
        (
            path.as_str(),
            &[("Cookie", cookie_b.as_str())],
            404,
            Some("Profile not found"),
        ),
        (
            missing.as_str(),
            &[("Cookie", cookie_a.as_str())],
            404,
            Some("Profile not found"),
        ),
        (
            path.as_str(),
            &[("Cookie", cookie_a.as_str()), ("Host", "evil.test")],
            403,
            Some("Invalid request host"),
        ),
    ];
    for &(path, headers, status, detail) in refusals {
        let before_files = files(&directory);
        let before_rows = profiles(&directory);
        let (got_status, body) = server.send(Method::DELETE, path, headers).await;
        assert_eq!(
            (got_status, body["detail"].as_str()),
            (status, detail),
            "{path} {headers:?}"
        );
        assert_eq!(files(&directory), before_files, "refusal mutated files");
        assert_eq!(profiles(&directory), before_rows, "refusal mutated rows");
    }
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    for prefix in ["", "/api/v1", "/api/v2"] {
        let (status, created) = server
            .send_json(
                Method::POST,
                &format!("{prefix}/plans"),
                &[("Cookie", cookie_a.as_str())],
                &json!({"name": format!("Del{prefix}")}),
            )
            .await;
        assert_eq!(status, 200, "{prefix}");
        let del_id = created["id"].as_i64().unwrap();
        let (status, body) = server
            .send(
                Method::DELETE,
                &format!("{prefix}/plans/{del_id}"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 204, "{prefix}");
        assert!(body.is_null());
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/plans/{del_id}"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(
            (status, body["detail"].as_str()),
            (404, Some("Profile not found")),
            "{prefix}"
        );
    }

    assert!(
        profiles(&directory)
            .iter()
            .any(|row| row.1 == id && row.2 == "KeepMe")
    );
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
fn last_used_and_config(directory: &Path, id: i64) -> (Option<String>, Option<String>) {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.query_row(
        "SELECT last_used_at, config_modified_at FROM build_profiles WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[cfg(feature = "desktop_rebuild")]
fn seed_base_layer(directory: &Path, tenant: &str, profile_id: i64, source_name: &str) -> i64 {
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.execute(
        "INSERT INTO projects(tenant_id,name,url) VALUES(?1,?2,?3)",
        rusqlite::params![tenant, source_name, "https://example.test/layer"],
    )
    .unwrap();
    let project_id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES(?1,?2,0,'base',?3)",
        rusqlite::params![tenant, profile_id, project_id],
    )
    .unwrap();
    project_id
}

/// GET layers returns ordered layer detail; detail/touch keep domain freshness reasons.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_layer_read_preserves_stale_reasons() {
    use pp_storage::{
        auth::Secret,
        build_graph::{BuildCommand, BuildOutcome, PositiveId, wire::profile_summary},
        read_model::Credential,
    };
    use std::{sync::atomic::AtomicBool, time::Duration};

    let (directory, owner) = open();
    let (mut server, cookie_a, cookie_b) = spawn_builds_with_auth(&owner, "default").await;

    let (status, created) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"LayerPlan"}),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_i64().unwrap();
    let tenant = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .expect("tenant");
    let project_id = seed_base_layer(&directory, &tenant, id, "Base Source");

    for prefix in ["", "/api/v1", "/api/v2"] {
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/plans/{id}/layers"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 200, "{prefix} {body}");
        assert_eq!(body["profile_id"], id);
        let layers = body["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["layer_order"], 0);
        assert_eq!(layers[0]["layer_type"], "base");
        assert_eq!(layers[0]["project_id"], project_id);
        assert_eq!(layers[0]["project_name"], "Base Source");
        let (status, _) = server
            .send(
                Method::HEAD,
                &format!("{prefix}/plans/{id}/layers"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 200, "HEAD layers {prefix}");
    }

    let token = cookie_a.strip_prefix("pp_session=").unwrap();
    let domain = {
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(policy())
            .unwrap();
        match client
            .execute(
                Credential::Session(Secret::new(token.into())),
                BuildCommand::Read {
                    build: PositiveId::new(id as u64).unwrap(),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
            )
            .unwrap()
        {
            BuildOutcome::Read { profile } => {
                serde_json::to_value(profile_summary(*profile)).unwrap()
            }
            other => panic!("{other:?}"),
        }
    };

    let (status, detail) = server
        .send(
            Method::GET,
            &format!("/plans/{id}"),
            &[("Cookie", cookie_a.as_str())],
        )
        .await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["freshness"], domain["freshness"]);
    assert_eq!(detail["build_stale"], domain["build_stale"]);
    assert!(
        detail["freshness"]["reasons"].is_array()
            && !detail["freshness"]["reasons"]
                .as_array()
                .unwrap()
                .is_empty(),
        "domain freshness reasons must reach HTTP unchanged: {detail}"
    );

    let (status, touched) = server
        .send(
            Method::POST,
            &format!("/plans/{id}/touch"),
            &[("Cookie", cookie_a.as_str())],
        )
        .await;
    assert_eq!(status, 200, "{touched}");
    assert_eq!(
        touched["freshness"]["reasons"],
        detail["freshness"]["reasons"]
    );
    assert_eq!(
        touched["freshness"]["untracked_sources"],
        detail["freshness"]["untracked_sources"]
    );
    assert!(touched.get("layers").is_none());

    let before = profiles(&directory);
    let snapshot = files(&directory);
    let (status, body) = server
        .send(
            Method::GET,
            &format!("/plans/{id}/layers"),
            &[("Cookie", cookie_b.as_str())],
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Profile not found"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    server.stop().await;
    shutdown(owner, directory);
}

/// POST touch drives `BuildCommand::Touch`: bumps last_used_at only.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_touch_uses_existing_command() {
    let (directory, owner) = open();
    let (mut server, cookie_a, cookie_b) = spawn_builds_with_auth(&owner, "default").await;

    let (status, created) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"TouchPlan"}),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_i64().unwrap();
    {
        let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        db.execute(
            "UPDATE build_profiles SET last_used_at='old', config_modified_at='old' WHERE id=?1",
            [id],
        )
        .unwrap();
    }

    for prefix in ["", "/api/v1", "/api/v2"] {
        let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
        db.execute(
            "UPDATE build_profiles SET last_used_at='old' WHERE id=?1",
            [id],
        )
        .unwrap();
        drop(db);
        let (status, body) = server
            .send(
                Method::POST,
                &format!("{prefix}/plans/{id}/touch"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(status, 200, "{prefix} {body}");
        assert_summary_shape(&body, prefix, id, "TouchPlan");
        assert_ne!(body["last_used_at"], "old");
        let (last_used, config) = last_used_and_config(&directory, id);
        assert_ne!(last_used.as_deref(), Some("old"));
        assert_eq!(config.as_deref(), Some("old"));
    }

    let before = profiles(&directory);
    let snapshot = files(&directory);
    let path = format!("/plans/{id}/touch");
    let missing = format!("/plans/{}/touch", id + 1_000_000);
    type Refusal<'a> = (&'a str, &'a [(&'a str, &'a str)], u16, Option<&'a str>);
    let refusals: &[Refusal<'_>] = &[
        (path.as_str(), &[], 401, Some("Authentication required")),
        (
            path.as_str(),
            &[("Cookie", cookie_b.as_str())],
            404,
            Some("Profile not found"),
        ),
        (
            missing.as_str(),
            &[("Cookie", cookie_a.as_str())],
            404,
            Some("Profile not found"),
        ),
    ];
    for &(path, headers, status, detail) in refusals {
        let before_files = files(&directory);
        let before_rows = profiles(&directory);
        let (last_used, config) = last_used_and_config(&directory, id);
        let (got_status, body) = server.send(Method::POST, path, headers).await;
        assert_eq!(
            (got_status, body["detail"].as_str()),
            (status, detail),
            "{path} {headers:?}"
        );
        assert_eq!(files(&directory), before_files);
        assert_eq!(profiles(&directory), before_rows);
        assert_eq!(last_used_and_config(&directory, id), (last_used, config));
    }
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);

    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
fn layer_rows(directory: &Path, profile_id: i64) -> Vec<(i64, String, Option<i64>)> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let mut statement = db
        .prepare(
            "SELECT id, layer_type, project_id FROM profile_layers WHERE profile_id=?1 ORDER BY layer_order, id",
        )
        .unwrap();
    statement
        .query_map([profile_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[cfg(feature = "desktop_rebuild")]
fn seed_project(directory: &Path, tenant: &str, name: &str) -> i64 {
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.execute(
        "INSERT INTO projects(tenant_id,name,url) VALUES(?1,?2,?3)",
        rusqlite::params![tenant, name, format!("https://example.test/{name}")],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// One graph command per request; duplicate/missing refusals leave rows/files unchanged.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_layer_mutations_are_atomic() {
    let (directory, owner) = open();
    let (mut server, cookie_a, _) = spawn_builds_with_auth(&owner, "default").await;
    let (status, created) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"MutatePlan"}),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_i64().unwrap();
    let tenant = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .expect("tenant");
    let base = seed_project(&directory, &tenant, "Base");
    let addon = seed_project(&directory, &tenant, "Addon");
    let other = seed_project(&directory, &tenant, "Other");
    let auth = [("Cookie", cookie_a.as_str())];

    let (status, body) = server
        .send_json(
            Method::PUT,
            &format!("/plans/{id}/layers/base"),
            &auth,
            &json!({"project_id": base}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["layers"].as_array().unwrap().len(), 1);
    assert_eq!(body["layers"][0]["project_id"], base);

    let (status, body) = server
        .send_json(
            Method::POST,
            &format!("/api/v1/plans/{id}/layers"),
            &auth,
            &json!({"project_id": addon}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let layers = body["layers"].as_array().unwrap();
    let base_layer_id = layers
        .iter()
        .find(|layer| layer["layer_type"] == "base")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let layer_id = layers
        .iter()
        .find(|layer| layer["layer_type"] == "addon")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let duplicate = "Source \"Addon\" is already attached to this build";
    let missing_layer = id + 1_000_000;

    let before = layer_rows(&directory, id);
    let snapshot = files(&directory);
    let refusals = [
        (
            Method::POST,
            format!("/plans/{id}/layers"),
            Some(json!({"project_id": addon})),
            409,
            duplicate,
        ),
        (
            Method::PUT,
            format!("/plans/{id}/layers/base"),
            Some(json!({"project_id": addon})),
            404,
            duplicate,
        ),
        (
            Method::PUT,
            format!("/plans/{id}/layers/{base_layer_id}"),
            Some(json!({"project_id": addon})),
            404,
            duplicate,
        ),
        (
            Method::DELETE,
            format!("/plans/{id}/layers/{missing_layer}"),
            None,
            404,
            "Layer not found",
        ),
        (
            Method::PUT,
            format!("/plans/{id}/layers/{missing_layer}"),
            Some(json!({"project_id": other})),
            404,
            "Layer not found",
        ),
        (
            Method::PUT,
            format!("/plans/{id}/layers/base"),
            Some(json!({"project_id": 9_007_199_254_740_991u64})),
            404,
            "Project not found",
        ),
    ];
    for (method, path, body, status, detail) in &refusals {
        let (got, response) = match body {
            Some(body) => server.send_json(method.clone(), path, &auth, body).await,
            None => server.send(method.clone(), path, &auth).await,
        };
        assert_eq!(
            (got, response["detail"].as_str()),
            (*status, Some(*detail)),
            "{method} {path} {response}"
        );
        assert_eq!(layer_rows(&directory, id), before, "{method} {path}");
        assert_eq!(files(&directory), snapshot, "{method} {path}");
    }

    let (status, body) = server
        .send_json(
            Method::PUT,
            &format!("/api/v2/plans/{id}/layers/{layer_id}"),
            &auth,
            &json!({"project_id": other}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|layer| layer["id"] == layer_id)
            .unwrap()["project_id"],
        other
    );

    let (status, body) = server
        .send(
            Method::DELETE,
            &format!("/plans/{id}/layers/{layer_id}"),
            &auth,
        )
        .await;
    assert_eq!(status, 204, "{body}");
    assert_eq!(
        layer_rows(&directory, id),
        vec![(before[0].0, "base".into(), Some(base))]
    );

    server.stop().await;
    shutdown(owner, directory);
}

/// Cross-tenant / unauthenticated mutate answers refusal and leaves rows/files unchanged.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn build_layer_wrong_owner_has_no_effects() {
    let (directory, owner) = open();
    let (mut server, cookie_a, cookie_b) = spawn_builds_with_auth(&owner, "default").await;
    let (status, created) = server
        .send_json(
            Method::POST,
            "/plans",
            &[("Cookie", cookie_a.as_str())],
            &json!({"name":"OwnedLayers"}),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_i64().unwrap();
    let tenant = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .expect("tenant");
    let base = seed_project(&directory, &tenant, "OwnerBase");
    let addon = seed_project(&directory, &tenant, "OwnerAddon");
    let (status, body) = server
        .send_json(
            Method::PUT,
            &format!("/plans/{id}/layers/base"),
            &[("Cookie", cookie_a.as_str())],
            &json!({"project_id": base}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let layer_id = body["layers"][0]["id"].as_i64().unwrap();
    let before_rows = profiles(&directory);
    let before_layers = layer_rows(&directory, id);
    let snapshot = files(&directory);
    let project = json!({"project_id": addon});
    let foreign = [("Cookie", cookie_b.as_str())];
    let cases = [
        (
            Method::PUT,
            format!("/plans/{id}/layers/base"),
            Some(&project),
            &foreign[..],
            404,
        ),
        (
            Method::POST,
            format!("/plans/{id}/layers"),
            Some(&project),
            &foreign[..],
            404,
        ),
        (
            Method::PUT,
            format!("/plans/{id}/layers/{layer_id}"),
            Some(&project),
            &foreign[..],
            404,
        ),
        (
            Method::DELETE,
            format!("/plans/{id}/layers/{layer_id}"),
            None,
            &foreign[..],
            404,
        ),
        (
            Method::POST,
            format!("/plans/{id}/layers"),
            Some(&project),
            &[][..],
            401,
        ),
    ];
    for (method, path, body, headers, status) in cases {
        let (got, response) = match body {
            Some(body) => server.send_json(method.clone(), &path, headers, body).await,
            None => server.send(method.clone(), &path, headers).await,
        };
        assert_eq!(got, status, "{method} {path} {response}");
        assert_eq!(
            response["detail"].as_str(),
            Some(if status == 401 {
                "Authentication required"
            } else {
                "Profile not found"
            })
        );
        assert_eq!(profiles(&directory), before_rows);
        assert_eq!(layer_rows(&directory, id), before_layers);
        assert_eq!(files(&directory), snapshot);
    }
    server.stop().await;
    shutdown(owner, directory);
}

#[test]
fn scratch_root_is_empty_after_builds_http_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_builds_http_tests"]);
}
