use super::reads::{call, session};
use super::*;
const SEED: &str =
    include_str!("../../../../../../fixtures/parity/mcp-proposals/build-planning/seed.sql");
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_replays_node_text_and_confirmation_identity() {
    let (server, headers) = keyed(SEED).await;
    std::fs::create_dir_all(server._directory.join("repos/2")).unwrap();
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
    for (tool, message) in [
        ("add_addon", "plan_id and source_name required"),
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
                json!({"plan_id":2,"source_name":"Secret"})
            )
            .await,
            (json!({"error":message}).to_string(), true)
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_addon_refuses_an_isolated_source_outside_its_workspace() {
    let seed = SEED.replace("<data-dir>/repos/2", "/tmp/outside-source");
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
    let credential = format!("Bearer {}", instance.create_key().await.1);
    let headers = session(&instance, &credential).await;
    (instance, headers)
}
