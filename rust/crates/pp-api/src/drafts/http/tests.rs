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
        let owner = pp_storage::WriterOwner::open(&directory, pp_storage::Limits::default())
            .unwrap()
            .0;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = DraftHttpConfig::from_startup(&origin, tenant, flag)
            .unwrap()
            .with_limits(body_limit, Duration::from_millis(100))
            .unwrap();
        let client = owner.working_drafts_with_policy(policy()).unwrap();
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
        "/plans/1/drafts/recompute",
        "/plans/1/drafts/2/rebase",
        "/plans/1/drafts/2/reconciliation",
        "/plans/1/drafts/2/apply",
        "/plans/1/save",
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
    for body in [
        json!({"expected_snapshot_digest":DIGEST,"decisions":[]}),
        json!({"expected_snapshot_digest":DIGEST,"decision":{"kind":"set_included","draft_part_ids":[0],"value":true}}),
        json!({"expected_snapshot_digest":DIGEST,"decision":{"kind":"set_quantity_override","draft_part_ids":[4],"value":10001}}),
    ] {
        assert_eq!(
            json_reply(
                server
                    .request(Method::PATCH, "/plans/1/drafts/2/parts")
                    .json(&body),
                400
            )
            .await,
            json!({"detail":"Request is invalid","code":"invalid_request"})
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
