use pp_storage::{Limits, WriterOwner};
use reqwest::{Client, Method};

#[cfg(feature = "desktop_rebuild")]
use reqwest::Response;
use serde_json::Value;
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
    let directory = Scratch::new("pp-jobs-http-");
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

fn job_rows(directory: &Path) -> Vec<(String, String, String)> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let mut statement = db
        .prepare("SELECT tenant, id, kind FROM durable_jobs ORDER BY tenant, id")
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
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    async fn spawn(listener: tokio::net::TcpListener, router: axum::Router) -> Self {
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        });
        Self {
            origin,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .unwrap(),
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    async fn send(&self, method: Method, path: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        self.send_with_origin(method, path, headers, Some(&self.origin))
            .await
    }

    async fn send_with_origin(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        origin: Option<&str>,
    ) -> (u16, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        (
            response.status().as_u16(),
            pp_api::problem::without_envelope(response.json().await.unwrap_or(Value::Null)),
        )
    }

    /// Sends a request whose body is not parsed: `POST` refusals carry no JSON.
    #[cfg(feature = "desktop_rebuild")]
    async fn send_raw(&self, method: Method, path: &str, headers: &[(&str, &str)]) -> Response {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", &self.origin);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.unwrap()
    }

    async fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.unwrap();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
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

/// Without `desktop_rebuild`, jobs HTTP is not linked into pp-api.
#[cfg(not(feature = "desktop_rebuild"))]
async fn feature_off_404() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = job_rows(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    for path in [
        "/api/v1/jobs",
        "/api/v1/jobs/missing",
        "/jobs/missing",
        "/ws/jobs/missing",
    ] {
        let (status, body) = server.send(Method::GET, path, &[]).await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("jobs").is_none() && body.get("job_id").is_none());
    }
    server.stop().await;
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn jobs_list_matches_node_filters() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn jobs_list_flag_off_and_wrong_tenant_are_refused() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_detail_aliases_match() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_detail_head_has_no_body_and_keeps_status() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_detail_mutations_unregistered() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_response_refusal_releases_permit() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn retained_job_response_preserves_public_json() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_ws_rejects_invalid_origin_or_principal() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_ws_head_does_not_subscribe() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_ws_delivers_initial_and_terminal_snapshot() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_ws_revocation_closes_without_further_data() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_ws_reconnect_returns_authoritative_snapshot() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn job_ws_subscribe_during_commits_loses_and_repeats_nothing() {
    feature_off_404().await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn jobs_router_shutdown_closes_subscriptions() {
    feature_off_404().await;
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
async fn register_account(server: &Server, email: &str) -> (String, String) {
    let registered = server
        .client
        .post(format!("{}/auth/register", server.origin))
        .header("Origin", &server.origin)
        .json(&serde_json::json!({"email": email, "password": "password-1234"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200, "{email}");
    let cookie = registered.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let user: Value = pp_api::problem::without_envelope(registered.json().await.unwrap());
    let tenant = user["user"]["user_id"].as_str().unwrap().to_owned();
    (cookie, tenant)
}

#[cfg(feature = "desktop_rebuild")]
async fn register(server: &Server, email: &str) -> String {
    register_account(server, email).await.0
}

#[cfg(feature = "desktop_rebuild")]
async fn create_api_key(server: &Server, cookie: &str) -> String {
    let created = server
        .client
        .post(format!("{}/settings/api-keys", server.origin))
        .header("Origin", &server.origin)
        .header("Cookie", cookie)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let body: Value = pp_api::problem::without_envelope(created.json().await.unwrap());
    body["key"].as_str().unwrap().to_owned()
}

#[cfg(feature = "desktop_rebuild")]
fn kinds(body: &Value) -> Vec<(String, String)> {
    body["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| {
            (
                job["kind"].as_str().unwrap().into(),
                job["status"].as_str().unwrap().into(),
            )
        })
        .collect()
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn jobs_list_matches_node_filters() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use pp_storage::{
        auth::Secret,
        jobs::{
            CompletedResult, Credential, JobKind, Outcome, Payload, SourceUpdatesResult,
            UserOperation, WorkerAdmission, WorkerOperation,
        },
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
    let cookie = register(&bootstrap, "jobs-list@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let enqueue = |key: &str, payload: Payload| match owner
        .lock()
        .unwrap()
        .jobs(chosen)
        .unwrap()
        .submit(
            Credential::Session(Secret::new(token.clone())),
            UserOperation::Enqueue {
                key: key.into(),
                payload_version: 1,
                payload,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    {
        Outcome::Job(job, _) => job.job_id,
        _ => panic!("enqueued job expected"),
    };
    let worker = owner
        .lock()
        .unwrap()
        .job_worker_with_policy(
            chosen,
            WorkerAdmission {
                kinds: vec![
                    (JobKind::CheckSourceUpdates, 1),
                    (JobKind::ExportChecklistHtml, 1),
                ],
                total: 2,
                per_resource: 1,
                lease_seconds: 60,
            },
        )
        .unwrap();
    let check_id = enqueue("check", Payload::CheckSourceUpdates {});
    let mut lease = worker.claim().unwrap().unwrap().lease;
    assert_eq!(lease.job_id(), check_id);
    worker
        .update(&mut lease, WorkerOperation::Progress(10))
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::FinishPublic {
                artifact: None,
                result: Box::new(CompletedResult::SourceUpdates(SourceUpdatesResult {
                    checked: vec![],
                    skipped: vec![],
                    updates_available: 0,
                    checked_count: 0,
                })),
            },
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let export_id = enqueue("export", Payload::ExportChecklistHtml { profile_id: 7 });
    let mut lease = worker.claim().unwrap().unwrap().lease;
    assert_eq!(lease.job_id(), export_id);
    worker
        .update(&mut lease, WorkerOperation::Progress(10))
        .unwrap();
    worker.update(&mut lease, WorkerOperation::Fail).unwrap();

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let headers = [("Cookie", cookie.as_str())];
    let all: Vec<(String, String)> = [
        ("export-checklist-html", "error"),
        ("check-source-updates", "done"),
    ]
    .into_iter()
    .map(|(k, s)| (k.into(), s.into()))
    .collect();
    // Literal Node retained fixtures from fixtures/parity/jobs-engine.
    let cases: &[(&str, Vec<(String, String)>)] = &[
        ("", all.clone()),
        (
            "?status=done",
            vec![("check-source-updates".into(), "done".into())],
        ),
        ("?status=bogus", vec![]),
        (
            "?profile_id=7",
            vec![("export-checklist-html".into(), "error".into())],
        ),
        ("?profile_id=8", vec![]),
        ("?profile_id=NaN", vec![]),
        ("?since=1970-01-01T00:00:00.000Z", all.clone()),
        ("?since=2100-01-01T00:00:00.000Z", vec![]),
        ("?since=invalid", all.clone()),
        ("?status=&status=done", vec![]),
    ];
    for (query, want) in cases {
        let (status, body) = server
            .send(Method::GET, &format!("/api/v1/jobs{query}"), &headers)
            .await;
        assert_eq!(status, 200, "{query}");
        assert_eq!(kinds(&body), *want, "{query}");
    }
    let head = server
        .client
        .request(
            Method::HEAD,
            format!("{}/api/v1/jobs?since=invalid", server.origin),
        )
        .header("Origin", &server.origin)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.bytes().await.unwrap().len(), 0);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn jobs_list_flag_off_and_wrong_tenant_are_refused() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use pp_storage::{
        auth::Secret,
        jobs::{Credential, Outcome, Payload, UserOperation},
    };
    use std::{sync::atomic::AtomicBool, time::Duration};

    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = job_rows(&directory);
    for flag in [None, Some(""), Some("0"), Some("false"), Some("TRUE")] {
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let jobs = owner.lock().unwrap().jobs(policy()).unwrap();
        let mut server = Server::spawn(
            bound,
            jobs_router(
                JobsHttpConfig::from_startup(&origin, "default", flag).unwrap(),
                jobs,
            ),
        )
        .await;
        for method in [Method::GET, Method::HEAD] {
            let (status, body) = server.send(method, "/api/v1/jobs", &[]).await;
            assert_eq!(status, 404, "{flag:?}");
            assert!(body.get("jobs").is_none());
        }
        server.stop().await;
    }
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );

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
    let cookie_a = register(&bootstrap, "jobs-a@example.test").await;
    let cookie_b = register(&bootstrap, "jobs-b@example.test").await;
    let token_a = cookie_a.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;
    match owner
        .lock()
        .unwrap()
        .jobs(chosen)
        .unwrap()
        .submit(
            Credential::Session(Secret::new(token_a)),
            UserOperation::Enqueue {
                key: "owned".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    {
        Outcome::Job(_, _) => {}
        _ => panic!("enqueued job expected"),
    }
    let before_files = files(&directory);
    let before_jobs = job_rows(&directory);
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("true")).unwrap(),
            jobs,
        ),
    )
    .await;
    let (status, body) = server.send(Method::GET, "/api/v1/jobs", &[]).await;
    assert_eq!(status, 401);
    assert_eq!(
        body,
        serde_json::json!({"detail": "Authentication required"})
    );
    let (status, body) = server
        .send(
            Method::GET,
            "/api/v1/jobs",
            &[("Cookie", cookie_b.as_str())],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["jobs"], serde_json::json!([]));
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (before_files, before_jobs)
    );
    let (status, body) = server
        .send(
            Method::GET,
            "/api/v1/jobs",
            &[("Cookie", cookie_a.as_str())],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
    server.stop().await;
    shutdown(owner, directory);
}

/// The configured API-key tenant selects the tenant. A key minted for another
/// tenant is refused before any job row or file can change.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn jobs_list_refuses_wrong_tenant_api_key() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use pp_storage::{
        auth::Secret,
        jobs::{Credential, Outcome, Payload, UserOperation},
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
    let (cookie, tenant) = register_account(&bootstrap, "jobs-key@example.test").await;
    let key = create_api_key(&bootstrap, &cookie).await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;
    match owner
        .lock()
        .unwrap()
        .jobs(chosen)
        .unwrap()
        .submit(
            Credential::Session(Secret::new(token)),
            UserOperation::Enqueue {
                key: "owned".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    {
        Outcome::Job(_, _) => {}
        _ => panic!("enqueued job expected"),
    }

    let snapshot = files(&directory);
    let before = job_rows(&directory);
    let authorization = format!("Bearer {key}");

    // The key belongs to `tenant`, so the configured `default` tenant must refuse it.
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    for method in [Method::GET, Method::HEAD] {
        let get = method == Method::GET;
        let (status, body) = server
            .send(
                method,
                "/api/v1/jobs",
                &[("Authorization", authorization.as_str())],
            )
            .await;
        assert_eq!(status, 401);
        if get {
            assert_eq!(
                body,
                serde_json::json!({"detail": "Authentication required"})
            );
        } else {
            assert_eq!(body, Value::Null);
        }
    }
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot.clone(), before.clone())
    );
    server.stop().await;

    // The same key resolves under its own tenant, proving the refusal was tenant routing.
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, &tenant, Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let (status, body) = server
        .send(
            Method::GET,
            "/api/v1/jobs",
            &[("Authorization", authorization.as_str())],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
    server.stop().await;
    shutdown(owner, directory);
}

/// Foreign transport identity is refused by the shared `loopback::admit` before
/// the jobs handler runs, so no job row or file changes.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn jobs_list_refuses_foreign_host_and_origin() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use pp_storage::{
        auth::Secret,
        jobs::{Credential, Outcome, Payload, UserOperation},
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
    let (cookie, _tenant) = register_account(&bootstrap, "jobs-admit@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;
    match owner
        .lock()
        .unwrap()
        .jobs(chosen)
        .unwrap()
        .submit(
            Credential::Session(Secret::new(token)),
            UserOperation::Enqueue {
                key: "owned".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    {
        Outcome::Job(_, _) => {}
        _ => panic!("enqueued job expected"),
    }

    let snapshot = files(&directory);
    let before = job_rows(&directory);
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("true")).unwrap(),
            jobs,
        ),
    )
    .await;
    for method in [Method::GET, Method::HEAD] {
        let get = method == Method::GET;
        let (status, body) = server
            .send(
                method.clone(),
                "/api/v1/jobs",
                &[("Cookie", cookie.as_str()), ("Host", "evil.example")],
            )
            .await;
        assert_eq!(status, 403, "foreign host via {method}");
        if get {
            assert_eq!(body, serde_json::json!({"detail": "Invalid request host"}));
        } else {
            assert_eq!(body, Value::Null);
        }
        let (status, body) = server
            .send_with_origin(
                method.clone(),
                "/api/v1/jobs",
                &[("Cookie", cookie.as_str())],
                Some("http://evil.example"),
            )
            .await;
        assert_eq!(status, 403, "foreign origin via {method}");
        if get {
            assert_eq!(
                body,
                serde_json::json!({"detail": "Invalid request origin"})
            );
        } else {
            assert_eq!(body, Value::Null);
        }
    }
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );

    // The same route admits the loopback Host/Origin and returns the row.
    let (status, body) = server
        .send(Method::GET, "/api/v1/jobs", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
async fn enqueue_and_finish_public(
    owner: &Arc<Mutex<WriterOwner>>,
    chosen: pp_storage::auth::AuthPolicy,
    token: &str,
) -> String {
    use pp_storage::{
        auth::Secret,
        jobs::{
            CompletedResult, Credential, JobKind, Outcome, Payload, SourceUpdatesResult,
            UserOperation, WorkerAdmission, WorkerOperation,
        },
    };
    use std::{sync::atomic::AtomicBool, time::Duration};

    let job_id = match owner
        .lock()
        .unwrap()
        .jobs(chosen)
        .unwrap()
        .submit(
            Credential::Session(Secret::new(token.to_owned())),
            UserOperation::Enqueue {
                key: "detail".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    {
        Outcome::Job(job, _) => job.job_id,
        _ => panic!("enqueued job expected"),
    };
    let worker = owner
        .lock()
        .unwrap()
        .job_worker_with_policy(
            chosen,
            WorkerAdmission {
                kinds: vec![(JobKind::CheckSourceUpdates, 1)],
                total: 1,
                per_resource: 1,
                lease_seconds: 60,
            },
        )
        .unwrap();
    let mut lease = worker.claim().unwrap().unwrap().lease;
    assert_eq!(lease.job_id(), job_id);
    worker
        .update(&mut lease, WorkerOperation::Progress(40))
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::FinishPublic {
                artifact: None,
                result: Box::new(CompletedResult::SourceUpdates(SourceUpdatesResult {
                    checked: vec![],
                    skipped: vec![],
                    updates_available: 0,
                    checked_count: 0,
                })),
            },
        )
        .unwrap();
    job_id
}

/// `/jobs/{id}` and `/api/v1/jobs/{id}` return the same public snapshot; refusals
/// leave rows and files unchanged.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_detail_aliases_match() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie_a = register(&bootstrap, "job-detail-a@example.test").await;
    let cookie_b = register(&bootstrap, "job-detail-b@example.test").await;
    let token = cookie_a.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let snapshot = files(&directory);
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let headers = [("Cookie", cookie_a.as_str())];
    let (status_v1, body_v1) = server
        .send(Method::GET, &format!("/api/v1/jobs/{job_id}"), &headers)
        .await;
    let (status_root, body_root) = server
        .send(Method::GET, &format!("/jobs/{job_id}"), &headers)
        .await;
    assert_eq!(status_v1, 200);
    assert_eq!((status_root, body_root), (status_v1, body_v1.clone()));
    assert_eq!(body_v1["job_id"], job_id);
    assert_eq!(body_v1["status"], "done");
    assert_eq!(body_v1["kind"], "check-source-updates");
    assert!(body_v1.get("result").is_some());
    assert_eq!(body_v1["error"], Value::Null);

    for path in [
        format!("/api/v1/jobs/{job_id}-missing"),
        format!("/jobs/{job_id}-missing"),
    ] {
        let (status, body) = server.send(Method::GET, &path, &headers).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(body, serde_json::json!({"detail": "Job not found"}));
    }
    let (status, body) = server
        .send(
            Method::GET,
            &format!("/api/v1/jobs/{job_id}"),
            &[("Cookie", cookie_b.as_str())],
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(body, serde_json::json!({"detail": "Job not found"}));
    let (status, body) = server
        .send(Method::GET, &format!("/jobs/{job_id}"), &[])
        .await;
    assert_eq!(status, 401);
    assert_eq!(
        body,
        serde_json::json!({"detail": "Authentication required"})
    );
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );
    // Flag-off 404 for these aliases is covered by feature-off stubs and
    // `jobs_list_flag_off_and_wrong_tenant_are_refused` (empty router).
    server.stop().await;
    shutdown(owner, directory);
}

/// The API-key credential selects the configured tenant exactly as the session
/// cookie does: same status and body at both aliases, while a key minted for
/// another tenant stays refused. Rows are unchanged; only WAL bytes may grow,
/// because every admitted command opens a `BEGIN IMMEDIATE` transaction.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_detail_api_key_matches_cookie() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let (cookie_a, tenant_a) = register_account(&bootstrap, "job-detail-key-a@example.test").await;
    let (cookie_b, _tenant_b) = register_account(&bootstrap, "job-detail-key-b@example.test").await;
    let key_a = create_api_key(&bootstrap, &cookie_a).await;
    let key_b = create_api_key(&bootstrap, &cookie_b).await;
    let token = cookie_a.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, &tenant_a, Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let bearer_a = format!("Bearer {key_a}");
    let bearer_b = format!("Bearer {key_b}");
    for path in [format!("/api/v1/jobs/{job_id}"), format!("/jobs/{job_id}")] {
        let (status_key, body_key) = server
            .send(Method::GET, &path, &[("Authorization", bearer_a.as_str())])
            .await;
        let (status_cookie, body_cookie) = server
            .send(Method::GET, &path, &[("Cookie", cookie_a.as_str())])
            .await;
        assert_eq!(status_key, 200, "{path}");
        assert_eq!(
            (status_cookie, body_cookie),
            (status_key, body_key.clone()),
            "{path}"
        );
        assert_eq!(body_key["job_id"], job_id, "{path}");
        assert_eq!(body_key["status"], "done", "{path}");
        assert_eq!(body_key["kind"], "check-source-updates", "{path}");
        assert!(body_key.get("result").is_some(), "{path}");

        // A key minted for another tenant cannot read this row through either alias.
        let (status, body) = server
            .send(Method::GET, &path, &[("Authorization", bearer_b.as_str())])
            .await;
        assert_eq!(status, 401, "{path}");
        assert_eq!(
            body,
            serde_json::json!({"detail": "Authentication required"}),
            "{path}"
        );
    }
    assert_eq!(job_rows(&directory), before);
    server.stop().await;
    shutdown(owner, directory);
}

/// Mutation routes stay unregistered: `POST` answers 405 on both aliases with
/// an empty body. A `/ws/jobs/{id}` handshake with no session answers 401
/// before any subscription. No row is created or changed.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_detail_mutations_unregistered() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-detail-405@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let headers = [("Cookie", cookie.as_str())];

    // The path matches a registered route but the method does not, so the router
    // refuses with 405 rather than falling through to 404.
    for path in [format!("/jobs/{job_id}"), format!("/api/v1/jobs/{job_id}")] {
        let response = server.send_raw(Method::POST, &path, &headers).await;
        assert_eq!(response.status(), 405, "{path}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body["type"], "urn:print-partner:problem:method-not-allowed",
            "{path}"
        );
    }

    // No session is refused in the guard, before subscribe.
    let ws = server
        .client
        .get(format!("{}/ws/jobs/{job_id}", server.origin))
        .header("Origin", &server.origin)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13")
        .send()
        .await
        .unwrap();
    assert_eq!(ws.status(), 401);
    assert_eq!(
        pp_api::problem::without_envelope(ws.json::<serde_json::Value>().await.unwrap()),
        serde_json::json!({"detail": "Authentication required"})
    );

    // The same id still reads back over GET, so the 405 was method-only.
    let (status, body) = server
        .send(Method::GET, &format!("/jobs/{job_id}"), &headers)
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["job_id"], job_id);
    assert_eq!(job_rows(&directory), before);
    server.stop().await;
    shutdown(owner, directory);
}

/// HEAD shares status and auth with GET and returns an empty body.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_detail_head_has_no_body_and_keeps_status() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-detail-head@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let snapshot = files(&directory);
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("true")).unwrap(),
            jobs,
        ),
    )
    .await;
    for path in [format!("/api/v1/jobs/{job_id}"), format!("/jobs/{job_id}")] {
        let get = server
            .client
            .request(Method::GET, format!("{}{path}", server.origin))
            .header("Origin", &server.origin)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        let get_status = get.status();
        let get_body = get.bytes().await.unwrap();
        assert_eq!(get_status, 200, "{path}");
        assert!(!get_body.is_empty(), "{path}");

        let head = server
            .client
            .request(Method::HEAD, format!("{}{path}", server.origin))
            .header("Origin", &server.origin)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(head.status(), get_status, "{path}");
        assert_eq!(head.bytes().await.unwrap().len(), 0, "{path}");
    }

    let missing = format!("/jobs/{job_id}-missing");
    let get = server
        .client
        .request(Method::GET, format!("{}{missing}", server.origin))
        .header("Origin", &server.origin)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 404);
    let _ = get.bytes().await.unwrap();
    let head = server
        .client
        .request(Method::HEAD, format!("{}{missing}", server.origin))
        .header("Origin", &server.origin)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 404);
    assert_eq!(head.bytes().await.unwrap().len(), 0);
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );
    server.stop().await;
    shutdown(owner, directory);
}

