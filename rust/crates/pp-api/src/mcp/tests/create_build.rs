//! `propose_create_build` and its `confirm_apply` (GRE-1441 PR 1), through
//! the MCP router with an API key.
use super::reads::{call, session};
use super::*;
use pp_storage::read_model::{
    Credential,
    planning::{self, BuildPlanningPhase},
};

const ORDERS_ON: &str = "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles','{\"orders\":true}') ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value;";

pub(super) async fn orders_on() -> (McpServer, Vec<(String, String)>) {
    let server = McpServer::seeded(ORDERS_ON).await;
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let headers = session(&server, &bearer).await;
    (server, headers)
}

pub(super) async fn json_call(
    server: &McpServer,
    headers: &[(String, String)],
    tool: &str,
    args: Value,
) -> (Value, bool) {
    let (text, error) = call(server, headers, tool, args).await;
    (serde_json::from_str(&text).unwrap(), error)
}

async fn plans(server: &McpServer, headers: &[(String, String)]) -> Value {
    json_call(server, headers, "list_plans", json!({})).await.0["plans"].clone()
}

pub(super) async fn pending(server: &McpServer, headers: &[(String, String)]) -> Vec<Value> {
    let (actions, _) = json_call(server, headers, "list_pending_actions", json!({})).await;
    actions["actions"].as_array().unwrap().clone()
}

/// Proposes and returns the action id, checking that propose wrote nothing.
async fn propose(server: &McpServer, headers: &[(String, String)], args: Value) -> String {
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    let builds = plans(server, headers).await;
    let (proposal, error) = json_call(server, headers, "propose_create_build", args).await;
    assert!(!error, "{proposal}");
    assert_eq!(proposal["status"], "proposed", "{proposal}");
    assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());
    assert_eq!(builds, plans(server, headers).await);
    proposal["action"]["id"].as_str().unwrap().to_owned()
}

pub(super) async fn confirm(
    server: &McpServer,
    headers: &[(String, String)],
    id: &str,
) -> (Value, bool) {
    json_call(server, headers, "confirm_apply", json!({ "action_id": id })).await
}

