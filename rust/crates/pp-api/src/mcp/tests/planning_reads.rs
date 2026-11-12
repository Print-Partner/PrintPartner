//! `get_build_planning_state` and `get_plan_draft` (GRE-1442) through the MCP
//! router with an API key. The fixture's Build 1 has a consumed draft 1 and
//! an open draft 2 with three Parts; the seed gives it a brief that selects
//! draft 2. Build 3 has no brief and no draft.
use super::reads::{call, session};
use super::*;
use pp_storage::{
    read_model::{Credential, planning},
    working_drafts::{self, Outcome as DraftOutcome},
};

const BUILD: u64 = 1;
const EMPTY: u64 = 3;

fn difference(id: &str, group: &str) -> Value {
    json!({ "id": id, "group_id": group, "family": "f", "kind": "changed", "source_a": "A",
        "source_b": "B", "path_a": "a.stl", "path_b": "b.stl", "detail": "d" })
}

fn seed() -> String {
    let brief = json!({
        "version": 1, "build_id": 1, "special_request": "Voron for Sam", "requirements": [],
        "evidence": [
            { "id": "e1", "url": "https://github.com/example/voron",
              "normalized_url": "https://github.com/example/voron", "kind": "canonical_design",
              "source_id": 1, "source_role": "structural_base" },
            { "id": "e2", "url": "https://www.printables.com/model/1",
              "normalized_url": "https://www.printables.com/model/1", "kind": "mod",
              "input_kind": "model_page", "upload_required": true },
            // A model page whose upload arrived: no decision left.
            { "id": "e3", "url": "https://www.printables.com/model/3",
              "normalized_url": "https://www.printables.com/model/3", "kind": "mod",
              "input_kind": "model_page", "upload_required": true },
            { "id": "e4", "url": "upload://e3", "normalized_url": "upload://e3", "kind": "mod",
              "input_kind": "upload", "derived_from_evidence_id": "e3" },
        ],
        "contributions": [
            { "id": "c1", "evidence_id": "e1", "slot": "frame", "responsibility": "printable_parts",
              "path_scopes": ["stl"], "confidence": "high", "evidence_text": "README", "status": "proposed" },
        ],
        "differences": [difference("d1", "g1"), difference("d2", "g2"), difference("d3", "g1")],
        "resolutions": { "g2": { "resolution": "choose_source_a", "rationale": "r",
            "custom_resolution": null, "resolved_at": "2026-11-07T12:00:00.000Z" } }, "role_filaments": [], "draft_id": 2,
        "draft_review_blockers": ["legacy"], "created_at": "2026-11-07T12:00:00.000Z",
        "updated_at": "2026-11-07T12:00:00.000Z",
    });
    format!(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','build_planning.v1.1','{}');\
         INSERT INTO build_profiles(tenant_id,id,name) VALUES('default',{EMPTY},'Empty Build');",
        brief.to_string().replace('\'', "''")
    )
}

async fn server() -> (McpServer, Vec<(String, String)>) {
    let server = McpServer::seeded(&seed()).await;
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let headers = session(&server, &bearer).await;
    (server, headers)
}

async fn read(server: &McpServer, headers: &[(String, String)], tool: &str, args: Value) -> Value {
    let (text, error) = call(server, headers, tool, args).await;
    assert!(!error, "{tool}: {text}");
    serde_json::from_str(&text).unwrap()
}

async fn refused(
    server: &McpServer,
    headers: &[(String, String)],
    tool: &str,
    args: Value,
) -> Value {
    let (text, error) = call(server, headers, tool, args).await;
    assert!(error, "{tool}: {text}");
    let refusal: Value = serde_json::from_str(&text).unwrap();
    assert!(refusal["error"].is_string(), "{refusal}");
    refusal["code"].clone()
}

fn session_credential() -> Credential {
    Credential::Session(Secret::new(FIXTURE_SESSION.into()))
}

