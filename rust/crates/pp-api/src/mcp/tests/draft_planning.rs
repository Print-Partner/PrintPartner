use super::planning::{assert_refused, propose};
use super::reads::{call, session};
use super::*;
const SEED: &str =
    include_str!("../../../../../../fixtures/parity/mcp-proposals/draft-planning/seed.sql");

async fn fixture() -> (McpServer, Vec<(String, String)>) {
    let server = McpServer::seeded(SEED).await;
    let root = server._directory.join("repos/4");
    std::fs::create_dir_all(&root).unwrap();
    for name in ["bracket", "gear"] {
        std::fs::write(
            root.join(format!("{name}.stl")),
            format!("solid {name}\nendsolid {name}\n"),
        )
        .unwrap();
    }
    let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    (server, headers)
}
async fn apply(server: &McpServer, headers: &[(String, String)], tool: &str, args: Value) -> Value {
    let id = propose(server, headers, tool, args).await;
    let (text, failed) = call(server, headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(!failed, "{text}");
    serde_json::from_str(&text).unwrap()
}
fn parts(result: &Value) -> Vec<(i64, Value, bool)> {
    let parts = result["result"]["draft"]["parts"].as_array().unwrap();
    parts
        .iter()
        .map(|p| {
            (
                p["id"].as_i64().unwrap(),
                p["quantityOverride"].clone(),
                p["included"] == true,
            )
        })
        .collect()
}
// Acceptance is what a client observes: results, the drafts left behind, and no Accept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_planning_edits_drafts_and_roles_without_accepting() {
    let (server, headers) = fixture().await;
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();
    let edited = apply(
        &server,
        &headers,
        "propose_edit_plan_draft_parts",
        json!({"plan_id":1,"draft_id":2,"parts":[{"part_id":4,"quantity_override":4},{"id":5,"included":true}]}),
    )
    .await;
    assert_eq!(edited["result"]["edit_result"], "updated");
    assert_eq!(
        parts(&edited),
        [
            (4, json!(4), true),
            (5, json!(1), true),
            (6, json!(2), true)
        ]
    );
    for assignment in [
        json!({"role":" accent ","inventory_kind":"custom","color_hex":"#aA1122","substitution_confirmed":true}),
        json!({"role":"primary","inventory_kind":"catalog","inventory_id":"abs-matte::black-pantone-7c","color_hex":"#C0AFA1","requested_name":"Black (Pantone 7C)","requested_brand":"ABS Matte"}),
    ] {
        let assigned = apply(
            &server,
            &headers,
            "propose_assign_role_filament",
            json!({"plan_id":1,"assignment":assignment}),
        )
        .await;
        assert_eq!(assigned["ok"], true, "{assigned}");
    }
    let rebuilt = apply(
        &server,
        &headers,
        "propose_rebuild_plan",
        json!({"plan_id":3,"idempotency_key":"fixture-rebuild"}),
    )
    .await;
    assert_eq!(
        rebuilt["result"],
        json!({"draft_id":3,"recompute":"created"})
    );
    let edited = apply(
        &server,
        &headers,
        "propose_edit_plan_draft_parts",
        json!({"plan_id":3,"draft_id":3,"parts":[{"part_id":7,"quantity_override":2}]}),
    )
    .await;
    assert_eq!(
        parts(&edited),
        [(7, json!(2), true), (8, Value::Null, true)]
    );
    let id = propose(
        &server,
        &headers,
        "propose_assign_role_filament",
        json!({"plan_id":1,"assignment":{"role":"accent","inventory_kind":"catalog","inventory_id":"unknown","color_hex":"#123456"}}),
    )
    .await;
    let (text, failed) = call(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
    assert!(failed, "{text}");
    let (text, failed) = call(
        &server,
        &headers,
        "dismiss_proposed_action",
        json!({"action_id":id}),
    )
    .await;
    assert!(!failed, "{text}");
    for plan_id in [2, 999_999] {
        for (tool, args) in [
            ("propose_rebuild_plan", json!({"plan_id":plan_id})),
            (
                "propose_edit_plan_draft_parts",
                json!({"plan_id":plan_id,"draft_id":2,"parts":[{"part_id":4,"included":false}]}),
            ),
            (
                "propose_assign_role_filament",
                json!({"plan_id":plan_id,"assignment":{"role":"x","inventory_kind":"custom","color_hex":"#123456","substitution_confirmed":true}}),
            ),
        ] {
            let (text, failed) = call(&server, &headers, tool, args).await;
            assert!(failed, "{tool} {plan_id}: {text}");
        }
    }
    let after = pp_testkit::mcp_planning::state(&database).unwrap();
    for table in [7, 8] {
        assert_eq!(before[table], after[table], "Accepted Plan history changed");
    }
    assert_eq!(before[3][0], after[3][0], "Accepted Build changed");
    let roles = pp_testkit::mcp_planning::setting(&database, "role_filaments_1").unwrap();
    assert_eq!(roles["accent"]["filament_custom_hex"], "#aA1122");
    assert_eq!(
        roles["primary"]["filament_color_id"],
        "abs-matte::black-pantone-7c"
    );
    let brief = pp_testkit::mcp_planning::setting(&database, "build_planning.v1.1").unwrap();
    assert!(brief.get("draft_id").is_none());
    assert_eq!(brief["requirements"][0]["status"], "satisfied");
    assert_eq!(
        pp_testkit::mcp_planning::setting(&database, "build_planning.v1.3").unwrap()["draft_id"],
        3
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_draft_planning_revalidates_and_rolls_back_without_publication() {
    for (tool, args, sql, detail) in [
        (
            "propose_edit_plan_draft_parts",
            // A stale expected digest is a concurrent edit; stored data stays valid.
            json!({"plan_id":1,"draft_id":2,"expected_snapshot_digest":"0".repeat(64),"parts":[{"part_id":4,"included":false}]}),
            "",
            "conflict",
        ),
        (
            "propose_rebuild_plan",
            json!({"plan_id":3}),
            "UPDATE projects SET last_commit_sha='changed' WHERE id=4",
            "Build planning changed since the proposal; propose again",
        ),
        (
            "propose_assign_role_filament",
            json!({"plan_id":1,"assignment":{"role":"accent","inventory_kind":"custom","color_hex":"#123456","substitution_confirmed":true}}),
            "UPDATE build_profiles SET accepted_plan_version=2 WHERE id=1",
            "Build planning changed since the proposal; propose again",
        ),
        (
            "propose_rebuild_plan",
            json!({"plan_id":3}),
            "DELETE FROM build_profiles WHERE id=3",
            "Plan not found",
        ),
    ] {
        let (server, headers) = fixture().await;
        let id = propose(&server, &headers, tool, args).await;
        pp_testkit::mcp_planning::edit(&server._directory.join("print-partner.db"), sql).unwrap();
        if detail == "conflict" {
            let database = server._directory.join("print-partner.db");
            let before = pp_testkit::mcp_planning::state(&database).unwrap();
            let (text, failed) =
                call(&server, &headers, "confirm_apply", json!({"action_id":id})).await;
            assert!(failed, "{text}");
            let refused = serde_json::from_str::<Value>(&text).unwrap();
            assert_eq!(refused["ok"], false, "{text}");
            assert!(refused["result"]["draft"].is_object(), "{text}");
            assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());
            let (pending, _) = call(&server, &headers, "list_pending_actions", json!({})).await;
            assert!(pending.contains(&id));
        } else {
            assert_refused(&server, &headers, &id, detail).await;
        }
    }
    let (server, headers) = fixture().await;
    let id = propose(
        &server,
        &headers,
        "propose_rebuild_plan",
        json!({"plan_id":1}),
    )
    .await;
    pp_testkit::mcp_planning::edit(
        &server._directory.join("print-partner.db"),
        "UPDATE plan_drafts SET snapshot_digest=printf('%064d',0) WHERE id=2",
    )
    .unwrap();
    assert_refused(
        &server,
        &headers,
        &id,
        "Build planning changed since the proposal; propose again",
    )
    .await;
    // Rebuild first changes the managed layers, then fails preparation: all roll back.
    let (server, headers) = fixture().await;
    let database = server._directory.join("print-partner.db");
    pp_testkit::mcp_planning::edit(&database, "DELETE FROM profile_layers WHERE profile_id=3")
        .unwrap();
    std::fs::remove_file(server._directory.join("repos/4/bracket.stl")).unwrap();
    std::fs::remove_file(server._directory.join("repos/4/gear.stl")).unwrap();
    let id = propose(
        &server,
        &headers,
        "propose_rebuild_plan",
        json!({"plan_id":3}),
    )
    .await;
    assert_refused(
        &server,
        &headers,
        &id,
        "Plan draft recompute failed: no_stls",
    )
    .await;
}

struct Inventory {
    entered: tokio::sync::Notify,
    release: Arc<Semaphore>,
    reads: std::sync::atomic::AtomicUsize,
}
impl crate::filaments::CatalogReader for Inventory {
    fn read(
        &self,
        _: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<Value>>> + Send>> {
        panic!("MCP must resolve a specific filament")
    }
    fn resolve(
        &self,
        _: Value,
        id: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Value>> + Send>> {
        assert_eq!(id, 7);
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.entered.notify_one();
        // The owned permit lets the test change access while inventory is in flight.
        let release = self.release.clone();
        Box::pin(async move {
            release.acquire_owned().await.unwrap().forget();
            Some(json!({"display_name":"Red","brand":"Fixture","hex":"#123456"}))
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_role_filament_verifies_only_on_confirm_and_rechecks_access_after_network() {
    for change in ["", "feature", "integration", "key"] {
        let inventory = Arc::new(Inventory {
            entered: tokio::sync::Notify::new(),
            release: Arc::new(Semaphore::new(0)),
            reads: Default::default(),
        });
        let reader = inventory.clone();
        let server = McpServer::open(Some("1"), "default", SEED, |config| {
            config.with_filament_reader(reader)
        })
        .await;
        let database = server._directory.join("print-partner.db");
        pp_testkit::mcp_planning::save(&database, "feature_toggles", &json!({"spoolman":true}))
            .unwrap();
        pp_testkit::mcp_planning::save(&database,"integrations",&json!([{"id":"stock","type":"spoolman","config":{"baseUrl":"http://127.0.0.1:9999","enabled":true}}])).unwrap();
        let (key, secret) = server.create_key().await;
        let headers = session(&server, &format!("Bearer {secret}")).await;
        let id=propose(&server,&headers,"propose_assign_role_filament",json!({"plan_id":1,"assignment":{"role":"accent","inventory_kind":"spoolman","inventory_id":"spoolman:stock:filament:7","color_hex":"#123456","requested_name":" Red ","requested_brand":"fixture"}})).await;
        assert_eq!(inventory.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        let (confirmed, before) = tokio::join!(
            call(&server, &headers, "confirm_apply", json!({"action_id":id})),
            async {
                inventory.entered.notified().await;
                match change {
                    "feature" => pp_testkit::mcp_planning::save(
                        &database,
                        "feature_toggles",
                        &json!({"spoolman":false}),
                    )
                    .unwrap(),
                    "integration" => {
                        pp_testkit::mcp_planning::save(&database, "integrations", &json!([]))
                            .unwrap()
                    }
                    "key" => server.revoke(key).await,
                    _ => {}
                }
                let before = pp_testkit::mcp_planning::state(&database).unwrap();
                inventory.release.add_permits(1);
                before
            }
        );
        let (text, failed) = confirmed;
        let result: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(inventory.reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        if change.is_empty() {
            assert!(!failed, "{text}");
            assert_eq!(result, json!({"ok":true,"result":{"role":"accent"}}));
            assert!(
                pp_testkit::mcp_planning::setting(&database, "build_planning.v1.1")
                    .unwrap()
                    .get("draft_id")
                    .is_none()
            );
        } else {
            assert!(failed, "{text}");
            assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());
            if change != "key" {
                let (text, _) = call(&server, &headers, "list_pending_actions", json!({})).await;
                assert!(text.contains(&id));
            }
        }
    }
}