/// Two refused reads must not stick the two serializer permits. The next read
/// of the real job is 200, and rows stay unchanged.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_response_refusal_releases_permit() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-permit@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let snapshot = files(&directory);
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let headers = [("Cookie", cookie.as_str())];
    for path in [
        format!("/jobs/{job_id}-missing"),
        format!("/api/v1/jobs/{job_id}-missing"),
    ] {
        let (status, body) = server.send(Method::GET, &path, &headers).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(body, serde_json::json!({"detail": "Job not found"}));
    }
    let (status, body) = server
        .send(Method::GET, &format!("/jobs/{job_id}"), &[])
        .await;
    assert_eq!(status, 401);
    assert_eq!(
        body,
        serde_json::json!({"detail": "Authentication required"})
    );
    let (status, body) = server
        .send(Method::GET, &format!("/api/v1/jobs/{job_id}"), &headers)
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["job_id"], job_id);
    assert_eq!(body["status"], "done");
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );
    server.stop().await;
    shutdown(owner, directory);
}

/// Read-one JSON is the public snapshot: the completed result is present and
/// `updated_at` is absent. The list keeps `updated_at` and the same result.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn retained_job_response_preserves_public_json() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use std::collections::BTreeSet;

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
    let cookie = register(&bootstrap, "job-public-json@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("true")).unwrap(),
            jobs,
        ),
    )
    .await;
    let headers = [("Cookie", cookie.as_str())];
    let expected_result = serde_json::json!({
        "checked": [],
        "skipped": [],
        "updates_available": 0,
        "checked_count": 0
    });
    let public_keys: BTreeSet<&str> = [
        "created_at",
        "error",
        "finished_at",
        "job_id",
        "kind",
        "message",
        "progress",
        "result",
        "status",
    ]
    .into_iter()
    .collect();
    for path in [format!("/jobs/{job_id}"), format!("/api/v1/jobs/{job_id}")] {
        let (status, body) = server.send(Method::GET, &path, &headers).await;
        assert_eq!(status, 200, "{path}");
        let keys: BTreeSet<_> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, public_keys, "{path}");
        assert!(body.get("updated_at").is_none(), "{path}");
        assert_eq!(body["job_id"], job_id, "{path}");
        assert_eq!(body["kind"], "check-source-updates", "{path}");
        assert_eq!(body["status"], "done", "{path}");
        assert_eq!(body["message"], "Complete", "{path}");
        assert_eq!(body["progress"], 100, "{path}");
        assert_eq!(body["error"], serde_json::Value::Null, "{path}");
        assert_eq!(body["result"], expected_result, "{path}");
        assert!(body["created_at"].as_str().unwrap().contains('T'), "{path}");
        assert!(
            body["finished_at"].as_str().unwrap().contains('T'),
            "{path}"
        );
    }
    let (status, list) = server.send(Method::GET, "/api/v1/jobs", &headers).await;
    assert_eq!(status, 200);
    let listed = &list["jobs"][0];
    assert_eq!(listed["result"], expected_result);
    assert!(
        listed
            .get("updated_at")
            .unwrap()
            .as_str()
            .unwrap()
            .contains('T')
    );
    assert_eq!(job_rows(&directory), before);
    server.stop().await;
    shutdown(owner, directory);
}

