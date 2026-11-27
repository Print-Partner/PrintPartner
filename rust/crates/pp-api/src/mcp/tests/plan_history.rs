use super::reads::{session, tool};
use super::*;
use crate::mcp::plan_history::TOOLS;

async fn fixture(seed: &str) -> (McpServer, Vec<(String, String)>) {
    let server = McpServer::seeded(seed).await;
    std::fs::create_dir_all(server._directory.join("repos/1/revisions/fixture")).unwrap();
    let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    (server, headers)
}

fn arguments(name: &str, plan: i64) -> Value {
    match name {
        "compare_plans" => json!({"plan_a_id": 1, "plan_b_id": plan}),
        "search_plan_parts" => json!({"plan_id": plan, "query": "bracket"}),
        _ => json!({"plan_id": plan}),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_plan_history_only_reads_the_callers_builds() {
    let (server, headers) = fixture("").await;
    let headers_ref = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect::<Vec<_>>();
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let (status, body, _) = server.post("/api/v1/mcp", &headers_ref, &list).await;
    assert_eq!(status, 200, "{body}");
    let result = rpc(&body);
    for name in TOOLS {
        let listed = result["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("{name} not listed"));
        if matches!(name, "get_plan_snapshot" | "search_plan_parts") {
            assert_eq!(
                listed["inputSchema"]["properties"]["revision_id"]["type"],
                "integer"
            );
        }
        assert_eq!(
            server
                .unkeyed_call_status(&headers, name, arguments(name, 1))
                .await,
            401
        );
        let foreign = tool(&server, &headers, name, arguments(name, 2)).await;
        let absent = tool(&server, &headers, name, arguments(name, 9)).await;
        assert_eq!(foreign, absent, "{name}");
        assert!(foreign.1, "{name}");
        assert_eq!(foreign.0["code"], "plan_not_found", "{name}: {}", foreign.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_plan_history_preserves_current_part_and_draft_identity() {
    let (server, headers) = fixture(
        "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Empty Plan');
         INSERT INTO app_settings(tenant_id,key,value)
            VALUES('default','kit_manifest_3','{\"name\":\"Fixture Kit\",\"selections\":{\"size\":\"small\"}}');
         UPDATE build_profiles SET config_modified_at='2030-01-01T00:00:00.000Z' WHERE id=1",
    ).await;
    let call = |name, args| tool(&server, &headers, name, args);
    let (snapshot, failed) = call("get_plan_snapshot", json!({"plan_id":1})).await;
    assert!(!failed, "{snapshot}");
    assert_eq!(snapshot["basis"]["revision_id"], 1);
    assert_eq!(snapshot["build_stale"], true);
    assert_eq!(
        snapshot["draft"],
        json!({"draft_id":2, "base_revision_id":1, "stale":false})
    );
    let (parts, failed) = call("search_plan_parts", json!({"plan_id":1,"query":"BRACKET"})).await;
    assert!(!failed, "{parts}");
    let part = &parts["parts"][0];
    assert_eq!(
        (part["part_id"].as_i64(), part["revision_part_id"].as_i64()),
        (Some(1), Some(1))
    );
    assert_eq!(part["required_unit_tokens"].as_array().unwrap().len(), 3);
    let (review, failed) = call("get_plan_review", json!({"plan_id":1})).await;
    assert!(!failed, "{review}");
    assert_eq!(review["basis"]["revision_id"], 1);
    assert_eq!(review["totals"]["included_parts"], 2);
    let (options, failed) = call("get_plan_option_groups", json!({"plan_id":3})).await;
    assert!(!failed, "{options}");
    assert_eq!(
        options,
        json!({"plan_id":3,"selections":{"size":"small"},"option_groups":{},"sources":[]})
    );
    let (diff, failed) = call("compare_plans", json!({"plan_a_id":1,"plan_b_id":3})).await;
    assert!(!failed, "{diff}");
    assert_eq!(diff["plan_b"]["basis"]["kind"], "empty");
    assert_eq!(diff["kit_selections"]["only_b"]["size"], "small");
    assert_eq!(diff["parts"]["only_a"]["sample"][0], *part);
    let (missing, failed) = call("get_plan_snapshot", json!({"plan_id":1,"revision_id":42})).await;
    assert!(failed, "{missing}");
    assert_eq!(missing["code"], "not_found");

    let db = rusqlite::Connection::open(server._directory.join("print-partner.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute("UPDATE parts SET notes='compatibility edit' WHERE id=1", [])
        .unwrap();
    let (stale, failed) = call("search_plan_parts", json!({"plan_id":1,"query":"bracket"})).await;
    assert!(failed, "{stale}");
    assert_eq!(stale["code"], "stale_plan");
}
