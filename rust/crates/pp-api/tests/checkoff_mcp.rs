#![cfg(feature = "desktop_rebuild")]
//! GRE-1170: a Checkoff burst through MCP `propose_verify_printer_checkoff`
//! and `confirm_apply`, served over loopback TCP with the HTTP Checkoff routes
//! from one storage owner, on a disposable copy of a real SQLite fixture.
//! Covers replay and reconciliation by action id across a restart, the
//! operation lookup route, and an undo refused after a later burst. GRE-1898:
//! a Spoolman filament and spool assigned through MCP reach the Filament
//! shortfalls read and a confirmed burst's deduction.
use checkoff_http::Fixture;
use pp_api::{
    accepted::{AcceptedHttpConfig, InventoryReader, accepted_router},
    checkoff::checkoff_router,
    mcp::{McpClients, McpHttpConfig, mcp_router},
    serving::Serving,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::{
        build_workflow::PendingInputs,
        stock::{Inventory, InventoryFilament, InventorySpool, SpoolmanIntegration},
    },
};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;
#[path = "support/mcp_rpc.rs"]
mod mcp_rpc;

const UNKNOWN_ACTION: &str = "Unknown, in-flight, or already applied/dismissed action_id";

/// Spoolman `sm-1` as these tests see it: filament 3 (`#112233`) with
/// spool 7 holding 500 g. Counts reads; nothing is contacted.
#[derive(Default)]
struct Stock(AtomicUsize);

impl InventoryReader for Stock {
    fn read(
        &self,
        integration: SpoolmanIntegration,
    ) -> Pin<Box<dyn Future<Output = Option<Inventory>> + Send>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let inventory = (integration.id == "sm-1").then(|| Inventory {
            filaments: [(
                3,
                InventoryFilament {
                    label: "Acme PLA · Night".into(),
                    hex: Some("#112233".into()),
                },
            )]
            .into(),
            spools: vec![InventorySpool {
                id: 7,
                filament_id: 3,
                remaining_g: Some(500.0),
            }],
        });
        Box::pin(std::future::ready(inventory))
    }
}

/// The Checkoff and accepted routes and MCP from one owner on a loopback
/// listener, all reading Spoolman through `stock`.
struct Served {
    origin: String,
    client: Client,
    bearer: String,
    serving: Serving,
    owner: WriterOwner,
    stock: Arc<Stock>,
}

