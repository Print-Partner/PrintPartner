#![cfg(feature = "desktop_rebuild")]
//! GRE-1170: a Checkoff burst through MCP `propose_verify_printer_checkoff`
//! and `confirm_apply`, served over loopback TCP with the HTTP Checkoff routes
//! from one storage owner, on a disposable copy of a real SQLite fixture.
//! Covers replay and reconciliation by action id across a restart, the
//! operation lookup route, and an undo refused after a later burst.
use checkoff_http::Fixture;
use pp_api::{
    accepted::AcceptedHttpConfig,
    checkoff::checkoff_router,
    mcp::{McpHttpConfig, mcp_router},
    serving::Serving,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::build_workflow::PendingInputs,
};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;
#[path = "support/mcp_rpc.rs"]
mod mcp_rpc;

const UNKNOWN_ACTION: &str = "Unknown, in-flight, or already applied/dismissed action_id";

/// The Checkoff routes and MCP from one owner on a loopback listener.
struct Served {
    origin: String,
    client: Client,
    bearer: String,
    serving: Serving,
    owner: WriterOwner,
}

impl Served {
    async fn start(fixture: &Fixture, bearer: Option<String>) -> Self {
        let owner = WriterOwner::open(&fixture.0, Limits::default()).unwrap().0;
        let (listener, origin) = mcp_rpc::listen().await;
        let progress = owner.checkoff_progress().unwrap();
        let auth = owner
            .rebuild_auth_with_policy(mcp_rpc::policy(), Default::default())
            .unwrap();
        let router = checkoff_router(
            AcceptedHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            progress.clone(),
        )
        .merge(mcp_router(
            McpHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            auth.clone(),
            owner
                .build_workflow(mcp_rpc::policy(), Arc::new(PendingInputs))
                .unwrap(),
            progress,
        ));
        let bearer = match bearer {
            Some(bearer) => bearer,
            None => {
                let created = tokio::task::spawn_blocking(move || {
                    auth.submit(
                        auth::Request::CreateKey {
                            session: Secret::new("required-unit-fixture-secret".into()),
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
                let auth::Outcome::KeyCreated { key, .. } = created else {
                    panic!("API key creation");
                };
                format!("Bearer {}", key.expose())
            }
        };
        Self {
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            bearer,
            serving: Serving::start(listener, router),
            owner,
        }
    }

    async fn stop(mut self) -> String {
        self.serving.stop().await.unwrap();
        self.owner.shutdown().unwrap();
        self.bearer
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> (u16, String, Option<String>) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("authorization", &self.bearer)
            .header("origin", &self.origin)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request.body(body.to_string());
        }
        mcp_rpc::send(request).await
    }

    async fn http(&self, method: Method, path: &str, body: Option<&Value>) -> (u16, Value) {
        let (status, text, _) = self.request(method, path, &[], body).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn mcp(&self) -> Mcp<'_> {
        let initialize = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "gre-1170", "version": "0.0.1" },
            },
        });
        let (status, body, session) = self
            .request(Method::POST, "/api/v1/mcp", &[], Some(&initialize))
            .await;
        assert_eq!(status, 200, "{body}");
        let mcp = Mcp {
            served: self,
            session: session.unwrap(),
        };
        let notified = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let (status, body, _) = self
            .request(Method::POST, "/api/v1/mcp", &mcp.headers(), Some(&notified))
            .await;
        assert_eq!(status, 202, "{body}");
        mcp
    }
}

struct Mcp<'a> {
    served: &'a Served,
    session: String,
}

impl Mcp<'_> {
    fn headers(&self) -> [(&str, &str); 2] {
        [
            ("mcp-session-id", self.session.as_str()),
            ("mcp-protocol-version", "2025-06-18"),
        ]
    }

    /// The tool's JSON text and whether it is a tool error.
    async fn call(&self, name: &str, arguments: Value) -> (Value, bool) {
        let call = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": name, "arguments": arguments } });
        let (status, body, _) = self
            .served
            .request(Method::POST, "/api/v1/mcp", &self.headers(), Some(&call))
            .await;
        assert_eq!(status, 200, "{body}");
        let rpc = mcp_rpc::rpc(&body);
        let result = &rpc["result"];
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            serde_json::from_str(text).unwrap(),
            result["isError"] == true,
        )
    }

    async fn propose(&self, link_id: &str, decisions: Value) -> String {
        let (proposal, error) = self
            .call(
                "propose_verify_printer_checkoff",
                json!({ "link_id": link_id, "decisions": decisions }),
            )
            .await;
        assert!(!error, "{proposal}");
        assert_eq!(proposal["status"], "proposed");
        assert_eq!(
            proposal["action"]["type"],
            "propose_verify_printer_checkoff"
        );
        assert_eq!(proposal["action"]["plan_id"], 1);
        proposal["action"]["id"].as_str().unwrap().to_owned()
    }

    async fn confirm(&self, action_id: &str) -> (Value, bool) {
        self.call("confirm_apply", json!({ "action_id": action_id }))
            .await
    }
}