/// Wrong origin, wrong listener, missing credential, and an expired session
/// are refused before `subscribe`. Another tenant's `GET` upgrades and closes
/// `1008` `Job not found`. `HEAD` for that caller stays `404`. Rows and files
/// stay put.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_ws_rejects_invalid_origin_or_principal() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-ws-a@example.test").await;
    let other = register(&bootstrap, "job-ws-b@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let path = format!("/ws/jobs/{job_id}");
    let snapshot = files(&directory);
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs.clone(),
        ),
    )
    .await;
    let bound = bind().await;
    let mut wrong_listener = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup("http://127.0.0.1:1", "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;

    for method in [Method::GET, Method::HEAD] {
        let (status, body) =
            ws_exchange(&server, &method, &path, "https://evil.test", Some(&cookie)).await;
        assert_refusal(&method, status, &body, 403, "Invalid request origin");
        let (status, body) = ws_exchange(
            &wrong_listener,
            &method,
            &path,
            &wrong_listener.origin,
            Some(&cookie),
        )
        .await;
        assert_refusal(&method, status, &body, 403, "Invalid request host");
        let (status, body) = ws_exchange(&server, &method, &path, &server.origin, None).await;
        assert_refusal(&method, status, &body, 401, "Authentication required");
    }
    let (status, body) =
        ws_exchange(&server, &Method::HEAD, &path, &server.origin, Some(&other)).await;
    assert_refusal(&Method::HEAD, status, &body, 404, "Job not found");
    let mut foreign = open_socket(&server, &path, Some(&other)).await;
    assert_eq!(
        expect_close(&mut foreign).await,
        (1008, "Job not found".to_owned())
    );
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );

    expire_sessions(&directory);
    let expired_files = files(&directory);
    let expired_rows = job_rows(&directory);
    for method in [Method::GET, Method::HEAD] {
        let (status, body) =
            ws_exchange(&server, &method, &path, &server.origin, Some(&cookie)).await;
        assert_refusal(&method, status, &body, 401, "Authentication required");
    }
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (expired_files, expired_rows)
    );
    server.stop().await;
    wrong_listener.stop().await;
    shutdown(owner, directory);
}