/// Runs SQL on the database while the server holds it.
pub(super) fn execute(server: &McpServer, sql: &str) {
    let connection =
        rusqlite::Connection::open(server._directory.join("print-partner.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection.execute_batch(sql).unwrap();
}

pub(super) async fn build_planning(server: &McpServer, build: u64) -> planning::Outcome {
    let client = server
        .owner
        .build_planning(policy(), server.owner.source_reads().unwrap())
        .unwrap();
    tokio::task::spawn_blocking(move || {
        client
            .read(
                Credential::Session(Secret::new(FIXTURE_SESSION.into())),
                planning::Request {
                    build: Some(PositiveId::new(build).unwrap()),
                    query: planning::Query::BuildPlanning {
                        draft_id_valid: true,
                    },
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_build_creates_the_build_with_its_brief() {
    let (server, headers) = orders_on().await;
    let before = plans(&server, &headers).await;
    let request = "  Voron 2.4 for Sam.\nBlack and red ABS.  ";
    let id = propose(
        &server,
        &headers,
        json!({ "name": " Voron for Sam ", "request": request, "urls": [] }),
    )
    .await;
    assert_eq!(plans(&server, &headers).await, before);

    let (confirmed, error) = confirm(&server, &headers, &id).await;
    assert!(!error, "{confirmed}");
    assert_eq!(confirmed["ok"], true);
    assert_eq!(confirmed["result"]["name"], "Voron for Sam");
    assert_eq!(confirmed["result"]["idempotent_replay"], false);
    let build = confirmed["result"]["plan_id"].as_u64().unwrap();
    let listed = plans(&server, &headers).await;
    assert_eq!(
        listed.as_array().unwrap().len(),
        before.as_array().unwrap().len() + 1
    );
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|plan| plan["id"] == build)
    );
    assert!(pending(&server, &headers).await.is_empty());

    let planning::Outcome::BuildPlanning(response) = build_planning(&server, build).await else {
        panic!("the brief reads");
    };
    let planning = response.planning.expect("the new Build has a brief");
    assert_eq!(
        planning.brief.special_request,
        "Voron 2.4 for Sam.\nBlack and red ABS."
    );
    assert!(planning.brief.requirements.is_empty());
    assert!(planning.brief.evidence.is_empty());
    assert_eq!(planning.planning_phase, BuildPlanningPhase::Preparing);

    let (workflow, error) = json_call(
        &server,
        &headers,
        "get_build_workflow",
        json!({ "plan_id": build }),
    )
    .await;
    assert!(!error, "{workflow}");
    let database = rusqlite::Connection::open(server._directory.join("print-partner.db")).unwrap();
    let count = |sql: &str| -> i64 {
        database
            .query_row(sql, [build as i64], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM profile_layers WHERE profile_id=?1"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM plan_drafts WHERE profile_id=?1"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM plan_revisions WHERE profile_id=?1"),
        0
    );

    // The new brief admits the brief tool.
    let (brief, error) = json_call(
        &server,
        &headers,
        "propose_update_build_brief",
        json!({ "plan_id": build, "requirements": [{ "key": "color", "value": "black" }] }),
    )
    .await;
    assert!(!error, "{brief}");
    assert_eq!(brief["status"], "proposed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_build_replays_a_confirmed_idempotency_key() {
    let (server, headers) = orders_on().await;
    let args = json!({ "name": "Voron for Sam", "request": "r", "idempotency_key": "sam-1" });
    let first = propose(&server, &headers, args.clone()).await;
    let second = propose(&server, &headers, args).await;
    let (created, _) = confirm(&server, &headers, &first).await;
    let count = plans(&server, &headers).await.as_array().unwrap().len();
    let (replayed, error) = confirm(&server, &headers, &second).await;
    assert!(!error, "{replayed}");
    assert_eq!(replayed["result"]["plan_id"], created["result"]["plan_id"]);
    assert_eq!(replayed["result"]["idempotent_replay"], true);
    assert_eq!(
        plans(&server, &headers).await.as_array().unwrap().len(),
        count
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_build_propose_refuses_with_a_code() {
    let (server, headers) = orders_on().await;
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    let builds = plans(&server, &headers).await;
    let long_key = "k".repeat(201);
    for (args, code) in [
        (json!({ "name": "  ", "request": "r" }), "blank_name"),
        (json!({ "request": "r" }), "blank_name"),
        (json!({ "name": "A", "request": " \n " }), "blank_request"),
        (json!({ "name": "A", "request": 7 }), "blank_request"),
        (
            json!({ "name": "A", "request": "r", "urls": ["https://github.com/x/y"] }),
            "urls_not_supported",
        ),
        (
            json!({ "name": "A", "request": "r", "idempotency_key": 7 }),
            "invalid_idempotency_key",
        ),
        (
            json!({ "name": "A", "request": "r", "idempotency_key": long_key }),
            "invalid_idempotency_key",
        ),
        (
            json!({ "name": "Fixture Build", "request": "r" }),
            "name_taken",
        ),
    ] {
        let (refused, error) =
            json_call(&server, &headers, "propose_create_build", args.clone()).await;
        assert!(error, "{args}: {refused}");
        assert_eq!(refused["code"], code, "{args}: {refused}");
        assert!(refused["error"].is_string(), "{refused}");
    }
    execute(
        &server,
        "UPDATE app_settings SET value='{\"orders\":false}' WHERE key='feature_toggles';",
    );
    let (refused, error) = json_call(
        &server,
        &headers,
        "propose_create_build",
        json!({ "name": "A", "request": "r" }),
    )
    .await;
    assert!(error);
    assert_eq!(refused["code"], "orders_module_off");
    assert!(pending(&server, &headers).await.is_empty());
    execute(
        &server,
        "UPDATE app_settings SET value='{\"orders\":true}' WHERE key='feature_toggles';",
    );
    assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());
    assert_eq!(builds, plans(&server, &headers).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_build_confirm_refusals_write_nothing_and_keep_the_proposal() {
    let (server, headers) = orders_on().await;
    let database = server._directory.join("print-partner.db");
    let taken = propose(
        &server,
        &headers,
        json!({ "name": "Twice", "request": "r" }),
    )
    .await;
    let orders = propose(
        &server,
        &headers,
        json!({ "name": "Later", "request": "r" }),
    )
    .await;
    let (_, error) = confirm(
        &server,
        &headers,
        &propose(
            &server,
            &headers,
            json!({ "name": "Twice", "request": "r" }),
        )
        .await,
    )
    .await;
    assert!(!error);

    let builds = plans(&server, &headers).await;
    execute(
        &server,
        "UPDATE app_settings SET value='{\"orders\":false}' WHERE key='feature_toggles';",
    );
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    let (refused, error) = confirm(&server, &headers, &orders).await;
    assert!(error, "{refused}");
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["code"], "orders_module_off");
    assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());

    execute(
        &server,
        "UPDATE app_settings SET value='{\"orders\":true}' WHERE key='feature_toggles';",
    );
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    let (refused, error) = confirm(&server, &headers, &taken).await;
    assert!(error, "{refused}");
    assert_eq!(
        refused,
        json!({ "ok": false, "code": "name_taken", "detail": refused["detail"] })
    );
    assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());
    assert_eq!(plans(&server, &headers).await, builds);

    let kept: Vec<_> = pending(&server, &headers)
        .await
        .into_iter()
        .map(|action| action["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(kept.contains(&taken) && kept.contains(&orders), "{kept:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_create_build_without_a_key_is_refused_and_proposes_nothing() {
    let (server, headers) = orders_on().await;
    let session_id = headers
        .iter()
        .find(|(name, _)| name == "mcp-session-id")
        .map(|(_, value)| value.as_str())
        .unwrap();
    let request = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": { "name": "propose_create_build", "arguments": { "name": "A", "request": "r" } } });
    let unkeyed = [
        ("mcp-session-id", session_id),
        ("mcp-protocol-version", "2025-06-18"),
        // Same-origin, so only the key check can refuse.
        ("origin", SELF),
    ];
    let (status, _, _) = server.post("/api/v1/mcp", &unkeyed, &request).await;
    assert_eq!(status, 401);
    assert!(pending(&server, &headers).await.is_empty());
}
