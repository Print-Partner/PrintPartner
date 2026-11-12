use super::*;
use reqwest::{Client, Method, RequestBuilder};
use std::net::SocketAddr;

#[path = "tests/draft_planning.rs"]
mod draft_planning;
#[path = "../../tests/support/mcp_rpc.rs"]
mod mcp_rpc;
#[path = "tests/planning.rs"]
mod planning;
#[path = "tests/progress_reads.rs"]
mod progress_reads;
#[path = "tests/reads.rs"]
mod reads;
use mcp_rpc::{policy, rpc};

const FIXTURE_SESSION: &str = "required-unit-fixture-secret";
/// Header value replaced by the server's own origin.
const SELF: &str = "<origin>";

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "ticket-t-16", "version": "0.0.1" },
        },
    })
}

struct McpServer {
    origin: String,
    client: Client,
    owner: pp_storage::WriterOwner,
    task: tokio::task::JoinHandle<()>,
    /// Last field: removed only after `owner` has dropped, even on panic.
    _directory: crate::scratch::Scratch,
}

impl McpServer {
    async fn start(flag: Option<&str>) -> Self {
        Self::routed(flag, "default").await
    }

    async fn routed(flag: Option<&str>, tenant: &str) -> Self {
        Self::open(flag, tenant, "", |config| config).await
    }

    /// The flag on for `default`, with `seed` run on the fixture first.
    async fn seeded(seed: &str) -> Self {
        Self::open(Some("1"), "default", seed, |config| config).await
    }

    async fn configured(
        flag: Option<&str>,
        tenant: &str,
        configure: impl FnOnce(McpHttpConfig) -> McpHttpConfig,
    ) -> Self {
        Self::open(flag, tenant, "", configure).await
    }