/// `HEAD` still answers when all 128 stream slots are held, so it did not
/// subscribe. Dropping one slot lets a real `subscribe` through.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_ws_head_does_not_subscribe() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use pp_storage::{
        auth::Secret,
        jobs::{Credential, JobSubscription},
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
    let cookie = register(&bootstrap, "job-ws-head@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_and_finish_public(&owner, chosen, &token).await;
    let path = format!("/ws/jobs/{job_id}");
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let cancelled = AtomicBool::new(false);
    let mut held: Vec<JobSubscription> = Vec::new();
    for _ in 0..128 {
        held.push(
            jobs.subscribe(
                Credential::Session(Secret::new(token.clone())),
                job_id.clone(),
                &cancelled,
                Duration::from_secs(5),
            )
            .unwrap(),
        );
    }
    let snapshot = files(&directory);
    let before = job_rows(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs.clone(),
        ),
    )
    .await;
    let head = server
        .client
        .request(Method::HEAD, format!("{}{path}", server.origin))
        .header("Origin", &server.origin)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.bytes().await.unwrap().len(), 0);
    let (status, body) = ws_exchange(
        &server,
        &Method::HEAD,
        &format!("/ws/jobs/{job_id}-missing"),
        &server.origin,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 404);
    assert!(body.get("job_id").is_none());
    assert_eq!(
        (files(&directory), job_rows(&directory)),
        (snapshot, before)
    );

    held.pop();
    jobs.subscribe(
        Credential::Session(Secret::new(token)),
        job_id,
        &cancelled,
        Duration::from_secs(5),
    )
    .unwrap();
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
async fn ws_exchange(
    server: &Server,
    method: &Method,
    path: &str,
    origin: &str,
    cookie: Option<&str>,
) -> (u16, serde_json::Value) {
    let mut request = server
        .client
        .request(method.clone(), format!("{}{path}", server.origin))
        .header("Origin", origin)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13");
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.bytes().await.unwrap();
    let body = pp_api::problem::without_envelope(
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    );
    (status, body)
}

