//! GRE-441 read tools over streamable HTTP: `list_plans`,
//! `get_plan_decisions` and `list_plan_snapshots` answer Node's text for the
//! caller's tenant only, and refuse a call without a key.

use super::super::reads::TOOLS;
use super::*;

/// Rows for Build 1 (tenant `default`) and Build 2 (tenant `foreign`).
const SEED: &str = r#"
INSERT INTO plan_decisions
  (id,tenant_id,profile_id,created_at,actor,kind,action_type,params_json,label,summary,rationale,result_json)
VALUES
  (1,'default',1,'2026-11-01T10:00:00.000Z','assistant','applied_action','propose_rebuild_plan','{"z":1,"a":[2,"x"]}','Rebuild','Rebuilt plan',NULL,'{"ok":true}'),
  (2,'default',1,'2026-11-02T10:00:00.000Z','user','dismissed_action',NULL,'not json','Skip','','too slow',''),
  (3,'default',1,'2026-11-03T10:00:00.000Z','user','user_note',NULL,'[1]','Note','Keep PLA',NULL,NULL),
  (4,'foreign',2,'2026-11-04T10:00:00.000Z','user','user_note',NULL,'{}','Foreign','secret',NULL,NULL);
INSERT INTO plan_snapshots (id,tenant_id,profile_id,name,created_at,source,payload_json) VALUES
  (1,'default',1,'Before PETG','2026-11-02T09:00:00.000Z','user','{}'),
  (2,'default',1,'Auto','2026-11-01T09:00:00.000Z','auto','{"layers":[]}'),
  (3,'foreign',2,'Foreign','2026-11-01T08:00:00.000Z','user','{}');
"#;

/// Node's `JSON.stringify` of each seeded `default` decision, oldest first.
const DECISIONS: [&str; 3] = [
    r#"{"id":1,"plan_id":1,"created_at":"2026-11-01T10:00:00.000Z","actor":"assistant","kind":"applied_action","action_type":"propose_rebuild_plan","params":{"z":1,"a":[2,"x"]},"label":"Rebuild","summary":"Rebuilt plan","rationale":null,"result":{"ok":true}}"#,
    r#"{"id":2,"plan_id":1,"created_at":"2026-11-02T10:00:00.000Z","actor":"user","kind":"dismissed_action","action_type":null,"params":{},"label":"Skip","summary":"","rationale":"too slow","result":null}"#,
    r#"{"id":3,"plan_id":1,"created_at":"2026-11-03T10:00:00.000Z","actor":"user","kind":"user_note","action_type":null,"params":{},"label":"Note","summary":"Keep PLA","rationale":null,"result":null}"#,
];

/// An initialized session: the headers each later request sends.
pub(super) async fn session(server: &McpServer, bearer: &str) -> Vec<(String, String)> {
    let auth = [("authorization", bearer)];
    let (status, body, session) = server.post("/api/v1/mcp", &auth, &initialize()).await;
    assert_eq!(status, 200, "{body}");
    let headers = vec![
        ("authorization".to_owned(), bearer.to_owned()),
        ("mcp-session-id".to_owned(), session.unwrap()),
        ("mcp-protocol-version".to_owned(), "2025-06-18".to_owned()),
    ];
    let notified = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let (status, body, _) = server
        .post("/api/v1/mcp", &pairs(&headers), &notified)
        .await;
    assert_eq!(status, 202, "{body}");
    headers
}

fn pairs(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// `tools/call`: the tool's text and its `isError` flag.
pub(super) async fn call(
    server: &McpServer,
    headers: &[(String, String)],
    name: &str,
    arguments: Value,
) -> (String, bool) {
    let request = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                          "params": { "name": name, "arguments": arguments } });
    let (status, body, _) = server.post("/api/v1/mcp", &pairs(headers), &request).await;
    assert_eq!(status, 200, "{name}: {body}");
    let result = &rpc(&body)["result"];
    let text = result["content"][0]["text"].as_str().unwrap().to_owned();
    (text, result["isError"] == true)
}

/// `call`, with the text parsed as JSON.
pub(super) async fn tool(
    server: &McpServer,
    headers: &[(String, String)],
    name: &str,
    arguments: Value,
) -> (Value, bool) {
    let (text, failed) = call(server, headers, name, arguments).await;
    (serde_json::from_str(&text).unwrap(), failed)
}

