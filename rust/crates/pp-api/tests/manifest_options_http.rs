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
    let directory = Scratch::new("pp-manifest-http-");
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
    serving: Option<pp_api::fixtures::Serving>,
}

impl Server {
    async fn spawn(listener: tokio::net::TcpListener, router: axum::Router) -> Self {
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let serving = pp_api::fixtures::Serving::start(listener, router);
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

const KIT_PATHS: &[&str] = &[
    "/plans/1/kit-manifest",
    "/api/v1/plans/1/kit-manifest",
    "/api/v2/plans/1/kit-manifest",
];

const BUILDER_PATHS: &[&str] = &[
    "/plans/1/plan-manifest-builder",
    "/api/v1/plans/1/plan-manifest-builder",
    "/api/v2/plans/1/plan-manifest-builder",
];

/// Without `desktop_rebuild`, kit-manifest HTTP is not linked into pp-api.
#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn kit_manifest_read_flag_off_is_404() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let before = profiles(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    for path in KIT_PATHS {
        let (status, body) = server.send(Method::GET, path, &[]).await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("kit").is_none());
        let (status, body) = server.send(Method::PUT, path, &[]).await;
        assert_eq!(status, 404, "PUT {path}");
        assert!(body.get("kit").is_none());
    }
    for path in BUILDER_PATHS {
        let (status, body) = server.send(Method::GET, path, &[]).await;
        assert_eq!(status, 404, "{path}");
        assert!(body.get("profile_id").is_none());
    }
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn kit_manifest_read_matches_storage() {
    let (directory, owner) = open();
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server.send(Method::GET, "/plans/1/kit-manifest", &[]).await;
    assert_eq!(status, 404);
    assert!(body.get("kit").is_none());
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn kit_manifest_save_rejects_changed_basis() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server.send(Method::PUT, "/plans/1/kit-manifest", &[]).await;
    assert_eq!(status, 404);
    assert!(body.get("kit").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
    shutdown(owner, directory);
}

#[cfg(not(feature = "desktop_rebuild"))]
#[tokio::test]
async fn kit_manifest_save_rejects_unknown_selection() {
    let (directory, owner) = open();
    let snapshot = files(&directory);
    let mut server = Server::spawn(bind().await, axum::Router::new()).await;
    let (status, body) = server.send(Method::PUT, "/plans/1/kit-manifest", &[]).await;
    assert_eq!(status, 404);
    assert!(body.get("kit").is_none());
    server.stop().await;
    assert_eq!(files(&directory), snapshot);
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
fn store_kit(directory: &Path, tenant: &str, build: i64, kit: &Value) {
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3)
         ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        rusqlite::params![tenant, format!("kit_manifest_{build}"), kit.to_string()],
    )
    .unwrap();
}

