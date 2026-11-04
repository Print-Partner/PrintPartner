use pp_storage::{
    Limits, WriterOwner,
    catalog::{CreateSource, Outcome, Request, SourceSummary},
};
#[cfg(feature = "desktop_rebuild")]
use pp_storage::{
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    catalog::Credentials,
};
use reqwest::{Client, Method};
use serde_json::{Map, Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

#[cfg(feature = "desktop_rebuild")]
#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;

#[cfg(feature = "desktop_rebuild")]
fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::FirstAccountOnly,
        first_user: FirstUserTenant::NewUser,
        session_tenant: SessionTenantPolicy::SingleAccountDefault,
    }
}

/// Returns the directory first so that, on panic, `owner` drops before the
/// scratch directory is removed.
fn open() -> (Scratch, Arc<Mutex<WriterOwner>>) {
    let directory = Scratch::new("pp-catalog-http-");
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    (directory, Arc::new(Mutex::new(owner)))
}

fn sample(name: &str, note: &str) -> CreateSource {
    let metadata: Map<String, Value> = serde_json::from_value(json!({
        "note": note,
        "remote_update_status": "up_to_date",
        "remote_checked_at": "2026-01-02T00:00:00.000Z"
    }))
    .unwrap();
    CreateSource {
        name: name.into(),
        url: Some(format!("https://example.test/{name}")),
        branch: Some("main".into()),
        tag: Some("v1".into()),
        source_kind: Some("github".into()),
        source_type: Some("git".into()),
        role: Some("reference".into()),
        metadata: Some(metadata),
    }
}

fn list(owner: &WriterOwner) -> Vec<SourceSummary> {
    match owner
        .local_source_catalog()
        .execute(Request::List {})
        .unwrap()
    {
        Outcome::Sources(rows) => rows,
        _ => panic!("list"),
    }
}