/// Confirms the action a successful propose answered.
pub(super) async fn confirm(
    server: &McpServer,
    headers: &[(String, String)],
    proposed: &Value,
) -> (Value, bool) {
    assert_eq!(proposed["status"], "proposed", "{proposed}");
    let id = &proposed["action"]["id"];
    tool(server, headers, "confirm_apply", json!({ "action_id": id })).await
}

/// A same-origin `tools/call` without an API key gets 401 and no session.
pub(super) async fn assert_needs_key(server: &McpServer, name: &str, arguments: Value) {
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": { "name": name, "arguments": arguments } });
    let (status, body, session) = server
        .post("/api/v1/mcp", &[("origin", SELF)], &request)
        .await;
    assert_eq!(status, 401, "{body}");
    assert!(session.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_62_mcp_domain_read_tools_answer_node_text_for_the_callers_tenant() {
    let server = McpServer::seeded(SEED).await;
    let (_, key) = server.create_key().await;
    let headers = session(&server, &format!("Bearer {key}")).await;

    // Build 2 belongs to `foreign` and is not listed.
    let plans = call(&server, &headers, "list_plans", json!({})).await;
    assert_eq!(
        plans,
        (
            r#"{"plans":[{"id":1,"name":"Fixture Build","part_count":3,"build_stale":false}]}"#
                .to_owned(),
            false
        )
    );

    let all = format!(r#"{{"plan_id":1,"decisions":[{}]}}"#, DECISIONS.join(","));
    let latest_two = format!(
        r#"{{"plan_id":1,"decisions":[{}]}}"#,
        DECISIONS[1..].join(",")
    );
    let latest = format!(r#"{{"plan_id":1,"decisions":[{}]}}"#, DECISIONS[2]);
    for (arguments, want) in [
        (json!({ "plan_id": 1 }), &all),
        (json!({ "plan_id": "1", "limit": 40 }), &all),
        (json!({ "plan_id": 1.7, "limit": 2 }), &latest_two),
        (json!({ "plan_id": 1, "limit": "2.9" }), &latest_two),
        (json!({ "plan_id": 1, "limit": 0 }), &latest),
        (json!({ "plan_id": 1, "limit": -5 }), &latest),
    ] {
        let got = call(&server, &headers, "get_plan_decisions", arguments.clone()).await;
        assert_eq!(got, (want.clone(), false), "{arguments}");
    }

    let snapshots = call(
        &server,
        &headers,
        "list_plan_snapshots",
        json!({ "plan_id": 1 }),
    )
    .await;
    assert_eq!(
        snapshots,
        (
            concat!(
                r#"{"plan_id":1,"snapshots":["#,
                r#"{"id":2,"plan_id":1,"name":"Auto","created_at":"2026-11-01T09:00:00.000Z","source":"auto"},"#,
                r#"{"id":1,"plan_id":1,"name":"Before PETG","created_at":"2026-11-02T09:00:00.000Z","source":"user"}]}"#
            )
            .to_owned(),
            false
        )
    );

    // Another tenant's Build, a missing one and an unreadable id all read as
    // no owned Build, as Node's `resolvePlanId` answers them.
    for tool in ["get_plan_decisions", "list_plan_snapshots"] {
        for arguments in [
            json!({ "plan_id": 2 }),
            json!({ "plan_id": 99 }),
            json!({ "plan_id": 0 }),
            json!({ "plan_id": "x" }),
            json!({}),
        ] {
            let got = call(&server, &headers, tool, arguments.clone()).await;
            assert_eq!(
                got,
                (r#"{"error":"plan_id required"}"#.to_owned(), true),
                "{tool} {arguments}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_62_mcp_domain_read_tools_refuse_a_call_without_a_key() {
    let server = McpServer::seeded(SEED).await;
    let (_, key) = server.create_key().await;
    let headers = session(&server, &format!("Bearer {key}")).await;
    let keyless: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(name, _)| name != "authorization")
        .chain([("origin".to_owned(), SELF.to_owned())])
        .collect();
    for name in TOOLS {
        let request = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                              "params": { "name": name, "arguments": { "plan_id": 1 } } });
        let (status, body, _) = server.post("/api/v1/mcp", &pairs(&keyless), &request).await;
        assert_eq!(status, 401, "{name}: {body}");
        assert!(
            !body.contains("Fixture Build") && !body.contains("Rebuild"),
            "{name}: {body}"
        );
    }
}
