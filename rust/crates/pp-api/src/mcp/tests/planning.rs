use super::reads::{call, session};
use super::*;
const SEED: &str =
    include_str!("../../../../../../fixtures/parity/mcp-proposals/build-planning/seed.sql");
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_replays_node_text_and_confirmation_identity() {
    let (server, headers) = keyed(SEED).await;
    let documents: Value = serde_json::from_str(include_str!(
        "../../../../../../fixtures/parity/mcp-proposals/build-planning/documents.json"
    ))
    .unwrap();
    for source in documents["sources"].as_array().unwrap() {
        let root = server
            ._directory
            .join("assistant-domain/sources")
            .join(source["source_name"].as_str().unwrap());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("compatibility.yaml"), source.to_string()).unwrap();
    }
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../../../fixtures/parity/mcp-proposals/build-planning/addon-brief.json"
    ))
    .unwrap();
    let mut ids = Vec::<(String, String)>::new();
    for step in golden["steps"].as_array().unwrap() {
        let request = &step["request"]["params"];
        let mut arguments = request["arguments"].to_string();
        for (placeholder, raw) in &ids {
            arguments = arguments.replace(placeholder, raw);
        }
        let (mut text, is_error) = call(
            &server,
            &headers,
            request["name"].as_str().unwrap(),
            serde_json::from_str(&arguments).unwrap(),
        )
        .await;
        let parsed: Value = serde_json::from_str(&text).unwrap();
        if let Some(id) = parsed["action"]["id"].as_str() {
            ids.push((format!("<action-id-{}>", ids.len() + 1), id.into()));
        }
        for (placeholder, raw) in &ids {
            text = text.replace(raw, placeholder);
        }
        text = text.replace(
            &server._directory.to_string_lossy().to_string(),
            "<data-dir>",
        );
        assert_eq!(
            text,
            step["response"]["content"][0]["text"].as_str().unwrap(),
            "{request}"
        );
        assert_eq!(is_error, step["response"]["isError"] == true, "{request}");
    }
    let (pending, failed) = call(&server, &headers, "list_pending_actions", json!({})).await;
    assert!(!failed);
    assert_eq!(
        serde_json::from_str::<Value>(&pending).unwrap(),
        json!({"actions":[]})
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_confirmation_preserves_failed_proposals_and_account_isolation() {
    let (server, headers) = keyed(SEED).await;
    let (text, error) = call(
        &server,
        &headers,
        "propose_update_build_brief",
        json!({"plan_id":1,"requirements":[{"key":"x","value":"y","status":"invalid"}]}),
    )
    .await;
    assert!(!error);
    let proposal: Value = serde_json::from_str(&text).unwrap();
    let id = proposal["action"]["id"].as_str().unwrap();
    let (text, error) = call(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(error);
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"ok":false,"detail":"Invalid requirement"})
    );
    let (pending, _) = call(&server, &headers, "list_pending_actions", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&pending).unwrap()["actions"][0]["id"],
        id
    );
    for build in [2, 999] {
        for (tool, message) in [
            ("add_addon", "Plan not found"),
            (
                "propose_update_build_brief",
                "Build planning brief not found",
            ),
        ] {
            assert_eq!(
                call(
                    &server,
                    &headers,
                    tool,
                    json!({"plan_id":build,"source_name":"Secret"})
                )
                .await,
                (json!({"error":message}).to_string(), true)
            );
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_addon_refuses_an_isolated_source_outside_its_workspace() {
    let outside = tempfile::tempdir().unwrap();
    let seed = SEED.replace("<data-dir>/repos/2", outside.path().to_str().unwrap());
    let (server, headers) = keyed(&seed).await;
    let expected = json!({"error":"Source \"Addon\" exists but is not synced. Ask the user to sync it before adding as an addon.","synced":false});
    assert_eq!(
        call(
            &server,
            &headers,
            "add_addon",
            json!({"plan_id":1,"source_name":"Addon"})
        )
        .await,
        (expected.to_string(), true)
    );
}

async fn keyed(seed: &str) -> (McpServer, Vec<(String, String)>) {
    let instance = McpServer::seeded(seed).await;
    std::fs::create_dir_all(instance._directory.join("repos/2")).unwrap();
    let credential = format!("Bearer {}", instance.create_key().await.1);
    let headers = session(&instance, &credential).await;
    (instance, headers)
}

async fn propose(
    server: &McpServer,
    headers: &[(String, String)],
    tool: &str,
    args: Value,
) -> String {
    let before = pp_testkit::mcp_planning::state(&server._directory.join("print-partner.db"));
    let (text, error) = call(server, headers, tool, args).await;
    assert!(!error, "{text}");
    assert_eq!(
        before,
        pp_testkit::mcp_planning::state(&server._directory.join("print-partner.db"))
    );
    serde_json::from_str::<Value>(&text).unwrap()["action"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn assert_refused(server: &McpServer, headers: &[(String, String)], id: &str) {
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database);
    let (text, error) = call(server, headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(error, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["ok"], false);
    assert_eq!(before, pp_testkit::mcp_planning::state(&database));
    let (pending, _) = call(server, headers, "list_pending_actions", json!({})).await;
    assert!(
        serde_json::from_str::<Value>(&pending).unwrap()["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == id)
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_addon_revalidates_source_at_confirmation() {
    let outside = tempfile::tempdir().unwrap();
    for sql in [
        "UPDATE projects SET last_synced_at=NULL WHERE id=2".to_owned(),
        "DELETE FROM projects WHERE id=2".to_owned(),
        format!(
            "UPDATE projects SET local_path='{}' WHERE id=2",
            outside.path().to_str().unwrap().replace('\'', "''")
        ),
    ] {
        let (server, headers) = keyed(SEED).await;
        let database = server._directory.join("print-partner.db");
        pp_testkit::mcp_planning::save(
            &database,
            "kit_manifest_1",
            &json!({"exclude":["keep.stl"]}),
        );
        let id = propose(
            &server,
            &headers,
            "add_addon",
            json!({"plan_id":1,"source_name":"Addon","suggested_excludes":["new.stl"]}),
        )
        .await;
        pp_testkit::mcp_planning::edit(&database, &sql);
        assert_refused(&server, &headers, &id).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_brief_confirmation_validates_current_evidence_and_enums() {
    for case in ["valid", "removed", "slot", "enum", "finding"] {
        let (server, headers) = keyed(SEED).await;
        let database = server._directory.join("print-partner.db");
        let mut brief = pp_testkit::mcp_planning::setting(&database, "build_planning.v1.1");
        brief["draft_id"] = json!(42);
        brief["evidence"] = json!([{"id":"e1","url":"https://example.test/source","normalized_url":"https://example.test/source","kind":"mod"}]);
        pp_testkit::mcp_planning::save(&database, "build_planning.v1.1", &brief);
        let mut args = json!({"plan_id":1,"contributions":[{"id":"c1","evidence_id":"e1","slot":"toolhead","responsibility":"printable_parts","confidence":"high","evidence_text":"Confirmed source","path_scopes":["parts/"]}],"compatibility_findings":[{"id":"f1","subject":"Toolhead","detail":"Confirmed fit","status":"satisfied","evidence_ids":["e1"]}]});
        if case == "slot" {
            args["contributions"][0]["slot"] = json!("bad-slot");
        }
        if case == "enum" {
            args["contributions"][0]["responsibility"] = json!("unknown");
        }
        if case == "finding" {
            args["compatibility_findings"][0]["status"] = json!("unknown");
        }
        let id = propose(&server, &headers, "propose_update_build_brief", args).await;
        if case == "removed" {
            brief["evidence"] = json!([]);
            pp_testkit::mcp_planning::save(&database, "build_planning.v1.1", &brief);
        }
        if case != "valid" {
            assert_refused(&server, &headers, &id).await;
            continue;
        }
        let (text, error) = call(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
        assert!(!error, "{text}");
        let saved = pp_testkit::mcp_planning::setting(&database, "build_planning.v1.1");
        assert!(saved.get("draft_id").is_none());
        assert_eq!(saved["contributions"][0]["evidence_id"], "e1");
        assert_eq!(saved["compatibility_findings"][0]["status"], "satisfied");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_addon_uses_the_data_directory_catalog_slot() {
    let (server, headers) = keyed(SEED).await;
    let path = server._directory.join("kit-catalog.yaml");
    std::fs::write(&path, json!({"addon_categories":{"toolhead":{"rule":"pick_one","replaces_slot":"custom_head","sources":[{"name":"Addon"},{"name":"Base"}]}}}).to_string()).unwrap();
    let (text, error) = call(
        &server,
        &headers,
        "add_addon",
        json!({"plan_id":1,"source_name":"Addon"}),
    )
    .await;
    assert!(!error, "{text}");
    assert!(
        serde_json::from_str::<Value>(&text).unwrap()["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["slot"] == "custom_head")
    );
    std::fs::remove_file(path).unwrap();
    let (text, error) = call(
        &server,
        &headers,
        "add_addon",
        json!({"plan_id":1,"source_name":"Addon"}),
    )
    .await;
    assert!(!error, "{text}");
    assert!(
        serde_json::from_str::<Value>(&text).unwrap()["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["slot"] != "custom_head")
    );
}