/// reqwest drops the body of a HEAD response, so only GET carries `detail`.
#[cfg(feature = "desktop_rebuild")]
fn assert_refusal(
    method: &Method,
    status: u16,
    body: &serde_json::Value,
    expected: u16,
    text: &str,
) {
    assert_eq!(status, expected, "{method} {text}");
    if *method == Method::GET {
        assert_eq!(
            body.get("detail").and_then(serde_json::Value::as_str),
            Some(text),
            "{method}"
        );
    } else {
        assert!(body.get("job_id").is_none(), "{method}");
    }
}

/// The first frame is the queued job. Claim may send one later snapshot before
/// the terminal `done` frame. That frame is followed by a normal close.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_ws_delivers_initial_and_terminal_snapshot() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-ws-pump@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_queued(&owner, chosen, &token).await;
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let mut socket = open_socket(&server, &format!("/ws/jobs/{job_id}"), Some(&cookie)).await;
    let initial = expect_text(&mut socket).await;
    assert_eq!(initial["job_id"], job_id);
    assert_ne!(initial["status"], "done");
    assert!(initial.get("updated_at").is_none());
    finish_queued(&owner, chosen, &job_id);
    let mut terminal = None;
    for _ in 0..4 {
        let frame = expect_text(&mut socket).await;
        assert_eq!(frame["job_id"], job_id);
        assert!(frame.get("updated_at").is_none());
        if frame["status"] == "done" {
            terminal = Some(frame);
            break;
        }
    }
    let terminal = terminal.expect("terminal snapshot");
    assert!(terminal.get("result").is_some());
    assert_eq!(expect_close(&mut socket).await, (1000, String::new()));
    server.stop().await;
    shutdown(owner, directory);
}