fn seed(owner: &WriterOwner) -> Vec<SourceSummary> {
    let catalog = owner.local_source_catalog();
    for (name, note) in [("Alpha", "kept"), ("Zebra", "other")] {
        catalog
            .execute(Request::Create {
                source: sample(name, note),
            })
            .unwrap();
    }
    let rows = list(owner);
    assert_eq!(
        rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
        ["Alpha", "Zebra"]
    );
    assert_eq!(rows[0].metadata.as_ref().unwrap()["note"], "kept");
    assert_eq!(rows[0].update_status.as_deref(), Some("up_to_date"));
    assert!(rows[0].content_available && rows[0].local_path.is_some());
    rows
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
                let rel = entry
                    .path()
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, entry.metadata().unwrap().len()));
            }
        }
    }
    out.sort();
    out
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
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap();
        Self {
            origin,
            client,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    async fn send(&self, method: Method, path: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        self.exchange(method, path, headers, None).await
    }

    async fn exchange(
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
        if let Some(body) = body {
            request = request.json(body);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        if matches!(status, 200 | 400 | 401 | 403 | 409) {
            assert_eq!(
                response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
                Some(if status == 200 {
                    "application/json"
                } else {
                    "application/problem+json"
                })
            );
        }
        (
            status,
            pp_api::problem::without_envelope(response.json().await.unwrap_or(Value::Null)),
        )
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

async fn listener() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    (listener, origin)
}

fn shutdown(owner: Arc<Mutex<WriterOwner>>, directory: Scratch) {
    seal(owner);
    drop(directory);
}

fn seal(owner: Arc<Mutex<WriterOwner>>) {
    let Ok(owner) = Arc::try_unwrap(owner) else {
        panic!("router still holds storage");
    };
    owner.into_inner().unwrap().shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
fn insert_safeguards(directory: &Path, history_id: i64, referenced_id: i64) {
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.execute_batch(&format!(
        "PRAGMA foreign_keys=ON;
         INSERT INTO source_revisions(tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) VALUES('default',{history_id},'retained-a','digest','{history_id}/revisions/retained-a','2026-10-03');
         INSERT INTO build_profiles(id,tenant_id,name) VALUES(1,'default','Build');
         INSERT INTO profile_layers(tenant_id,profile_id,layer_type,project_id) VALUES('default',1,'base',{referenced_id});"
    ))
    .unwrap();
}

#[cfg(feature = "desktop_rebuild")]
fn stored_rows(directory: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let mut rows = Vec::new();
    for sql in [
        "SELECT id || ':' || name FROM projects ORDER BY id",
        "SELECT project_id || ':' || upstream_revision_key || ':' || manifest_digest || ':' || snapshot_locator || ':' || synced_at FROM source_revisions ORDER BY id",
        "SELECT project_id || ':' || layer_type FROM profile_layers ORDER BY id",
    ] {
        let mut statement = db.prepare(sql).unwrap();
        rows.extend(
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(|row| row.unwrap()),
        );
    }
    rows
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn catalog_reads_flag_off_are_404() {
    let (directory, owner) = open();
    let before = {
        let guard = owner.lock().unwrap();
        seed(&guard)
    };
    let snapshot = files(&directory);
    let (listener, _) = listener().await;
    let mut server = Server::spawn(listener, axum::Router::new()).await;
    refuse_mounted_reads(&server).await;
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(list(&owner.lock().unwrap()), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn catalog_create_patch_match_node_coercions() {
    unmounted_write(
        Method::POST,
        "/sources",
        json!({"name":12,"source_kind":"GitHub","metadata":{"category":" A / B "}}),
    )
    .await;
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn catalog_patch_wrong_owner_is_refused() {
    unmounted_write(
        Method::PATCH,
        "/api/v1/sources/1",
        json!({"name":"Stolen","metadata":{"note":"stolen"}}),
    )
    .await;
}

#[cfg(not(feature = "desktop_rebuild"))]
async fn unmounted_write(method: Method, path: &str, body: Value) {
    let (directory, owner) = open();
    let before = {
        let guard = owner.lock().unwrap();
        seed(&guard)
    };
    let snapshot = files(&directory);
    let (listener, _) = listener().await;
    let mut server = Server::spawn(listener, axum::Router::new()).await;
    let (status, response) = server.exchange(method, path, &[], Some(&body)).await;
    assert_eq!(status, 404, "{path}");
    assert!(response.get("name").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(list(&owner.lock().unwrap()), before);
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_reads_flag_off_are_404() {
    use pp_api::catalog::{CatalogHttpConfig, admit_catalog_routes};
    let (directory, owner) = open();
    let before = {
        let guard = owner.lock().unwrap();
        seed(&guard)
    };
    let snapshot = files(&directory);
    for flag in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("TRUE"),
        Some(" true "),
    ] {
        let (bound, origin) = listener().await;
        let router = admit_catalog_routes(
            CatalogHttpConfig::from_startup(&origin, "default", policy(), flag).unwrap(),
            Arc::clone(&owner),
        );
        let mut server = Server::spawn(bound, router).await;
        refuse_mounted_reads(&server).await;
        server.stop().await;
    }
    assert_eq!(files(&directory), snapshot);
    assert_eq!(list(&owner.lock().unwrap()), before);
    shutdown(owner, directory);
}

async fn refuse_mounted_reads(server: &Server) {
    for path in [
        "/sources",
        "/sources/1",
        "/api/v1/sources",
        "/api/v1/sources/1",
        "/sources/activity",
        "/api/v1/sources/activity",
        "/sources/1/import-rules",
        "/api/v1/sources/1/import-rules",
        "/sources/1/naming",
        "/api/v1/sources/1/naming",
        "/settings/source-categories",
        "/api/v1/settings/source-categories",
        "/settings/stl-naming",
        "/api/v1/settings/stl-naming",
    ] {
        let (status, body) = server.send(Method::GET, path, &[]).await;
        assert_eq!(status, 404, "{path}");
        assert!(
            body.get("sources").is_none()
                && body.get("name").is_none()
                && body.get("categories").is_none()
                && body.get("events").is_none()
                && body.get("rules").is_none()
                && body.get("use_defaults").is_none()
                && body.get("effective").is_none()
                && body.get("profile").is_none()
        );
    }
    for path in ["/sources/1", "/api/v1/sources/1"] {
        let (status, body) = server
            .exchange(
                Method::PATCH,
                path,
                &[],
                Some(&json!({"name":"Stolen","metadata":{"note":"stolen"}})),
            )
            .await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("name").is_none());
    }
    let (status, _) = server.send(Method::POST, "/sources", &[]).await;
    assert_eq!(status, 404);
    for path in [
        "/settings/stl-naming/preview",
        "/api/v1/settings/stl-naming/preview",
    ] {
        let (status, body) = server
            .exchange(
                Method::POST,
                path,
                &[],
                Some(&json!({"relative_path":"gear.stl"})),
            )
            .await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("role").is_none() && body.get("filename").is_none());
    }
    for path in ["/sources/1", "/api/v1/sources/1"] {
        let (status, body) = server.send(Method::DELETE, path, &[]).await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("name").is_none());
    }
    for path in [
        "/settings/source-categories",
        "/api/v1/settings/source-categories",
    ] {
        let (status, body) = server
            .exchange(
                Method::PUT,
                path,
                &[],
                Some(&json!({"categories":["Stolen"],"replacements":{}})),
            )
            .await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("categories").is_none());
    }
    for path in ["/sources/1/naming", "/api/v1/sources/1/naming"] {
        let (status, body) = server
            .exchange(Method::PUT, path, &[], Some(&json!({"use_defaults":true})))
            .await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("use_defaults").is_none() && body.get("effective").is_none());
    }
    for path in ["/sources/1/import-rules", "/api/v1/sources/1/import-rules"] {
        let (status, body) = server
            .exchange(Method::PUT, path, &[], Some(&json!({"rules":["parts/"]})))
            .await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("rules").is_none());
    }
    for path in ["/settings/stl-naming", "/api/v1/settings/stl-naming"] {
        let (status, body) = server
            .exchange(
                Method::PUT,
                path,
                &[],
                Some(&json!({"profile":{"quantity":{"default":2}}})),
            )
            .await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("profile").is_none());
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_reads_match_existing_source_metadata() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        catalog::{CatalogHttpConfig, admit_catalog_routes},
    };
    let (directory, owner) = open();
    let seeded = {
        let guard = owner.lock().unwrap();
        seed(&guard)
    };
    let (bound, origin) = listener().await;
    let auth = owner.lock().unwrap().auth_with_policy(policy()).unwrap();
    let router = admit_catalog_routes(
        CatalogHttpConfig::from_startup(&origin, "default", policy(), Some("true")).unwrap(),
        Arc::clone(&owner),
    )
    .merge(auth_router(
        AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false).unwrap(),
        auth,
        ProviderClient::new(None, None).unwrap(),
        ResetMailer::disabled(),
    ));
    let mut server = Server::spawn(bound, router).await;
    let registered = server
        .client
        .post(format!("{}/auth/register", server.origin))
        .header("Origin", &server.origin)
        .json(&json!({"email":"reader@example.test","password":"password-1234"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200);
    let cookie = registered.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let token = cookie.strip_prefix("pp_session=").unwrap();
    let session_rows = {
        let guard = owner.lock().unwrap();
        match guard
            .source_catalog_with_policy(Credentials::Session(Secret::new(token.into())), policy())
            .unwrap()
            .execute(Request::List {})
            .unwrap()
        {
            Outcome::Sources(rows) => rows,
            _ => panic!("session list"),
        }
    };
    assert_eq!(session_rows, seeded);
    let expected = json!({ "sources": session_rows });
    for prefix in ["", "/api/v1"] {
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/sources"),
                &[("Cookie", &cookie)],
            )
            .await;
        assert_eq!(status, 200, "{prefix}");
        assert_eq!(body, expected);
        for row in &session_rows {
            let (status, body) = server
                .send(
                    Method::GET,
                    &format!("{prefix}/sources/{}", row.id),
                    &[("Cookie", &cookie)],
                )
                .await;
            assert_eq!(status, 200);
            assert_eq!(body, serde_json::to_value(row).unwrap());
        }
    }
    let key = server
        .client
        .post(format!("{}/settings/api-keys", server.origin))
        .header("Origin", &server.origin)
        .header("Cookie", &cookie)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(key.status(), 201);
    let key: Value = pp_api::problem::without_envelope(key.json().await.unwrap());
    let bearer = format!("Bearer {}", key["key"].as_str().unwrap());
    let (status, body) = server
        .send(Method::GET, "/sources", &[("Authorization", &bearer)])
        .await;
    assert_eq!(status, 200);
    assert_eq!(body, expected);
    let rows = list(&owner.lock().unwrap());
    let snapshot = files(&directory);
    let (status, body) = server.send(Method::GET, "/sources", &[]).await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (401, Some("Authentication required"))
    );
    let (status, body) = server
        .send(
            Method::GET,
            "/sources",
            &[("Cookie", &cookie), ("x-forwarded-for", "203.0.113.8")],
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (403, Some("Invalid request host"))
    );
    let (status, body) = server
        .send(Method::GET, "/sources/nope", &[("Cookie", &cookie)])
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Source not found"))
    );
    let (status, body) = server
        .exchange(
            Method::POST,
            "/sources",
            &[("Cookie", &cookie)],
            Some(&json!({})),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Source name is required"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(list(&owner.lock().unwrap()), rows);
    let (status, body) = server
        .send(Method::GET, "/sources/999999", &[("Cookie", &cookie)])
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Source not found"))
    );
    assert_eq!(list(&owner.lock().unwrap()), rows);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