    async fn open(
        flag: Option<&str>,
        tenant: &str,
        seed: &str,
        configure: impl FnOnce(McpHttpConfig) -> McpHttpConfig,
    ) -> Self {
        let directory = crate::scratch::Scratch::new("pp-mcp-http-");
        let database = directory.join("print-partner.db");
        std::fs::write(
            &database,
            include_bytes!("../../../pp-storage/tests/fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(&seed.replace(
                "<data-dir>",
                &directory.to_string_lossy().replace('\'', "''"),
            ))
            .unwrap();
        let owner = pp_storage::WriterOwner::open(&directory, pp_storage::Limits::default())
            .unwrap()
            .0;
        let (listener, origin) = mcp_rpc::listen().await;
        let config = configure(McpHttpConfig::from_startup(&origin, tenant, flag).unwrap());
        let (router, _) = test_router(&owner, config, policy());
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
            _directory: directory,
        }
    }

    async fn auth(&self, request: auth::Request) -> auth::Outcome {
        let client = self
            .owner
            .rebuild_auth_with_policy(policy(), Default::default())
            .unwrap();
        tokio::task::spawn_blocking(move || {
            client
                .submit(
                    request,
                    Arc::new(AtomicBool::new(false)),
                    Duration::from_secs(2),
                )
                .unwrap()
                .recv()
                .unwrap()
                .unwrap()
        })
        .await
        .unwrap()
    }

    /// Creates a Settings key through the fixture session: `(id, ppk_ secret)`.
    async fn create_key(&self) -> (String, String) {
        let created = self
            .auth(auth::Request::CreateKey {
                session: Secret::new(FIXTURE_SESSION.into()),
            })
            .await;
        let auth::Outcome::KeyCreated { info, key } = created else {
            panic!("API key creation");
        };
        (info.id, key.expose().to_owned())
    }

    async fn revoke(&self, key_id: String) {
        let revoked = self
            .auth(auth::Request::RevokeKey {
                session: Secret::new(FIXTURE_SESSION.into()),
                key_id,
            })
            .await;
        assert!(matches!(
            revoked,
            auth::Outcome::KeyChanged { changed: true, .. }
        ));
    }

    /// POSTs `body` to `path`; returns status, body text and any MCP session id.
    async fn post(
        &self,
        path: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> (u16, String, Option<String>) {
        mcp_rpc::send(self.request(path, headers, body)).await
    }

    fn request(&self, path: &str, headers: &[(&str, &str)], body: &Value) -> RequestBuilder {
        let mut request = self
            .client
            .request(Method::POST, format!("{}{path}", self.origin))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body.to_string());
        for (name, value) in headers {
            request = request.header(*name, value.replace(SELF, &self.origin));
        }
        request
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_16_mcp_requires_a_resolved_key() {
    let server = McpServer::start(Some("1")).await;
    let (_, key) = server.create_key().await;
    assert!(key.starts_with("ppk_"));
    let bearer = format!("Bearer {key}");
    let cases: [(&str, Vec<(&str, &str)>); 5] = [
        ("no credential", vec![]),
        (
            "session cookie",
            vec![("cookie", "pp_session=required-unit-fixture-secret")],
        ),
        (
            "unknown key",
            vec![(
                "authorization",
                "Bearer ppk_0000000000000000000000000000000000000000",
            )],
        ),
        (
            "malformed key",
            vec![("x-print-partner-api-key", "not-a-key")],
        ),
        (
            "two credentials",
            vec![
                ("authorization", bearer.as_str()),
                ("x-print-partner-api-key", key.as_str()),
            ],
        ),
    ];
    // Same-origin, so only the credential check can refuse. Keyless callers
    // without Origin are already refused by loopback admission (403).
    for path in ["/mcp", "/api/v1/mcp"] {
        for (name, headers) in &cases {
            let mut headers = headers.clone();
            headers.push(("origin", SELF));
            let (status, body, session) = server.post(path, &headers, &initialize()).await;
            assert_eq!(status, 401, "{path} {name}: {body}");
            assert_eq!(
                rpc(&body),
                json!({ "detail": KEY_REQUIRED }),
                "{path} {name}"
            );
            assert!(session.is_none(), "{path} {name}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_16_mcp_accepts_a_ppk_key_until_it_is_revoked() {
    let server = McpServer::start(Some("1")).await;
    let (key_id, key) = server.create_key().await;
    let bearer = format!("Bearer {key}");
    for (path, headers) in [
        ("/mcp", vec![("authorization", bearer.as_str())]),
        (
            "/api/v1/mcp",
            vec![("x-print-partner-api-key", key.as_str())],
        ),
    ] {
        let (status, body, session) = server.post(path, &headers, &initialize()).await;
        assert_eq!(status, 200, "{path}: {body}");
        assert!(session.is_some(), "{path}");
        let result = &rpc(&body)["result"];
        assert!(result["capabilities"]["tools"].is_object(), "{body}");
        assert_eq!(result["serverInfo"], server_info()["serverInfo"]);
        // The negotiated revision is the one the client asked for (GRE-1303).
        assert_eq!(result["protocolVersion"], "2025-06-18", "{body}");
    }
    // A revision the server does not know falls back to the one it advertises.
    let mut unknown = initialize();
    unknown["params"]["protocolVersion"] = json!("2099-01-01");
    let (status, body, _) = server
        .post("/mcp", &[("authorization", bearer.as_str())], &unknown)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        rpc(&body)["result"]["protocolVersion"],
        "2025-11-25",
        "{body}"
    );
    server.revoke(key_id).await;
    let (status, body, session) = server
        .post(
            "/api/v1/mcp",
            &[("authorization", bearer.as_str())],
            &initialize(),
        )
        .await;
    assert_eq!(status, 401, "{body}");
    assert!(session.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_16_mcp_refuses_foreign_origin_and_host() {
    let server = McpServer::start(Some("1")).await;
    let (_, key) = server.create_key().await;
    let bearer = format!("Bearer {key}");
    let host = server.origin.strip_prefix("http://").unwrap().to_owned();
    let rebound = host.replace("127.0.0.1", "localhost");
    for (name, extra) in [
        ("foreign origin", ("origin", "http://evil.example")),
        ("other port origin", ("origin", "http://127.0.0.1:1")),
        ("rebound host", ("host", rebound.as_str())),
        ("forwarded", ("x-forwarded-host", "evil.example")),
        ("cross-site fetch", ("sec-fetch-site", "cross-site")),
    ] {
        let headers = [("authorization", bearer.as_str()), extra];
        let (status, body, session) = server.post("/api/v1/mcp", &headers, &initialize()).await;
        assert_eq!(status, 403, "{name}: {body}");
        assert!(session.is_none(), "{name}");
    }
    let same_origin = [("authorization", bearer.as_str()), ("origin", SELF)];
    let (status, body, _) = server
        .post("/api/v1/mcp", &same_origin, &initialize())
        .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_16_mcp_lists_tools_and_write_tools_only_propose() {
    let server = McpServer::start(Some("1")).await;
    let (_, key) = server.create_key().await;
    let bearer = format!("Bearer {key}");
    let (status, body, session) = server
        .post(
            "/api/v1/mcp",
            &[("authorization", bearer.as_str())],
            &initialize(),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let session = session.unwrap();
    let headers = [
        ("authorization", bearer.as_str()),
        ("mcp-session-id", session.as_str()),
        ("mcp-protocol-version", "2025-06-18"),
    ];
    let notified = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let (status, body, _) = server.post("/api/v1/mcp", &headers, &notified).await;
    assert_eq!(status, 202, "{body}");

    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let (status, body, _) = server.post("/api/v1/mcp", &headers, &list).await;
    assert_eq!(status, 200, "{body}");
    let names: Vec<String> = rpc(&body)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    for expected in ["confirm_apply", "list_pending_actions", "start_sync"] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} in {names:?}"
        );
    }

    // An unported write tool reports a tool error and leaves nothing pending.
    let call = |id: u64, name: &str| {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": name, "arguments": { "source_name": "Fixture" } } })
    };
    let (status, body, _) = server
        .post("/api/v1/mcp", &headers, &call(3, "start_sync"))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(rpc(&body)["result"]["isError"], true, "{body}");
    let (status, body, _) = server
        .post("/api/v1/mcp", &headers, &call(4, "list_pending_actions"))
        .await;
    assert_eq!(status, 200, "{body}");
    let text = rpc(&body)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({ "actions": [] })
    );

    // The session id does not stand in for the key.
    let keyless = [
        ("mcp-session-id", session.as_str()),
        ("mcp-protocol-version", "2025-06-18"),
        ("origin", SELF),
    ];
    let (status, _, _) = server.post("/api/v1/mcp", &keyless, &list).await;
    assert_eq!(status, 401);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_16_mcp_is_absent_without_the_rebuild_flag() {
    let server = McpServer::start(None).await;
    let (_, key) = server.create_key().await;
    let bearer = format!("Bearer {key}");
    for path in ["/mcp", "/api/v1/mcp"] {
        let (status, _, _) = server
            .post(path, &[("authorization", bearer.as_str())], &initialize())
            .await;
        assert_eq!(status, 404, "{path}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_16_mcp_refuses_a_key_from_another_tenant() {
    // The fixture session's key belongs to `default`; this listener routes `other`.
    let server = McpServer::routed(Some("1"), "other").await;
    let (_, key) = server.create_key().await;
    let bearer = format!("Bearer {key}");
    for path in ["/mcp", "/api/v1/mcp"] {
        let (status, body, session) = server
            .post(path, &[("authorization", bearer.as_str())], &initialize())
            .await;
        assert_eq!(status, 401, "{path}: {body}");
        assert_eq!(rpc(&body), json!({ "detail": KEY_REQUIRED }), "{path}");
        assert!(session.is_none(), "{path}");
    }
}

/// GRE-1039: Node's application budget on Rust MCP. An admitted request
/// counts before its key is checked, so keyless guesses use it up too; past
/// the limit even a valid key gets the gateway's 429 until the window resets.
/// A foreign Origin is refused first and costs nothing. The test clock moves
/// only when the test advances it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_16_mcp_rate_limits_requests_before_the_key_check() {
    let clock = rate_limit::ManualClock::new();
    let server = McpServer::configured(Some("1"), "default", |config| {
        config.with_budget(3, Duration::from_secs(60), clock.clone())
    })
    .await;
    let (_, key) = server.create_key().await;
    let bearer = format!("Bearer {key}");
    let send = |path: &'static str, headers: Vec<(&'static str, String)>| {
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        server.request(path, &headers, &initialize()).send()
    };
    let header = |response: &reqwest::Response, name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    let keyed_headers = || vec![("authorization", bearer.clone())];

    let mut foreign = keyed_headers();
    foreign.push(("origin", "http://evil.example".into()));
    let refused = send("/mcp", foreign).await.unwrap();
    assert_eq!(refused.status(), 403);
    assert_eq!(header(&refused, "x-ratelimit-remaining"), None);

    for (path, remaining) in [("/mcp", "2"), ("/api/v1/mcp", "1")] {
        let keyless = send(path, vec![("origin", server.origin.clone())])
            .await
            .unwrap();
        assert_eq!(keyless.status(), 401, "{path}");
        assert_eq!(header(&keyless, "x-ratelimit-limit").as_deref(), Some("3"));
        assert_eq!(header(&keyless, "x-ratelimit-reset").as_deref(), Some("60"));
        assert_eq!(
            header(&keyless, "x-ratelimit-remaining").as_deref(),
            Some(remaining),
            "{path}"
        );
    }
    let admitted = send("/mcp", keyed_headers()).await.unwrap();
    assert_eq!(admitted.status(), 200);
    assert_eq!(
        header(&admitted, "x-ratelimit-remaining").as_deref(),
        Some("0")
    );

    // 1.5 s and then 0.5 s left: `retry-after` rounds the remainder up.
    for (advance, reset, after) in [
        (Duration::from_millis(58_500), "2", "2 seconds"),
        (Duration::from_secs(1), "1", "1 second"),
    ] {
        clock.advance(advance);
        for path in ["/mcp", "/api/v1/mcp"] {
            let limited = send(path, keyed_headers()).await.unwrap();
            assert_eq!(limited.status(), 429, "{path}");
            assert!(header(&limited, "mcp-session-id").is_none(), "{path}");
            let expected = json!({
                "statusCode": 429,
                "error": "Too Many Requests",
                "message": format!("Rate limit exceeded, retry in {after}"),
            })
            .to_string();
            for (name, value) in [
                ("content-type", "application/json; charset=utf-8"),
                ("cache-control", "no-store"),
                ("retry-after", reset),
                ("x-ratelimit-reset", reset),
                ("x-ratelimit-limit", "3"),
                ("x-ratelimit-remaining", "0"),
            ] {
                assert_eq!(
                    header(&limited, name).as_deref(),
                    Some(value),
                    "{path} {name}"
                );
            }
            assert_eq!(
                header(&limited, "content-length"),
                Some(expected.len().to_string()),
                "{path}"
            );
            assert_eq!(limited.text().await.unwrap(), expected, "{path}");
        }
    }

    // At exactly 60 s a new window starts.
    clock.advance(Duration::from_millis(500));
    let reset = send("/api/v1/mcp", keyed_headers()).await.unwrap();
    assert_eq!(reset.status(), 200);
    assert!(header(&reset, "mcp-session-id").is_some());
    assert_eq!(
        header(&reset, "x-ratelimit-remaining").as_deref(),
        Some("2")
    );
    assert_eq!(header(&reset, "x-ratelimit-reset").as_deref(), Some("60"));
}
