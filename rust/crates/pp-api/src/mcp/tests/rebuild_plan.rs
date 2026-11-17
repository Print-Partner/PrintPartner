//! `propose_rebuild_plan` as an MCP client sees it. Build 3 has no layers,
//! no draft and no Accepted Plan. Sources 4 (`Base`) and 5 (`Addon`) are
//! synced local Sources; the brief gives them their roles.
use super::reads::{assert_needs_key, confirm, session, tool};
use super::*;
use pp_testkit::rebuild_fixture::{
    SEED, brief, evidence, roles, scoped_choices, two_bases, unresolved_choices, write_sources,
    write_stl,
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
        (unresolved_choices(), true, "unresolved_source_choices"),
    ] {
        let (server, headers, database) = fixture(Some(brief), files).await;
        let before = snapshot(&database);
        let (refused, failed) = rebuild(&server, &headers, json!({"plan_id":3})).await;
        assert!(failed, "{code}: {refused}");
        assert_eq!(refused["code"], code, "{refused}");
        if code == "unresolved_source_choices" {
            assert_eq!(refused["groups"], json!(["group-1"]), "{refused}");
        }
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
async fn mcp_rebuild_keeps_scoped_addon_parts_and_drops_the_superseded_file() {
    let (server, headers, database) = fixture(Some(scoped_choices()), true).await;
    let root = database.parent().unwrap();
    write_stl(root, 4, "superseded.stl").unwrap();
    write_stl(root, 5, "mods/a/plate.stl").unwrap();
    write_stl(root, 5, "mods/b/other.stl").unwrap();
    let (confirmed, failed) = rebuild(&server, &headers, json!({"plan_id": 3})).await;
    assert!(!failed, "{confirmed}");
    assert_eq!(confirmed["result"]["recompute"], "created");
    // The rows confirm stored. `get_plan_draft` drops the selection when
    // hydration's scanned differences differ from this fixture's group.
    let draft_id = confirmed["result"]["draft_id"].as_i64().unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    let mut query = db
        .prepare(
            "SELECT source_layer, relative_path FROM plan_draft_parts WHERE draft_id=?1 ORDER BY source_layer, relative_path",
        )
        .unwrap();
    let parts: Vec<(String, String)> = query
        .query_map([draft_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        parts,
        [
            ("addon:Addon".into(), "mods/a/plate.stl".into()),
            ("base:Base".into(), "bracket.stl".into()),
            ("base:Base".into(), "gear.stl".into()),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_rebuild_removes_a_managed_layer_and_keeps_one_attached_in_the_ui() {
    let (server, headers, database) = fixture(Some(brief(roles(), json!({}))), true).await;
    let (confirmed, failed) = rebuild(
        &server,
        &headers,
        json!({"plan_id": 3, "idempotency_key": "first"}),
    )
    .await;
    assert!(!failed, "{confirmed}");
    let root = database.parent().unwrap();
    write_stl(root, 6, "ui.stl").unwrap();
    let root_text = root.to_str().unwrap().replace('\'', "''");
    pp_testkit::mcp_planning::edit(
        &database,
        &format!(
            "INSERT INTO projects(id,tenant_id,name,url,source_type,branch,local_path,last_synced_at,last_commit_sha) VALUES(6,'default','Extra','','local','main','{root_text}/repos/6','2026-11-01T00:00:00.000Z','rev6');
             INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES('default',3,2,'addon',6);"
        ),
    )
    .unwrap();
    let mut stored = pp_testkit::mcp_planning::setting(&database, "build_planning.v1.3").unwrap();
    stored["evidence"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["id"] == "addon")
        .unwrap()["source_role"] = json!("evidence");
    pp_testkit::mcp_planning::save(&database, "build_planning.v1.3", &stored).unwrap();
    assert_eq!(
        pp_testkit::mcp_planning::layers(&database, 3).unwrap(),
        [("base".into(), 4), ("addon".into(), 5), ("addon".into(), 6)]
    );
    let (confirmed, failed) = rebuild(
        &server,
        &headers,
        json!({"plan_id": 3, "idempotency_key": "second"}),
    )
    .await;
    assert!(!failed, "{confirmed}");
    assert_eq!(
        confirmed["result"]["sources"],
        json!([{"source_id": 4, "role": "structural_base", "revision": "rev4"}])
    );
    assert_eq!(
        pp_testkit::mcp_planning::layers(&database, 3).unwrap(),
        [("base".into(), 4), ("addon".into(), 6)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_rebuild_needs_an_api_key() {
    let (server, _, database) = fixture(Some(brief(roles(), json!({}))), true).await;
    let before = snapshot(&database);
    assert_needs_key(&server, TOOL, json!({"plan_id":3})).await;
    assert_eq!(snapshot(&database), before);
}