fn node_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../fixtures/parity/catalog-source-http/node.json"
    ))
    .unwrap()
}

#[cfg(feature = "desktop_rebuild")]
fn assert_source(actual: &Value, expected: &Value, directory: &Path) {
    for (key, value) in expected.as_object().unwrap() {
        assert_eq!(&actual[key], value, "{key}");
    }
    let id = actual["id"].as_i64().unwrap();
    let path = directory
        .canonicalize()
        .unwrap()
        .join("repos")
        .join(id.to_string());
    assert_eq!(actual["local_path"].as_str(), path.to_str());
    assert_eq!(
        actual.as_object().unwrap().len(),
        expected.as_object().unwrap().len() + 2
    );
}

#[cfg(feature = "desktop_rebuild")]
async fn serve(account_tenant: bool) -> (Scratch, Arc<Mutex<WriterOwner>>, Server, String) {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        catalog::{CatalogHttpConfig, admit_catalog_routes},
    };
    use pp_storage::auth::SessionTenantPolicy;
    let (directory, owner) = open();
    let chosen = if account_tenant {
        AuthPolicy {
            registration: pp_storage::auth::RegistrationPolicy::FirstAccountOnly,
            first_user: pp_storage::auth::FirstUserTenant::NewUser,
            session_tenant: SessionTenantPolicy::AccountTenant,
        }
    } else {
        policy()
    };
    let (bound, origin) = listener().await;
    let auth = owner.lock().unwrap().auth_with_policy(chosen).unwrap();
    let router = admit_catalog_routes(
        CatalogHttpConfig::from_startup(&origin, "default", chosen, Some("1")).unwrap(),
        Arc::clone(&owner),
    )
    .merge(auth_router(
        AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false).unwrap(),
        auth,
        ProviderClient::new(None, None).unwrap(),
        ResetMailer::disabled(),
    ));
    let server = Server::spawn(bound, router).await;
    let registered = server
        .client
        .post(format!("{}/auth/register", server.origin))
        .header("Origin", &server.origin)
        .json(&json!({"email":"owner@example.test","password":"password-1234"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200);
    let cookie = registered.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (directory, owner, server, cookie)
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_create_patch_match_node_coercions() {
    let fixture = node_fixture();
    let (directory, owner, mut server, cookie) = serve(false).await;
    let (status, created) = server
        .exchange(
            Method::POST,
            "/sources",
            &[("Cookie", &cookie)],
            Some(&fixture["create"]["body"]),
        )
        .await;
    assert_eq!(status, 200);
    assert_source(&created, &fixture["create"]["source"], &directory);
    let id = created["id"].as_i64().unwrap();
    let (status, patched) = server
        .exchange(
            Method::PATCH,
            &format!("/api/v1/sources/{id}"),
            &[("Cookie", &cookie)],
            Some(&fixture["patch"]["body"]),
        )
        .await;
    assert_eq!(status, 200);
    assert_source(&patched, &fixture["patch"]["source"], &directory);
    let snapshot = files(&directory);
    let rows = list(&owner.lock().unwrap());
    let (status, body) = server
        .exchange(
            Method::PATCH,
            &format!("/sources/{id}"),
            &[("Cookie", &cookie)],
            Some(&json!({"local_path":"/tmp/outside","metadata":{"note":"nope"}})),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Raw local_path requires native Source selection"))
    );
    let (status, stored) = server
        .send(
            Method::GET,
            &format!("/sources/{id}"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(stored, patched);
    assert_eq!(files(&directory), snapshot);
    assert_eq!(list(&owner.lock().unwrap()), rows);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_patch_wrong_owner_is_refused() {
    let (directory, owner) = open();
    let seeded = {
        let guard = owner.lock().unwrap();
        seed(&guard)
    };
    let foreign_id = seeded[0].id;
    let (bound, origin) = listener().await;
    let account = AuthPolicy {
        registration: pp_storage::auth::RegistrationPolicy::FirstAccountOnly,
        first_user: pp_storage::auth::FirstUserTenant::NewUser,
        session_tenant: pp_storage::auth::SessionTenantPolicy::AccountTenant,
    };
    let auth = owner.lock().unwrap().auth_with_policy(account).unwrap();
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        catalog::{CatalogHttpConfig, admit_catalog_routes},
    };
    let router = admit_catalog_routes(
        CatalogHttpConfig::from_startup(&origin, "default", account, Some("true")).unwrap(),
        Arc::clone(&owner),
    )
    .merge(auth_router(
        AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false).unwrap(),
        auth,
        ProviderClient::new(None, None).unwrap(),
        ResetMailer::disabled(),
    ));
    let mut server = Server::spawn(bound, router).await;
    let registered = server
        .client
        .post(format!("{}/auth/register", server.origin))
        .header("Origin", &server.origin)
        .json(&json!({"email":"stranger@example.test","password":"password-1234"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200);
    let cookie = registered.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (status, body) = server
        .send(Method::GET, "/sources", &[("Cookie", &cookie)])
        .await;
    assert_eq!((status, &body), (200, &json!({"sources":[]})));
    let snapshot = files(&directory);
    let (status, body) = server
        .exchange(
            Method::PATCH,
            &format!("/api/v1/sources/{foreign_id}"),
            &[("Cookie", &cookie)],
            Some(&json!({"name":"Stolen","metadata":{"note":"stolen"}})),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Source not found"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(list(&owner.lock().unwrap()), seeded);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_delete_wrong_owner_is_refused() {
    let (directory, owner) = open();
    let seeded = {
        let guard = owner.lock().unwrap();
        seed(&guard)
    };
    let foreign_id = seeded[0].id;
    let (bound, origin) = listener().await;
    let account = AuthPolicy {
        registration: pp_storage::auth::RegistrationPolicy::FirstAccountOnly,
        first_user: pp_storage::auth::FirstUserTenant::NewUser,
        session_tenant: pp_storage::auth::SessionTenantPolicy::AccountTenant,
    };
    let auth = owner.lock().unwrap().auth_with_policy(account).unwrap();
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        catalog::{CatalogHttpConfig, admit_catalog_routes},
    };
    let router = admit_catalog_routes(
        CatalogHttpConfig::from_startup(&origin, "default", account, Some("true")).unwrap(),
        Arc::clone(&owner),
    )
    .merge(auth_router(
        AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false).unwrap(),
        auth,
        ProviderClient::new(None, None).unwrap(),
        ResetMailer::disabled(),
    ));
    let mut server = Server::spawn(bound, router).await;
    let registered = server
        .client
        .post(format!("{}/auth/register", server.origin))
        .header("Origin", &server.origin)
        .json(&json!({"email":"stranger@example.test","password":"password-1234"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200);
    let cookie = registered.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (status, body) = server
        .send(Method::GET, "/sources", &[("Cookie", &cookie)])
        .await;
    assert_eq!((status, &body), (200, &json!({"sources":[]})));
    let snapshot = files(&directory);
    let (status, body) = server
        .send(
            Method::DELETE,
            &format!("/api/v1/sources/{foreign_id}"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Source not found"))
    );
    assert_eq!(files(&directory), snapshot);
    assert_eq!(list(&owner.lock().unwrap()), seeded);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
async fn post_source(server: &Server, cookie: &str, name: &str) -> Value {
    let (status, body) = server
        .exchange(
            Method::POST,
            "/sources",
            &[("Cookie", cookie)],
            Some(&json!({"name": name, "source_kind": "local"})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    body
}

#[cfg(feature = "desktop_rebuild")]
async fn refuse(server: &Server, cookie: &str, path: &str, detail: &str) {
    let (status, body) = server
        .send(Method::DELETE, path, &[("Cookie", cookie)])
        .await;
    assert_eq!((status, body), (409, json!({ "detail": detail })), "{path}");
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_delete_busy_source_preserves_revisions() {
    let (directory, owner, mut server, cookie) = serve(false).await;
    let history = post_source(&server, &cookie, "History").await;
    let referenced = post_source(&server, &cookie, "Referenced").await;
    let history_id = history["id"].as_i64().unwrap();
    let referenced_id = referenced["id"].as_i64().unwrap();
    insert_safeguards(&directory, history_id, referenced_id);
    let before = stored_rows(&directory);
    let mut lease = {
        let guard = owner.lock().unwrap();
        guard
            .local_source_catalog()
            .begin_work(
                history_id,
                &std::sync::atomic::AtomicBool::new(false),
                std::time::Duration::from_secs(5),
            )
            .unwrap()
    };
    refuse(
        &server,
        &cookie,
        &format!("/sources/{history_id}"),
        "Source has active work and cannot be deleted",
    )
    .await;
    refuse(
        &server,
        &cookie,
        &format!("/api/v1/sources/{referenced_id}"),
        "Source is referenced and cannot be deleted",
    )
    .await;
    assert_eq!(stored_rows(&directory), before);
    lease.release().unwrap();
    refuse(
        &server,
        &cookie,
        &format!("/sources/{history_id}"),
        "Source has immutable revision history and cannot be deleted",
    )
    .await;
    assert_eq!(stored_rows(&directory), before);
    for (id, previous) in [(history_id, &history), (referenced_id, &referenced)] {
        let (status, body) = server
            .send(
                Method::GET,
                &format!("/sources/{id}"),
                &[("Cookie", &cookie)],
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(body["id"], previous["id"]);
        assert_eq!(body["name"], previous["name"]);
    }
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
fn stored_categories(directory: &Path, tenant: &str) -> Option<String> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    match db.query_row(
        "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='source_categories'",
        [tenant],
        |row| row.get(0),
    ) {
        Ok(value) => Some(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(error) => panic!("{error}"),
    }
}

#[cfg(feature = "desktop_rebuild")]
fn category_tenants(directory: &Path) -> Vec<(String, String)> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let mut statement = db
        .prepare(
            "SELECT tenant_id, value FROM app_settings WHERE key='source_categories' ORDER BY tenant_id",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[cfg(feature = "desktop_rebuild")]
fn default_naming_profile() -> Value {
    json!({
        "roles": [
            {"id":"primary","label":"Primary","markers":[]},
            {"id":"accent","label":"Accent","markers":["[a]"]},
            {"id":"clear","label":"Clear","markers":["[c]"]},
            {"id":"opaque","label":"Opaque","markers":["[o]"]}
        ],
        "quantity":{"regex":"[ _]x([0-9]+)\\.stl$","default":1},
        "slug":{"strip_markers":true,"strip_quantity":true},
        "folder_rules":[],
        "export_role_order":["primary","accent","clear","opaque"]
    })
}

#[cfg(feature = "desktop_rebuild")]
fn database_bytes(directory: &Path) -> Vec<u8> {
    std::fs::read(directory.join("print-partner.db")).unwrap()
}

#[cfg(feature = "desktop_rebuild")]
fn storage_preview(owner: &WriterOwner, relative_path: &str, profile: Option<Value>) -> Value {
    use pp_storage::catalog::{NamingOverride, Outcome, Request};
    let profile = profile.map(|value| serde_json::from_value::<NamingOverride>(value).unwrap());
    match owner
        .local_source_catalog()
        .execute(Request::PreviewNaming {
            relative_path: relative_path.into(),
            profile,
        })
        .unwrap()
    {
        Outcome::Preview(preview) => serde_json::to_value(preview).unwrap(),
        _ => panic!("preview"),
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_global_naming_round_trip() {
    use pp_storage::catalog::{Outcome, Request};
    let (directory, owner, mut server, cookie) = serve(false).await;
    let default = default_naming_profile();
    let (status, got) = server
        .send(Method::GET, "/settings/stl-naming", &[("Cookie", &cookie)])
        .await;
    assert_eq!(status, 200, "{got}");
    assert_eq!(got, json!({"profile": default}));
    let storage = {
        let guard = owner.lock().unwrap();
        match guard
            .local_source_catalog()
            .execute(Request::GetGlobalNaming {})
            .unwrap()
        {
            Outcome::Data(value) => value,
            _ => panic!("global naming"),
        }
    };
    assert_eq!(got["profile"], storage);
    let mut profile = default_naming_profile();
    profile["quantity"]["default"] = json!(9);
    profile["roles"][0]["label"] = json!("Main");
    for prefix in ["", "/api/v1"] {
        let (status, saved) = server
            .exchange(
                Method::PUT,
                &format!("{prefix}/settings/stl-naming"),
                &[("Cookie", &cookie)],
                Some(&json!({"profile": profile})),
            )
            .await;
        assert_eq!(status, 200, "{prefix} {saved}");
        assert_eq!(saved, json!({"profile": profile}));
        let (status, again) = server
            .send(
                Method::GET,
                &format!("{prefix}/settings/stl-naming"),
                &[("Cookie", &cookie)],
            )
            .await;
        assert_eq!((status, again), (200, saved));
    }
    let (status, reset) = server
        .exchange(
            Method::PUT,
            "/settings/stl-naming",
            &[("Cookie", &cookie)],
            Some(&json!({})),
        )
        .await;
    assert_eq!(status, 200, "{reset}");
    assert_eq!(reset, json!({"profile": default}));
    let before_bytes = database_bytes(&directory);
    let snapshot = files(&directory);
    let mut blank = default_naming_profile();
    blank["roles"][0]["label"] = json!(" ");
    let (status, refused) = server
        .exchange(
            Method::PUT,
            "/api/v1/settings/stl-naming",
            &[("Cookie", &cookie)],
            Some(&json!({"profile": blank})),
        )
        .await;
    assert_eq!(
        (status, refused["detail"].as_str()),
        (400, Some("Blank naming label"))
    );
    assert_eq!(database_bytes(&directory), before_bytes);
    assert_eq!(files(&directory), snapshot);
    let (status, after) = server
        .send(Method::GET, "/settings/stl-naming", &[("Cookie", &cookie)])
        .await;
    assert_eq!((status, after), (200, reset));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_global_naming_preserves_source_override() {
    let (directory, owner, mut server, cookie) = serve(false).await;
    let created = post_source(&server, &cookie, "Override").await;
    let id = created["id"].as_i64().unwrap();
    let mut override_profile = default_naming_profile();
    override_profile["quantity"]["default"] = json!(4);
    override_profile["roles"][1]["markers"] = json!(["[x]"]);
    let (status, saved) = server
        .exchange(
            Method::PUT,
            &format!("/sources/{id}/naming"),
            &[("Cookie", &cookie)],
            Some(&json!({"use_defaults":false,"override":override_profile})),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["use_defaults"], false);
    assert_eq!(saved["effective"]["quantity"]["default"], 4);
    let before_naming = saved.clone();
    let mut global = default_naming_profile();
    global["quantity"]["default"] = json!(11);
    global["roles"][0]["label"] = json!("Global");
    let (status, updated) = server
        .exchange(
            Method::PUT,
            "/settings/stl-naming",
            &[("Cookie", &cookie)],
            Some(&json!({"profile": global})),
        )
        .await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["profile"]["quantity"]["default"], 11);
    let (status, after) = server
        .send(
            Method::GET,
            &format!("/api/v1/sources/{id}/naming"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, after), (200, before_naming));
    let (status, defaults) = server
        .exchange(
            Method::PUT,
            &format!("/sources/{id}/naming"),
            &[("Cookie", &cookie)],
            Some(&json!({"use_defaults":true})),
        )
        .await;
    assert_eq!(status, 200, "{defaults}");
    assert_eq!(defaults["use_defaults"], true);
    assert_eq!(defaults["effective"]["quantity"]["default"], 11);
    assert_eq!(defaults["effective"]["roles"][0]["label"], "Global");
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_naming_preview_matches_storage() {
    let (directory, owner, mut server, cookie) = serve(false).await;
    let created = post_source(&server, &cookie, "Naming").await;
    let id = created["id"].as_i64().unwrap();
    let mut override_profile = default_naming_profile();
    override_profile["roles"][1]["markers"] = json!(["[custom]"]);
    override_profile["quantity"]["default"] = json!(7);
    let (status, saved) = server
        .exchange(
            Method::PUT,
            &format!("/sources/{id}/naming"),
            &[("Cookie", &cookie)],
            Some(&json!({"use_defaults":false,"override":override_profile})),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["use_defaults"], false);
    assert_eq!(saved["effective"]["quantity"]["default"], 7);
    assert_eq!(
        saved["effective"]["roles"][1]["markers"],
        json!(["[custom]"])
    );
    let (status, got) = server
        .send(
            Method::GET,
            &format!("/api/v1/sources/{id}/naming"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, got), (200, saved.clone()));
    let relative_path = "[custom]bracket x3.stl";
    let preview_profile = json!({
        "roles":[{"id":"accent","markers":["[custom]"]}],
        "quantity":{"default":7}
    });
    let before_bytes = database_bytes(&directory);
    let snapshot = files(&directory);
    let expected = storage_preview(
        &owner.lock().unwrap(),
        relative_path,
        Some(preview_profile.clone()),
    );
    assert_eq!(
        expected,
        json!({
            "role":"accent",
            "quantity":3,
            "part_slug":"bracket",
            "filename":"[custom]bracket x3.stl"
        })
    );
    for prefix in ["", "/api/v1"] {
        let (status, body) = server
            .exchange(
                Method::POST,
                &format!("{prefix}/settings/stl-naming/preview"),
                &[("Cookie", &cookie)],
                Some(&json!({
                    "relative_path": relative_path,
                    "profile": preview_profile
                })),
            )
            .await;
        assert_eq!(status, 200, "{prefix} {body}");
        assert_eq!(body, expected, "{prefix}");
    }
    // Preview used the same accent marker and quantity default as the saved override.
    assert_eq!(saved["effective"]["quantity"]["default"], 7);
    assert_eq!(saved["effective"]["roles"][1]["markers"][0], "[custom]");
    assert_eq!(expected["role"], "accent");
    assert_eq!(database_bytes(&directory), before_bytes);
    assert_eq!(files(&directory), snapshot);
    let (status, after) = server
        .send(
            Method::GET,
            &format!("/sources/{id}/naming"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, after), (200, saved));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_invalid_override_does_not_write() {
    let (directory, owner, mut server, cookie) = serve(false).await;
    let created = post_source(&server, &cookie, "Guard").await;
    let id = created["id"].as_i64().unwrap();
    let mut good = default_naming_profile();
    good["roles"][0]["label"] = json!("Kept");
    let (status, saved) = server
        .exchange(
            Method::PUT,
            &format!("/sources/{id}/naming"),
            &[("Cookie", &cookie)],
            Some(&json!({"use_defaults":false,"override":good})),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    let before_bytes = database_bytes(&directory);
    let snapshot = files(&directory);
    let mut blank = default_naming_profile();
    blank["roles"][0]["label"] = json!(" ");
    let (status, body) = server
        .exchange(
            Method::PUT,
            &format!("/api/v1/sources/{id}/naming"),
            &[("Cookie", &cookie)],
            Some(&json!({"use_defaults":false,"override":blank})),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Blank naming label"))
    );
    let (status, body) = server
        .exchange(
            Method::POST,
            "/settings/stl-naming/preview",
            &[("Cookie", &cookie)],
            Some(&json!({
                "relative_path":"gear.stl",
                "profile":{"roles":[]}
            })),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Empty roles override"))
    );
    let (status, body) = server
        .exchange(
            Method::PUT,
            &format!("/sources/{id}/naming"),
            &[("Cookie", &cookie)],
            Some(&json!({"use_defaults":false,"override":{
                "roles":[],
                "quantity":{"regex":"[ _]x([0-9]+)\\.stl$","default":1},
                "slug":{"strip_markers":true,"strip_quantity":true},
                "folder_rules":[],
                "export_role_order":["primary","accent","clear","opaque"]
            }})),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("Naming requires primary role"))
    );
    let (status, missing) = server
        .exchange(
            Method::PUT,
            "/sources/999999/naming",
            &[("Cookie", &cookie)],
            Some(&json!({"use_defaults":true})),
        )
        .await;
    assert_eq!(
        (status, missing["detail"].as_str()),
        (404, Some("Source not found"))
    );
    assert_eq!(database_bytes(&directory), before_bytes);
    assert_eq!(files(&directory), snapshot);
    let (status, after) = server
        .send(
            Method::GET,
            &format!("/sources/{id}/naming"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, after), (200, saved));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_import_rules_round_trip() {
    use pp_storage::catalog::{Outcome, Request};
    let (directory, owner, mut server, cookie) = serve(false).await;
    let created = post_source(&server, &cookie, "Rules").await;
    let id = created["id"].as_i64().unwrap();
    let (status, empty) = server
        .send(
            Method::GET,
            &format!("/sources/{id}/import-rules"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200, "{empty}");
    assert_eq!(empty, json!({"rules":[],"legacy_import_all":true}));
    let body = json!({"rules":[" /STLs ","\\STLs","a.STL",12,""]});
    let (status, saved) = server
        .exchange(
            Method::PUT,
            &format!("/sources/{id}/import-rules"),
            &[("Cookie", &cookie)],
            Some(&body),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["rules"], json!(["STLs/", "a.STL", "12/"]));
    let (status, got) = server
        .send(
            Method::GET,
            &format!("/api/v1/sources/{id}/import-rules"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200, "{got}");
    assert_eq!(
        got,
        json!({"rules":["STLs/","a.STL","12/"],"legacy_import_all":false})
    );
    let storage = {
        let guard = owner.lock().unwrap();
        match guard
            .local_source_catalog()
            .execute(Request::GetImportRules { id })
            .unwrap()
        {
            Outcome::Data(value) => value,
            _ => panic!("import rules"),
        }
    };
    assert_eq!(got, storage);
    let before_bytes = database_bytes(&directory);
    let snapshot = files(&directory);
    let (status, missing) = server
        .exchange(
            Method::PUT,
            "/sources/999999/import-rules",
            &[("Cookie", &cookie)],
            Some(&json!({"rules":["nope/"]})),
        )
        .await;
    assert_eq!(
        (status, missing["detail"].as_str()),
        (404, Some("Source not found"))
    );
    let (status, refused) = server
        .exchange(
            Method::PUT,
            &format!("/api/v1/sources/{id}/import-rules"),
            &[("Cookie", &cookie)],
            Some(&json!({"rules":["parts/"],"local_path":"/tmp/outside"})),
        )
        .await;
    assert_eq!(
        (status, refused["detail"].as_str()),
        (400, Some("Raw local_path requires native Source selection"))
    );
    assert_eq!(database_bytes(&directory), before_bytes);
    assert_eq!(files(&directory), snapshot);
    let (status, after) = server
        .send(
            Method::GET,
            &format!("/sources/{id}/import-rules"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, after), (200, got));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_activity_limit_matches_node() {
    let (directory, owner, mut server, cookie) = serve(false).await;
    let created = post_source(&server, &cookie, "Activity").await;
    let source_id = created["id"].as_i64().unwrap();
    {
        let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        for (tenant, kind, payload) in [
            (
                "default",
                "source.updated",
                format!(r#"{{"source_id":{source_id},"source_name":"Activity"}}"#),
            ),
            (
                "default",
                "source.sync_failed",
                r#"{"source_name":"Broken","error":"boom"}"#.into(),
            ),
            ("default", "source.update_available", "{}".into()),
            ("default", "unrelated", "{}".into()),
            (
                "foreign",
                "source.updated",
                r#"{"source_id":99,"source_name":"Foreign"}"#.into(),
            ),
        ] {
            db.execute(
                "INSERT INTO app_events(tenant_id,at,kind,payload_json) VALUES(?1,'2026-10-23T00:00:00.000Z',?2,?3)",
                rusqlite::params![tenant, kind, payload],
            )
            .unwrap();
        }
        for i in 0..110 {
            db.execute(
                "INSERT INTO app_events(tenant_id,at,kind,payload_json) VALUES('default','2026-10-23T00:00:00.000Z','source.updated',?1)",
                rusqlite::params![format!(r#"{{"source_name":"Pad {i}"}}"#)],
            )
            .unwrap();
        }
    }
    for prefix in ["", "/api/v1"] {
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/sources/activity"),
                &[("Cookie", &cookie)],
            )
            .await;
        assert_eq!(status, 200, "{prefix} {body}");
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 20, "{prefix}");
        assert!(body.get("events").is_some());
        assert!(events.iter().all(|event| {
            event.get("id").is_some()
                && event.get("at").is_some()
                && event.get("kind").is_some()
                && event.get("source_name").is_some()
                && event.as_object().unwrap().contains_key("source_id")
                && event.as_object().unwrap().contains_key("detail")
        }));
        assert!(events.iter().all(|event| event["source_name"] != "Foreign"));
    }
    let (status, zero) = server
        .send(
            Method::GET,
            "/sources/activity?limit=0",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(zero["events"].as_array().unwrap().len(), 1);
    let (status, five) = server
        .send(
            Method::GET,
            "/api/v1/sources/activity?limit=5",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(five["events"].as_array().unwrap().len(), 5);
    let (status, oversize) = server
        .send(
            Method::GET,
            "/sources/activity?limit=150",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(oversize["events"].as_array().unwrap().len(), 100);
    let (status, bad) = server
        .send(
            Method::GET,
            "/sources/activity?limit=abc",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(bad["events"].as_array().unwrap().len(), 20);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_categories_round_trip() {
    let (directory, owner, mut server, cookie) = serve(false).await;
    let defaults = json!([
        "Printer kits",
        "Toolheads",
        "Probes & sensors",
        "Mods",
        "Hardware",
        "Other"
    ]);
    for prefix in ["", "/api/v1"] {
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/settings/source-categories"),
                &[("Cookie", &cookie)],
            )
            .await;
        assert_eq!(status, 200, "{prefix}");
        assert_eq!(body["categories"], defaults);
        assert_eq!(body["tree"].as_array().unwrap().len(), 6);
    }
    let (status, saved) = server
        .exchange(
            Method::PUT,
            "/settings/source-categories",
            &[("Cookie", &cookie)],
            Some(&json!({
                "categories": [" Parts / Mechanical ", "Mods"],
                "replacements": {}
            })),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        saved["categories"],
        json!(["Parts", "Parts/Mechanical", "Mods"])
    );
    assert_eq!(saved["tree"][0]["path"], "Parts");
    assert_eq!(saved["tree"][0]["children"][0]["path"], "Parts/Mechanical");
    assert_eq!(saved["tree"][0]["children"][0]["parent"], "Parts");
    assert_eq!(saved["tree"][1]["path"], "Mods");
    let (status, body) = server
        .send(
            Method::GET,
            "/api/v1/settings/source-categories",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, body), (200, saved.clone()));
    let before = stored_categories(&directory, "default");
    let snapshot = files(&directory);
    let (status, body) = server
        .exchange(
            Method::PUT,
            "/settings/source-categories",
            &[("Cookie", &cookie)],
            Some(&json!({"categories":[],"replacements":{}})),
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (400, Some("At least one category is required"))
    );
    assert_eq!(stored_categories(&directory, "default"), before);
    assert_eq!(files(&directory), snapshot);
    let (status, body) = server
        .send(
            Method::GET,
            "/settings/source-categories",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, body), (200, saved));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_categories_are_tenant_scoped() {
    let (directory, owner) = open();
    {
        let guard = owner.lock().unwrap();
        assert!(matches!(
            guard
                .local_source_catalog()
                .execute(Request::SaveCategoriesWithTree {
                    categories: vec!["Default Only".into()],
                    replacements: Default::default(),
                })
                .unwrap(),
            Outcome::Categories(_)
        ));
    }
    let default_before = stored_categories(&directory, "default");
    assert_eq!(default_before.as_deref(), Some(r#"["Default Only"]"#));
    let (bound, origin) = listener().await;
    let account = AuthPolicy {
        registration: pp_storage::auth::RegistrationPolicy::FirstAccountOnly,
        first_user: pp_storage::auth::FirstUserTenant::NewUser,
        session_tenant: pp_storage::auth::SessionTenantPolicy::AccountTenant,
    };
    let auth = owner.lock().unwrap().auth_with_policy(account).unwrap();
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        catalog::{CatalogHttpConfig, admit_catalog_routes},
    };
    let router = admit_catalog_routes(
        CatalogHttpConfig::from_startup(&origin, "default", account, Some("true")).unwrap(),
        Arc::clone(&owner),
    )
    .merge(auth_router(
        AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false).unwrap(),
        auth,
        ProviderClient::new(None, None).unwrap(),
        ResetMailer::disabled(),
    ));
    let mut server = Server::spawn(bound, router).await;
    let registered = server
        .client
        .post(format!("{}/auth/register", server.origin))
        .header("Origin", &server.origin)
        .json(&json!({"email":"other@example.test","password":"password-1234"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200);
    let cookie = registered.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let token = cookie.strip_prefix("pp_session=").unwrap();
    let tenant = {
        let guard = owner.lock().unwrap();
        let pp_storage::auth::Outcome::User(Some(user)) = guard
            .auth_with_policy(account)
            .unwrap()
            .submit(
                pp_storage::auth::Request::ResolveSession {
                    token: Secret::new(token.into()),
                },
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                std::time::Duration::from_secs(1),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        else {
            panic!("session");
        };
        assert_ne!(user.tenant_id, "default");
        user.tenant_id
    };
    let (status, body) = server
        .send(
            Method::GET,
            "/settings/source-categories",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        body["categories"],
        json!([
            "Printer kits",
            "Toolheads",
            "Probes & sensors",
            "Mods",
            "Hardware",
            "Other"
        ])
    );
    let (status, body) = server
        .exchange(
            Method::PUT,
            "/api/v1/settings/source-categories",
            &[("Cookie", &cookie)],
            Some(&json!({
                "categories": ["Other Tenant"],
                "replacements": {}
            })),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["categories"], json!(["Other Tenant"]));
    assert_eq!(stored_categories(&directory, "default"), default_before);
    assert_eq!(
        stored_categories(&directory, &tenant).as_deref(),
        Some(r#"["Other Tenant"]"#)
    );
    let mut rows = category_tenants(&directory);
    rows.sort();
    let mut expected = vec![
        ("default".into(), r#"["Default Only"]"#.into()),
        (tenant, r#"["Other Tenant"]"#.into()),
    ];
    expected.sort();
    assert_eq!(rows, expected);
    let (status, body) = server
        .send(
            Method::GET,
            "/settings/source-categories",
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["categories"], json!(["Other Tenant"]));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_delete_unreferenced_source_succeeds() {
    let (directory, owner, mut server, cookie) = serve(false).await;
    let keeper = post_source(&server, &cookie, "Keeper").await;
    let disposable = post_source(&server, &cookie, "Disposable").await;
    let id = disposable["id"].as_i64().unwrap();
    let keeper_id = keeper["id"].as_i64().unwrap();
    let path = std::path::PathBuf::from(disposable["local_path"].as_str().unwrap());
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("kept.stl"), b"retained").unwrap();
    let (status, body) = server
        .send(
            Method::DELETE,
            &format!("/api/v1/sources/{id}"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, body), (204, Value::Null));
    let (status, body) = server
        .send(
            Method::GET,
            &format!("/sources/{id}"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (404, Some("Source not found"))
    );
    let (status, body) = server
        .send(
            Method::GET,
            &format!("/sources/{keeper_id}"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!((status, body), (200, keeper));
    assert_eq!(std::fs::read(path.join("kept.stl")).unwrap(), b"retained");
    let rows = stored_rows(&directory);
    assert!(rows.iter().any(|row| row == &format!("{keeper_id}:Keeper")));
    assert!(rows.iter().all(|row| row != &format!("{id}:Disposable")));
    assert!(rows.iter().all(|row| !row.starts_with(&format!("{id}:"))));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_patch_stale_metadata_is_conflict() {
    if !matches!(
        std::env::var("PP_DESKTOP_REBUILD").ok().as_deref(),
        Some("1" | "true")
    ) {
        let output = child_process::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "catalog_patch_stale_metadata_is_conflict",
                "--test-threads=1",
            ])
            .env("PP_DESKTOP_REBUILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let fixture = node_fixture();
    let (directory, owner, mut server, cookie) = serve(false).await;
    let (status, created) = server
        .exchange(
            Method::POST,
            "/sources",
            &[("Cookie", &cookie)],
            Some(&fixture["create"]["body"]),
        )
        .await;
    assert_eq!(status, 200);
    let id = created["id"].as_i64().unwrap();
    let stale = match created["catalog_metadata_version"].as_i64() {
        Some(version) if version != 0 => 0,
        _ => 4,
    };
    let snapshot = files(&directory);
    let (status, body) = server
        .exchange(
            Method::PATCH,
            &format!("/sources/{id}"),
            &[("Cookie", &cookie)],
            Some(&json!({
                "metadata": {"note": "stale"},
                "metadata_version": stale
            })),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str(), body["detail"].as_str(),),
        (
            409,
            Some("catalog_metadata_conflict"),
            Some("Catalog metadata changed; retry from the current version")
        )
    );
    let (status, stored) = server
        .send(
            Method::GET,
            &format!("/sources/{id}"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(stored["metadata"]["note"], created["metadata"]["note"]);
    assert_eq!(files(&directory), snapshot);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn catalog_patch_non_integer_metadata_version_is_rejected() {
    if !matches!(
        std::env::var("PP_DESKTOP_REBUILD").ok().as_deref(),
        Some("1" | "true")
    ) {
        let output = child_process::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "catalog_patch_non_integer_metadata_version_is_rejected",
                "--test-threads=1",
            ])
            .env("PP_DESKTOP_REBUILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let fixture = node_fixture();
    let (directory, owner, mut server, cookie) = serve(false).await;
    let (status, created) = server
        .exchange(
            Method::POST,
            "/sources",
            &[("Cookie", &cookie)],
            Some(&fixture["create"]["body"]),
        )
        .await;
    assert_eq!(status, 200);
    let id = created["id"].as_i64().unwrap();
    let snapshot = files(&directory);
    for version in [json!("3"), json!(3.5), json!(true)] {
        let (status, body) = server
            .exchange(
                Method::PATCH,
                &format!("/sources/{id}"),
                &[("Cookie", &cookie)],
                Some(&json!({
                    "metadata": {"note": "should-not-land"},
                    "metadata_version": version
                })),
            )
            .await;
        assert_eq!(
            (status, body["detail"].as_str()),
            (400, Some("metadata_version must be an integer"))
        );
    }
    let (status, stored) = server
        .send(
            Method::GET,
            &format!("/sources/{id}"),
            &[("Cookie", &cookie)],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(stored["metadata"]["note"], created["metadata"]["note"]);
    assert_eq!(files(&directory), snapshot);
    server.stop().await;
    shutdown(owner, directory);
}