/// Logout revokes the live stream. The socket closes `1008` and sends no
/// snapshot after the one already delivered.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn job_ws_revocation_closes_without_further_data() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-ws-revoke@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_queued(&owner, chosen, &token).await;
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let mut socket = open_socket(&server, &format!("/ws/jobs/{job_id}"), Some(&cookie)).await;
    let initial = expect_text(&mut socket).await;
    assert_eq!(initial["job_id"], job_id);
    logout(&owner, chosen, &token);
    assert_eq!(
        expect_close(&mut socket).await,
        (1008, "Session revoked".to_owned())
    );
    server.stop().await;
    shutdown(owner, directory);
}

/// Seventeen snapshots published before the pump admits another frame close
/// the socket with 1011. The current_thread runtime keeps the pump from
/// running during those publishes. The next upgrade returns the current
/// public snapshot.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test(flavor = "current_thread")]
async fn job_ws_reconnect_returns_authoritative_snapshot() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-ws-reconnect@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_queued(&owner, chosen, &token).await;
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let mut lagged = open_socket(&server, &format!("/ws/jobs/{job_id}"), Some(&cookie)).await;
    let initial = expect_text(&mut lagged).await;
    assert_eq!(initial["job_id"], job_id);
    assert_ne!(initial["progress"], 16);
    publish_until_lag(&owner, chosen, &job_id);
    assert_eq!(
        expect_close(&mut lagged).await,
        (1011, "Job stream unavailable".to_owned())
    );
    let mut again = open_socket(&server, &format!("/ws/jobs/{job_id}"), Some(&cookie)).await;
    let current = expect_text(&mut again).await;
    assert_eq!(current["job_id"], job_id);
    assert_eq!(current["progress"], 16);
    assert!(current.get("updated_at").is_none());
    drop(again);
    server.stop().await;
    shutdown(owner, directory);
}

/// A worker commits the claim, progress 1 through 12, and the finish: 14
/// commits after the queued snapshot. The first socket opens before the claim
/// and must see 15 distinct frames. After each of the first 13 commits the
/// worker reports it, and the test upgrades one more socket while the worker
/// goes on to the next commit. Each of those sockets must see an exact suffix
/// of the 15 frames that starts at or after the commit it was opened behind.
/// A gap would be a lost event, a repeated frame a duplicate, and an earlier
/// start a stale snapshot. A socket opened after the finish replays only the
/// final snapshot, then closes 1000.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_ws_subscribe_during_commits_loses_and_repeats_nothing() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use std::time::Duration;

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
    let cookie = register(&bootstrap, "job-ws-window@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_queued(&owner, chosen, &token).await;
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let path = format!("/ws/jobs/{job_id}");
    let mut reference = open_socket(&server, &path, Some(&cookie)).await;
    let queued = expect_text(&mut reference).await;
    assert_eq!(queued["job_id"], job_id);
    assert_ne!(queued["status"], "done");

    let (committed, mut commits) = tokio::sync::mpsc::unbounded_channel();
    let writer = {
        let owner = owner.clone();
        let job_id = job_id.clone();
        tokio::task::spawn_blocking(move || {
            progress_then_finish(
                &owner,
                chosen,
                &job_id,
                12,
                Duration::from_millis(3),
                committed,
            )
        })
    };
    let mut joined = Vec::new();
    while let Some(count) = commits.recv().await {
        if count < 14 {
            joined.push((count, open_socket(&server, &path, Some(&cookie)).await));
        }
    }
    writer.await.unwrap();
    assert_eq!(joined.len(), 13);

    let (rest, close) = frames_until_close(&mut reference).await;
    assert_eq!(close, (1000, String::new()));
    let mut full = vec![queued];
    full.extend(rest);
    assert_eq!(full.len(), 15, "{full:?}");
    for (index, frame) in full.iter().enumerate() {
        assert!(!full[index + 1..].contains(frame), "repeated {frame:?}");
    }
    let progress: Vec<_> = full
        .iter()
        .filter_map(|frame| frame["progress"].as_i64())
        .collect();
    assert!(
        progress.windows(2).all(|pair| pair[0] < pair[1]),
        "{progress:?}"
    );
    assert_eq!(
        progress
            .iter()
            .copied()
            .filter(|step| (1..=12).contains(step))
            .collect::<Vec<_>>(),
        (1..=12).collect::<Vec<_>>()
    );
    let done = full.last().unwrap().clone();
    assert_eq!(done["status"], "done");
    assert_eq!(
        full.iter()
            .filter(|frame| frame["status"] == "done")
            .count(),
        1
    );

    for (count, mut socket) in joined {
        let (frames, close) = frames_until_close(&mut socket).await;
        assert_eq!(close, (1000, String::new()));
        assert!(!frames.is_empty());
        assert!(
            full.ends_with(&frames),
            "frames {frames:?} are not a suffix of {full:?}"
        );
        assert!(
            full.len() - frames.len() >= count,
            "socket opened after commit {count} started at frame {}",
            full.len() - frames.len()
        );
    }

    let mut after = open_socket(&server, &path, Some(&cookie)).await;
    assert_eq!(
        frames_until_close(&mut after).await,
        (vec![done], (1000, String::new()))
    );
    server.stop().await;
    shutdown(owner, directory);
}

