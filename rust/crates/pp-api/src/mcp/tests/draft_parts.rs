//! `propose_edit_plan_draft_parts` as an MCP client sees it. The fixture's
//! Build 1 has Accepted Plan version 1, consumed draft 1 (Parts 1 to 3) and
//! open draft 2 (Parts 4 to 6). Tests check codes and data, never messages.
use super::reads::{call, session};
use super::*;

const TOOL: &str = "propose_edit_plan_draft_parts";

async fn fixture() -> (McpServer, Vec<(String, String)>, std::path::PathBuf) {
    let server = McpServer::seeded("").await;
    let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    let database = server._directory.join("print-partner.db");
    (server, headers, database)
}
async fn tool(
    server: &McpServer,
    headers: &[(String, String)],
    name: &str,
    args: Value,
) -> (Value, bool) {
    let (text, failed) = call(server, headers, name, args).await;
    (serde_json::from_str(&text).unwrap(), failed)
}
async fn propose(server: &McpServer, headers: &[(String, String)], parts: Value) -> String {
    let (proposed, failed) = tool(
        server,
        headers,
        TOOL,
        json!({"plan_id":1,"draft_id":2,"parts":parts}),
    )
    .await;
    assert!(!failed, "{proposed}");
    proposed["action"]["id"].as_str().unwrap().to_owned()
}
async fn pending(server: &McpServer, headers: &[(String, String)]) -> Vec<String> {
    let (listed, _) = tool(server, headers, "list_pending_actions", json!({})).await;
    listed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| action["id"].as_str().unwrap().to_owned())
        .collect()
}
fn parts(database: &std::path::Path) -> Vec<(i64, bool, Option<i64>)> {
    pp_testkit::mcp_planning::draft_parts(database, 2).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_part_edit_proposes_without_writing_and_confirms_through_the_plan_edit() {
    let (server, headers, database) = fixture().await;
    let before = parts(&database);
    let (remaining, _) = tool(&server, &headers, "get_remaining", json!({"plan_id":1})).await;
    let (proposed, failed) = tool(
        &server,
        &headers,
        TOOL,
        json!({"plan_id":1,"draft_id":2,"parts":[{"part_id":4,"included":false},{"part_id":5,"quantity_override":3}]}),
    )
    .await;
    assert!(!failed, "{proposed}");
    assert_eq!(proposed["status"], "proposed");
    let action = &proposed["action"];
    assert_eq!(action["type"], TOOL);
    assert_eq!(action["plan_id"], 1);
    assert_eq!(
        action["params"]["parts"],
        json!([{"part_id":4,"included":false},{"part_id":5,"quantity_override":3}])
    );
    let digest = action["params"]["expected_snapshot_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(digest.len(), 64);
    let id = action["id"].as_str().unwrap().to_owned();
    assert_eq!(parts(&database), before, "propose wrote the draft");
    assert_eq!(pending(&server, &headers).await, std::slice::from_ref(&id));

    let (confirmed, failed) =
        tool(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(!failed, "{confirmed}");
    assert_eq!(confirmed["ok"], true);
    let result = &confirmed["result"];
    assert_eq!(result["draft_id"], 2);
    assert_ne!(result["snapshot_digest"], json!(digest));
    assert_eq!(result["parts"][0]["part_id"], 4);
    assert_eq!(result["parts"][0]["included"], false);
    assert_eq!(result["parts"][1]["quantity_override"], 3);
    let after = parts(&database);
    assert_eq!((after[0].0, after[0].1), (4, false));
    assert_eq!((after[1].0, after[1].2), (5, Some(3)));
    assert!(pending(&server, &headers).await.is_empty());
    // The Accepted Plan is unchanged.
    assert_eq!(
        tool(&server, &headers, "get_remaining", json!({"plan_id":1}))
            .await
            .0,
        remaining
    );
    let (decisions, _) = tool(
        &server,
        &headers,
        "get_plan_decisions",
        json!({"plan_id":1}),
    )
    .await;
    let logged = decisions.to_string();
    assert!(
        logged.contains("applied_action") && logged.contains(TOOL),
        "{logged}"
    );

    // A null quantity_override clears the override.
    let id = propose(
        &server,
        &headers,
        json!([{"part_id":5,"quantity_override":null}]),
    )
    .await;
    let (cleared, failed) = tool(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(!failed, "{cleared}");
    let (id, _, quantity) = parts(&database)[1];
    assert_eq!((id, quantity), (5, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_part_edit_refuses_a_changed_draft_and_keeps_the_proposal() {
    let (server, headers, database) = fixture().await;
    let stale = propose(&server, &headers, json!([{"part_id":4,"included":false}])).await;
    // Another edit through the same Plan edit operation changes the draft first.
    let other = propose(
        &server,
        &headers,
        json!([{"part_id":6,"quantity_override":5}]),
    )
    .await;
    assert!(
        !tool(
            &server,
            &headers,
            "confirm_apply",
            json!({"action_id":other})
        )
        .await
        .1
    );
    let edited = parts(&database);
    assert_eq!(edited[2], (6, true, Some(5)));

    let (refused, failed) = tool(
        &server,
        &headers,
        "confirm_apply",
        json!({"action_id":stale}),
    )
    .await;
    assert!(failed);
    assert_eq!(
        (refused["ok"].clone(), refused["code"].clone()),
        (json!(false), json!("draft_changed"))
    );
    assert_eq!(parts(&database), edited, "the newer edit was overwritten");
    assert_eq!(
        pending(&server, &headers).await,
        std::slice::from_ref(&stale)
    );

    let (dismissed, _) = tool(
        &server,
        &headers,
        "dismiss_proposed_action",
        json!({"action_id":stale}),
    )
    .await;
    assert_eq!(dismissed["ok"], true);
    assert!(pending(&server, &headers).await.is_empty());
    assert_eq!(parts(&database), edited);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_part_edit_refuses_bad_proposals_with_a_code() {
    let (server, headers, database) = fixture().await;
    let before = parts(&database);
    for (args, code) in [
        (
            json!({"plan_id":999,"draft_id":2,"parts":[{"part_id":4,"included":false}]}),
            "not_found",
        ),
        (
            json!({"plan_id":1,"draft_id":99,"parts":[{"part_id":4,"included":false}]}),
            "not_found",
        ),
        (
            json!({"plan_id":1,"draft_id":1,"parts":[{"part_id":1,"included":false}]}),
            "not_open",
        ),
        (
            json!({"plan_id":1,"draft_id":2,"parts":[]}),
            "parts_required",
        ),
        (
            json!({"plan_id":1,"draft_id":2,"parts":[{"part_id":1,"included":false}]}),
            "part_not_found",
        ),
        (
            json!({"plan_id":1,"draft_id":2,"parts":[{"part_id":4}]}),
            "edit_empty",
        ),
        (
            json!({"plan_id":1,"draft_id":2,"parts":[{"part_id":4,"quantity_override":-1}]}),
            "invalid_edit",
        ),
        (
            json!({"plan_id":1,"draft_id":2,"expected_snapshot_digest":"x","parts":[{"part_id":4,"included":false}]}),
            "invalid_digest",
        ),
        (
            json!({"plan_id":1,"parts":[{"part_id":4,"included":false}]}),
            "arguments_required",
        ),
    ] {
        let (refused, failed) = tool(&server, &headers, TOOL, args.clone()).await;
        assert!(failed, "{args}");
        assert_eq!(refused["code"], code, "{args}: {refused}");
    }
    assert!(pending(&server, &headers).await.is_empty());
    assert_eq!(parts(&database), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_part_edit_needs_an_api_key() {
    let (server, _, database) = fixture().await;
    let before = parts(&database);
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":TOOL,
        "arguments":{"plan_id":1,"draft_id":2,"parts":[{"part_id":4,"included":false}]}}});
    let (status, body, session) = server
        .post("/api/v1/mcp", &[("origin", SELF)], &request)
        .await;
    assert_eq!(status, 401, "{body}");
    assert!(session.is_none());
    assert_eq!(parts(&database), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_part_edit_takes_the_ids_get_plan_draft_returns() {
    let (server, headers, database) = fixture().await;
    let (read, failed) = tool(&server, &headers, "get_plan_draft", json!({"plan_id":1})).await;
    assert!(!failed, "{read}");
    let draft = &read["draft"];
    let part = &draft["parts"][0];
    let part_id = part["draft_part_id"].clone();
    let args = json!({
        "plan_id": 1,
        "draft_id": draft["draft_id"],
        "expected_snapshot_digest": draft["snapshot_digest"],
        "parts": [{"part_id": part_id, "included": !part["included"].as_bool().unwrap()}],
    });
    let (proposed, failed) = tool(&server, &headers, TOOL, args).await;
    assert!(!failed, "{proposed}");
    let id = proposed["action"]["id"].as_str().unwrap();
    let (confirmed, failed) =
        tool(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(!failed, "{confirmed}");
    assert_eq!(confirmed["result"]["draft_id"], draft["draft_id"]);
    assert_eq!(confirmed["result"]["parts"][0]["part_id"], part_id);
    let stored = parts(&database);
    let edited = stored.iter().find(|row| json!(row.0) == part_id).unwrap();
    assert_eq!(edited.1, !part["included"].as_bool().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_part_edit_refused_after_it_committed_is_outcome_unknown() {
    let (server, headers, database) = fixture().await;
    // Without the accepted revision's Required-unit set, the Part edit still
    // commits, then the automatic Required-unit selection is refused. The
    // disposable database drops the delete guards only to remove that set.
    pp_testkit::mcp_planning::edit(
        &database,
        "PRAGMA foreign_keys=OFF;
         DROP TRIGGER trg_required_units_immutable_delete;
         DROP TRIGGER trg_plan_revision_required_units_immutable_delete;
         DROP TRIGGER trg_plan_revision_required_unit_sets_immutable_delete;
         DELETE FROM plan_revision_required_units WHERE revision_id=1;
         DELETE FROM plan_revision_required_unit_sets WHERE revision_id=1;
         DELETE FROM required_units WHERE profile_id=1 AND created_in_revision_id=1;",
    )
    .unwrap();
    let id = propose(&server, &headers, json!([{"part_id":4,"included":false}])).await;
    let (refused, failed) = tool(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(failed, "{refused}");
    assert_eq!(refused["code"], "outcome_unknown", "{refused}");
    assert_eq!(refused["outcome_unknown"], true);
    let (part, included, _) = parts(&database)[0];
    assert_eq!((part, included), (4, false), "the edit committed");
    assert!(pending(&server, &headers).await.is_empty());
}