/// What `GET /plans/1/build-planning` serves: `brief` and `planning_phase`.
async fn http_planning(server: &McpServer) -> Value {
    let client = server
        .owner
        .build_planning(policy(), server.owner.source_reads().unwrap())
        .unwrap();
    let outcome = tokio::task::spawn_blocking(move || {
        client.read(
            session_credential(),
            planning::Request {
                build: Some(PositiveId::new(BUILD).unwrap()),
                query: planning::Query::BuildPlanning {
                    draft_id_valid: true,
                },
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let planning::Outcome::BuildPlanning(response) = outcome else {
        panic!("the brief reads");
    };
    serde_json::to_value(response.planning.unwrap()).unwrap()
}

/// What `GET /plans/1/drafts/2` serves.
async fn http_workspace(server: &McpServer, draft: u64) -> Value {
    let client = server
        .owner
        .working_drafts_with_observations(
            policy(),
            Arc::new(server.owner.source_reads().unwrap()),
            Default::default(),
        )
        .unwrap();
    let outcome = tokio::task::spawn_blocking(move || {
        client.execute(
            session_credential(),
            PositiveId::new(BUILD).unwrap(),
            working_drafts::Request::Workspace {
                draft_id: PositiveId::new(draft).unwrap(),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let DraftOutcome::Workspace { workspace } = outcome else {
        panic!("the workspace reads");
    };
    serde_json::to_value(workspace).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_state_returns_the_brief_and_open_decisions() {
    let (server, headers) = server().await;
    let state = read(
        &server,
        &headers,
        "get_build_planning_state",
        json!({ "plan_id": BUILD }),
    )
    .await;
    let e1 = &state["brief"]["evidence"][0];
    assert_eq!(
        (&e1["id"], &e1["source_id"], &e1["source_role"]),
        (&json!("e1"), &json!(1), &json!("structural_base"))
    );
    assert!(
        state["brief"].get("draft_review_blockers").is_none(),
        "{state}"
    );
    assert_eq!(
        state["planning_phase"],
        json!({ "kind": "draft", "draft_id": 2 })
    );
    assert_eq!(
        (
            &state["grouped_difference_count"],
            &state["difference_count"]
        ),
        (&json!(2), &json!(3))
    );
    assert_eq!(
        state["next_required_decisions"],
        json!([
            { "kind": "upload", "evidence_id": "e2" },
            { "kind": "contribution", "contribution_id": "c1" },
            { "kind": "difference_group", "group_id": "g1" },
        ])
    );
    let mut http = http_planning(&server).await;
    http["brief"]
        .as_object_mut()
        .unwrap()
        .remove("draft_review_blockers");
    assert_eq!(state["brief"], http["brief"]);
    assert_eq!(state["planning_phase"], http["planning_phase"]);

    assert_eq!(
        refused(
            &server,
            &headers,
            "get_build_planning_state",
            json!({ "plan_id": EMPTY })
        )
        .await,
        "no_planning_brief"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_plan_draft_returns_the_selected_or_open_draft() {
    let (server, headers) = server().await;
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();

    let selected = read(
        &server,
        &headers,
        "get_plan_draft",
        json!({ "plan_id": BUILD }),
    )
    .await;
    assert_eq!(
        selected["planning_phase"],
        json!({ "kind": "draft", "draft_id": 2 })
    );
    let draft = &selected["draft"];
    let workspace = http_workspace(&server, 2).await;
    let mut expected = workspace["draft"].clone();
    expected["parts"] = workspace["parts"].clone();
    assert_eq!(draft, &expected);
    assert_eq!(draft["draft_id"], 2);
    assert!(draft["snapshot_digest"].is_string());
    let parts: Vec<_> = draft["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| {
            (
                part["draft_part_id"].clone(),
                part["relative_path"].clone(),
                part["included"].clone(),
                part["quantity_effective"].clone(),
            )
        })
        .collect();
    assert_eq!(parts.len(), 3, "{draft}");
    assert!(parts.iter().all(|(id, path, included, quantity)| {
        id.is_i64() && path.is_string() && included.is_boolean() && quantity.is_i64()
    }));

    assert_eq!(
        before,
        pp_testkit::mcp_planning::state(&database).unwrap(),
        "reads write nothing"
    );

    // Without a brief, the Build's open draft; no phase.
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    connection
        .execute(
            "DELETE FROM app_settings WHERE key='build_planning.v1.1'",
            [],
        )
        .unwrap();
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    let open = read(
        &server,
        &headers,
        "get_plan_draft",
        json!({ "plan_id": BUILD }),
    )
    .await;
    assert_eq!(open["draft"], expected);
    assert!(open.get("planning_phase").is_none(), "{open}");

    let none = read(
        &server,
        &headers,
        "get_plan_draft",
        json!({ "plan_id": EMPTY }),
    )
    .await;
    assert_eq!(none, json!({ "draft": null }));
    assert_eq!(
        before,
        pp_testkit::mcp_planning::state(&database).unwrap(),
        "reads write nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_reads_refuse_a_bad_or_unknown_plan_id_with_a_code() {
    let (server, headers) = server().await;
    for tool in ["get_build_planning_state", "get_plan_draft"] {
        for (args, code) in [
            (json!({ "plan_id": 999 }), "plan_not_found"),
            (json!({ "plan_id": 2 }), "plan_not_found"), // the foreign tenant's Build
            (json!({ "plan_id": "1" }), "invalid_plan_id"),
            (json!({ "plan_id": 0 }), "invalid_plan_id"),
            (json!({}), "invalid_plan_id"),
        ] {
            assert_eq!(
                refused(&server, &headers, tool, args.clone()).await,
                code,
                "{tool} {args}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_planning_reads_need_a_key() {
    let (server, headers) = server().await;
    let session_id = headers
        .iter()
        .find(|(name, _)| name == "mcp-session-id")
        .map(|(_, value)| value.as_str())
        .unwrap();
    for tool in ["get_build_planning_state", "get_plan_draft"] {
        let request = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": tool, "arguments": { "plan_id": BUILD } } });
        let unkeyed = [
            ("mcp-session-id", session_id),
            ("mcp-protocol-version", "2025-06-18"),
            // Same-origin, so only the key check can refuse.
            ("origin", SELF),
        ];
        let (status, _, _) = server.post("/api/v1/mcp", &unkeyed, &request).await;
        assert_eq!(status, 401, "{tool}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_plan_draft_follows_the_brief_selection_when_it_moves() {
    let (server, headers) = server().await;
    let connection =
        rusqlite::Connection::open(server._directory.join("print-partner.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    let update = |sql: &str| {
        connection
            .execute(
                &format!("UPDATE app_settings SET value={sql} WHERE key='build_planning.v1.1'"),
                [],
            )
            .unwrap();
    };

    // The selected draft is gone: no draft, and the phase says so.
    update("json_set(value,'$.draft_id',99)");
    let missing = read(
        &server,
        &headers,
        "get_plan_draft",
        json!({ "plan_id": BUILD }),
    )
    .await;
    assert_eq!(
        missing,
        json!({ "draft": null, "planning_phase": { "kind": "missing_draft", "draft_id": 99 } })
    );

    // The consumed draft 1 is selected: it is returned as it is.
    update("json_set(value,'$.draft_id',1)");
    let consumed = read(
        &server,
        &headers,
        "get_plan_draft",
        json!({ "plan_id": BUILD }),
    )
    .await;
    assert_eq!(consumed["draft"]["draft_id"], 1);
    assert_eq!(consumed["draft"]["state"], "consumed");

    // Hydration clears a selection whose Sources moved: the open draft, phase preparing.
    update(
        "json_set(json_set(value,'$.draft_id',2),'$.draft_source_revisions',json('{\"1\":\"moved\"}'))",
    );
    let cleared = read(
        &server,
        &headers,
        "get_plan_draft",
        json!({ "plan_id": BUILD }),
    )
    .await;
    assert_eq!(cleared["draft"]["draft_id"], 2);
    assert_eq!(cleared["planning_phase"], json!({ "kind": "preparing" }));
}
