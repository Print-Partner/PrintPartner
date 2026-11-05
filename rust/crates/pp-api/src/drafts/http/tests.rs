use super::*;
use axum::{
    Extension,
    body::Body,
    extract::ConnectInfo,
    routing::{get, patch},
};
use reqwest::Client;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const COOKIE: &str = "pp_session=fixture-session";
/// Header value replaced by the server's own origin.
const SELF: &str = "<origin>";
const SESSION: &[(&str, &str)] = &[("Origin", SELF), ("Cookie", COOKIE)];

/// Echoes the admitted principal so tests observe what handlers get.
async fn probe(Extension(principal): Extension<PresentedCredential>) -> Json<Value> {
    Json(match principal {
        PresentedCredential::Session(secret) => json!({"session":secret}),
        PresentedCredential::ApiKey { tenant, secret } => json!({"tenant":tenant,"key":secret}),
    })
}

/// Extracts the whole body, so Axum's own extractor limit would apply here.
async fn measure(body: Bytes) -> Json<Value> {
    Json(json!({"len":body.len()}))
}

struct Server {
    origin: String,
    client: Client,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn start(flag: Option<&str>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = DraftHttpConfig::from_startup(&origin, "default", flag).unwrap();
        let routes = Router::new()
            .route("/plans/{id}/drafts", get(probe))
            .route("/plans/{id}/drafts/{draft_id}/parts", patch(probe))
            .route("/plans/{id}/drafts/{draft_id}/body", patch(measure));
        let service =
            admit_draft_routes(config, routes).into_make_service_with_connect_info::<SocketAddr>();
        let task = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
        let client = Client::builder().no_proxy().build().unwrap();
        Self {
            origin,
            client,
            task,
        }
    }
    fn host(&self) -> &str {
        self.origin.strip_prefix("http://").unwrap()
    }
    /// Sends `GET` to the list path or `PATCH` to the parts path under `prefix`.
    async fn send(&self, prefix: &str, edit: bool, headers: &[(&str, &str)]) -> (u16, Value) {
        let (method, path) = match edit {
            true => (Method::PATCH, "/plans/1/drafts/2/parts"),
            false => (Method::GET, "/plans/1/drafts"),
        };
        let url = format!("{}{prefix}{path}", self.origin);
        let mut request = self.client.request(method, url);
        for (name, value) in headers {
            request = request.header(*name, value.replace(SELF, &self.origin));
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        if status != 404 {
            assert_eq!(response.headers()["content-type"], media_type(status));
        }
        (
            status,
            crate::problem::without_envelope(response.json().await.unwrap_or(Value::Null)),
        )
    }
    async fn raw(&self, head: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(self.host()).await.unwrap();
        let host = self.host();
        let request =
            format!("{head}\r\nHost: {host}\r\nCookie: {COOKIE}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let read = stream.read_to_end(&mut response);
        tokio::time::timeout(Duration::from_secs(8), read)
            .await
            .unwrap()
            .unwrap();
        String::from_utf8(response).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn request(uri: &str, host: Option<&str>, peer: Option<[u8; 4]>) -> axum::extract::Request {
    let mut builder = axum::extract::Request::builder().uri(uri);
    if let Some(host) = host {
        builder = builder.header("host", host);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((peer, 9000))));
    }
    request
}

#[test]
fn transport_admission_requires_loopback_peer_and_matching_authority() {
    let config = DraftHttpConfig::new("https://127.0.0.1:1234", "default").unwrap();
    let (local, host, lo) = (
        "https://127.0.0.1:1234/plans/1/drafts",
        Some("127.0.0.1:1234"),
        Some([127, 0, 0, 1]),
    );
    for (uri, host, peer, status) in [
        ("/plans/1/drafts", host, None, Some(500)),
        ("/plans/1/drafts", host, Some([192, 0, 2, 1]), Some(403)),
        ("/plans/1/drafts", host, lo, None),
        ("/plans/1/drafts", None, lo, Some(403)),
        (local, None, lo, None),
        (local, host, lo, None),
        (local, Some("evil.test"), lo, Some(403)),
        ("https://evil.test/plans/1/drafts", host, lo, Some(403)),
    ] {
        let result = admit_request(&config, &request(uri, host, peer));
        assert_eq!(
            result.err().map(|error| error.0.as_u16()),
            status,
            "{uri} {host:?}"
        );
    }
    let mut duplicate = request("/plans/1/drafts", host, lo);
    duplicate
        .headers_mut()
        .append("host", "127.0.0.1:1234".parse().unwrap());
    assert_eq!(admit_request(&config, &duplicate).unwrap_err().0, 403);
}

#[test]
fn startup_config_accepts_only_loopback_origins_and_bounded_tenants() {
    for (origin, tenant, valid) in [
        ("http://example.test", "default", false),
        ("http://127.0.0.1:1234/path", "default", false),
        ("ftp://127.0.0.1", "default", false),
        ("http://[::1]:1234", "default", true),
        ("http://127.0.0.1", " ", false),
        ("http://127.0.0.1", &"t".repeat(512), true),
        ("http://127.0.0.1", &"t".repeat(513), false),
    ] {
        assert_eq!(
            DraftHttpConfig::new(origin, tenant).is_ok(),
            valid,
            "{origin}"
        );
    }
}

#[tokio::test]
async fn runtime_gate_is_exact_and_mounts_both_prefixes() {
    let flags = [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("TRUE"),
        Some(" true "),
        Some("yes"),
    ];
    for flag in flags.into_iter().chain([Some("1"), Some("true")]) {
        let s = Server::start(flag).await;
        let enabled = matches!(flag, Some("1" | "true"));
        for (prefix, edit) in [
            ("", false),
            ("", true),
            ("/api/v2", false),
            ("/api/v2", true),
        ] {
            let (status, _) = s.send(prefix, edit, SESSION).await;
            assert_eq!(status, if enabled { 200 } else { 404 }, "{flag:?}");
        }
        let (status, _) = s.send("", false, &[("Host", "wrong.test")]).await;
        assert_eq!(status, if enabled { 403 } else { 404 });
    }
}

/// `method | headers | status | detail`; headers are `name=value` joined by `,`.
const REFUSALS: &str = "
GET   | Origin=<origin>,Cookie=pp_session=s,forwarded=x         | 403 | Invalid request host
GET   | Origin=<origin>,Cookie=pp_session=s,x-forwarded-host=x  | 403 | Invalid request host
GET   | Origin=<origin>,Cookie=pp_session=s,x-forwarded-for=x   | 403 | Invalid request host
GET   | Origin=<origin>,Cookie=pp_session=s,x-forwarded-proto=x | 403 | Invalid request host
GET   | Origin=<origin>,Cookie=pp_session=s,Host=evil.test      | 403 | Invalid request host
GET   | Origin=https://evil.test,Cookie=pp_session=s            | 403 | Invalid request origin
GET   | Origin=<origin>,Cookie=pp_session=s,sec-fetch-site=cross-site | 403 | Invalid request origin
PATCH | Cookie=pp_session=s                                     | 403 | Invalid request origin
PATCH | Authorization=Bearer k,Origin=http://evil.test          | 403 | Invalid request origin
PATCH | Authorization=Bearer k,Origin=null                      | 403 | Invalid request origin
PATCH | Authorization=Bearer k,Cookie=pp_session=s              | 403 | Invalid request origin
GET   |                                                         | 401 | Authentication required
GET   | Authorization=Basic k                                   | 401 | Authentication required
GET   | Authorization=Bearer                                    | 401 | Authentication required
GET   | Origin=<origin>,Cookie=pp_session=s,Authorization=Bearer k | 401 | Authentication required
GET   | Authorization=Bearer a,Authorization=Bearer b           | 401 | Authentication required
GET   | Authorization=Bearer a,x-print-partner-api-key=b        | 401 | Authentication required
GET   | x-print-partner-api-key=a,x-print-partner-api-key=b     | 401 | Authentication required
GET   | Cookie=pp_session=a; pp_session=b                       | 401 | Authentication required
GET   | Cookie=pp_session=                                      | 401 | Authentication required
";

#[tokio::test]
async fn admission_rejects_ambiguous_credentials_hosts_and_origins() {
    let s = Server::start(Some("true")).await;
    for row in REFUSALS.trim().lines() {
        let [method, headers, status, detail] =
            row.split('|').map(str::trim).collect::<Vec<_>>()[..]
        else {
            panic!("{row}");
        };
        let headers: Vec<_> = headers
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        let reply = s.send("", method == "PATCH", &headers).await;
        assert_eq!(
            reply,
            (status.parse().unwrap(), json!({"detail":detail})),
            "{row}"
        );
    }
    let oversized = format!("Bearer {}", "k".repeat(4097));
    let reply = s.send("", false, &[("Authorization", &oversized)]).await;
    assert_eq!(reply.0, 401);
    let session = json!({"session":"fixture-session"});
    assert_eq!(s.send("/api/v2", false, SESSION).await, (200, session));
    let routed = (200, json!({"tenant":"default","key":"secret"}));
    let keyed = [
        ("Authorization", "Bearer  secret "),
        ("x-tenant-id", "foreign"),
    ];
    assert_eq!(s.send("", true, &keyed).await, routed);
    let custom = [("x-print-partner-api-key", "secret")];
    assert_eq!(s.send("", false, &custom).await, routed);
    let twice = format!("GET /plans/1/drafts HTTP/1.1\r\nHost: {}", s.host());
    let raw = s.raw(&twice).await;
    assert!(
        raw.starts_with("HTTP/1.1 400") || raw.starts_with("HTTP/1.1 403"),
        "{raw}"
    );
    let twice = format!(
        "GET /plans/1/drafts HTTP/1.1\r\nOrigin: {0}\r\nOrigin: {0}",
        s.origin
    );
    let raw = s.raw(&twice).await;
    assert!(
        raw.starts_with("HTTP/1.1 403") && raw.contains("Invalid request origin"),
        "{raw}"
    );
}

impl Server {
    async fn body(&self, method: Method, body: reqwest::Body) -> (u16, Value) {
        let path = match method {
            Method::PATCH => "/plans/1/drafts/2/body",
            _ => "/plans/1/drafts",
        };
        let response = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", &self.origin)
            .header("Cookie", COOKIE)
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (
            status,
            crate::problem::without_envelope(response.json().await.unwrap_or(Value::Null)),
        )
    }
    /// Opens a PATCH that promises a body and never sends it.
    async fn stall(&self) -> tokio::net::TcpStream {
        let mut stream = tokio::net::TcpStream::connect(self.host()).await.unwrap();
        let head = format!(
            "PATCH /plans/1/drafts/2/parts HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\n\
             Cookie: {COOKIE}\r\nContent-Length: 10\r\n\r\n",
            self.host(),
            self.origin
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream
    }
    async fn busy(&self) -> bool {
        let (status, body) = self.send("", false, SESSION).await;
        match status {
            503 => {
                assert_eq!(
                    body,
                    json!({"detail":"Draft service temporarily unavailable"})
                );
                true
            }
            200 => false,
            other => panic!("unexpected {other}"),
        }
    }
    /// Waits for in-flight stalls to release their permits.
    async fn recovers(&self) {
        tokio::time::timeout(Duration::from_secs(8), async {
            while self.busy().await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn bodies_are_bounded_by_method_and_patch_limit() {
    let s = Server::start(Some("1")).await;
    let large = vec![b'x'; 3 * 1024 * 1024];
    assert_eq!(
        s.body(Method::PATCH, large.into()).await,
        (200, json!({"len":3 * 1024 * 1024}))
    );
    let too_large = (413, json!({"detail":"Request body too large"}));
    let read = vec![b'x'; 1024 * 1024 + 1];
    assert_eq!(s.body(Method::GET, read.into()).await, too_large);
    let edit = vec![b'x'; 8 * 1024 * 1024 + 1];
    assert_eq!(s.body(Method::PATCH, edit.into()).await, too_large);
    let mut stream = tokio::net::TcpStream::connect(s.host()).await.unwrap();
    let head = format!(
        "PATCH /plans/1/drafts/2/body HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\n\
         Cookie: {COOKIE}\r\nTransfer-Encoding: chunked\r\n\r\n",
        s.host(),
        s.origin
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let chunk = [b"100000\r\n".as_slice(), &[b'x'; 1 << 20], b"\r\n"].concat();
    for _ in 0..9 {
        // The server may answer and stop reading before every chunk is sent.
        if stream.write_all(&chunk).await.is_err() {
            break;
        }
    }
    let mut response = vec![0; 1024];
    let read = stream.read(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response[..read]);
    assert!(
        response.starts_with("HTTP/1.1 413") && response.contains("Request body too large"),
        "{response}"
    );
    let target = format!("{}/plans/1/drafts?q={}", s.origin, "x".repeat(16 * 1024));
    let response = s.client.get(target).header("Cookie", COOKIE).send().await;
    let response = response.unwrap();
    assert_eq!(response.status(), 414);
    assert_eq!(
        crate::problem::without_envelope(response.json::<Value>().await.unwrap()),
        json!({"detail":"Request target too long"})
    );
}

#[tokio::test]
async fn stalled_bodies_exhaust_admission_then_release_it() {
    let s = Server::start(Some("1")).await;
    let mut stalls = Vec::new();
    for _ in 0..64 {
        stalls.push(s.stall().await);
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while !s.busy().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for mut stream in stalls {
        let mut response = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut response));
        let _ = read.await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 408") && response.contains("Request body timed out"),
            "{response}"
        );
    }
    s.recovers().await;

    let mut stalls = Vec::new();
    for _ in 0..64 {
        stalls.push(s.stall().await);
    }
    while !s.busy().await {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(stalls);
    s.recovers().await;
}

#[tokio::test]
async fn response_body_holds_admission_until_consumed_or_dropped() {
    let admission = Arc::new(Semaphore::new(1));
    for consume in [true, false] {
        let body = Body::new(AdmittedBody {
            body: Body::from("workspace response"),
            _permit: admission.clone().try_acquire_owned().unwrap(),
        });
        assert_eq!(admission.available_permits(), 0);
        if consume {
            assert_eq!(to_bytes(body, 1024).await.unwrap(), "workspace response");
        } else {
            drop(body);
        }
        assert_eq!(admission.available_permits(), 1);
    }
}

const DIGEST: &str = "ae11e670337e3e5eab736b29b4e294edbea8b564661cf15972625783f8930ee3";
const FIXTURE_COOKIE: &str = "pp_session=required-unit-fixture-secret";

fn policy() -> pp_storage::auth::AuthPolicy {
    pp_storage::auth::AuthPolicy {
        registration: pp_storage::auth::RegistrationPolicy::Open,
        session_tenant: pp_storage::auth::SessionTenantPolicy::AccountTenant,
        first_user: pp_storage::auth::FirstUserTenant::NewUser,
    }
}

struct ReadServer {
    origin: String,
    client: Client,
    owner: pp_storage::WriterOwner,
    task: tokio::task::JoinHandle<()>,
    /// Last field: removed only after `owner` has dropped, even on panic.
    directory: crate::scratch::Scratch,
}
impl ReadServer {
    async fn start(flag: Option<&str>, tenant: &str) -> Self {
        Self::open(flag, tenant, None, "", 8 * 1024 * 1024).await
    }
    async fn start_held(hold: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::open(Some("true"), "default", Some(hold), "", 8 * 1024 * 1024).await
    }
    async fn start_limited(body_limit: usize, setup: &str) -> Self {
        Self::open(Some("true"), "default", None, setup, body_limit).await
    }
    async fn open(
        flag: Option<&str>,
        tenant: &str,
        hold: Option<Arc<dyn Fn() + Send + Sync>>,
        setup: &str,
        body_limit: usize,
    ) -> Self {
        let directory = crate::scratch::Scratch::new("pp-drafts-http-");
        std::fs::write(
            directory.join("print-partner.db"),
            include_bytes!("../../../../pp-storage/tests/fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        if !setup.is_empty() {
            rusqlite::Connection::open(directory.join("print-partner.db"))
                .unwrap()
                .execute_batch(setup)
                .unwrap();
        }
        Self::serve(directory, flag, tenant, hold, body_limit).await
    }
    async fn serve(
        directory: crate::scratch::Scratch,
        flag: Option<&str>,
        tenant: &str,
        hold: Option<Arc<dyn Fn() + Send + Sync>>,
        body_limit: usize,
    ) -> Self {
        let owner = pp_storage::WriterOwner::open(&directory, pp_storage::Limits::default())
            .unwrap()
            .0;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = DraftHttpConfig::from_startup(&origin, tenant, flag)
            .unwrap()
            .with_limits(body_limit, Duration::from_millis(100))
            .unwrap();
        let reads = Arc::new(owner.source_reads().unwrap());
        let client = owner
            .working_drafts_with_observations(policy(), reads, Default::default())
            .unwrap();
        let auth = owner
            .rebuild_auth_with_policy(policy(), Default::default())
            .unwrap();
        let router = match hold {
            Some(hold) => draft_router_with_hold(config, client, auth, hold),
            None => draft_router(config, client, auth),
        };
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            owner,
            task,
            directory,
        }
    }
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", &self.origin)
            .header("Cookie", FIXTURE_COOKIE)
    }
    fn host(&self) -> &str {
        self.origin.strip_prefix("http://").unwrap()
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.directory.join("print-partner.db")).unwrap()
    }
    fn included(&self, part_id: i64) -> i64 {
        self.sql()
            .query_row(
                "SELECT included FROM plan_draft_parts WHERE id=?1",
                [part_id],
                |row| row.get(0),
            )
            .unwrap()
    }
    async fn close(self) {
        self.task.abort();
        let _ = self.task.await;
        self.owner.shutdown().unwrap();
    }
    /// Shuts the writer down and serves the same database again.
    async fn restart(self) -> Self {
        let Self {
            task,
            owner,
            directory,
            ..
        } = self;
        task.abort();
        let _ = task.await;
        owner.shutdown().unwrap();
        Self::serve(directory, Some("true"), "default", None, 8 * 1024 * 1024).await
    }
}

/// An invalid-body problem without its field errors; an unknown field is named.
fn invalid(mut problem: Value, body: &str) -> Value {
    let errors = problem.as_object_mut().unwrap().remove("errors");
    if body.contains("\"extra\"") && body.contains("expected_lifecycle_version") {
        let named = json!([{"pointer":"/extra","detail":"is not allowed"}]);
        assert_eq!(errors, Some(named), "{body}");
    }
    problem
}

/// Errors are problem documents; successes are plain JSON.
fn media_type(status: u16) -> &'static str {
    if status >= 400 {
        crate::problem::PROBLEM_CONTENT_TYPE
    } else {
        "application/json"
    }
}

async fn json_reply(request: reqwest::RequestBuilder, status: u16) -> Value {
    let response = request.send().await.unwrap();
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(response.headers()["content-type"], media_type(status));
    crate::problem::without_envelope(response.json().await.unwrap())
}

fn open_draft() -> Value {
    json!({"draft_id":2,"state":"open","lifecycle_version":0,"snapshot_digest":DIGEST,"base":{"revision_id":1,"plan_version":1}})
}

fn edit(digest: &str) -> Value {
    json!({"expected_snapshot_digest":digest,"decisions":[
        {"kind":"set_included","draft_part_ids":[5],"value":true},
        {"kind":"set_quantity_override","draft_part_ids":[4],"value":null}
    ]})
}

fn abandon_body(lifecycle: u64) -> Value {
    json!({"expected_lifecycle_version": lifecycle})
}

#[tokio::test]
async fn listing_and_workspace_have_literal_wire_contracts() {
    let server = ReadServer::start(Some("true"), "default").await;
    let expected = json!({"profile_id":1,"drafts":[
        {"draft_id":1,"state":"consumed","lifecycle_version":1,
         "snapshot_digest":"3912383330ef528d51a15c351d69eaffd856d7e8dc582ddfc66bf0bf5053b2b0",
         "base":{"revision_id":null,"plan_version":0}},
        open_draft()
    ]});
    let workspace = json!({
        "profile_id":1,"draft":open_draft(),"parts":[
            {"draft_part_id":4,"base_revision_part_id":1,"part_key":"bracket.stl","filename":"bracket.stl","relative_path":"bracket.stl","source_layer":"base:Fixture Source","role":"primary","quantity_inferred":1,"quantity_override":3,"quantity_effective":3,"included":true},
            {"draft_part_id":5,"base_revision_part_id":2,"part_key":"excluded.stl","filename":"excluded.stl","relative_path":"excluded.stl","source_layer":"base:Fixture Source","role":"primary","quantity_inferred":1,"quantity_override":1,"quantity_effective":1,"included":false},
            {"draft_part_id":6,"base_revision_part_id":3,"part_key":"gear.stl","filename":"gear.stl","relative_path":"gear.stl","source_layer":"base:Fixture Source","role":"primary","quantity_inferred":1,"quantity_override":2,"quantity_effective":2,"included":true}
        ],"diff":{"base_is_current":true,"added":[],"removed":[],"changed":[{
            "before":{"revision_part_id":2,"filename":"excluded.stl","relative_path":"excluded.stl","source_layer":"base:Fixture Source"},
            "after":{"draft_part_id":5,"base_revision_part_id":2,"part_key":"excluded.stl","filename":"excluded.stl","relative_path":"excluded.stl","source_layer":"base:Fixture Source","role":"primary","quantity_inferred":1,"quantity_override":1,"quantity_effective":1,"included":false},"fields":["status"]
        }]},"reconciliation":{"kind":"unresolved","conflicts":[]}
    });
    for prefix in ["", "/api/v2"] {
        assert_eq!(
            json_reply(
                server.request(Method::GET, &format!("{prefix}/plans/1/drafts")),
                200
            )
            .await,
            expected
        );
        assert_eq!(
            json_reply(
                server.request(Method::GET, &format!("{prefix}/plans/1/drafts/2")),
                200
            )
            .await,
            workspace
        );
    }
    for path in [
        "/plans/1/drafts/2/reconciliation",
        "/plans/1/save",
        "/api/v2/plans/1/save",
        "/plans/1/review",
        "/api/v1/plans/1/review",
    ] {
        let status = server
            .request(Method::POST, path)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert!(matches!(status, 404 | 405), "{path} {status}");
    }
    server.close().await;
}

#[tokio::test]
async fn missing_resources_cross_tenant_reads_and_invalid_ids_are_hidden() {
    let server = ReadServer::start(Some("true"), "default").await;
    let missing = json!({"detail":"Plan not found","code":"profile_not_found"});
    for profile in [2, 999] {
        for (method, suffix, body) in [
            (Method::GET, "", json!({})),
            (Method::GET, "/2", json!({})),
            (Method::PATCH, "/2/parts", edit(DIGEST)),
            (Method::POST, "/2/abandon", abandon_body(0)),
        ] {
            assert_eq!(
                json_reply(
                    server
                        .request(method, &format!("/plans/{profile}/drafts{suffix}"))
                        .json(&body),
                    404
                )
                .await,
                missing
            );
        }
    }
    for (method, suffix, body) in [
        (Method::GET, "", json!({})),
        (Method::PATCH, "/parts", edit(DIGEST)),
        (Method::POST, "/abandon", abandon_body(0)),
    ] {
        assert_eq!(
            json_reply(
                server
                    .request(method, &format!("/plans/1/drafts/999{suffix}"))
                    .json(&body),
                404
            )
            .await,
            json!({"detail":"Plan draft not found","code":"draft_not_found"})
        );
    }
    for value in [
        "0",
        "-1",
        "9007199254740992",
        "18446744073709551616",
        "text",
    ] {
        for path in [
            format!("/plans/{value}/drafts"),
            format!("/plans/1/drafts/{value}"),
        ] {
            assert_eq!(
                json_reply(server.request(Method::GET, &path), 400).await,
                json!({"detail":"Request is invalid","code":"invalid_request"})
            );
        }
    }
    assert_eq!(
        json_reply(
            server
                .client
                .get(format!("{}/plans/1/drafts", server.origin))
                .header("Cookie", "pp_session=not-a-session"),
            401
        )
        .await,
        json!({"detail":"Authentication required"})
    );
    server.close().await;
}

#[tokio::test]
async fn api_keys_record_use_and_reject_foreign_or_revoked() {
    let server = ReadServer::start(Some("true"), "default").await;
    let (key_id, secret) = create_key(&server).await;
    let last_used = || {
        server
            .sql()
            .query_row(
                "SELECT json_extract(value,'$[0].lastUsedAt') FROM app_settings WHERE tenant_id='default' AND key='api_keys_v1'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
    };
    let clear_last_used = || {
        server
            .sql()
            .execute(
                "UPDATE app_settings SET value=json_set(value,'$[0].lastUsedAt',NULL) WHERE tenant_id='default' AND key='api_keys_v1'",
                [],
            )
            .unwrap();
    };
    assert_eq!(last_used(), None);
    let bearer = server
        .client
        .get(format!("{}/plans/1/drafts", server.origin))
        .header("Authorization", format!("Bearer {secret}"))
        .header("x-tenant-id", "foreign");
    assert_eq!(json_reply(bearer, 200).await["drafts"][1], open_draft());
    assert!(last_used().is_some());
    clear_last_used();
    assert_eq!(last_used(), None);
    let custom = server
        .client
        .get(format!("{}/api/v2/plans/1/drafts/2", server.origin))
        .header("x-print-partner-api-key", &secret);
    assert_eq!(json_reply(custom, 200).await["draft"], open_draft());
    assert!(last_used().is_some());
    let auth = server
        .owner
        .rebuild_auth_with_policy(policy(), Default::default())
        .unwrap();
    let revoked = tokio::task::spawn_blocking(move || {
        auth.submit(
            pp_storage::auth::Request::RevokeKey {
                session: pp_storage::auth::Secret::new("required-unit-fixture-secret".into()),
                key_id,
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    })
    .await
    .unwrap();
    assert!(matches!(
        revoked,
        pp_storage::auth::Outcome::KeyChanged { changed: true, .. }
    ));
    for secret in [secret.as_str(), "invalid"] {
        assert_eq!(
            json_reply(
                server
                    .client
                    .get(format!("{}/plans/1/drafts", server.origin))
                    .header("Authorization", format!("Bearer {secret}"),),
                401
            )
            .await,
            json!({"detail":"Authentication required"})
        );
    }
    server.close().await;
    let foreign = ReadServer::start(Some("true"), "foreign").await;
    let (_, foreign_key) = create_key(&foreign).await;
    assert_eq!(
        json_reply(
            foreign
                .client
                .get(format!("{}/plans/1/drafts", foreign.origin))
                .header("Authorization", format!("Bearer {foreign_key}")),
            401
        )
        .await,
        json!({"detail":"Authentication required"})
    );
    foreign.close().await;
}

fn create_key(server: &ReadServer) -> impl std::future::Future<Output = (String, String)> + '_ {
    let auth = server
        .owner
        .rebuild_auth_with_policy(policy(), Default::default())
        .unwrap();
    async move {
        let outcome = tokio::task::spawn_blocking(move || {
            auth.submit(
                pp_storage::auth::Request::CreateKey {
                    session: pp_storage::auth::Secret::new("required-unit-fixture-secret".into()),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        })
        .await
        .unwrap();
        let pp_storage::auth::Outcome::KeyCreated { info, key } = outcome else {
            panic!("API key creation");
        };
        (info.id, key.expose().to_owned())
    }
}

#[tokio::test]
async fn corrupt_draft_reads_hide_the_stored_detail() {
    let server = ReadServer::start(Some("true"), "default").await;
    server
        .sql()
        .execute(
            "UPDATE plan_drafts SET snapshot_digest='private fixture detail' WHERE id=2",
            [],
        )
        .unwrap();
    for path in ["/plans/1/drafts", "/api/v2/plans/1/drafts/2"] {
        let body = json_reply(server.request(Method::GET, path), 500).await;
        assert_eq!(
            body,
            json!({"detail":"Plan draft data is inconsistent","code":"internal_error"})
        );
        assert!(!body.to_string().contains("private fixture detail"));
    }
    server.close().await;
}

#[tokio::test]
async fn disabled_gate_mounts_no_draft_reads() {
    let server = ReadServer::start(None, "default").await;
    let status = server
        .request(Method::GET, "/plans/1/drafts")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 404);
    assert_eq!(
        server
            .sql()
            .query_row("SELECT state FROM plan_drafts WHERE id=2", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "open"
    );
    server.close().await;
}

struct Gate {
    entered: std::sync::Mutex<usize>,
    release: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: std::sync::Mutex::new(0),
            release: std::sync::Mutex::new(false),
            cv: std::sync::Condvar::new(),
        })
    }
    fn wait(&self) {
        let mut entered = self.entered.lock().unwrap();
        *entered += 1;
        self.cv.notify_all();
        drop(entered);
        let mut release = self.release.lock().unwrap();
        while !*release {
            release = self.cv.wait(release).unwrap();
        }
    }
    fn entered(&self) -> usize {
        *self.entered.lock().unwrap()
    }
    fn release(&self) {
        *self.release.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

#[tokio::test]
async fn dropped_handler_keeps_read_capacity_until_the_worker_exits() {
    let gate = Gate::new();
    let hold = {
        let gate = Arc::clone(&gate);
        Arc::new(move || gate.wait())
    };
    let server = ReadServer::start_held(hold).await;
    let mut streams = Vec::new();
    for _ in 0..64 {
        let mut stream = tokio::net::TcpStream::connect(server.host()).await.unwrap();
        let request = format!(
            "GET /plans/1/drafts HTTP/1.1\r\nHost: {}\r\nCookie: {FIXTURE_COOKIE}\r\nConnection: keep-alive\r\n\r\n",
            server.host()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        streams.push(stream);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while gate.entered() < 64 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(streams);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = server
                .client
                .get(format!("{}/plans/1/drafts", server.origin))
                .send()
                .await
                .unwrap()
                .status();
            if status == 401 {
                break;
            }
            assert_eq!(status, 503);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let blocked = tokio::time::timeout(
        Duration::from_millis(800),
        server.request(Method::GET, "/plans/1/drafts").send(),
    )
    .await
    .expect("read capacity was released while workers were still inside");
    let blocked = blocked.unwrap();
    assert_eq!(blocked.status(), 503);
    assert_eq!(
        crate::problem::without_envelope(blocked.json::<Value>().await.unwrap()),
        json!({"detail":"Draft service temporarily unavailable"})
    );
    assert_eq!(gate.entered(), 64);
    gate.release();
    let recovered = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = server
                .request(Method::GET, "/plans/1/drafts")
                .send()
                .await
                .unwrap();
            if response.status().as_u16() == 200 {
                break crate::problem::without_envelope(response.json::<Value>().await.unwrap());
            }
            assert_eq!(response.status(), 503);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(recovered["drafts"][1], open_draft());
    server.close().await;
}

#[tokio::test]
async fn failure_codes_map_without_private_detail() {
    let cases: Vec<(&str, anyhow::Error, u16, Option<&str>)> = vec![
        (
            "cancelled",
            anyhow::Error::new(Failure::Cancelled),
            503,
            Some("request_cancelled"),
        ),
        (
            "queue",
            anyhow::Error::new(Failure::QueueFull),
            503,
            Some("writer_queue_full"),
        ),
        (
            "stopped",
            anyhow::Error::new(Failure::Stopped),
            503,
            Some("writer_stopped"),
        ),
        (
            "unknown",
            anyhow::Error::new(Failure::OutcomeUnknown),
            503,
            Some("outcome_unknown"),
        ),
        (
            "corrupt",
            anyhow::Error::new(Failure::CorruptOrigin),
            500,
            Some("internal_error"),
        ),
        (
            "session",
            anyhow::Error::new(pp_storage::auth::AuthFailure::SessionRequired),
            401,
            None,
        ),
        (
            "owner",
            anyhow::Error::new(pp_storage::auth::AuthFailure::OwnerMappingRequired),
            403,
            None,
        ),
        (
            "auth queue",
            anyhow::Error::new(pp_storage::auth::AuthFailure::QueueFull),
            503,
            Some("writer_queue_full"),
        ),
        (
            "auth stopped",
            anyhow::Error::new(pp_storage::auth::AuthFailure::Stopped),
            503,
            Some("writer_stopped"),
        ),
        (
            "auth unknown",
            anyhow::Error::new(pp_storage::auth::AuthFailure::CommitUnknown),
            503,
            Some("outcome_unknown"),
        ),
        (
            "admission",
            anyhow::Error::new(pp_storage::required_units::AdmissionFailure::Cancelled),
            503,
            Some("request_cancelled"),
        ),
        (
            "plain auth",
            anyhow::anyhow!("Authentication required"),
            401,
            None,
        ),
        (
            "locked",
            anyhow::anyhow!("database is locked"),
            503,
            Some("transaction_unavailable"),
        ),
        (
            "deadline",
            anyhow::anyhow!("Writer transaction admission deadline expired"),
            503,
            Some("transaction_unavailable"),
        ),
        (
            "wrapped locked",
            anyhow::anyhow!("database is locked").context("Plan draft update failed"),
            503,
            Some("transaction_unavailable"),
        ),
        (
            "wrapped deadline",
            anyhow::anyhow!("Writer transaction admission deadline expired")
                .context("Plan draft update failed"),
            503,
            Some("transaction_unavailable"),
        ),
        (
            "wrapped private",
            anyhow::anyhow!("private fixture detail").context("Plan draft update failed"),
            500,
            Some("internal_error"),
        ),
        (
            "writer full",
            anyhow::anyhow!("Writer queue full"),
            503,
            Some("writer_queue_full"),
        ),
        (
            "storage stopped",
            anyhow::anyhow!("Storage stopped"),
            503,
            Some("writer_stopped"),
        ),
        (
            "private",
            anyhow::anyhow!("private fixture detail"),
            500,
            Some("internal_error"),
        ),
    ];
    for (label, error, status, code) in cases {
        let response = storage_failure(&error);
        assert_eq!(response.status().as_u16(), status, "{label}");
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            !body.to_string().contains("private fixture detail"),
            "{label}"
        );
        match code {
            Some(code) => assert_eq!(body["code"], code, "{label}"),
            None => assert!(body.get("code").is_none(), "{label} {body}"),
        }
    }
}

#[test]
fn limits_can_only_be_lowered() {
    let config = DraftHttpConfig::new("http://127.0.0.1:1", "default").unwrap();
    let wait = Duration::from_millis(100);
    let lowered = config.clone().with_limits(1024, Duration::ZERO).unwrap();
    assert_eq!((lowered.body_limit, lowered.wait), (1024, Duration::ZERO));
    assert!(config.clone().with_limits(8 * 1024 * 1024, wait).is_ok());
    assert!(
        config
            .clone()
            .with_limits(8 * 1024 * 1024 + 1, wait)
            .is_err()
    );
    assert!(config.clone().with_limits(1024, wait * 2).is_err());
    assert!(config.with_limits(0, wait).is_err());
}

#[tokio::test]
async fn edits_persist_and_preserve_node_conflict_shapes() {
    let server = ReadServer::start(Some("true"), "default").await;
    let before = json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await;
    // Node 409 is `{code}` plus `workspace` only — no synthetic detail.
    let mut expected = json!({"code":"draft_changed","workspace":before});
    assert_eq!(
        json_reply(
            server
                .request(Method::PATCH, "/plans/1/drafts/2/parts")
                .json(&edit(&"0".repeat(64))),
            409
        )
        .await,
        expected
    );
    let changed = json_reply(
        server
            .request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts")
            .json(&edit(DIGEST)),
        200,
    )
    .await;
    assert_eq!(
        changed["draft"],
        json!({"draft_id":2,"state":"open","lifecycle_version":0,"snapshot_digest":"9a9bc08be4b343d523a2915b4f9c4288edfc35103d24886bf731859a9128f035","base":{"revision_id":1,"plan_version":1}})
    );
    assert_eq!(changed["parts"][0]["quantity_override"], Value::Null);
    assert_eq!(changed["parts"][0]["quantity_effective"], 1);
    assert_eq!(changed["parts"][1]["included"], true);
    assert_eq!(
        changed["reconciliation"],
        json!({"kind":"ready","reused_units":4,"new_units":0,"surplus_units":2})
    );
    assert_eq!(server.included(5), 1);
    assert_eq!(
        server
            .sql()
            .query_row(
                "SELECT quantity_override,quantity_effective FROM plan_draft_parts WHERE id=4",
                [],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (None, 1)
    );
    assert_eq!(
        json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await,
        changed
    );
    server
        .sql()
        .execute_batch("UPDATE plan_drafts SET state='abandoned', lifecycle_version=1 WHERE id=2")
        .unwrap();
    let current = json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await;
    expected["workspace"] = current;
    expected["code"] = json!("not_open");
    assert_eq!(
        json_reply(
            server
                .request(Method::PATCH, "/plans/1/drafts/2/parts")
                .json(&edit(DIGEST)),
            409
        )
        .await,
        expected
    );
    server.close().await;
}

#[tokio::test]
async fn api_key_patch_skips_origin_and_rejects_foreign_or_revoked() {
    let server = ReadServer::start(Some("true"), "default").await;
    let (key_id, secret) = create_key(&server).await;
    let patch = |secret: &str| {
        server
            .client
            .request(
                Method::PATCH,
                format!("{}/plans/1/drafts/2/parts", server.origin),
            )
            .header("Authorization", format!("Bearer {secret}"))
            .json(&edit(DIGEST))
    };
    assert_eq!(
        json_reply(patch(&secret), 200).await["parts"][1]["included"],
        true
    );
    for origin in ["http://evil.test", "null"] {
        assert_eq!(
            json_reply(patch(&secret).header("Origin", origin), 403).await,
            json!({"detail":"Invalid request origin"})
        );
    }
    assert_eq!(
        json_reply(patch(&secret).header("Cookie", FIXTURE_COOKIE), 403).await,
        json!({"detail":"Invalid request origin"})
    );
    let auth = server
        .owner
        .rebuild_auth_with_policy(policy(), Default::default())
        .unwrap();
    let revoked = tokio::task::spawn_blocking(move || {
        auth.submit(
            pp_storage::auth::Request::RevokeKey {
                session: pp_storage::auth::Secret::new("required-unit-fixture-secret".into()),
                key_id,
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    })
    .await
    .unwrap();
    assert!(matches!(
        revoked,
        pp_storage::auth::Outcome::KeyChanged { changed: true, .. }
    ));
    for secret in [secret.as_str(), "invalid"] {
        assert_eq!(
            json_reply(patch(secret), 401).await,
            json!({"detail":"Authentication required"})
        );
    }
    server.close().await;
}

/// The real edit wire is untagged, so serde reports member failures at the
/// root; the problem still names the failing field (ERR-03).
#[tokio::test]
async fn problem_response_edit_field_errors_name_the_member() {
    let server = ReadServer::start(Some("true"), "default").await;
    let one = |decision: Value| json!({"expected_snapshot_digest": DIGEST, "decision": decision});
    let included = |value: Value| json!({"kind":"set_included","draft_part_ids":[4],"value":value});
    let cases = [
        (one(included(json!("yes"))), "/decision", "is invalid"),
        (
            json!({"decision": included(json!(true))}),
            "/expected_snapshot_digest",
            "is required",
        ),
        (
            json!({"expected_snapshot_digest": DIGEST}),
            "/decision",
            "is required",
        ),
        (
            json!({"expected_snapshot_digest": DIGEST, "decisions": [included(json!(true)), {"kind":"rename"}]}),
            "/decisions/1/kind",
            "is invalid",
        ),
        (
            json!({"expected_snapshot_digest": DIGEST, "decision": included(json!(true)), "decisions": []}),
            "/decisions",
            "is not allowed",
        ),
    ];
    for (body, pointer, detail) in cases {
        let response = server
            .request(Method::PATCH, "/plans/1/drafts/2/parts")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{body}");
        let problem: Value = response.json().await.unwrap();
        assert_eq!(
            problem["type"], "urn:print-partner:problem:validation",
            "{body}"
        );
        assert_eq!(
            problem["errors"],
            json!([{"pointer": pointer, "detail": detail}]),
            "{body}"
        );
    }
    server.close().await;
}

#[tokio::test]
async fn edit_bounds_and_typed_bodies_reject_before_mutation() {
    let server = ReadServer::start_limited(256, "").await;
    for body in [
        "{",
        "null",
        "[]",
        "{}",
        "{\"expected_snapshot_digest\":-1}",
        "{\"expected_snapshot_digest\":null,\"extra\":true}",
    ] {
        assert_eq!(
            invalid(
                json_reply(
                    server
                        .request(Method::PATCH, "/plans/1/drafts/2/parts")
                        .body(body.to_owned()),
                    400
                )
                .await,
                body
            ),
            json!({"detail":"Request is invalid","code":"invalid_request"})
        );
    }
    // A batch rule names no field; a bad decision value names the decision.
    let invalid_decision = json!({"detail":"Request is invalid","code":"invalid_request",
        "errors":[{"pointer":"/decision","detail":"is invalid"}]});
    for (body, expected) in [
        (
            json!({"expected_snapshot_digest":DIGEST,"decisions":[]}),
            json!({"detail":"Request is invalid","code":"invalid_request"}),
        ),
        (
            json!({"expected_snapshot_digest":DIGEST,"decision":{"kind":"set_included","draft_part_ids":[0],"value":true}}),
            invalid_decision.clone(),
        ),
        (
            json!({"expected_snapshot_digest":DIGEST,"decision":{"kind":"set_quantity_override","draft_part_ids":[4],"value":10001}}),
            invalid_decision.clone(),
        ),
    ] {
        assert_eq!(
            json_reply(
                server
                    .request(Method::PATCH, "/plans/1/drafts/2/parts")
                    .json(&body),
                400
            )
            .await,
            expected
        );
    }
    assert_eq!(
        json_reply(
            server
                .request(Method::PATCH, "/plans/1/drafts/2/parts")
                .body("x".repeat(257)),
            413
        )
        .await,
        json!({"detail":"Request body too large"})
    );
    assert_eq!(
        server
            .sql()
            .query_row(
                "SELECT state,lifecycle_version FROM plan_drafts WHERE id=2",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        ("open".into(), 0)
    );
    assert_eq!(server.included(5), 0);
    server.close().await;

    let server = ReadServer::start_limited(8 * 1024 * 1024, "").await;
    let mut bulk = edit(DIGEST).to_string();
    bulk.extend(std::iter::repeat_n(' ', 3 * 1024 * 1024));
    let edited = json_reply(
        server
            .request(Method::PATCH, "/plans/1/drafts/2/parts")
            .body(bulk),
        200,
    )
    .await;
    assert_eq!(edited["parts"][1]["included"], true);
    assert_eq!(server.included(5), 1);
    assert_eq!(
        json_reply(
            server
                .request(Method::PATCH, "/plans/1/drafts/2/parts")
                .body(" ".repeat(8 * 1024 * 1024 + 1)),
            413
        )
        .await,
        json!({"detail":"Request body too large"})
    );
    server.close().await;
}

#[tokio::test]
async fn failed_selection_keeps_committed_edit_and_sanitizes_errors() {
    let server = ReadServer::start_limited(
        1024 * 1024,
        "CREATE TRIGGER injected_failure BEFORE INSERT ON plan_draft_required_unit_reconciliations BEGIN SELECT RAISE(ABORT,'private fixture detail'); END",
    )
    .await;
    assert_eq!(
        json_reply(
            server
                .request(Method::PATCH, "/plans/1/drafts/2/parts")
                .json(&edit(DIGEST)),
            500
        )
        .await,
        json!({"detail":"Plan draft update failed","code":"internal_error"})
    );
    assert_eq!(server.included(5), 1);
    assert_eq!(
        json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await["parts"][1]["included"],
        true
    );
    server.close().await;
}

#[tokio::test]
async fn present_edit_matches_node_conflict_and_domain_shapes() {
    use pp_storage::required_units::{DomainCode, Outcome as WorkspaceOutcome};
    let conflict = present_edit(WorkspaceOutcome::DraftChanged { workspace: None });
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let conflict_body: Value = crate::problem::without_envelope(
        serde_json::from_slice(&to_bytes(conflict.into_body(), 4096).await.unwrap()).unwrap(),
    );
    assert_eq!(conflict_body, json!({"code":"draft_changed"}));
    let domain = present_edit(WorkspaceOutcome::DomainError {
        code: DomainCode::RequiredUnitSetUnavailable,
    });
    assert_eq!(domain.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let domain_body: Value = crate::problem::without_envelope(
        serde_json::from_slice(&to_bytes(domain.into_body(), 4096).await.unwrap()).unwrap(),
    );
    assert_eq!(domain_body, json!({"code":"required_unit_set_unavailable"}));
    let baseline = present_edit(WorkspaceOutcome::AcceptedBaselineRequired);
    assert_eq!(baseline.status(), StatusCode::CONFLICT);
    let baseline_body: Value = crate::problem::without_envelope(
        serde_json::from_slice(&to_bytes(baseline.into_body(), 4096).await.unwrap()).unwrap(),
    );
    assert_eq!(baseline_body, json!({"code":"accepted_baseline_required"}));
}

#[tokio::test]
async fn abandon_persists_and_preserves_node_conflict_shapes() {
    let server = ReadServer::start(Some("true"), "default").await;
    let abandoned = json!({
        "draft_id":2,"state":"abandoned","lifecycle_version":1,
        "snapshot_digest":DIGEST,"base":{"revision_id":1,"plan_version":1}
    });
    for prefix in ["", "/api/v2"] {
        let server = ReadServer::start(Some("true"), "default").await;
        assert_eq!(
            json_reply(
                server
                    .request(Method::POST, &format!("{prefix}/plans/1/drafts/2/abandon"))
                    .json(&abandon_body(0)),
                200
            )
            .await,
            abandoned
        );
        assert_eq!(
            server
                .sql()
                .query_row(
                    "SELECT state,lifecycle_version FROM plan_drafts WHERE id=2",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                )
                .unwrap(),
            ("abandoned".into(), 1)
        );
        let listed = json_reply(
            server.request(Method::GET, &format!("{prefix}/plans/1/drafts")),
            200,
        )
        .await;
        assert_eq!(listed["drafts"][1], abandoned);
        let workspace = json_reply(
            server.request(Method::GET, &format!("{prefix}/plans/1/drafts/2")),
            200,
        )
        .await;
        assert_eq!(workspace["draft"], abandoned);
        // Idempotent retry with the same lifecycle token returns Ready.
        assert_eq!(
            json_reply(
                server
                    .request(Method::POST, &format!("{prefix}/plans/1/drafts/2/abandon"))
                    .json(&abandon_body(0)),
                200
            )
            .await,
            abandoned
        );
        // Stale token: 409 `{code,workspace}` with no synthetic detail.
        let stale = json_reply(
            server
                .request(Method::POST, &format!("{prefix}/plans/1/drafts/2/abandon"))
                .json(&abandon_body(99)),
            409,
        )
        .await;
        assert_eq!(stale["code"], "draft_changed");
        assert_eq!(stale["workspace"]["draft"], abandoned);
        assert!(stale.get("detail").is_none());
        server.close().await;
    }
    // Fixture draft 1 is already consumed → not_open with workspace, no detail.
    let current = json_reply(server.request(Method::GET, "/plans/1/drafts/1"), 200).await;
    let not_open = json_reply(
        server
            .request(Method::POST, "/plans/1/drafts/1/abandon")
            .json(&abandon_body(1)),
        409,
    )
    .await;
    assert_eq!(not_open, json!({"code":"not_open","workspace":current}));
    assert!(not_open.get("detail").is_none());
    server.close().await;
}

#[tokio::test]
async fn abandon_bounds_and_typed_bodies_reject_before_mutation() {
    let server = ReadServer::start(Some("true"), "default").await;
    for body in [
        "{",
        "null",
        "[]",
        "{}",
        "{\"expected_lifecycle_version\":-1}",
        "{\"expected_lifecycle_version\":0,\"extra\":true}",
        "{\"expected_lifecycle_version\":null}",
    ] {
        assert_eq!(
            invalid(
                json_reply(
                    server
                        .request(Method::POST, "/plans/1/drafts/2/abandon")
                        .body(body.to_owned()),
                    400
                )
                .await,
                body
            ),
            json!({"detail":"Request is invalid","code":"invalid_request"})
        );
    }
    assert_eq!(
        server
            .sql()
            .query_row(
                "SELECT state,lifecycle_version FROM plan_drafts WHERE id=2",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        ("open".into(), 0)
    );
    server.close().await;
}

#[tokio::test]
async fn present_abandon_matches_node_conflict_shapes() {
    use pp_storage::required_units::{DraftBase, DraftIdentity};
    let draft = DraftIdentity {
        draft_id: 2,
        state: "abandoned".into(),
        lifecycle_version: 1,
        snapshot_digest: DIGEST.into(),
        base: DraftBase {
            revision_id: Some(1),
            plan_version: 1,
        },
    };
    let ready = present_abandon(AbandonOutcome::Ready {
        draft: draft.clone(),
    });
    assert_eq!(ready.status(), StatusCode::OK);
    let ready_body: Value =
        serde_json::from_slice(&to_bytes(ready.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(
        ready_body,
        json!({"draft_id":2,"state":"abandoned","lifecycle_version":1,"snapshot_digest":DIGEST,"base":{"revision_id":1,"plan_version":1}})
    );
    let missing = present_abandon(AbandonOutcome::DraftNotFound);
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing_body: Value = crate::problem::without_envelope(
        serde_json::from_slice(&to_bytes(missing.into_body(), 4096).await.unwrap()).unwrap(),
    );
    assert_eq!(
        missing_body,
        json!({"detail":"Plan draft not found","code":"draft_not_found"})
    );
}

/// A second Plan with the same order number, a printed bracket unit, an
/// accepted Plate holding that unit, a watched checkoff link, and Spoolman
/// on with the bracket on spool 7.
const PREVIEW_SETUP: &str = "
UPDATE build_profiles SET order_number='#1001' WHERE id=1;
INSERT INTO build_profiles(id,tenant_id,name,order_number) VALUES(3,'default','Second Build','#1001');
UPDATE print_progress SET completed=1 WHERE part_id=1 AND unit_index=0;
UPDATE parts SET spoolman_spool_id=7 WHERE id=1;
INSERT INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles','{\"spoolman\":true}');
INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.checkoff_links',
  '[{\"id\":\"link-1\",\"state\":\"watching\",\"profile_id\":1,\"filename\":\"bracket.3mf\",\"units\":[{\"part_id\":1,\"unit_index\":0}]}]');
INSERT INTO accepted_plate_revisions(id,tenant_id,profile_id,plan_revision_id,plan_version,plan_revision_digest,required_unit_mapping_digest,layout_digest,expected_plate_count,expected_unit_count,revision_number,created_at)
  SELECT 1,'default',1,1,1,r.snapshot_digest,s.mapping_digest,'layout',1,1,1,'2026-08-21T00:00:00.000Z'
  FROM plan_revisions r JOIN plan_revision_required_unit_sets s ON s.revision_id=r.id WHERE r.id=1;
INSERT INTO accepted_plates(tenant_id,revision_id,plate_id,ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um)
  VALUES('default',1,'plate-a',1,'sim-1','Simulator','sim',256000,256000,256000,5000);
INSERT INTO accepted_plate_units(tenant_id,revision_id,plate_id,required_unit_token,x_um,y_um,width_um,depth_um,height_um)
  VALUES('default',1,'plate-a','ppu_00000000000000000000000000000001',0,0,1000,1000,1000);
INSERT INTO accepted_plate_heads(tenant_id,profile_id,current_revision_id) VALUES('default',1,1);
";
const PREVIEW: &str = "/api/v2/plans/1/drafts/2/accept-preview";

/// Every table's rows, sorted, plus the schema.
fn logical_database(connection: &rusqlite::Connection) -> Vec<(String, Vec<String>)> {
    let rows = |sql: &str| -> Vec<String> {
        let mut statement = connection.prepare(sql).unwrap();
        let width = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                (0..width)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|values| format!("{values:?}"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        rows
    };
    let tables = rows("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name");
    let mut dump = vec![(
        "sqlite_master".to_owned(),
        rows("SELECT type,name,tbl_name,sql FROM sqlite_master"),
    )];
    for table in tables {
        let name: String = serde_json::from_str::<Vec<Value>>(
            &table
                .trim_start_matches("[Text(")
                .trim_end_matches(")]")
                .replace('\\', ""),
        )
        .map(|_| String::new())
        .unwrap_or_else(|_| table[7..table.len() - 3].to_owned());
        let sql = format!("SELECT * FROM \"{name}\"");
        dump.push((name, rows(&sql)));
    }
    dump
}

/// Every file under the data directory with its bytes, except SQLite's
/// database and shared-memory files; the WAL is kept by length.
fn managed_files(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(root).unwrap().display().to_string();
            if path.is_dir() {
                pending.push(path);
            } else if name.ends_with("-wal") {
                let length = std::fs::metadata(&path).unwrap().len();
                files.insert(name, length.to_le_bytes().to_vec());
            } else if !name.ends_with(".db") && !name.ends_with("-shm") {
                files.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// Excludes nothing and shrinks the bracket from 3 to 1, as `edit` does.
async fn changed_draft(server: &ReadServer) -> Value {
    json_reply(
        server
            .request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts")
            .json(&edit(DIGEST)),
        200,
    )
    .await
}

fn accept(
    server: &ReadServer,
    revision: &Value,
    key: &str,
    remap_checkoff_links: bool,
) -> pp_storage::plan_publication::Outcome {
    let client = server.owner.publication_with_policy(policy()).unwrap();
    let mut request = revision.clone();
    let object = request.as_object_mut().unwrap();
    object.remove("draft_id");
    object.insert("remap_checkoff_links".into(), json!(remap_checkoff_links));
    let command = pp_storage::plan_publication::PublicationCommand::new(
        serde_json::from_value(json!(1)).unwrap(),
        serde_json::from_value(revision["draft_id"].clone()).unwrap(),
        serde_json::from_value(request).unwrap(),
        key.into(),
        pp_storage::read_model::Credential::Session(pp_storage::auth::Secret::new(
            "required-unit-fixture-secret".into(),
        )),
    )
    .unwrap();
    client
        .apply(command, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
}

#[tokio::test]
async fn accept_preview_lists_changes_and_checks_then_accept_refuses_its_stale_revision() {
    let server = ReadServer::start_limited(8 * 1024 * 1024, PREVIEW_SETUP).await;
    let changed = changed_draft(&server).await;
    let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    let digest = changed["draft"]["snapshot_digest"].clone();
    assert_eq!(preview["kind"], "changed");
    assert_eq!(
        preview["revision"],
        json!({"draft_id":2,"expected_snapshot_digest":digest,"expected_lifecycle_version":0,
            "expected_base":{"revision_id":1,"plan_version":1}})
    );
    // The live Part moved to spool 7 after the revision stored none; Accept
    // carries the live spool into the new revision.
    assert_eq!(
        preview["changes"]["fields"],
        json!([
            {"draft_part_id":4,"filename":"bracket.stl","field":"spoolman_spool_id",
                "old":null,"new":{"kind":"text","value":"7"}},
            {"draft_part_id":4,"filename":"bracket.stl","field":"quantity_override",
                "old":{"kind":"integer","value":3},"new":null},
            {"draft_part_id":4,"filename":"bracket.stl","field":"quantity_effective",
                "old":{"kind":"integer","value":3},"new":{"kind":"integer","value":1}},
            {"draft_part_id":5,"filename":"excluded.stl","field":"status",
                "old":{"kind":"text","value":"base"},"new":{"kind":"text","value":"excluded"}},
            {"draft_part_id":5,"filename":"excluded.stl","field":"included",
                "old":{"kind":"flag","value":false},"new":{"kind":"flag","value":true}}
        ])
    );
    assert_eq!(preview["changes"]["parts_added"], json!([]));
    assert_eq!(preview["changes"]["parts_removed"], json!([]));
    assert_eq!(preview["changes"]["inputs"], json!([]));
    assert_eq!(preview["changes"]["required_units"]["added"], json!([]));
    let unit = |token: u8, index: i64, printed: bool| {
        json!({"token":format!("ppu_{token:032}"),"filename":"bracket.stl","unit_index":index,
            "printed":printed,"assembled":false})
    };
    assert_eq!(
        preview["changes"]["required_units"]["removed"],
        json!([unit(2, 1, false), unit(3, 2, false)])
    );
    assert_eq!(
        preview["changes"]["required_units"]["printed_affected"],
        json!([unit(1, 0, true)])
    );
    assert_eq!(
        preview["changes"]["plates"],
        json!({"cleared":[{"plate_id":"plate-a","ordinal":1,"printer_name":"Simulator","unit_count":1}]})
    );
    assert_eq!(
        preview["changes"]["stock"],
        json!({"kind":"on","spools":[{"spool_id":"7","unprinted_before":2,"unprinted_after":0}]})
    );
    assert_eq!(
        preview["changes"]["duplicate_orders"],
        json!([{"profile_id":3,"name":"Second Build","order_number":"#1001"}])
    );
    assert_eq!(
        preview["changes"]["production"],
        json!({"checkoff_link_count":1,"send_queue_item_count":0,"unmappable":[]})
    );
    assert_eq!(preview["changes"]["blocked_by"], json!([]));
    assert_eq!(preview["snapshot"]["saved_before_accept"], true);
    // A later edit makes the preview's revision stale; Accept refuses it.
    let restore = json!({"expected_snapshot_digest":digest,
        "decision":{"kind":"set_included","draft_part_ids":[5],"value":false}});
    json_reply(
        server
            .request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts")
            .json(&restore),
        200,
    )
    .await;
    let stale = accept(&server, &preview["revision"], "stale-preview", true);
    assert!(
        matches!(stale, pp_storage::plan_publication::Outcome::DraftChanged),
        "{stale:?}"
    );
    let fresh = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    assert_ne!(fresh["revision"], preview["revision"]);
    let applied = accept(&server, &fresh["revision"], "fresh-preview", true);
    assert!(
        matches!(
            applied,
            pp_storage::plan_publication::Outcome::Applied { .. }
        ),
        "{applied:?}"
    );
    // A consumed draft: the preview refuses as a second Accept with a new key.
    let consumed = json_reply(server.request(Method::GET, PREVIEW), 409).await;
    assert_eq!(consumed["code"], "already_applied");
    let again = accept(&server, &fresh["revision"], "second-accept", true);
    assert_eq!(as_problem(&again), consumed);
    server.close().await;
}

#[tokio::test]
async fn accept_preview_refusals_match_accept() {
    let server = ReadServer::start(Some("true"), "default").await;
    // The fixture draft differs from its base and has no Required-unit
    // selection yet, so Accept would refuse it.
    assert_eq!(
        json_reply(server.request(Method::GET, PREVIEW), 409).await,
        json!({"code":"reconciliation_required","reason":"missing"})
    );
    // Fixture draft 1 is consumed and has an Accept receipt.
    let consumed = json_reply(
        server.request(Method::GET, "/api/v2/plans/1/drafts/1/accept-preview"),
        409,
    )
    .await;
    assert_eq!(consumed["code"], "already_applied");
    let first = json!({"draft_id":1,"expected_snapshot_digest":"3912383330ef528d51a15c351d69eaffd856d7e8dc582ddfc66bf0bf5053b2b0",
        "expected_lifecycle_version":0,"expected_base":{"revision_id":null,"plan_version":0}});
    assert_eq!(
        as_problem(&accept(&server, &first, "consumed", false)),
        consumed
    );
    assert_eq!(
        json_reply(
            server.request(Method::GET, "/api/v2/plans/1/drafts/9/accept-preview"),
            404
        )
        .await,
        json!({"detail":"Plan draft not found","code":"draft_not_found"})
    );
    assert_eq!(
        json_reply(
            server.request(Method::GET, "/api/v2/plans/9/drafts/2/accept-preview"),
            404
        )
        .await,
        json!({"detail":"Plan not found","code":"profile_not_found"})
    );
    let unknown = server
        .request(Method::GET, "/api/v2/plans/1/drafts/2/accept-preview/x")
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status().as_u16(), 404);
    server
        .sql()
        .execute_batch(
            "UPDATE build_profiles SET archived_at='2026-11-04T00:00:00.000Z' WHERE id=1",
        )
        .unwrap();
    assert_eq!(
        json_reply(server.request(Method::GET, PREVIEW), 409).await,
        json!({"code":"build_archived"})
    );
    // Accept checks the Plan before the draft, so a missing draft on an
    // archived Plan is `build_archived` too, not `draft_not_found`.
    let missing = json_reply(
        server.request(Method::GET, "/api/v2/plans/1/drafts/9/accept-preview"),
        409,
    )
    .await;
    let draft_9 = json!({"draft_id":9,"expected_snapshot_digest":DIGEST,
        "expected_lifecycle_version":0,"expected_base":{"revision_id":1,"plan_version":1}});
    assert_eq!(
        as_problem(&accept(&server, &draft_9, "missing", false)),
        missing
    );
    assert_eq!(missing, json!({"code":"build_archived"}));
    server.close().await;
}

#[tokio::test]
async fn accept_preview_writes_nothing() {
    let server = ReadServer::start_limited(8 * 1024 * 1024, PREVIEW_SETUP).await;
    changed_draft(&server).await;
    let database = || logical_database(&server.sql());
    let (rows, files) = (database(), managed_files(&server.directory));
    for path in [
        PREVIEW,
        "/plans/1/drafts/2/accept-preview",
        "/api/v2/plans/1/drafts/1/accept-preview",
        "/api/v2/plans/1/drafts/9/accept-preview",
    ] {
        let response = server.request(Method::GET, path).send().await.unwrap();
        assert!(response.status().as_u16() < 500, "{path}");
    }
    assert_eq!(database(), rows);
    assert_eq!(managed_files(&server.directory), files);
    server.close().await;
}

/// Both pinned by pp-storage's `accept_preview_test_revision_digests_are_pinned`.
const LIVE_SPOOL_REVISION_DIGEST: &str =
    "44de35eded4fab323f4df212ba4634a51912d1af49829c5672d86ebd7e552f7c";
const ALIGNED_REVISION_DIGEST: &str =
    "19ccb8c4f3f9e8d9c4da6083b64bd8fe25a2bfce6545a94782da40b99ca82b63";

/// Marks the fixture's accepted excluded Part `excluded`, as its open draft
/// does, so the draft equals the accepted Plan. The immutability triggers are
/// dropped for the update and recreated from the fixture's own SQL.
fn aligned_setup() -> String {
    revision_setup("", ALIGNED_REVISION_DIGEST)
}

/// [`aligned_setup`] plus `extra` revision and projection updates; `digest` is
/// the resulting revision digest.
fn revision_setup(extra: &str, digest: &str) -> String {
    let directory = crate::scratch::Scratch::new("pp-preview-align-");
    let path = directory.join("fixture.db");
    std::fs::write(
        &path,
        include_bytes!("../../../../pp-storage/tests/fixtures/required-units/unchanged.db"),
    )
    .unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let (mut drop, mut create) = (String::new(), String::new());
    for name in [
        "trg_plan_revisions_immutable_update",
        "trg_plan_revision_parts_immutable_update",
        "trg_parts_invalidate_accepted_revision_update",
    ] {
        let sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name=?1",
                [name],
                |row| row.get(0),
            )
            .unwrap();
        drop.push_str(&format!("DROP TRIGGER {name};\n"));
        create.push_str(&format!("{sql};\n"));
    }
    format!(
        "{drop}UPDATE plan_revision_parts SET status='excluded' WHERE id=2;
UPDATE parts SET status='excluded' WHERE id=2;
{extra}
UPDATE plan_revisions SET snapshot_digest='{digest}' WHERE id=1;
{create}"
    )
}

/// A no-op edit, which selects Required units without changing the draft.
async fn select_units(server: &ReadServer) -> Value {
    let edit = json!({"expected_snapshot_digest":DIGEST,
        "decision":{"kind":"set_included","draft_part_ids":[5],"value":false}});
    json_reply(
        server
            .request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts")
            .json(&edit),
        200,
    )
    .await
}

/// The Accept request fields for a workspace's draft.
fn revision_of(workspace: &Value) -> Value {
    let draft = &workspace["draft"];
    json!({"draft_id":draft["draft_id"],"expected_snapshot_digest":draft["snapshot_digest"],
        "expected_lifecycle_version":draft["lifecycle_version"],"expected_base":draft["base"]})
}

/// Accept's refusal as the preview's problem body carries it.
fn as_problem(outcome: &pp_storage::plan_publication::Outcome) -> Value {
    let mut body = serde_json::to_value(outcome).unwrap();
    let kind = body.as_object_mut().unwrap().remove("kind").unwrap();
    body["code"] = kind;
    body
}

#[tokio::test]
async fn accept_preview_of_a_draft_equal_to_the_accepted_plan_is_no_change() {
    let setup = aligned_setup();
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    let workspace = select_units(&server).await;
    assert_eq!(workspace["reconciliation"]["kind"], "ready");
    assert_eq!(
        json_reply(server.request(Method::GET, PREVIEW), 200).await,
        json!({"kind":"no_change","revision":revision_of(&workspace),
            "snapshot":{"saved_before_accept":true,
                "detail":"Print Partner saves a snapshot of the accepted Plan before it accepts this draft."}})
    );
    server.close().await;
}

/// An unchanged draft gets `no_change` only when Accept, sent without
/// `remap_checkoff_links`, would pass. Otherwise both refuse alike.
#[tokio::test]
async fn accept_preview_of_an_unchanged_draft_refuses_exactly_as_accept() {
    let setup = aligned_setup();
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    let unselected = json!({"draft_id":2,"expected_snapshot_digest":DIGEST,
        "expected_lifecycle_version":0,"expected_base":{"revision_id":1,"plan_version":1}});
    let preview = json_reply(server.request(Method::GET, PREVIEW), 409).await;
    assert_eq!(
        preview,
        json!({"code":"reconciliation_required","reason":"missing"})
    );
    assert_eq!(
        as_problem(&accept(&server, &unselected, "unselected", false)),
        preview
    );

    let revision = revision_of(&select_units(&server).await);
    server
        .sql()
        .execute_batch(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.checkoff_links',
              '[{\"id\":\"link-1\",\"state\":\"watching\",\"profile_id\":1,\"filename\":\"bracket.3mf\",\"units\":[{\"part_id\":1,\"unit_index\":0}]}]')",
        )
        .unwrap();
    let preview = json_reply(server.request(Method::GET, PREVIEW), 409).await;
    assert_eq!(
        preview,
        json!({"code":"production_active","checkoff_link_count":1,"send_queue_item_count":0})
    );
    assert_eq!(
        as_problem(&accept(&server, &revision, "production", false)),
        preview
    );

    let job = json!({"job_id":"running-export","tenant":"default","kind":"export-stl-pack",
        "payload_version":1,"payload":{"kind":"export-stl-pack","payload":{"profile_id":1,
        "missing_only":false,"group_by":"color_dir","unit_tokens":[],"filename_grouping":null}},
        "state":"running","state_version":1,"attempt":1,"generation":1,"lease_until":null,
        "created_at":1,"updated_at":1,"finished_at":null,"cancel_requested":false,
        "progress":null,"result":null,"recovery":null,"effects":[]});
    let sql = server.sql();
    sql.execute_batch("DELETE FROM app_settings WHERE key='printer.checkoff_links'")
        .unwrap();
    sql.execute(
        "INSERT INTO durable_jobs VALUES('running-export','default','export-stl-pack','running','fixture',1,1,NULL,1,1,?1)",
        [job.to_string()],
    )
    .unwrap();
    let preview = json_reply(server.request(Method::GET, PREVIEW), 409).await;
    assert_eq!(
        preview,
        json!({"code":"execution_conflict","operations":[
            {"operation_id":"running-export","kind":"export-stl-pack","state":"running"}]})
    );
    assert_eq!(
        as_problem(&accept(&server, &revision, "running", false)),
        preview
    );
    server.close().await;
}

/// Accepted Part 1 and its live row on spool 7; the draft's Part 4 still says
/// none. Accept publishes the live spool, so the preview diffs that.
fn live_spool_setup() -> String {
    revision_setup(
        "UPDATE plan_revision_parts SET spoolman_spool_id='7' WHERE id=1;
UPDATE parts SET spoolman_spool_id='7' WHERE id=1;",
        LIVE_SPOOL_REVISION_DIGEST,
    )
}

#[tokio::test]
async fn accept_preview_ignores_draft_filament_that_accept_replaces_with_the_live_part() {
    let setup = live_spool_setup();
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    let workspace = select_units(&server).await;
    let spool = |sql: &str| -> Option<String> {
        server.sql().query_row(sql, [], |row| row.get(0)).unwrap()
    };
    assert_eq!(
        spool("SELECT spoolman_spool_id FROM plan_draft_parts WHERE id=4"),
        None
    );
    let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    assert_eq!(preview["kind"], "no_change");
    let applied = accept(&server, &revision_of(&workspace), "live-spool", false);
    assert!(
        matches!(
            applied,
            pp_storage::plan_publication::Outcome::Applied { .. }
        ),
        "{applied:?}"
    );
    assert_eq!(
        spool(
            "SELECT r.spoolman_spool_id FROM plan_revision_parts r JOIN build_profiles b
               ON b.accepted_plan_revision_id=r.revision_id WHERE b.id=1 AND r.filename='bracket.stl'"
        ),
        Some("7".into())
    );
    server.close().await;
}

#[tokio::test]
async fn accept_preview_lists_only_the_changes_accept_publishes() {
    let setup = live_spool_setup();
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    changed_draft(&server).await;
    let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    let integer = |value: i64| json!({"kind":"integer","value":value});
    let flag = |value: bool| json!({"kind":"flag","value":value});
    assert_eq!(
        preview["changes"]["fields"],
        json!([
            {"draft_part_id":4,"filename":"bracket.stl","field":"quantity_override",
                "old":integer(3),"new":null},
            {"draft_part_id":4,"filename":"bracket.stl","field":"quantity_effective",
                "old":integer(3),"new":integer(1)},
            {"draft_part_id":5,"filename":"excluded.stl","field":"included",
                "old":flag(false),"new":flag(true)}
        ])
    );
    server.close().await;
}

fn reconcile_request(
    server: &ReadServer,
    path: &str,
    key: Option<&str>,
) -> reqwest::RequestBuilder {
    let request = server.request(Method::PUT, path);
    match key {
        Some(key) => request.header("Idempotency-Key", key),
        None => request,
    }
}

fn reconcile_body(digest: &str, decisions: Value) -> Value {
    json!({"expected_snapshot_digest": digest, "decisions": decisions})
}

/// The draft events storage committed, oldest first.
fn draft_events(server: &ReadServer) -> Vec<(String, String, String)> {
    let sql = server.sql();
    let mut statement = sql
        .prepare("SELECT event_type,aggregate_id,payload_json FROM domain_events ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn reconciliation_selects_once_replays_by_key_and_refuses_conflicts() {
    let server = ReadServer::start(Some("true"), "default").await;
    let path = "/api/v2/plans/1/drafts/2/reconciliation";
    let selected = json_reply(
        reconcile_request(&server, path, Some(" select-1 "))
            .json(&reconcile_body(DIGEST, json!([]))),
        200,
    )
    .await;
    assert_eq!(
        selected["reconciliation"],
        json!({"kind":"ready","reused_units":6,"new_units":0,"surplus_units":0})
    );
    assert_ne!(selected["draft"]["snapshot_digest"], DIGEST);
    let events = vec![(
        "plan.draft_changed".to_owned(),
        "1".to_owned(),
        "{\"draft_id\":2}".to_owned(),
    )];
    assert_eq!(draft_events(&server), events);
    assert_eq!(
        json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await,
        selected
    );
    let before = logical_database(&server.sql());
    // The same key and payload on the root alias replays without a write.
    assert_eq!(
        json_reply(
            reconcile_request(
                &server,
                "/plans/1/drafts/2/reconciliation",
                Some("select-1")
            )
            .json(&reconcile_body(DIGEST, json!([]))),
            200
        )
        .await,
        selected
    );
    // The same key with another payload conflicts before the stale digest.
    assert_eq!(
        json_reply(
            reconcile_request(&server, path, Some("select-1")).json(&reconcile_body(
                DIGEST,
                json!([{"kind":"replace","target_draft_part_id":4}])
            )),
            409
        )
        .await,
        json!({"code":"idempotency_conflict"})
    );
    // A new key against the digest the selection replaced is draft_changed.
    assert_eq!(
        json_reply(
            reconcile_request(&server, path, Some("select-2"))
                .json(&reconcile_body(DIGEST, json!([]))),
            409
        )
        .await,
        json!({"code":"draft_changed","workspace":selected})
    );
    assert_eq!(logical_database(&server.sql()), before);
    assert_eq!(draft_events(&server), events);
    server.close().await;
}

#[tokio::test]
async fn reconciliation_refuses_bad_keys_bodies_and_targets_without_writing() {
    let server = ReadServer::start(Some("true"), "default").await;
    let path = "/plans/1/drafts/2/reconciliation";
    let before = logical_database(&server.sql());
    let invalid_request = json!({"detail":"Request is invalid","code":"invalid_request"});
    let body = reconcile_body(DIGEST, json!([]));
    let long = "k".repeat(161);
    for key in [None, Some(""), Some("   "), Some(long.as_str())] {
        assert_eq!(
            json_reply(reconcile_request(&server, path, key).json(&body), 400).await,
            invalid_request,
            "{key:?}"
        );
    }
    for body in [
        "{",
        "[]",
        "{}",
        "{\"expected_snapshot_digest\":\"00\",\"decisions\":[]}",
        "{\"expected_snapshot_digest\":\"ae11e670337e3e5eab736b29b4e294edbea8b564661cf15972625783f8930ee3\",\"decisions\":[{\"kind\":\"replace\",\"target_draft_part_id\":4},{\"kind\":\"replace\",\"target_draft_part_id\":4}]}",
        "{\"expected_snapshot_digest\":\"ae11e670337e3e5eab736b29b4e294edbea8b564661cf15972625783f8930ee3\",\"decisions\":[],\"extra\":1}",
    ] {
        let mut problem = json_reply(
            reconcile_request(&server, path, Some("bad-body")).body(body.to_owned()),
            400,
        )
        .await;
        problem.as_object_mut().unwrap().remove("errors");
        assert_eq!(problem, invalid_request, "{body}");
    }
    assert_eq!(
        json_reply(
            reconcile_request(&server, "/plans/1/drafts/9/reconciliation", Some("missing"))
                .json(&body),
            404
        )
        .await,
        json!({"detail":"Plan draft not found","code":"draft_not_found"})
    );
    assert_eq!(
        json_reply(
            reconcile_request(&server, "/plans/7/drafts/2/reconciliation", Some("missing"))
                .json(&body),
            404
        )
        .await,
        json!({"detail":"Plan not found","code":"profile_not_found"})
    );
    assert_eq!(
        json_reply(
            reconcile_request(&server, "/plans/1/drafts/0/reconciliation", Some("zero"))
                .json(&body),
            400
        )
        .await,
        invalid_request
    );
    // Node throws for a decision that does not apply; the route answers its
    // generic 500 and the transaction rolls back.
    assert_eq!(
        json_reply(
            reconcile_request(&server, path, Some("inapplicable")).json(&reconcile_body(
                DIGEST,
                json!([{"kind":"accept_prior_completion","target_draft_part_id":4,"predecessor_revision_part_id":3}])
            )),
            500
        )
        .await,
        json!({"detail":"Plan draft update failed","code":"internal_error"})
    );
    let anonymous = server
        .client
        .request(Method::PUT, format!("{}{path}", server.origin))
        .header("Origin", &server.origin)
        .header("Idempotency-Key", "anonymous")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    assert_eq!(logical_database(&server.sql()), before);
    assert!(draft_events(&server).is_empty());
    server.close().await;
}

#[tokio::test]
async fn reconciliation_under_write_contention_is_transaction_unavailable() {
    let server = ReadServer::start(Some("true"), "default").await;
    let holder = server.sql();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let problem = json_reply(
        reconcile_request(
            &server,
            "/plans/1/drafts/2/reconciliation",
            Some("contended"),
        )
        .json(&reconcile_body(DIGEST, json!([]))),
        503,
    )
    .await;
    assert_eq!(
        problem,
        json!({"detail":"Plan draft update is unavailable","code":"transaction_unavailable"})
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    holder.execute_batch("ROLLBACK").unwrap();
    assert!(draft_events(&server).is_empty());
    server.close().await;
}

#[tokio::test]
async fn reconciliation_without_a_key_is_refused_before_its_body_is_read() {
    let server = ReadServer::start_limited(1024, "").await;
    let oversized = vec![b'x'; 4096];
    assert_eq!(
        json_reply(
            reconcile_request(&server, "/plans/1/drafts/2/reconciliation", None).body(oversized),
            400
        )
        .await,
        json!({"detail":"Request is invalid","code":"invalid_request"})
    );
    let mut stream = tokio::net::TcpStream::connect(server.host()).await.unwrap();
    let head = format!(
        "PUT /plans/1/drafts/2/reconciliation HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\n\
         Cookie: {FIXTURE_COOKIE}\r\nIdempotency-Key:  \r\nContent-Length: 100\r\n\r\n{{",
        server.host(),
        server.origin
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut response = vec![0; 1024];
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut response))
        .await
        .expect("answered before the body timeout")
        .unwrap();
    let response = String::from_utf8_lossy(&response[..read]);
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    server.close().await;
}

fn keyed(server: &ReadServer, path: &str, key: Option<&str>) -> reqwest::RequestBuilder {
    let request = server.request(Method::POST, path);
    match key {
        Some(key) => request.header("Idempotency-Key", key),
        None => request,
    }
}

fn apply_body(digest: &str, lifecycle: u32) -> Value {
    json!({"expected_snapshot_digest":digest,"expected_lifecycle_version":lifecycle,"expected_base":{"revision_id":1,"plan_version":1}})
}

fn rebase_body(state: &str, lifecycle: u32) -> Value {
    json!({"expected_source_state":state,"expected_source_lifecycle_version":lifecycle,"expected_source_snapshot_digest":DIGEST})
}

/// The fixture Source's pinned revision directory, empty, so a preparation
/// keeps the accepted Parts.
fn with_source_revision(server: &ReadServer) {
    std::fs::create_dir_all(server.directory.join("repos/1/revisions/fixture")).unwrap();
}

#[tokio::test]
async fn ticket_t_61_publication_lifecycle_routes_refuse_bad_keys_bodies_and_targets_without_writing()
 {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let before = logical_database(&server.sql());
    let invalid = json!({"detail":"Request is invalid","code":"invalid_request"});
    let long = "k".repeat(161);
    let cases = [
        ("/plans/1/drafts/recompute", json!({"apply_manifest":false})),
        ("/api/v2/plans/1/drafts/2/rebase", rebase_body("open", 0)),
        ("/plans/1/drafts/2/apply", apply_body(DIGEST, 0)),
    ];
    for (path, body) in &cases {
        for key in [None, Some(""), Some("  "), Some(long.as_str())] {
            assert_eq!(
                json_reply(keyed(&server, path, key).json(body), 400).await,
                invalid,
                "{path} {key:?}"
            );
        }
        for bad in ["{", "[]", "{}", "{\"apply_manifest\":\"yes\"}"] {
            let mut problem =
                json_reply(keyed(&server, path, Some("bad")).body(bad.to_owned()), 400).await;
            problem.as_object_mut().unwrap().remove("errors");
            assert_eq!(problem, invalid, "{path} {bad}");
        }
    }
    let plan_missing = json!({"detail":"Plan not found","code":"profile_not_found"});
    let draft_missing = json!({"detail":"Plan draft not found","code":"draft_not_found"});
    for (path, body, expected) in [
        (
            "/plans/7/drafts/recompute",
            json!({"apply_manifest":true}),
            &plan_missing,
        ),
        (
            "/plans/7/drafts/2/rebase",
            rebase_body("open", 0),
            &plan_missing,
        ),
        (
            "/plans/1/drafts/9/rebase",
            rebase_body("open", 0),
            &draft_missing,
        ),
        (
            "/plans/7/drafts/2/apply",
            apply_body(DIGEST, 0),
            &plan_missing,
        ),
        (
            "/plans/1/drafts/9/apply",
            apply_body(DIGEST, 0),
            &draft_missing,
        ),
    ] {
        assert_eq!(
            json_reply(keyed(&server, path, Some("missing")).json(&body), 404).await,
            *expected,
            "{path}"
        );
    }
    assert_eq!(logical_database(&server.sql()), before);
    assert!(draft_events(&server).is_empty());
    server.close().await;
}

#[tokio::test]
async fn ticket_t_61_publication_recompute_creates_and_selects_a_draft_once_per_key() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let body = json!({"apply_manifest":false});
    let created = json_reply(
        keyed(&server, "/api/v2/plans/1/drafts/recompute", Some(" r-1 ")).json(&body),
        200,
    )
    .await;
    assert_eq!(created["draft"]["draft_id"], 3);
    assert_eq!(created["draft"]["state"], "open");
    assert_eq!(created["reconciliation"]["kind"], "ready");
    assert_eq!(
        json_reply(server.request(Method::GET, "/plans/1/drafts/3"), 200).await,
        created
    );
    let events = [2, 3, 3].map(|draft| {
        (
            "plan.draft_changed".to_owned(),
            "1".to_owned(),
            format!("{{\"draft_id\":{draft}}}"),
        )
    });
    assert_eq!(draft_events(&server), events);
    let states = json_reply(server.request(Method::GET, "/plans/1/drafts"), 200).await;
    let states: Vec<_> = states["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|draft| (draft["draft_id"].clone(), draft["state"].clone()))
        .collect();
    assert!(
        states.contains(&(json!(2), json!("abandoned"))),
        "{states:?}"
    );
    let before = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            keyed(&server, "/plans/1/drafts/recompute", Some("r-1")).json(&body),
            200
        )
        .await,
        created
    );
    assert_eq!(logical_database(&server.sql()), before);
    assert_eq!(draft_events(&server), events);
    server.close().await;
}

#[tokio::test]
async fn ticket_t_61_publication_rebase_refusals_carry_node_workspaces_without_writing() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let workspace = json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await;
    let before = logical_database(&server.sql());
    let path = "/plans/1/drafts/2/rebase";
    assert_eq!(
        json_reply(
            keyed(&server, path, Some("rb-1")).json(&rebase_body("abandoned", 0)),
            409
        )
        .await,
        json!({"code":"not_open","workspace":workspace})
    );
    assert_eq!(
        json_reply(
            keyed(&server, path, Some("rb-2")).json(&rebase_body("open", 3)),
            409
        )
        .await,
        json!({"code":"draft_changed","workspace":workspace})
    );
    assert_eq!(
        json_reply(
            keyed(&server, path, Some("rb-3")).json(&rebase_body("open", 0)),
            409
        )
        .await,
        json!({"code":"base_unchanged"})
    );
    assert_eq!(logical_database(&server.sql()), before);
    assert!(draft_events(&server).is_empty());
    server.close().await;
}

#[tokio::test]
async fn ticket_t_61_publication_accept_selects_an_unselected_draft_and_publishes_once() {
    let server = ReadServer::start(Some("true"), "default").await;
    let path = "/api/v2/plans/1/drafts/2/apply";
    let workspace = json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await;
    let before = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            keyed(&server, path, Some("a-0")).json(&apply_body(DIGEST, 5)),
            409
        )
        .await,
        json!({"code":"draft_changed","workspace":workspace})
    );
    assert_eq!(logical_database(&server.sql()), before);
    let receipt = json_reply(
        keyed(&server, path, Some("a-1")).json(&apply_body(DIGEST, 0)),
        200,
    )
    .await;
    assert_eq!(
        (
            &receipt["profile_id"],
            &receipt["draft_id"],
            &receipt["revision_id"]
        ),
        (&json!(1), &json!(2), &json!(2))
    );
    assert_eq!(
        (
            &receipt["plan_version"],
            &receipt["draft_lifecycle_version"]
        ),
        (&json!(2), &json!(1))
    );
    let events = vec![
        (
            "plan.draft_changed".to_owned(),
            "1".to_owned(),
            "{\"draft_id\":2}".to_owned(),
        ),
        (
            "plan.revision_accepted".to_owned(),
            "1".to_owned(),
            "{\"revision_id\":2}".to_owned(),
        ),
    ];
    assert_eq!(draft_events(&server), events);
    let after = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            keyed(&server, "/plans/1/drafts/2/apply", Some("a-1")).json(&apply_body(DIGEST, 0)),
            200
        )
        .await,
        receipt
    );
    assert_eq!(
        json_reply(
            keyed(&server, path, Some("a-1")).json(&apply_body(DIGEST, 4)),
            409
        )
        .await,
        json!({"code":"idempotency_conflict"})
    );
    assert_eq!(logical_database(&server.sql()), after);
    assert_eq!(draft_events(&server), events);
    server.close().await;
}

fn autosave_body(draft: Value, value: u32) -> Value {
    json!({
        "expected_base": {"revision_id": 1, "plan_version": 1},
        "expected_draft": draft,
        "remap_checkoff_links": false,
        "decisions": [{
            "kind": "set_quantity_override",
            "target": {"part_key": "bracket.stl", "relative_path": "bracket.stl", "source_layer": "base:Fixture Source"},
            "value": value,
        }],
    })
}

fn observed(digest: &Value) -> Value {
    json!({"draft_id": 2, "state": "open", "lifecycle_version": 0, "snapshot_digest": digest, "base": {"revision_id": 1, "plan_version": 1}})
}

/// Accepted Plan rows autosave must never write.
fn accepted_rows(server: &ReadServer) -> Vec<(String, Vec<String>)> {
    logical_database(&server.sql())
        .into_iter()
        .filter(|(table, _)| {
            table == "build_profiles"
                || table.starts_with("plan_revision")
                || table == "plan_apply_requests"
        })
        .collect()
}

#[tokio::test]
async fn ticket_t_61_autosave_writes_the_open_draft_once_per_key_and_never_publishes() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let accepted = accepted_rows(&server);
    let identities = part_identities(&server);
    let path = "/api/v2/plans/1/drafts/save";
    let saved = json_reply(
        keyed(&server, path, Some(" s-1 ")).json(&autosave_body(observed(&json!(DIGEST)), 4)),
        200,
    )
    .await;
    let receipt = &saved["receipt"];
    assert_eq!(
        (&receipt["profile_id"], &receipt["draft_id"]),
        (&json!(1), &json!(2))
    );
    let workspace = &saved["workspace"];
    assert_eq!(workspace["draft"]["draft_id"], 2);
    assert_eq!(workspace["draft"]["state"], "open");
    assert_eq!(workspace["draft"]["lifecycle_version"], 0);
    assert_eq!(
        workspace["draft"]["snapshot_digest"],
        receipt["snapshot_digest"]
    );
    assert_eq!(workspace["reconciliation"]["kind"], "ready");
    let bracket = workspace["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["part_key"] == "bracket.stl")
        .unwrap();
    assert_eq!(
        (
            &bracket["quantity_override"],
            &bracket["quantity_effective"]
        ),
        (&json!(4), &json!(4))
    );
    assert_eq!(
        json_reply(server.request(Method::GET, "/plans/1/drafts/2"), 200).await,
        *workspace
    );
    assert_eq!(accepted_rows(&server), accepted);
    let event = (
        "plan.draft_changed".to_owned(),
        "1".to_owned(),
        "{\"draft_id\":2}".to_owned(),
    );
    let events = vec![event.clone(), event];
    assert_eq!(draft_events(&server), events);

    let after = logical_database(&server.sql());
    // A committed retry replays from its receipt before the stale digest.
    assert_eq!(
        json_reply(
            keyed(&server, "/plans/1/drafts/save", Some("s-1"))
                .json(&autosave_body(observed(&json!(DIGEST)), 4)),
            200
        )
        .await,
        saved
    );
    assert_eq!(
        json_reply(
            keyed(&server, path, Some("s-1")).json(&autosave_body(observed(&json!(DIGEST)), 5)),
            409
        )
        .await,
        json!({"code": "idempotency_conflict"})
    );
    assert_eq!(
        json_reply(
            keyed(&server, path, Some("s-2")).json(&autosave_body(observed(&json!(DIGEST)), 5)),
            409
        )
        .await,
        json!({"code": "draft_changed"})
    );
    assert_eq!(logical_database(&server.sql()), after);
    assert_eq!(draft_events(&server), events);
    assert_eq!(part_identities(&server), identities);

    // The next save observes the saved draft and supersedes its selection.
    let next = json_reply(
        keyed(&server, path, Some("s-3"))
            .json(&autosave_body(observed(&receipt["snapshot_digest"]), 5)),
        200,
    )
    .await;
    assert_eq!(next["receipt"]["draft_id"], 2);
    assert_ne!(
        next["receipt"]["reconciliation_id"],
        receipt["reconciliation_id"]
    );
    assert_eq!(accepted_rows(&server), accepted);
    assert_eq!(part_identities(&server), identities);
    server.close().await;
}

#[tokio::test]
async fn ticket_t_61_autosave_receipt_replays_after_restart() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let body = autosave_body(observed(&json!(DIGEST)), 4);
    let path = "/plans/1/drafts/save";
    let saved = json_reply(keyed(&server, path, Some("r-1")).json(&body), 200).await;
    let after = logical_database(&server.sql());
    let server = server.restart().await;
    assert_eq!(
        json_reply(keyed(&server, path, Some("r-1")).json(&body), 200).await,
        saved
    );
    assert_eq!(logical_database(&server.sql()), after);
    server.close().await;
}

/// Draft and Part ids with their immutable keys, in id order.
fn part_identities(server: &ReadServer) -> Vec<(i64, i64, String)> {
    let connection = server.sql();
    let mut statement = connection
        .prepare("SELECT draft_id,id,part_key FROM plan_draft_parts ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn ticket_t_61_autosave_rolls_back_a_failure_inside_the_transaction() {
    let server = ReadServer::start_limited(
        8 * 1024 * 1024,
        "CREATE TRIGGER fail_autosave BEFORE INSERT ON plan_draft_required_unit_reconciliations BEGIN SELECT RAISE(ABORT,'injected'); END",
    )
    .await;
    with_source_revision(&server);
    let before = logical_database(&server.sql());
    let body = autosave_body(observed(&json!(DIGEST)), 4);
    let path = "/plans/1/drafts/save";
    let reply = keyed(&server, path, Some("fails"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status().as_u16(), 500);
    assert!(!reply.text().await.unwrap().contains("injected"));
    assert_eq!(logical_database(&server.sql()), before);
    assert!(draft_events(&server).is_empty());

    server
        .sql()
        .execute_batch("DROP TRIGGER fail_autosave")
        .unwrap();
    let saved = json_reply(keyed(&server, path, Some("fails")).json(&body), 200).await;
    assert_eq!(saved["receipt"]["draft_id"], 2);
    server.close().await;
}

#[tokio::test]
async fn ticket_t_61_autosave_creates_a_final_state_draft_when_none_is_open() {
    let server = ReadServer::start_limited(
        8 * 1024 * 1024,
        "UPDATE plan_drafts SET state='abandoned',lifecycle_version=lifecycle_version+1 WHERE state='open'",
    )
    .await;
    with_source_revision(&server);
    let accepted = accepted_rows(&server);
    let saved = json_reply(
        keyed(&server, "/plans/1/drafts/save", Some("first")).json(&autosave_body(Value::Null, 2)),
        200,
    )
    .await;
    let draft_id = saved["receipt"]["draft_id"].clone();
    assert_eq!(saved["workspace"]["draft"]["draft_id"], draft_id);
    assert_eq!(saved["workspace"]["draft"]["state"], "open");
    let bracket = saved["workspace"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["part_key"] == "bracket.stl")
        .unwrap();
    assert_eq!(bracket["quantity_override"], 2);
    assert_eq!(accepted_rows(&server), accepted);
    let before = logical_database(&server.sql());
    assert_eq!(
        json_reply(
            keyed(&server, "/plans/1/drafts/save", Some("first"))
                .json(&autosave_body(Value::Null, 2)),
            200
        )
        .await,
        saved
    );
    assert_eq!(logical_database(&server.sql()), before);
    server.close().await;
}

#[tokio::test]
async fn ticket_t_61_autosave_refusals_write_nothing() {
    let server = ReadServer::start(Some("true"), "default").await;
    with_source_revision(&server);
    let before = logical_database(&server.sql());
    let body = autosave_body(observed(&json!(DIGEST)), 4);
    let invalid = json!({"detail": "Request is invalid", "code": "invalid_request"});
    for key in [None, Some(" "), Some(&*"k".repeat(161))] {
        assert_eq!(
            json_reply(keyed(&server, "/plans/1/drafts/save", key).json(&body), 400).await,
            invalid
        );
    }
    let mut stale_base = body.clone();
    stale_base["expected_base"]["plan_version"] = json!(2);
    stale_base["expected_draft"]["base"]["plan_version"] = json!(2);
    let mut missing_part = body.clone();
    missing_part["decisions"][0]["target"] = json!({"part_key": "nope.stl", "relative_path": "nope.stl", "source_layer": "base:Fixture Source"});
    for (path, body, status, expected) in [
        (
            "/plans/1/drafts/save",
            stale_base,
            409,
            json!({"code": "base_changed"}),
        ),
        (
            "/plans/1/drafts/save",
            autosave_body(Value::Null, 4),
            409,
            json!({"code": "draft_changed"}),
        ),
        (
            "/plans/1/drafts/save",
            missing_part,
            422,
            json!({"code": "part_not_found"}),
        ),
        (
            "/plans/7/drafts/save",
            body.clone(),
            404,
            json!({"detail": "Plan not found", "code": "profile_not_found"}),
        ),
    ] {
        assert_eq!(
            json_reply(keyed(&server, path, Some("refused")).json(&body), status).await,
            expected,
            "{path}"
        );
    }
    assert_eq!(logical_database(&server.sql()), before);
    assert!(draft_events(&server).is_empty());
    server.close().await;
}