/// Owner shutdown closes every live job socket and a later upgrade is refused.
#[cfg(feature = "desktop_rebuild")]
#[tokio::test(flavor = "current_thread")]
async fn jobs_router_shutdown_closes_subscriptions() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        jobs::{JobsHttpConfig, jobs_router},
    };

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
    let cookie = register(&bootstrap, "job-ws-shutdown@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let job_id = enqueue_queued(&owner, chosen, &token).await;
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let jobs = owner.lock().unwrap().jobs(chosen).unwrap();
    let mut server = Server::spawn(
        bound,
        jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            jobs,
        ),
    )
    .await;
    let path = format!("/ws/jobs/{job_id}");
    let mut first = open_socket(&server, &path, Some(&cookie)).await;
    let mut second = open_socket(&server, &path, Some(&cookie)).await;
    assert_eq!(expect_text(&mut first).await["job_id"], job_id);
    assert_eq!(expect_text(&mut second).await["job_id"], job_id);
    let rows = job_rows(&directory);
    close_owner(owner);
    assert_eq!(
        expect_close(&mut first).await,
        (1011, "Job stream unavailable".to_owned())
    );
    assert_eq!(
        expect_close(&mut second).await,
        (1011, "Job stream unavailable".to_owned())
    );
    let (status, body) =
        ws_exchange(&server, &Method::GET, &path, &server.origin, Some(&cookie)).await;
    assert_refusal(&Method::GET, status, &body, 500, "Jobs service unavailable");
    assert_eq!(job_rows(&directory), rows);
    server.stop().await;
    drop(directory);
}