impl Served {
    async fn start(fixture: &Fixture, bearer: Option<String>) -> Self {
        let owner = WriterOwner::open(&fixture.0, Limits::default()).unwrap().0;
        let stock = Arc::new(Stock::default());
        let (listener, origin) = mcp_rpc::listen().await;
        let progress = owner.checkoff_progress().unwrap();
        let auth = owner
            .rebuild_auth_with_policy(mcp_rpc::policy(), Default::default())
            .unwrap();
        let router = checkoff_router(
            AcceptedHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            progress.clone(),
        )
        .merge(accepted_router(
            AcceptedHttpConfig::from_startup(&origin, "default", Some("1"))
                .unwrap()
                .with_stock(stock.clone()),
            owner.accepted_reads_with_policy(mcp_rpc::policy()).unwrap(),
        ))
        .merge(mcp_router(
            McpHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            auth.clone(),
            McpClients {
                sources: owner.source_files(mcp_rpc::policy()).unwrap(),
                builds: owner
                    .build_graph_with_observations(
                        mcp_rpc::policy(),
                        Arc::new(owner.source_reads().unwrap()),
                        Default::default(),
                    )
                    .unwrap(),
                workflow: owner
                    .build_workflow(mcp_rpc::policy(), Arc::new(PendingInputs))
                    .unwrap(),
                drafts: owner
                    .working_drafts_with_observations(
                        mcp_rpc::policy(),
                        Arc::new(owner.source_reads().unwrap()),
                        Default::default(),
                    )
                    .unwrap(),
                planning: owner
                    .build_planning(mcp_rpc::policy(), owner.source_reads().unwrap())
                    .unwrap(),
                progress,
                accepted: owner.accepted_reads_with_policy(mcp_rpc::policy()).unwrap(),
                filaments: owner.filaments_with_policy(mcp_rpc::policy()).unwrap(),
                web_discovery: None,
                review_observer: None,
                farm: pp_api::mcp::McpFarm {
                    reads: owner.farm_reads(mcp_rpc::policy()).unwrap(),
                    observations: Arc::new(pp_api::farm::Unwatched),
                    modules: owner.client(),
                    activity: owner.print_activity(mcp_rpc::policy()).unwrap(),
                    inventory: Some(stock.clone()),
                },
            },
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
            stock,
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

/// `(state, integration_id, spool_id, grams, units)` of a deduction row.
type DeductionRow = (String, Option<String>, Option<i64>, Option<f64>, i64);

/// GRE-1898: the assistant gives role `primary` Spoolman filament 3 and pins
/// spool 7. The Build's included accepted Parts carry both, the Filament
/// section reads that filament from Spoolman, and a confirmed burst records
/// one `pending` deduction for spool 7.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spoolman_role_filament_reaches_the_shortfalls_and_a_pending_deduction() {
    assign_read_and_burst(r#"{"spoolman":true}"#, None).await;
}

/// The same with Spoolman off for the user and on in Build 1's override:
/// every step reads the Build's setting, as Checkoff's deduction does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_builds_spoolman_override_reaches_the_shortfalls_and_a_pending_deduction() {
    assign_read_and_burst(r#"{"spoolman":false}"#, Some(r#"{"spoolman":true}"#)).await;
}

/// Spoolman on for the user and off in Build 1's override: the Filament
/// section answers `module_off` and reads nothing from Spoolman.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_builds_spoolman_override_off_keeps_the_shortfalls_from_reading_spoolman() {
    let fixture = Fixture::new();
    let served = Served::start(&fixture, None).await;
    seed_spoolman(
        &fixture,
        r#"{"spoolman":true}"#,
        Some(r#"{"spoolman":false}"#),
    );
    fixture
        .sql()
        .execute_batch(
            "UPDATE parts SET filament_color_id='spoolman:sm-1:filament:3' WHERE profile_id=1",
        )
        .unwrap();
    let (status, shortfalls) = served
        .http(Method::GET, "/api/v2/plans/1/filament-shortfalls", None)
        .await;
    assert_eq!(status, 200, "{shortfalls}");
    assert_eq!(
        shortfalls,
        json!({ "kind": "module_off", "module": "spoolman", "reason": "turned_off" })
    );
    assert_eq!(served.stock.0.load(Ordering::SeqCst), 0);
    served.stop().await;
}

/// The user's toggles, Build 1's Plan override when `plan` is `Some`,
/// Spoolman `sm-1`, Build 1's planning brief and 40 g over `link-verify`'s
/// 4 units. Run after the owner opens, which migrates the fixture.
fn seed_spoolman(fixture: &Fixture, user: &str, plan: Option<&str>) {
    let brief = json!({
        "version": 1, "build_id": 1, "special_request": "", "requirements": [],
        "evidence": [], "contributions": [], "compatibility_findings": null,
        "differences": [], "resolutions": {}, "role_filaments": [],
        "checklist_items": null, "managed_source_ids": null,
        "draft_source_revisions": null, "draft_id": null, "draft_review_blockers": [],
        "created_at": "2026-11-21T00:00:00.000Z", "updated_at": "2026-11-21T00:00:00.000Z",
    });
    let integrations = r#"[{"id":"sm-1","type":"spoolman","name":"Spoolman","config":{"base_url":"http://127.0.0.1:7912","enabled":true},"created_at":"2026-11-01T00:00:00.000Z","updated_at":"2026-11-01T00:00:00.000Z"}]"#;
    let sql = fixture.sql();
    let mut settings = vec![
        ("feature_toggles", user.to_owned()),
        ("integrations", integrations.to_owned()),
        ("build_planning.v1.1", brief.to_string()),
    ];
    if let Some(plan) = plan {
        settings.push(("feature_toggles:plan:1", plan.to_owned()));
    }
    for (key, value) in settings {
        sql.execute(
            "REPLACE INTO app_settings (tenant_id,key,value) VALUES ('default',?1,?2)",
            [key, value.as_str()],
        )
        .unwrap();
    }
    sql.execute_batch(
        "INSERT INTO print_link_grams (tenant_id, link_id, grams, units, basis, updated_at)
         VALUES ('default', 'link-verify', 40, 4, 'estimated', '2026-11-12T00:00:00.000Z');",
    )
    .unwrap();
}

async fn assign_read_and_burst(user: &str, plan: Option<&str>) {
    let fixture = Fixture::new();
    let served = Served::start(&fixture, None).await;
    seed_spoolman(&fixture, user, plan);
    let sql = fixture.sql();
    let mcp = served.mcp().await;
    let assignment = |spool: &str| {
        json!({ "plan_id": 1, "assignment": {
            "role": "Primary", "inventory_kind": "spoolman",
            "inventory_id": "spoolman:sm-1:filament:3", "color_hex": "#112233",
            "spoolman_spool_id": spool,
        } })
    };

    // A spool of another filament is refused at confirm and writes nothing.
    let (proposal, error) = mcp
        .call(
            "propose_assign_role_filament",
            assignment("spoolman:sm-1:spool:8"),
        )
        .await;
    assert!(!error, "{proposal}");
    let (refused, error) = mcp
        .confirm(proposal["action"]["id"].as_str().unwrap())
        .await;
    assert!(error, "{refused}");
    assert_eq!(refused["code"], "spoolman_spool_not_found");

    let (proposal, error) = mcp
        .call(
            "propose_assign_role_filament",
            assignment("spoolman:sm-1:spool:7"),
        )
        .await;
    assert!(!error, "{proposal}");
    let (applied, error) = mcp
        .confirm(proposal["action"]["id"].as_str().unwrap())
        .await;
    assert!(!error && applied["ok"] == true, "{applied}");
    assert_eq!(
        applied["result"],
        json!({ "role": "Primary", "updated_parts": 2 })
    );
    let parts: Vec<(i64, Option<String>, Option<String>)> = {
        let mut statement = sql
            .prepare("SELECT id, filament_color_id, spoolman_spool_id FROM parts WHERE profile_id=1 ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let pinned = |id| {
        (
            id,
            Some("spoolman:sm-1:filament:3".to_owned()),
            Some("spoolman:sm-1:spool:7".to_owned()),
        )
    };
    // Part 2 is excluded from the accepted Plan, so it keeps no filament.
    assert_eq!(parts, [pinned(1), (2, None, None), pinned(3)]);
    let reads = served.stock.0.load(Ordering::SeqCst);

    // The Filament section reads filament 3 from Spoolman.
    let (status, shortfalls) = served
        .http(Method::GET, "/api/v2/plans/1/filament-shortfalls", None)
        .await;
    assert_eq!(status, 200, "{shortfalls}");
    assert_eq!(served.stock.0.load(Ordering::SeqCst), reads + 1);
    let row = &shortfalls["filaments"][0];
    assert_eq!(
        row["filament_color_id"], "spoolman:sm-1:filament:3",
        "{shortfalls}"
    );
    assert_eq!(row["label"], "Acme PLA · Night");
    assert_eq!(row["grams_available"]["grams"], 500, "{shortfalls}");

    // A confirmed burst for Part 1 records one pending deduction for spool 7.
    let (proposal, error) = mcp
        .call(
            "propose_verify_printer_checkoff",
            json!({ "link_id": "link-verify", "decisions": [
                { "part_id": 1, "unit_index": 1, "result": "confirmed" },
                { "part_id": 1, "unit_index": 2, "result": "confirmed" },
            ] }),
        )
        .await;
    assert!(!error, "{proposal}");
    let (burst, _) = mcp
        .confirm(proposal["action"]["id"].as_str().unwrap())
        .await;
    assert_eq!(burst["ok"], true, "{burst}");
    let deductions: Vec<DeductionRow> = {
        let mut statement = sql
            .prepare(
                "SELECT state, integration_id, spool_id, grams, units FROM spoolman_deductions",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        deductions,
        [(
            "pending".into(),
            Some("sm-1".into()),
            Some(7),
            Some(20.0),
            2
        )]
    );
    served.stop().await;
}
