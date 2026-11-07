//! MCP accepts a draft through the Plan tab's Accept preview and Accept
//! (GRE-1236): the proposal holds the previewed revision, confirm copies the
//! draft header once, and a stale proposal is refused and stays pending.
use super::*;
use crate::mcp::{McpHttpConfig, mcp_router};

/// A key-authenticated MCP session on its own loopback listener, served from
/// the draft server's writer.
struct Mcp {
    origin: String,
    client: Client,
    headers: Vec<(&'static str, String)>,
    task: tokio::task::JoinHandle<()>,
}

impl Mcp {
    async fn start(server: &ReadServer) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let owner = &server.owner;
        let auth = owner
            .rebuild_auth_with_policy(policy(), Default::default())
            .unwrap();
        let workflow = owner
            .build_workflow(
                policy(),
                Arc::new(pp_storage::read_model::build_workflow::PendingInputs),
            )
            .unwrap();
        let reads = Arc::new(owner.source_reads().unwrap());
        let drafts = owner
            .working_drafts_with_observations(policy(), reads, Default::default())
            .unwrap();
        let planning = owner
            .build_planning(policy(), owner.source_reads().unwrap())
            .unwrap();
        let router = mcp_router(
            McpHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            auth.clone(),
            workflow,
            drafts,
            planning,
        );
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let created = tokio::task::spawn_blocking(move || {
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
        let pp_storage::auth::Outcome::KeyCreated { key, .. } = created else {
            panic!("API key creation");
        };
        let mut mcp = Self {
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            headers: vec![("authorization", format!("Bearer {}", key.expose()))],
            task,
        };
        let initialize = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"gre-1236","version":"0.0.1"}}});
        let response = mcp.send(&initialize).await;
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        mcp.headers.push(("mcp-session-id", session));
        mcp.headers
            .push(("mcp-protocol-version", "2025-06-18".into()));
        let initialized = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert_eq!(mcp.send(&initialized).await.status(), 202);
        mcp
    }

    async fn send(&self, body: &Value) -> reqwest::Response {
        let mut request = self
            .client
            .post(format!("{}/mcp", self.origin))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body.to_string());
        for (name, value) in &self.headers {
            request = request.header(*name, value);
        }
        request.send().await.unwrap()
    }

    /// Calls `name`; returns whether it is a tool error and its JSON text.
    async fn call(&self, name: &str, arguments: Value) -> (bool, Value) {
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}});
        let text = self.send(&body).await.text().await.unwrap();
        let data = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .find(|data| !data.is_empty())
            .unwrap_or(&text);
        let reply: Value = serde_json::from_str(data).unwrap();
        let result = &reply["result"];
        let content = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"] == true,
            serde_json::from_str(content).unwrap(),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Stores the Build planning brief: it selects `draft_id` and resolves
/// `differences` with `resolutions`.
fn brief(server: &ReadServer, draft_id: i64, differences: Value, resolutions: Value) {
    let brief = json!({
        "version": 1, "build_id": 1, "special_request": "", "requirements": [],
        "evidence": [], "contributions": [], "differences": differences,
        "resolutions": resolutions, "role_filaments": [], "draft_id": draft_id,
        "draft_review_blockers": [], "created_at": "2026-11-07T12:00:00.000Z",
        "updated_at": "2026-11-07T12:00:00.000Z",
    });
    let sql = server.sql();
    sql.execute(
        "DELETE FROM app_settings WHERE tenant_id='default' AND key='build_planning.v1.1'",
        [],
    )
    .unwrap();
    sql.execute(
        "INSERT INTO app_settings (tenant_id,key,value) VALUES ('default','build_planning.v1.1',?1)",
        [brief.to_string()],
    )
    .unwrap();
}

/// A custom resolution, and a chosen side whose excluded printable path
/// names a Source the brief's evidence does not.
fn unresolved_choices() -> (Value, Value) {
    let difference = |id: &str, group: &str| {
        json!({"id": id, "group_id": group, "family": "f", "kind": "changed",
            "source_a": "Alpha", "source_b": "Beta", "path_a": "a/part.stl",
            "path_b": "b/part.STL", "detail": "d"})
    };
    let resolution = |resolution: &str| {
        json!({"resolution": resolution, "rationale": "r", "custom_resolution": null,
            "resolved_at": "2026-11-07T12:00:00.000Z"})
    };
    (
        json!([difference("d1", "g1"), difference("d2", "g2")]),
        json!({"g1": resolution("custom"), "g2": resolution("choose_source_a")}),
    )
}

const UNRESOLVED: &str = "Working Plan cannot apply source choices: g1: custom resolution requires explicit draft review; g2: resolved printable path is missing from the draft: Beta/b/part.STL";

