use super::*;
use reqwest::{Client, Method};
use std::net::SocketAddr;

#[path = "../../tests/support/mcp_rpc.rs"]
mod mcp_rpc;
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
        let directory = crate::scratch::Scratch::new("pp-mcp-http-");
        std::fs::write(
            directory.join("print-partner.db"),
            include_bytes!("../../../pp-storage/tests/fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        let owner = pp_storage::WriterOwner::open(&directory, pp_storage::Limits::default())
            .unwrap()
            .0;
        let (listener, origin) = mcp_rpc::listen().await;
        let config = McpHttpConfig::from_startup(&origin, tenant, flag).unwrap();
        let auth = owner
            .rebuild_auth_with_policy(policy(), Default::default())
            .unwrap();
        let workflow = owner
            .build_workflow(
                policy(),
                Arc::new(pp_storage::read_model::build_workflow::PendingInputs),
            )
            .unwrap();
        let router = mcp_router(config, auth, workflow, owner.checkoff_progress().unwrap());
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
        let mut request = self
            .client
            .request(Method::POST, format!("{}{path}", self.origin))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body.to_string());
        for (name, value) in headers {
            request = request.header(*name, value.replace(SELF, &self.origin));
        }
        mcp_rpc::send(request).await
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
    }
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
