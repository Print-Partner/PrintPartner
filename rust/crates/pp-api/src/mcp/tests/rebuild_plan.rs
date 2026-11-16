//! `propose_rebuild_plan` as an MCP client sees it. Build 3 has no layers,
//! no draft and no Accepted Plan. Sources 4 (`Base`) and 5 (`Addon`) are
//! synced local Sources; the brief gives them their roles.
use super::reads::{assert_needs_key, confirm, session, tool};
use super::*;
use pp_testkit::rebuild_fixture::{
    SEED, brief, evidence, resolved, roles, two_bases, write_sources,
};
use std::path::{Path, PathBuf};

const TOOL: &str = "propose_rebuild_plan";
/// A server whose Sources hold printable files, and Build 3's `brief`.
async fn fixture(brief: Option<Value>, files: bool) -> (McpServer, Vec<(String, String)>, PathBuf) {
    let server = McpServer::seeded(SEED).await;
    write_sources(&server._directory, files).unwrap();
    let database = server._directory.join("print-partner.db");
    if let Some(brief) = brief {
        pp_testkit::mcp_planning::save(&database, "build_planning.v1.3", &brief).unwrap();
    }
    let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    (server, headers, database)
}
async fn rebuild(server: &McpServer, headers: &[(String, String)], args: Value) -> (Value, bool) {
    let (proposed, _) = tool(server, headers, TOOL, args).await;
    confirm(server, headers, &proposed).await
}
/// Layers, drafts and the stored brief.
type Snapshot = (Vec<(String, i64)>, Vec<(i64, String)>, Value);
fn snapshot(database: &Path) -> Snapshot {
    (
        pp_testkit::mcp_planning::layers(database, 3).unwrap(),
        pp_testkit::mcp_planning::drafts(database, 3).unwrap(),
        pp_testkit::mcp_planning::setting(database, "build_planning.v1.3").unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_rebuild_sets_layers_from_roles_and_selects_a_new_draft() {
    let (server, headers, database) = fixture(Some(brief(roles(), json!({}))), true).await;
    let before = snapshot(&database);
    let (proposed, failed) = tool(
        &server,
        &headers,
        TOOL,
        json!({"plan_id":3,"idempotency_key":"k"}),
    )
    .await;
    assert!(!failed, "{proposed}");
    assert_eq!(proposed["status"], "proposed");
    assert_eq!(snapshot(&database), before, "propose wrote");

    let (confirmed, failed) = confirm(&server, &headers, &proposed).await;
    assert!(!failed, "{confirmed}");
    let result = &confirmed["result"];
    assert_eq!(result["recompute"], "created");
    assert_eq!(result["snapshot_digest"].as_str().unwrap().len(), 64);
    assert_eq!(
        result["sources"],
        json!([
            {"source_id":4,"role":"structural_base","revision":"rev4"},
            {"source_id":5,"role":"addon","revision":"rev5"}
        ])
    );
    assert_eq!(
        pp_testkit::mcp_planning::layers(&database, 3).unwrap(),
        [("base".to_owned(), 4), ("addon".to_owned(), 5)]
    );
    let draft_id = result["draft_id"].as_i64().unwrap();
    assert_eq!(
        pp_testkit::mcp_planning::drafts(&database, 3).unwrap(),
        [(draft_id, "open".to_owned())]
    );
    let stored = pp_testkit::mcp_planning::setting(&database, "build_planning.v1.3").unwrap();
    assert_eq!(stored["draft_id"], draft_id);
    assert_eq!(stored["managed_source_ids"], json!([4, 5]));
    assert_eq!(
        stored["draft_source_revisions"],
        json!({"4":"rev4","5":"rev5"})
    );
    let (read, _) = tool(&server, &headers, "get_plan_draft", json!({"plan_id":3})).await;
    assert_eq!(read["draft"]["draft_id"], draft_id, "{read}");
    assert_eq!(
        read["draft"]["parts"].as_array().unwrap().len(),
        3,
        "{read}"
    );

    // The same key and inputs answer the draft the first rebuild made.
    let (again, failed) = rebuild(
        &server,
        &headers,
        json!({"plan_id":3,"idempotency_key":"k"}),
    )
    .await;
    assert!(!failed, "{again}");
    assert_eq!(again["result"]["recompute"], "existing");
    assert_eq!(again["result"]["draft_id"], draft_id);
    assert_eq!(
        pp_testkit::mcp_planning::drafts(&database, 3)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_rebuild_refusals_leave_layers_drafts_and_brief_unchanged() {
    for (brief, files, code) in [
        (
            brief(json!([evidence("addon", 5, "addon")]), json!({})),
            true,
            "no_structural_base",
        ),
        (
            brief(two_bases(), json!({})),
            true,
            "multiple_structural_bases",
        ),
        (brief(roles(), json!({})), false, "no_stls"),
        (
            brief(roles(), resolved()),
            true,
            "source_choices_unsupported",
        ),
    ] {
        let (server, headers, database) = fixture(Some(brief), files).await;
        let before = snapshot(&database);
        let (refused, failed) = rebuild(&server, &headers, json!({"plan_id":3})).await;
        assert!(failed, "{code}: {refused}");
        assert_eq!(refused["code"], code, "{refused}");
        assert_eq!(snapshot(&database), before, "{code} wrote");
    }
    // Without a brief, propose refuses and nothing becomes pending.
    let (server, headers, _) = fixture(None, true).await;
    let (refused, failed) = tool(&server, &headers, TOOL, json!({"plan_id":3})).await;
    assert!(failed);
    assert_eq!(refused["code"], "no_planning_brief");
    let (listed, _) = tool(&server, &headers, "list_pending_actions", json!({})).await;
    assert_eq!(listed["actions"], json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_rebuild_needs_an_api_key() {
    let (server, _, database) = fixture(Some(brief(roles(), json!({}))), true).await;
    let before = snapshot(&database);
    assert_needs_key(&server, TOOL, json!({"plan_id":3})).await;
    assert_eq!(snapshot(&database), before);
}