/// A Build whose brief selects draft 2, with a header saved in the Plan tab.
/// `select` selects the draft's Required-unit reconciliation first.
async fn header_draft(select: bool) -> ReadServer {
    let setup = aligned_setup();
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    if select {
        select_units(&server).await;
    }
    brief(&server, 2, json!([]), json!({}));
    server
        .sql()
        .execute(
            "INSERT INTO plan_draft_headers (draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at) VALUES (2,'default','Renamed by MCP','PO-9','Acme','2026-12-01',1,'2026-11-07T12:00:00.000Z')",
            [],
        )
        .unwrap();
    server
}

async fn proposed_header() -> ReadServer {
    header_draft(true).await
}

fn build_name(server: &ReadServer) -> String {
    server
        .sql()
        .query_row("SELECT name FROM build_profiles WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Every row but `app_settings`, where each MCP request stamps the key's
/// `lastUsedAt`.
fn plan_rows(server: &ReadServer) -> Vec<(String, Vec<String>)> {
    logical_database(&server.sql())
        .into_iter()
        .filter(|(table, _)| table != "app_settings")
        .collect()
}

fn snapshot_headers(server: &ReadServer) -> i64 {
    server
        .sql()
        .query_row("SELECT count(*) FROM plan_snapshot_headers", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gre_1236_mcp_accepts_the_previewed_revision_and_copies_the_header_once() {
    let server = proposed_header().await;
    let mcp = Mcp::start(&server).await;
    let accepted = build_name(&server);
    let (failed, proposed) = mcp
        .call(
            "propose_apply_plan_draft",
            json!({"plan_id": 1, "draft_id": 2}),
        )
        .await;
    assert!(!failed, "{proposed}");
    assert_eq!(proposed["status"], "proposed");
    let action = &proposed["action"];
    assert_eq!(action["params"]["revision"]["expected_header_version"], 1);
    assert!(action["params"]["revision"].get("draft_id").is_none());
    assert_eq!(proposed["preview"]["kind"], "changed");
    assert_eq!(
        proposed["preview"]["changes"]["header"][0],
        json!({"field":"name","old":accepted,"new":"Renamed by MCP"})
    );
    assert_eq!(build_name(&server), accepted, "a proposal writes nothing");

    let id = action["id"].as_str().unwrap();
    let (failed, confirmed) = mcp.call("confirm_apply", json!({"action_id": id})).await;
    assert!(!failed, "{confirmed}");
    assert_eq!(confirmed["ok"], true);
    let result = &confirmed["result"];
    assert_eq!(result["apply_result"]["kind"], "applied");
    assert_eq!(result["apply_result"]["receipt"]["revisionId"], 2);
    assert_eq!(result["apply_result"]["receipt"]["draftId"], 2);
    assert_eq!(
        result["planning_phase"],
        json!({"kind": "applied", "draft_id": 2, "revision_id": 2})
    );
    assert!(result["build_workflow"].is_object(), "{result}");
    assert_eq!(build_name(&server), "Renamed by MCP");
    assert_eq!(snapshot_headers(&server), 1);

    let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending, json!({"actions": []}));
    let (failed, again) = mcp.call("confirm_apply", json!({"action_id": id})).await;
    assert!(failed, "{again}");
    assert_eq!(snapshot_headers(&server), 1);
    drop(mcp);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gre_1236_mcp_refuses_a_stale_proposal_and_keeps_it_pending() {
    let server = proposed_header().await;
    let mcp = Mcp::start(&server).await;
    let (_, proposed) = mcp
        .call(
            "propose_apply_plan_draft",
            json!({"plan_id": 1, "draft_id": 2}),
        )
        .await;
    let id = proposed["action"]["id"].as_str().unwrap().to_owned();
    server
        .sql()
        .execute(
            "UPDATE plan_draft_headers SET name='Saved after the proposal',header_version=2 WHERE draft_id=2",
            [],
        )
        .unwrap();
    let before = plan_rows(&server);
    let (failed, refused) = mcp.call("confirm_apply", json!({"action_id": id})).await;
    assert!(failed, "{refused}");
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["result"]["apply_result"]["kind"], "draft_changed");
    assert_eq!(plan_rows(&server), before);
    let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending["actions"][0]["id"], id);
    drop(mcp);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gre_1236_mcp_proposes_nothing_when_accept_would_refuse() {
    let server = ReadServer::start(Some("true"), "default").await;
    let mcp = Mcp::start(&server).await;
    let (failed, no_brief) = mcp
        .call(
            "propose_apply_plan_draft",
            json!({"plan_id": 1, "draft_id": 99}),
        )
        .await;
    assert!(failed);
    assert_eq!(no_brief, json!({"error": "Build planning brief not found"}));
    brief(&server, 2, json!([]), json!({}));
    let (failed, missing) = mcp
        .call(
            "propose_apply_plan_draft",
            json!({"plan_id": 1, "draft_id": 99}),
        )
        .await;
    assert!(failed);
    assert_eq!(missing, json!({"error": "Selected Working Plan not found"}));
    let (failed, invalid) = mcp
        .call("propose_apply_plan_draft", json!({"plan_id": 1}))
        .await;
    assert!(failed);
    assert_eq!(invalid, json!({"error": "plan_id and draft_id required"}));
    let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending, json!({"actions": []}));
    drop(mcp);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gre_1236_mcp_proposes_nothing_while_production_would_refuse_accept() {
    let server = ReadServer::start_limited(8 * 1024 * 1024, PREVIEW_SETUP).await;
    changed_draft(&server).await;
    brief(&server, 2, json!([]), json!({}));
    let mcp = Mcp::start(&server).await;
    let before = plan_rows(&server);
    let (failed, refused) = mcp
        .call(
            "propose_apply_plan_draft",
            json!({"plan_id": 1, "draft_id": 2}),
        )
        .await;
    assert!(failed, "{refused}");
    assert_eq!(refused["error"], "Working Plan cannot be accepted");
    assert_eq!(
        refused["preview"]["changes"]["production"]["checkoff_link_count"],
        1
    );
    let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending, json!({"actions": []}));
    assert_eq!(plan_rows(&server), before);
    drop(mcp);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gre_1236_mcp_refuses_a_draft_the_brief_does_not_select_or_unresolved_choices() {
    let server = proposed_header().await;
    let mcp = Mcp::start(&server).await;
    let before = plan_rows(&server);
    let propose = || {
        mcp.call(
            "propose_apply_plan_draft",
            json!({"plan_id": 1, "draft_id": 2}),
        )
    };
    brief(&server, 3, json!([]), json!({}));
    let (failed, other) = propose().await;
    assert!(failed);
    assert_eq!(other, json!({"error": "Selected Working Plan not found"}));
    // Hydration drops the selection once the pinned Source revisions moved.
    brief(&server, 2, json!([]), json!({}));
    server
        .sql()
        .execute(
            "UPDATE app_settings SET value=json_set(value,'$.draft_source_revisions',json('{\"1\":\"moved\"}')) WHERE key='build_planning.v1.1'",
            [],
        )
        .unwrap();
    let (failed, moved) = propose().await;
    assert!(failed);
    assert_eq!(moved, json!({"error": "Selected Working Plan not found"}));
    let (differences, resolutions) = unresolved_choices();
    brief(&server, 2, differences.clone(), resolutions.clone());
    let (failed, unresolved) = propose().await;
    assert!(failed);
    assert_eq!(unresolved, json!({"error": UNRESOLVED}));
    let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending, json!({"actions": []}));

    brief(&server, 2, json!([]), json!({}));
    let (failed, proposed) = propose().await;
    assert!(!failed, "{proposed}");
    let id = proposed["action"]["id"].as_str().unwrap().to_owned();
    brief(&server, 2, differences, resolutions);
    let (failed, refused) = mcp.call("confirm_apply", json!({"action_id": id})).await;
    assert!(failed, "{refused}");
    assert_eq!(refused, json!({"ok": false, "detail": UNRESOLVED}));
    brief(&server, 3, json!([]), json!({}));
    let (failed, refused) = mcp.call("confirm_apply", json!({"action_id": id})).await;
    assert!(failed, "{refused}");
    assert_eq!(
        refused,
        json!({"ok": false, "detail": "Selected Working Plan not found"})
    );
    let (_, pending) = mcp.call("list_pending_actions", json!({})).await;
    assert_eq!(pending["actions"][0]["id"], id);
    assert_eq!(plan_rows(&server), before);
    drop(mcp);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gre_1236_mcp_confirm_selects_the_automatic_reconciliation_propose_writes_nothing() {
    let server = header_draft(false).await;
    let mcp = Mcp::start(&server).await;
    let before = plan_rows(&server);
    let (failed, proposed) = mcp
        .call(
            "propose_apply_plan_draft",
            json!({"plan_id": 1, "draft_id": 2}),
        )
        .await;
    assert!(!failed, "{proposed}");
    assert_eq!(proposed["action"]["params"]["revision"], Value::Null);
    assert_eq!(proposed["reconciliation"]["reason"], "missing");
    assert_eq!(plan_rows(&server), before, "a proposal writes nothing");
    let id = proposed["action"]["id"].as_str().unwrap();
    let (failed, confirmed) = mcp.call("confirm_apply", json!({"action_id": id})).await;
    assert!(!failed, "{confirmed}");
    assert_eq!(confirmed["result"]["apply_result"]["kind"], "applied");
    assert_eq!(build_name(&server), "Renamed by MCP");
    drop(mcp);
    server.close().await;
}