#[cfg(feature = "desktop_rebuild")]
async fn enqueue_queued(
    owner: &Arc<Mutex<WriterOwner>>,
    chosen: pp_storage::auth::AuthPolicy,
    token: &str,
) -> String {
    use pp_storage::{
        auth::Secret,
        jobs::{Credential, Outcome, Payload, UserOperation},
    };
    use std::{sync::atomic::AtomicBool, time::Duration};

    match owner
        .lock()
        .unwrap()
        .jobs(chosen)
        .unwrap()
        .submit(
            Credential::Session(Secret::new(token.to_owned())),
            UserOperation::Enqueue {
                key: "stream".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    {
        Outcome::Job(job, _) => job.job_id,
        _ => panic!("enqueued job expected"),
    }
}

#[cfg(feature = "desktop_rebuild")]
fn finish_queued(
    owner: &Arc<Mutex<WriterOwner>>,
    chosen: pp_storage::auth::AuthPolicy,
    job_id: &str,
) {
    use pp_storage::jobs::{
        CompletedResult, JobKind, SourceUpdatesResult, WorkerAdmission, WorkerOperation,
    };

    let worker = owner
        .lock()
        .unwrap()
        .job_worker_with_policy(
            chosen,
            WorkerAdmission {
                kinds: vec![(JobKind::CheckSourceUpdates, 1)],
                total: 1,
                per_resource: 1,
                lease_seconds: 60,
            },
        )
        .unwrap();
    let mut lease = worker.claim().unwrap().unwrap().lease;
    assert_eq!(lease.job_id(), job_id);
    worker
        .update(
            &mut lease,
            WorkerOperation::FinishPublic {
                artifact: None,
                result: Box::new(CompletedResult::SourceUpdates(SourceUpdatesResult {
                    checked: vec![],
                    skipped: vec![],
                    updates_available: 0,
                    checked_count: 0,
                })),
            },
        )
        .unwrap();
}

/// Claim and progress 1 through 15 fill the 16-slot mailbox. Progress 16
/// closes it as lagged. The stored progress is 16.
#[cfg(feature = "desktop_rebuild")]
fn publish_until_lag(
    owner: &Arc<Mutex<WriterOwner>>,
    chosen: pp_storage::auth::AuthPolicy,
    job_id: &str,
) {
    use pp_storage::jobs::{JobKind, WorkerAdmission, WorkerOperation};

    let worker = owner
        .lock()
        .unwrap()
        .job_worker_with_policy(
            chosen,
            WorkerAdmission {
                kinds: vec![(JobKind::CheckSourceUpdates, 1)],
                total: 1,
                per_resource: 1,
                lease_seconds: 60,
            },
        )
        .unwrap();
    let mut lease = worker.claim().unwrap().unwrap().lease;
    assert_eq!(lease.job_id(), job_id);
    for step in 1..=16 {
        worker
            .update(&mut lease, WorkerOperation::Progress(step))
            .unwrap();
    }
}

/// Claims the job, commits progress 1 through `steps`, then finishes it.
/// After each commit it sends the number of commits so far, then waits
/// `pause`.
#[cfg(feature = "desktop_rebuild")]
fn progress_then_finish(
    owner: &Arc<Mutex<WriterOwner>>,
    chosen: pp_storage::auth::AuthPolicy,
    job_id: &str,
    steps: u8,
    pause: std::time::Duration,
    committed: tokio::sync::mpsc::UnboundedSender<usize>,
) {
    use pp_storage::jobs::{
        CompletedResult, JobKind, SourceUpdatesResult, WorkerAdmission, WorkerOperation,
    };

    let worker = owner
        .lock()
        .unwrap()
        .job_worker_with_policy(
            chosen,
            WorkerAdmission {
                kinds: vec![(JobKind::CheckSourceUpdates, 1)],
                total: 1,
                per_resource: 1,
                lease_seconds: 60,
            },
        )
        .unwrap();
    let mut lease = worker.claim().unwrap().unwrap().lease;
    assert_eq!(lease.job_id(), job_id);
    committed.send(1).unwrap();
    std::thread::sleep(pause);
    for step in 1..=steps {
        worker
            .update(&mut lease, WorkerOperation::Progress(step))
            .unwrap();
        committed.send(1 + usize::from(step)).unwrap();
        std::thread::sleep(pause);
    }
    worker
        .update(
            &mut lease,
            WorkerOperation::FinishPublic {
                artifact: None,
                result: Box::new(CompletedResult::SourceUpdates(SourceUpdatesResult {
                    checked: vec![],
                    skipped: vec![],
                    updates_available: 0,
                    checked_count: 0,
                })),
            },
        )
        .unwrap();
    committed.send(2 + usize::from(steps)).unwrap();
}

/// Reads text frames until the close frame. Each read waits at most 30 s.
#[cfg(feature = "desktop_rebuild")]
async fn frames_until_close(
    stream: &mut tokio::net::TcpStream,
) -> (Vec<serde_json::Value>, (u16, String)) {
    let mut frames = Vec::new();
    loop {
        let (opcode, payload) =
            tokio::time::timeout(std::time::Duration::from_secs(30), read_frame(stream))
                .await
                .expect("frame within 30 s");
        match opcode {
            1 => frames.push(serde_json::from_slice(&payload).unwrap()),
            8 => {
                let code = u16::from_be_bytes([payload[0], payload[1]]);
                let reason = String::from_utf8(payload[2..].to_vec()).unwrap();
                return (frames, (code, reason));
            }
            other => panic!("unexpected opcode {other}"),
        }
    }
}

#[cfg(feature = "desktop_rebuild")]
fn close_owner(owner: Arc<Mutex<WriterOwner>>) {
    let owner = Arc::try_unwrap(owner).unwrap_or_else(|_| panic!("router still holds storage"));
    owner.into_inner().unwrap().shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
fn logout(owner: &Arc<Mutex<WriterOwner>>, chosen: pp_storage::auth::AuthPolicy, token: &str) {
    use pp_storage::auth::{Request, Secret};
    use std::{sync::atomic::AtomicBool, time::Duration};

    owner
        .lock()
        .unwrap()
        .auth_with_policy(chosen)
        .unwrap()
        .submit(
            Request::Logout {
                token: Secret::new(token.to_owned()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
}

#[cfg(feature = "desktop_rebuild")]
async fn open_socket(server: &Server, path: &str, cookie: Option<&str>) -> tokio::net::TcpStream {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let host = server.origin.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let cookie_line = cookie
        .map(|value| format!("Cookie: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{cookie_line}\r\n",
        origin = server.origin,
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut seen = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.unwrap();
        seen.push(byte[0]);
        if seen.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8(seen).unwrap();
    assert!(text.starts_with("HTTP/1.1 101"), "{text}");
    stream
}

#[cfg(feature = "desktop_rebuild")]
async fn read_frame(stream: &mut tokio::net::TcpStream) -> (u8, Vec<u8>) {
    use tokio::io::AsyncReadExt;

    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await.unwrap();
    let opcode = head[0] & 0x0f;
    let len = match head[1] & 0x7f {
        126 => {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext).await.unwrap();
            u16::from_be_bytes(ext) as usize
        }
        len => len as usize,
    };
    assert!(len < 65_536, "test frames stay under 64KiB");
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload).await.unwrap();
    }
    (opcode, payload)
}

#[cfg(feature = "desktop_rebuild")]
async fn expect_text(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
    let (opcode, payload) = read_frame(stream).await;
    assert_eq!(opcode, 1, "text frame");
    serde_json::from_slice(&payload).unwrap()
}

#[cfg(feature = "desktop_rebuild")]
async fn expect_close(stream: &mut tokio::net::TcpStream) -> (u16, String) {
    let (opcode, payload) = read_frame(stream).await;
    assert_eq!(opcode, 8, "close frame");
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = String::from_utf8(payload[2..].to_vec()).unwrap();
    (code, reason)
}

#[cfg(feature = "desktop_rebuild")]
fn expire_sessions(directory: &Path) {
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(
        db.execute(
            "UPDATE sessions SET expires_at='2000-01-01T00:00:00.000Z'",
            [],
        )
        .unwrap()
            >= 1
    );
}

#[test]
fn scratch_root_is_empty_after_jobs_http_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_jobs_http_tests"]);
}