#[cfg(feature = "desktop_rebuild")]
fn read_kit_storage(owner: &Mutex<WriterOwner>, token: &str, build: i64) -> Value {
    use pp_storage::{
        auth::Secret,
        build_graph::{BuildCommand, BuildOutcome, ManifestOptionsCommand, PositiveId},
        read_model::Credential,
    };
    use std::{sync::atomic::AtomicBool, time::Duration};
    let client = owner
        .lock()
        .unwrap()
        .build_graph_with_policy(policy())
        .unwrap();
    match client
        .execute(
            Credential::Session(Secret::new(token.into())),
            BuildCommand::Manifest(ManifestOptionsCommand::ReadKit {
                build: PositiveId::new(build as u64).unwrap(),
            }),
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap()
    {
        BuildOutcome::Manifest(manifest) => {
            serde_json::from_slice(&manifest.into_success_body().unwrap().into_bytes()).unwrap()
        }
        other => panic!("{other:?}"),
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn kit_manifest_read_flag_off_is_404() {
    use pp_api::builds::{BuildHttpConfig, build_router};
    let (directory, owner) = open();
    let before = profiles(&directory);
    let snapshot = files(&directory);
    for flag in [None, Some(""), Some("0"), Some("false"), Some("TRUE")] {
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let router = build_router(
            BuildHttpConfig::from_startup(&origin, "default", flag).unwrap(),
            owner
                .lock()
                .unwrap()
                .build_graph_with_policy(policy())
                .unwrap(),
        );
        let mut server = Server::spawn(bound, router).await;
        for path in KIT_PATHS {
            let (status, body) = server.send(Method::GET, path, &[]).await;
            assert_eq!(status, 404, "{flag:?} {path}");
            assert!(body.get("kit").is_none());
            let (status, body) = server.send(Method::PUT, path, &[]).await;
            assert_eq!(status, 404, "PUT {flag:?} {path}");
            assert!(body.get("kit").is_none());
        }
        for path in BUILDER_PATHS {
            let (status, body) = server.send(Method::GET, path, &[]).await;
            assert_eq!(status, 404, "{flag:?} {path}");
            assert!(body.get("profile_id").is_none());
        }
        server.stop().await;
    }
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn kit_manifest_read_matches_storage() {
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
    let cookie_a = register(&bootstrap, "kit-a@example.test").await;
    let cookie_b = register(&bootstrap, "kit-b@example.test").await;
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

    let stored = json!({
        "name": "Saved",
        "layers": [],
        "base_source_id": null,
        "addon_source_ids": [],
        "selections": {"toolhead": "mini"},
        "include": ["part.stl"],
        "exclude": [],
        "replacements": {},
        "choice_tree": [],
        "category_links": []
    });
    store_kit(&directory, &tenant_a, id_a, &stored);
    let expected = read_kit_storage(&owner, &token_a, id_a);
    assert_eq!(expected["kit"], stored);

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
    let (status, body) = server
        .send(Method::GET, &format!("/plans/{id_a}/kit-manifest"), &[])
        .await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (401, Some("Authentication required"))
    );

    for prefix in ["", "/api/v1", "/api/v2"] {
        let path = format!("{prefix}/plans/{id_a}/kit-manifest");
        let (status, body) = server
            .send(Method::GET, &path, &[("Cookie", cookie_a.as_str())])
            .await;
        assert_eq!((status, &body), (200, &expected), "{prefix}");
        let (status, _) = server
            .send(Method::HEAD, &path, &[("Cookie", cookie_a.as_str())])
            .await;
        assert_eq!(status, 200, "HEAD {prefix}");
        let (status, body) = server
            .send(
                Method::GET,
                &format!("{prefix}/plans/{id_b}/kit-manifest"),
                &[("Cookie", cookie_a.as_str())],
            )
            .await;
        assert_eq!(
            (status, body["detail"].as_str()),
            (404, Some("Profile not found")),
            "{prefix}"
        );
    }

    let empty = read_kit_storage(&owner, &token_b, id_b);
    let (status, body) = server
        .send(
            Method::GET,
            &format!("/plans/{id_b}/kit-manifest"),
            &[("Cookie", cookie_b.as_str())],
        )
        .await;
    assert_eq!((status, body), (200, empty));
    assert_eq!(files(&directory), snapshot);
    assert_eq!(profiles(&directory), before);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
fn kit_setting(directory: &Path, tenant: &str, build: i64) -> Option<String> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.query_row(
        "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
        rusqlite::params![tenant, format!("kit_manifest_{build}")],
        |row| row.get(0),
    )
    .ok()
}

#[cfg(feature = "desktop_rebuild")]
fn config_modified(directory: &Path, tenant: &str, build: i64) -> Option<String> {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.query_row(
        "SELECT config_modified_at FROM build_profiles WHERE tenant_id=?1 AND id=?2",
        rusqlite::params![tenant, build],
        |row| row.get(0),
    )
    .ok()
}

#[cfg(feature = "desktop_rebuild")]
fn attach_checkout(directory: &Path, tenant: &str, build: i64) {
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.execute(
        "INSERT INTO projects(tenant_id,name,url,local_path,source_kind,role)
         VALUES(?1,'Checkout','https://example.test/checkout','/checkout','local','base')",
        rusqlite::params![tenant],
    )
    .unwrap();
    let source_id: i64 = db.last_insert_rowid();
    db.execute(
        "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id)
         VALUES(?1,?2,0,'base',?3)",
        rusqlite::params![tenant, build, source_id],
    )
    .unwrap();
}

#[cfg(feature = "desktop_rebuild")]
struct BasisChangeReads {
    database: std::path::PathBuf,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "desktop_rebuild")]
impl pp_storage::working_drafts::observation::DraftReads for BasisChangeReads {
    fn read_checkout_manifest(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCheckoutReadRequest,
        _: pp_storage::working_drafts::observation::CheckoutManifestUse,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 1 {
            rusqlite::Connection::open(&self.database)
                .unwrap()
                .execute("UPDATE build_profiles SET config_modified_at='changed'", [])
                .unwrap();
        }
        budget.check()?;
        let bytes = b"option_groups:\n  toolhead:\n    variants:\n      - id: stock\n        parts: [part.stl]\nselections:\n  toolhead: stock\n"
            .to_vec();
        budget.document(bytes.len(), bytes.len())?;
        Ok(pp_storage::working_drafts::observation::DocumentRead::Bytes(bytes))
    }

    fn resolve_stl_root(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedStlReadRequest,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        Box<dyn pp_storage::working_drafts::observation::StlRootRead>,
    > {
        budget.check()?;
        Ok(Box::new(EmptyRoot))
    }

    fn read_catalog_document(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCatalogReadRequest,
        _: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        panic!("declared groups need no hints")
    }
}

#[cfg(feature = "desktop_rebuild")]
struct EmptyRoot;

#[cfg(feature = "desktop_rebuild")]
impl pp_storage::working_drafts::observation::StlRootRead for EmptyRoot {
    fn logical_resolved_path(&self) -> Option<&str> {
        Some("/checkout")
    }

    fn scan_stls(
        &mut self,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        Box<dyn pp_storage::working_drafts::observation::StlInventory>,
    > {
        budget.entry(1, 8)?;
        Ok(Box::new(EmptyInventory))
    }
}

#[cfg(feature = "desktop_rebuild")]
struct EmptyInventory;

#[cfg(feature = "desktop_rebuild")]
impl pp_storage::working_drafts::observation::StlInventory for EmptyInventory {
    fn entries(&self) -> &[pp_storage::working_drafts::observation::InventoryPath] {
        &[]
    }

    fn hash_tracked_winner(
        &mut self,
        _: usize,
        _: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::ArtifactRead,
    > {
        panic!("builder does not hash artifacts")
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn kit_manifest_save_rejects_unknown_selection() {
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
    let cookie = register(&bootstrap, "save-unknown@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let id = {
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(chosen)
            .unwrap();
        match client
            .execute(
                Credential::Session(Secret::new(token)),
                BuildCommand::Create {
                    name: BuildName::parse("Unknown Selection".into()).unwrap(),
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
    let tenant = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .expect("tenant");
    let before_kit = kit_setting(&directory, &tenant, id);
    let before_modified = config_modified(&directory, &tenant, id);
    let snapshot = files(&directory);

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
            BuildHttpConfig::from_startup(&origin, &tenant, Some("1")).unwrap(),
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

    let path = format!("/plans/{id}/kit-manifest");
    let (status, body) = server
        .send_body(
            Method::PUT,
            &path,
            &[("Cookie", cookie.as_str())],
            Some(&json!({"kit":{"selections":{"missing":"stock"}}})),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(
        body,
        json!({"detail":"kit.selections.missing is an unknown option group"})
    );
    assert_eq!(kit_setting(&directory, &tenant, id), before_kit);
    assert_eq!(config_modified(&directory, &tenant, id), before_modified);
    assert_eq!(files(&directory), snapshot);
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn kit_manifest_save_rejects_changed_basis() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        builds::{BuildHttpConfig, build_router},
    };
    use pp_storage::{
        auth::Secret,
        build_graph::{BuildCommand, BuildName, BuildOutcome},
        read_model::Credential,
        working_drafts::observation::PreparationLimits,
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
    let cookie = register(&bootstrap, "save-stale@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let id = {
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(chosen)
            .unwrap();
        match client
            .execute(
                Credential::Session(Secret::new(token)),
                BuildCommand::Create {
                    name: BuildName::parse("Changed Basis".into()).unwrap(),
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
    let tenant = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .expect("tenant");
    attach_checkout(&directory, &tenant, id);
    let before_kit = kit_setting(&directory, &tenant, id);
    let before_modified = config_modified(&directory, &tenant, id);

    let reads = Arc::new(BasisChangeReads {
        database: directory.join("print-partner.db"),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let (auth, graph) = {
        let locked = owner.lock().unwrap();
        (
            locked.auth_with_policy(chosen).unwrap(),
            locked
                .build_graph_with_observations(chosen, reads, PreparationLimits::default())
                .unwrap(),
        )
    };
    let mut server = Server::spawn(
        bound,
        build_router(
            BuildHttpConfig::from_startup(&origin, &tenant, Some("1")).unwrap(),
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

    let path = format!("/plans/{id}/kit-manifest");
    let (status, body) = server
        .send_body(
            Method::PUT,
            &path,
            &[("Cookie", cookie.as_str())],
            Some(&json!({"kit":{"name":"Refused"}})),
        )
        .await;
    assert_eq!(status, 409);
    assert_eq!(
        body,
        json!({
            "detail": "Build inputs changed during manifest observation",
            "code": "stale_observation",
        })
    );
    assert_eq!(kit_setting(&directory, &tenant, id), before_kit);
    // Adapter mutates config_modified_at during observation; SaveKit must still
    // refuse without writing the kit setting.
    assert_eq!(
        config_modified(&directory, &tenant, id).as_deref(),
        Some("changed")
    );
    assert_ne!(before_modified.as_deref(), Some("changed"));
    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
const BUILDER_MANIFEST: &[u8] = b"option_groups:\n  toolhead:\n    variants:\n      - id: stock\n        parts: [part.stl]\nselections:\n  toolhead: stock\n";

#[cfg(feature = "desktop_rebuild")]
const ZERO_VARIANT_MANIFEST: &[u8] = b"option_groups: {slow: {variants: []}}\n";

/// First observation pair sees a stamp change; ReadBuilder retries once and
/// succeeds when the second attempt's pair matches (storage Change::Once).
#[cfg(feature = "desktop_rebuild")]
struct OnceChangedReads {
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "desktop_rebuild")]
impl pp_storage::working_drafts::observation::DraftReads for OnceChangedReads {
    fn read_checkout_manifest(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCheckoutReadRequest,
        _: pp_storage::working_drafts::observation::CheckoutManifestUse,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        budget.check()?;
        let mut bytes = BUILDER_MANIFEST.to_vec();
        if call > 0 {
            bytes.extend_from_slice(b"# changed\n");
        }
        budget.document(bytes.len(), bytes.len())?;
        Ok(pp_storage::working_drafts::observation::DocumentRead::Bytes(bytes))
    }

    fn resolve_stl_root(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedStlReadRequest,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        Box<dyn pp_storage::working_drafts::observation::StlRootRead>,
    > {
        budget.check()?;
        Ok(Box::new(EmptyRoot))
    }

    fn read_catalog_document(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCatalogReadRequest,
        _: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        panic!("declared groups need no hints")
    }
}

/// Materializes a zero-variant checkout manifest, then refuses with Cancelled so
/// the HTTP boundary maps Failure::Cancelled (GRE-751 end-check lives in storage).
#[cfg(feature = "desktop_rebuild")]
struct ZeroVariantCancelReads;

#[cfg(feature = "desktop_rebuild")]
impl pp_storage::working_drafts::observation::DraftReads for ZeroVariantCancelReads {
    fn read_checkout_manifest(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCheckoutReadRequest,
        _: pp_storage::working_drafts::observation::CheckoutManifestUse,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        budget.check()?;
        let bytes = ZERO_VARIANT_MANIFEST.to_vec();
        budget.document(bytes.len(), bytes.len())?;
        Err(pp_storage::working_drafts::observation::ReadFailure::Cancelled)
    }

    fn resolve_stl_root(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedStlReadRequest,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        Box<dyn pp_storage::working_drafts::observation::StlRootRead>,
    > {
        budget.check()?;
        Ok(Box::new(EmptyRoot))
    }

    fn read_catalog_document(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCatalogReadRequest,
        _: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        panic!("zero-variant cancel does not read catalogs")
    }
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn builder_read_retries_one_changed_observation() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        builds::{BuildHttpConfig, build_router},
    };
    use pp_storage::{
        auth::Secret,
        build_graph::{BuildCommand, BuildName, BuildOutcome},
        read_model::Credential,
        working_drafts::observation::PreparationLimits,
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
    let cookie = register(&bootstrap, "builder-retry@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let id = {
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(chosen)
            .unwrap();
        match client
            .execute(
                Credential::Session(Secret::new(token)),
                BuildCommand::Create {
                    name: BuildName::parse("Builder Retry".into()).unwrap(),
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
    let tenant = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .expect("tenant");
    attach_checkout(&directory, &tenant, id);
    let before_kit = kit_setting(&directory, &tenant, id);
    let before_modified = config_modified(&directory, &tenant, id);

    let reads = Arc::new(OnceChangedReads {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let (auth, graph) = {
        let locked = owner.lock().unwrap();
        (
            locked.auth_with_policy(chosen).unwrap(),
            locked
                .build_graph_with_observations(chosen, reads.clone(), PreparationLimits::default())
                .unwrap(),
        )
    };
    let mut server = Server::spawn(
        bound,
        build_router(
            BuildHttpConfig::from_startup(&origin, &tenant, Some("1")).unwrap(),
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

    let path = format!("/plans/{id}/plan-manifest-builder");
    let (status, body) = server
        .send(Method::GET, &path, &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["profile_id"], id);
    assert!(body.get("merged_option_groups").is_some(), "{body}");
    assert_eq!(reads.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(kit_setting(&directory, &tenant, id), before_kit);
    assert_eq!(config_modified(&directory, &tenant, id), before_modified);

    for prefix in ["", "/api/v1", "/api/v2"] {
        let (status, _) = server
            .send(
                Method::HEAD,
                &format!("{prefix}/plans/{id}/plan-manifest-builder"),
                &[("Cookie", cookie.as_str())],
            )
            .await;
        assert_eq!(status, 200, "HEAD {prefix}");
    }

    server.stop().await;
    shutdown(owner, directory);
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn builder_read_zero_variants_honors_cancellation() {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        builds::{BuildHttpConfig, build_router},
    };
    use pp_storage::{
        auth::Secret,
        build_graph::{BuildCommand, BuildName, BuildOutcome},
        read_model::Credential,
        working_drafts::observation::PreparationLimits,
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
    let cookie = register(&bootstrap, "builder-cancel@example.test").await;
    let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
    bootstrap.stop().await;

    let id = {
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(chosen)
            .unwrap();
        match client
            .execute(
                Credential::Session(Secret::new(token)),
                BuildCommand::Create {
                    name: BuildName::parse("Builder Cancel".into()).unwrap(),
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
    let tenant = profiles(&directory)
        .into_iter()
        .find(|row| row.1 == id)
        .map(|row| row.0)
        .expect("tenant");
    attach_checkout(&directory, &tenant, id);
    let before_kit = kit_setting(&directory, &tenant, id);
    let before_modified = config_modified(&directory, &tenant, id);
    let snapshot = files(&directory);

    let bound = bind().await;
    let origin = format!("http://{}", bound.local_addr().unwrap());
    let (auth, graph) = {
        let locked = owner.lock().unwrap();
        (
            locked.auth_with_policy(chosen).unwrap(),
            locked
                .build_graph_with_observations(
                    chosen,
                    Arc::new(ZeroVariantCancelReads),
                    PreparationLimits::default(),
                )
                .unwrap(),
        )
    };
    let mut server = Server::spawn(
        bound,
        build_router(
            BuildHttpConfig::from_startup(&origin, &tenant, Some("1")).unwrap(),
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

    let path = format!("/plans/{id}/plan-manifest-builder");
    let (status, body) = server
        .send(Method::GET, &path, &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(status, 503);
    assert_eq!(
        body,
        json!({
            "detail": "Build service temporarily unavailable",
            "code": "request_cancelled",
        })
    );
    assert_eq!(kit_setting(&directory, &tenant, id), before_kit);
    assert_eq!(config_modified(&directory, &tenant, id), before_modified);
    assert_eq!(files(&directory), snapshot);
    server.stop().await;
    shutdown(owner, directory);
}

/// How `ScriptedReads` answers each checkout-manifest read.
#[cfg(feature = "desktop_rebuild")]
#[derive(Clone, Copy)]
enum Script {
    /// Same bytes every call: observation pairs match.
    Stable,
    /// Different bytes every call: every pair differs, so the one retry is exhausted.
    AlwaysChanged,
    /// Source cannot be read.
    Unavailable,
}

#[cfg(feature = "desktop_rebuild")]
struct ScriptedReads {
    script: Script,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "desktop_rebuild")]
impl pp_storage::working_drafts::observation::DraftReads for ScriptedReads {
    fn read_checkout_manifest(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCheckoutReadRequest,
        _: pp_storage::working_drafts::observation::CheckoutManifestUse,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        budget.check()?;
        let mut bytes = BUILDER_MANIFEST.to_vec();
        match self.script {
            Script::Stable => {}
            Script::AlwaysChanged => bytes.extend_from_slice(format!("# {call}\n").as_bytes()),
            Script::Unavailable => {
                return Err(pp_storage::working_drafts::observation::ReadFailure::Unavailable);
            }
        }
        budget.document(bytes.len(), bytes.len())?;
        Ok(pp_storage::working_drafts::observation::DocumentRead::Bytes(bytes))
    }

    fn resolve_stl_root(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedStlReadRequest,
        budget: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        Box<dyn pp_storage::working_drafts::observation::StlRootRead>,
    > {
        budget.check()?;
        Ok(Box::new(EmptyRoot))
    }

    fn read_catalog_document(
        &self,
        _: &pp_storage::working_drafts::observation::OwnedCatalogReadRequest,
        _: &mut pp_storage::working_drafts::observation::PreparationBudget<'_>,
    ) -> pp_storage::working_drafts::observation::ReadResult<
        pp_storage::working_drafts::observation::DocumentRead,
    > {
        panic!("declared groups need no hints")
    }
}

/// One owner with a Build (with checkout) and a second account in another
/// tenant, served by the flag-on Build router merged with auth.
#[cfg(feature = "desktop_rebuild")]
struct BuilderFixture {
    server: Server,
    owner: Arc<Mutex<WriterOwner>>,
    reads: Arc<ScriptedReads>,
    cookie: String,
    stranger: String,
    id: i64,
    tenant: String,
    /// Last field: removed only after `owner` has dropped, even on panic.
    directory: Scratch,
}

#[cfg(feature = "desktop_rebuild")]
impl BuilderFixture {
    async fn start(script: Script) -> Self {
        use pp_api::{
            auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
            builds::{BuildHttpConfig, build_router},
        };
        use pp_storage::{
            auth::Secret,
            build_graph::{BuildCommand, BuildName, BuildOutcome},
            read_model::Credential,
            working_drafts::observation::PreparationLimits,
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
        let cookie = register(&bootstrap, "builder-owner@example.test").await;
        let stranger = register(&bootstrap, "builder-stranger@example.test").await;
        let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
        bootstrap.stop().await;

        let id = {
            let client = owner
                .lock()
                .unwrap()
                .build_graph_with_policy(chosen)
                .unwrap();
            match client
                .execute(
                    Credential::Session(Secret::new(token)),
                    BuildCommand::Create {
                        name: BuildName::parse("Builder Mapping".into()).unwrap(),
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
        let tenant = profiles(&directory)
            .into_iter()
            .find(|row| row.1 == id)
            .map(|row| row.0)
            .expect("tenant");
        attach_checkout(&directory, &tenant, id);

        let reads = Arc::new(ScriptedReads {
            script,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let (auth, graph) = {
            let locked = owner.lock().unwrap();
            (
                locked.auth_with_policy(chosen).unwrap(),
                locked
                    .build_graph_with_observations(
                        chosen,
                        reads.clone(),
                        PreparationLimits::default(),
                    )
                    .unwrap(),
            )
        };
        let server = Server::spawn(
            bound,
            build_router(
                BuildHttpConfig::from_startup(&origin, &tenant, Some("1")).unwrap(),
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
        Self {
            server,
            owner,
            directory,
            reads,
            cookie,
            stranger,
            id,
            tenant,
        }
    }

    fn calls(&self) -> usize {
        self.reads.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn finish(mut self) {
        self.server.stop().await;
        drop(self.server);
        shutdown(self.owner, self.directory);
    }
}

#[cfg(feature = "desktop_rebuild")]
const PREFIXES: &[&str] = &["", "/api/v1", "/api/v2"];

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn builder_read_exhausted_retry_is_409_stale_observation() {
    let fixture = BuilderFixture::start(Script::AlwaysChanged).await;
    let before_kit = kit_setting(&fixture.directory, &fixture.tenant, fixture.id);
    let before_modified = config_modified(&fixture.directory, &fixture.tenant, fixture.id);
    for prefix in PREFIXES {
        let path = format!("{prefix}/plans/{}/plan-manifest-builder", fixture.id);
        let calls = fixture.calls();
        let (status, body) = fixture
            .server
            .send(Method::GET, &path, &[("Cookie", fixture.cookie.as_str())])
            .await;
        assert_eq!(status, 409, "{path}");
        assert_eq!(
            body,
            json!({
                "detail": "Build inputs changed during manifest observation",
                "code": "stale_observation",
            }),
            "{path}"
        );
        // Two attempts of a double observation: exactly one retry.
        assert_eq!(fixture.calls() - calls, 4, "{path}");
        let (status, _) = fixture
            .server
            .send(Method::HEAD, &path, &[("Cookie", fixture.cookie.as_str())])
            .await;
        assert_eq!(status, 409, "HEAD {path}");
    }
    assert_eq!(
        kit_setting(&fixture.directory, &fixture.tenant, fixture.id),
        before_kit
    );
    assert_eq!(
        config_modified(&fixture.directory, &fixture.tenant, fixture.id),
        before_modified
    );
    fixture.finish().await;
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn builder_read_source_failure_is_503_source_observation_failed() {
    let fixture = BuilderFixture::start(Script::Unavailable).await;
    for prefix in PREFIXES {
        let path = format!("{prefix}/plans/{}/plan-manifest-builder", fixture.id);
        let (status, body) = fixture
            .server
            .send(Method::GET, &path, &[("Cookie", fixture.cookie.as_str())])
            .await;
        assert_eq!(status, 503, "{path}");
        assert_eq!(
            body,
            json!({
                "detail": "Manifest source observation failed",
                "code": "source_observation_failed",
                "reason": "Unavailable",
            }),
            "{path}"
        );
        let (status, _) = fixture
            .server
            .send(Method::HEAD, &path, &[("Cookie", fixture.cookie.as_str())])
            .await;
        assert_eq!(status, 503, "HEAD {path}");
    }
    fixture.finish().await;
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn builder_read_refuses_bad_ids_missing_builds_and_anonymous_callers() {
    let fixture = BuilderFixture::start(Script::Stable).await;
    let owner = [("Cookie", fixture.cookie.as_str())];
    let stranger = [("Cookie", fixture.stranger.as_str())];
    for prefix in PREFIXES {
        for method in [Method::GET, Method::HEAD] {
            // Node reads the id with Number(), so none of these names a Plan.
            for bad in ["abc", "0", "-1", "1.5"] {
                let path = format!("{prefix}/plans/{bad}/plan-manifest-builder");
                let (status, body) = fixture.server.send(method.clone(), &path, &owner).await;
                assert_eq!(status, 404, "{method} {path}");
                if method == Method::GET {
                    assert_eq!(body, json!({"detail": "Profile not found"}), "{path}");
                }
                let (status, _) = fixture.server.send(method.clone(), &path, &[]).await;
                assert_eq!(status, 401, "anonymous {method} {path}");
            }

            let missing = format!("{prefix}/plans/{}/plan-manifest-builder", fixture.id + 1000);
            let mine = format!("{prefix}/plans/{}/plan-manifest-builder", fixture.id);
            for (path, headers) in [(&missing, &owner), (&mine, &stranger)] {
                let (status, body) = fixture.server.send(method.clone(), path, headers).await;
                assert_eq!(status, 404, "{method} {path}");
                if method == Method::GET {
                    assert_eq!(body, json!({"detail": "Profile not found"}), "{path}");
                }
            }

            let (status, body) = fixture.server.send(method.clone(), &mine, &[]).await;
            assert_eq!(status, 401, "{method} {mine}");
            if method == Method::GET {
                assert_eq!(body, json!({"detail": "Authentication required"}), "{mine}");
            }

            let (status, body) = fixture.server.send(method.clone(), &mine, &owner).await;
            assert_eq!(status, 200, "{method} {mine}");
            if method == Method::GET {
                assert_eq!(body["profile_id"], fixture.id, "{mine}");
            }
        }
    }
    // Only the owner's successful reads observed the source; refusals never did.
    assert_eq!(fixture.calls(), PREFIXES.len() * 2 * 2);
    fixture.finish().await;
}

#[test]
fn scratch_root_is_empty_after_manifest_options_http_tests() {
    cleanup::assert_test_run_clean(&[
        "--skip",
        "scratch_root_is_empty_after_manifest_options_http_tests",
    ]);
}