fn printed(fixture: &Fixture) -> i64 {
    fixture
        .sql()
        .query_row(
            "SELECT COUNT(*) FROM print_progress WHERE completed=1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn confirmed(part: i64, unit: i64) -> Value {
    json!([{ "part_id": part, "unit_index": unit, "result": "confirmed" }])
}

fn status_path(key: &str) -> String {
    format!(
        "/printer-checkoff/verify-batch?idempotency_key={}",
        key.replace(':', "%3A")
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_batch_mcp_applies_once_and_reconciles_by_action_id_across_restart() {
    let fixture = Fixture::new();
    let before = printed(&fixture);
    let served = Served::start(&fixture, None).await;
    let mcp = served.mcp().await;
    let action = mcp.propose("link-burst", confirmed(2, 0)).await;
    assert_eq!(printed(&fixture), before, "a proposal changes nothing");

    let (applied, error) = mcp.confirm(&action).await;
    assert!(!error, "{applied}");
    assert_eq!(applied["ok"], true);
    let receipt = applied["result"].clone();
    assert_eq!(receipt["idempotency_key"], format!("mcp:{action}"));
    assert_eq!(receipt["units_confirmed"], 1);
    assert!(receipt["undo_token"].as_str().unwrap().starts_with("ppb_"));
    assert_eq!(printed(&fixture), before + 1);

    // Confirming again answers from the stored burst and writes nothing.
    let (again, error) = mcp.confirm(&action).await;
    assert!(!error, "{again}");
    assert_eq!(
        (&again["ok"], &again["reconciled"], &again["result"]),
        (&json!(true), &json!(true), &receipt)
    );
    assert_eq!(printed(&fixture), before + 1);
    // Node matches action ids exactly, so a padded id names no action even
    // though the burst lookup would trim it to this action's key.
    for padded in [format!("{action} "), format!("\u{feff}{action}\u{feff}")] {
        let (unknown, is_error) = mcp.confirm(&padded).await;
        assert!(is_error, "{unknown}");
        assert_eq!(unknown["error"], UNKNOWN_ACTION);
    }
    assert_eq!(printed(&fixture), before + 1);
    let (status, lookup) = served
        .http(Method::GET, &status_path(&format!("mcp:{action}")), None)
        .await;
    assert_eq!(status, 200, "{lookup}");
    assert_eq!(lookup["state"], "applied");
    assert_eq!(lookup["receipt"], receipt);
    let bearer = served.stop().await;

    // A new process and a new MCP session reconcile the same action id.
    let served = Served::start(&fixture, Some(bearer)).await;
    let mcp = served.mcp().await;
    let (replayed, error) = mcp.confirm(&action).await;
    assert!(!error, "{replayed}");
    assert_eq!(replayed["result"], receipt);
    assert_eq!(printed(&fixture), before + 1);

    // After an HTTP undo the action reports undone, not applied.
    let (status, undone) = served
        .http(
            Method::POST,
            "/api/v2/printer-checkoff/verify-batch/undo",
            Some(&json!({ "undo_token": receipt["undo_token"] })),
        )
        .await;
    assert_eq!(status, 200, "{undone}");
    assert_eq!(printed(&fixture), before);
    let (after_undo, error) = mcp.confirm(&action).await;
    assert!(error, "{after_undo}");
    assert_eq!(
        (&after_undo["ok"], &after_undo["reconciled"]),
        (&json!(false), &json!(true))
    );
    let (_, lookup) = served
        .http(Method::GET, &status_path(&format!("mcp:{action}")), None)
        .await;
    assert_eq!(lookup["state"], "undone");
    assert_eq!(printed(&fixture), before);
    served.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_batch_mcp_undo_conflicts_and_lookups_write_nothing() {
    let fixture = Fixture::new();
    let before = printed(&fixture);
    let served = Served::start(&fixture, None).await;

    let (status, missing) = served
        .http(Method::GET, &status_path("never-sent"), None)
        .await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["code"], "burst_not_found");
    let (status, _) = served
        .http(Method::GET, "/printer-checkoff/verify-batch", None)
        .await;
    assert_eq!(status, 400);

    let mcp = served.mcp().await;
    for (arguments, error) in [
        (
            json!({ "link_id": "  ", "decisions": confirmed(3, 0) }),
            "link_id and decisions are required",
        ),
        (
            json!({ "link_id": "link-verify", "decisions": [] }),
            "link_id and decisions are required",
        ),
        (
            json!({ "link_id": "no-such-link", "decisions": confirmed(3, 0) }),
            "Checkoff link not found",
        ),
        (
            json!({ "link_id": "link-verify\u{85}", "decisions": confirmed(3, 0) }),
            "Checkoff link not found",
        ),
        (
            json!({ "link_id": "link-host-failed", "decisions": confirmed(3, 0) }),
            "Checkoff link is not awaiting verification",
        ),
    ] {
        let (answer, is_error) = mcp.call("propose_verify_printer_checkoff", arguments).await;
        assert!(is_error);
        assert_eq!(answer, json!({ "error": error }));
    }
    let (pending, _) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending, json!({ "actions": [] }));
    let (unknown, is_error) = mcp.confirm("not-an-action").await;
    assert!(is_error);
    assert_eq!(unknown["error"], UNKNOWN_ACTION);

    // A refused burst writes nothing and stays pending for a retry.
    let refused = mcp
        .propose(
            "link-verify",
            json!([{ "part_id": 3, "unit_index": 0, "result": "maybe" }]),
        )
        .await;
    let (answer, is_error) = mcp.confirm(&refused).await;
    assert!(is_error, "{answer}");
    assert_eq!(answer["ok"], false);
    assert_eq!(printed(&fixture), before);
    let (pending, _) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending["actions"][0]["id"], refused);

    // Burst A confirms Part 3's first unit; burst B its second. Undoing A
    // would leave B's unit above an unprinted one, so undo is refused.
    let first = mcp.propose("link-verify", confirmed(3, 0)).await;
    let (a, _) = mcp.confirm(&first).await;
    assert_eq!(a["ok"], true, "{a}");
    let second = mcp.propose("link-burst", confirmed(3, 1)).await;
    let (b, _) = mcp.confirm(&second).await;
    assert_eq!(b["ok"], true, "{b}");
    assert_eq!(printed(&fixture), before + 2);
    let (status, conflict) = served
        .http(
            Method::POST,
            "/printer-checkoff/verify-batch/undo",
            Some(&json!({ "undo_token": a["result"]["undo_token"] })),
        )
        .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(printed(&fixture), before + 2);
    let (_, lookup) = served
        .http(Method::GET, &status_path(&format!("mcp:{first}")), None)
        .await;
    assert_eq!(lookup["state"], "applied");
    served.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_batch_lookup_reports_expired_and_unknown_keys() {
    let fixture = Fixture::new();
    let served = Served::start(&fixture, None).await;
    let mcp = served.mcp().await;
    // A retired key is expired. Once the retired list is full, a key in
    // neither list may have been dropped, so the lookup says unknown.
    use sha2::Digest;
    let mut retired: Vec<String> = (0..4_999).map(|i| format!("{i:064x}")).collect();
    retired.push(hex::encode(sha2::Sha256::digest(b"old-key")));
    fixture
        .sql()
        .execute(
            "INSERT INTO app_settings (tenant_id,key,value) VALUES ('default','printer.checkoff_bursts_retired',?1)",
            [json!(retired).to_string()],
        )
        .unwrap();
    let (_, old) = served
        .http(Method::GET, &status_path("old-key"), None)
        .await;
    assert_eq!(old["state"], "expired", "{old}");
    let (status, forgotten) = served
        .http(Method::GET, &status_path("never-sent"), None)
        .await;
    assert_eq!((status, &forgotten["state"]), (200, &json!("unknown")));
    let (unknown_action, is_error) = mcp.confirm("never-sent").await;
    assert!(is_error);
    assert_eq!(unknown_action["error"], UNKNOWN_ACTION);
    let (overlong, is_error) = mcp.confirm(&"x".repeat(200)).await;
    assert!(is_error);
    assert_eq!(overlong["error"], UNKNOWN_ACTION);
    served.stop().await;
}
