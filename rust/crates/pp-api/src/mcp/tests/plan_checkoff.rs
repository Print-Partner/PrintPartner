use super::{
    reads::{assert_needs_key, confirm, session, tool},
    session_keys::ui,
    *,
};
use pp_storage::checkoff_progress::Request;

const READ: &str = "get_plan_checkoff";
const PROGRESS: &str = "propose_set_plan_progress";
const ASSEMBLY: &str = "propose_set_plan_assembly";

async fn fixture() -> (McpServer, Vec<(String, String)>) {
    let server = McpServer::seeded(
        "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Empty Plan')",
    )
    .await;
    std::fs::create_dir_all(server._directory.join("repos/1/revisions/fixture")).unwrap();
    let (_, key) = server.create_key().await;
    let headers = session(&server, &format!("Bearer {key}")).await;
    (server, headers)
}

async fn read(server: &McpServer, headers: &[(String, String)]) -> Value {
    let (state, failed) = tool(server, headers, READ, json!({"plan_id":1})).await;
    assert!(!failed, "{state}");
    assert_eq!(state["state"], "ready");
    state
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_plan_checkoff_confirms_once_and_undoes_without_changing_the_plan() {
    let (server, headers) = fixture().await;
    assert_needs_key(&server, READ, json!({"plan_id":1})).await;
    let (_, other_key) = server.create_key().await;
    let other_session = session(&server, &format!("Bearer {other_key}")).await;
    let before = read(&server, &headers).await;
    assert_eq!(
        before["total_units"].as_u64().unwrap(),
        before["printed_units"].as_u64().unwrap() + before["remaining_units"].as_u64().unwrap(),
    );
    let part = &before["parts"][0];
    let empty = part["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|unit| unit["completed"] == false)
        .unwrap();
    let expected = &before["accepted_basis"];
    let count = empty["unit_index"].as_u64().unwrap() + 1;
    let rows = json!([{"part_id":part["part_id"],"printed_count":count}]);
    let (proposed, failed) = tool(
        &server,
        &headers,
        PROGRESS,
        json!({"plan_id":1,"rows":rows,"expected":expected}),
    )
    .await;
    assert!(!failed, "{proposed}");
    assert_eq!(proposed["action"]["params"]["review"]["rows"], rows);
    assert_eq!(read(&server, &headers).await, before);
    let (receipt, failed) = confirm(&server, &other_session, &proposed).await;
    assert!(!failed, "{receipt}");
    assert_eq!(receipt["result"]["printed_added"], 1);
    assert_eq!(receipt["result"]["parts"][0]["printed_count"], count);
    let printed = read(&server, &headers).await;
    assert_eq!(printed["accepted_basis"], *expected);
    assert_eq!(
        printed["parts"][0]["units"][count as usize - 1]["token"],
        empty["token"]
    );

    let (assembly, failed) = tool(&server, &headers, ASSEMBLY, json!({
        "plan_id":1, "part_id":part["part_id"], "unit_index":empty["unit_index"], "assembled":true,
    })).await;
    assert!(!failed, "{assembly}");
    assert_eq!(read(&server, &headers).await, printed);
    let assembly_id = assembly["action"]["id"].as_str().unwrap();
    let (assembly_receipt, failed) = confirm(&server, &headers, &assembly).await;
    assert!(!failed, "{assembly_receipt}");
    let (status, body) = ui(&server, Method::POST, &format!("/{assembly_id}/apply")).await;
    assert_eq!((status, &body["code"]), (409, &json!("already_applied")));
    let assembled = read(&server, &headers).await;
    assert_eq!(
        assembled["parts"][0]["units"][count as usize - 1]["assembled"],
        true
    );
    let (duplicate, failed) = confirm(&server, &headers, &assembly).await;
    assert!(failed, "{duplicate}");
    assert_eq!(duplicate["code"], "already_applied");

    let progress = server.owner.checkoff_progress().unwrap();
    for (applied, want) in [(&assembly_receipt, &printed), (&receipt, &before)] {
        let undo = progress
            .apply(
                pp_storage::read_model::Credential::Session(Secret::new(FIXTURE_SESSION.into())),
                Request::UndoManualBurst {
                    profile_id: 1,
                    body: json!({"undo_token":applied["result"]["undo_token"]}),
                },
                &AtomicBool::new(false),
                Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(undo.status, 200, "{undo:?}");
        assert_eq!(&read(&server, &headers).await, want);
    }
    assert_eq!(read(&server, &headers).await, before);
    let (duplicate, failed) = confirm(&server, &headers, &proposed).await;
    assert!(failed, "{duplicate}");
    assert_eq!(duplicate["code"], "already_applied");
    assert_eq!(read(&server, &headers).await, before);

    let (clear, failed) = tool(
        &server,
        &headers,
        PROGRESS,
        json!({
            "plan_id":1, "rows":[{"part_id":part["part_id"],"printed_count":0}],
        }),
    )
    .await;
    assert!(!failed, "{clear}");
    let id = clear["action"]["id"].as_str().unwrap();
    let (status, body) = ui(&server, Method::POST, &format!("/{id}/apply")).await;
    assert_eq!((status, body), (200, json!("applied")));
    let cleared = read(&server, &headers).await;
    assert_eq!(cleared["accepted_basis"], *expected);
    assert_eq!(cleared["parts"][0]["printed_count"], 0);
    assert_eq!(cleared["parts"][0]["assembled_count"], 0);
    let (duplicate, failed) = confirm(&server, &headers, &clear).await;
    assert!(failed, "{duplicate}");
    assert_eq!(duplicate["code"], "already_applied");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_plan_checkoff_refuses_bad_coordinates_without_staging() {
    let (server, headers) = fixture().await;
    let state = read(&server, &headers).await;
    let part = &state["parts"][0]["part_id"];
    let row = json!({"part_id":part,"printed_count":1});
    assert_eq!(
        tool(&server, &headers, READ, json!({"plan_id":2})).await,
        tool(&server, &headers, READ, json!({"plan_id":98765})).await,
    );
    assert_eq!(
        tool(&server, &headers, READ, json!({"plan_id":3})).await,
        (json!({"plan_id":3,"state":"empty","parts":[]}), false),
    );
    for (name, args) in [
        (READ, json!({"plan_id":0})),
        (READ, json!({"plan_id":98765})),
        (PROGRESS, json!({"plan_id":3,"rows":[row]})),
        (PROGRESS, json!({"plan_id":1,"rows":[row,row]})),
        (
            PROGRESS,
            json!({"plan_id":1,"rows":[row],"expected":{"profile_id":2}}),
        ),
        (
            PROGRESS,
            json!({"plan_id":1,"rows":[{"part_id":part,"printed_count":9999}]}),
        ),
        (
            ASSEMBLY,
            json!({"plan_id":1,"part_id":part,"unit_index":9999,"assembled":true}),
        ),
        (
            ASSEMBLY,
            json!({"plan_id":1,"part_id":part,"unit_index":2,"assembled":true}),
        ),
    ] {
        let (reply, failed) = tool(&server, &headers, name, args).await;
        assert!(failed, "{name}: {reply}");
    }
    let (queue, failed) = tool(&server, &headers, "list_pending_actions", json!({})).await;
    assert!(!failed, "{queue}");
    assert_eq!(queue["actions"], json!([]));
    assert_eq!(read(&server, &headers).await, state);
}
