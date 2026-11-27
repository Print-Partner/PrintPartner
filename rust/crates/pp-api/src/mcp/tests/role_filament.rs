//! `propose_assign_role_filament` and its `confirm_apply` (GRE-1435), through
//! the MCP router with an API key, on a Build whose planning brief
//! `propose_create_build` created.
use super::create_build::{confirm, database, execute, json_call, pending};
use super::source_roles::Seeded;
use super::*;

const TOOL: &str = "propose_assign_role_filament";
const COLOR: &str = "abs-matte::black-pantone-7c";

fn catalog(hex: &str) -> Value {
    json!({
        "role": " Accent ",
        "inventory_kind": "catalog",
        "inventory_id": COLOR,
        "requested_name": "Black (Pantone 7C)",
        "requested_brand": "ABS Matte",
        "color_hex": hex,
        "substitution_confirmed": false,
    })
}

async fn seeded() -> Seeded {
    let seeded = Seeded::new().await;
    let mut brief = seeded.brief();
    brief["requirements"] = json!([
        { "key": "color_accent", "value": "black", "status": "unverified", "detail": null },
        { "key": "color_primary", "value": "red", "status": "unverified", "detail": null },
    ]);
    seeded.store(&brief);
    seeded
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_assign_role_filament_saves_the_default_and_satisfies_the_requirement() {
    let seeded = seeded().await;
    let before = seeded.state();
    let projects: i64 = database(&seeded.server)
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    let (proposal, error) = json_call(
        &seeded.server,
        &seeded.headers,
        TOOL,
        json!({ "plan_id": seeded.build, "assignment": catalog("#C0AFA1") }),
    )
    .await;
    assert!(!error, "{proposal}");
    assert_eq!(before, seeded.state());
    let id = proposal["action"]["id"].as_str().unwrap();
    let (applied, error) = confirm(&seeded.server, &seeded.headers, id).await;
    assert!(!error && applied["ok"] == true, "{applied}");
    assert_eq!(applied["result"]["role"], "Accent");
    assert_eq!(applied["result"]["updated_parts"], 0);

    let brief = seeded.brief();
    assert!(brief["draft_id"].is_null());
    assert_eq!(brief["requirements"][0]["status"], "satisfied");
    assert_eq!(brief["requirements"][1]["status"], "unverified");
    assert_eq!(brief["role_filaments"].as_array().unwrap().len(), 1);
    assert_eq!(brief["role_filaments"][0]["inventory_id"], COLOR);
    // Saved under the key draft preparation looks up: the role lowercased.
    let defaults = role_defaults(&seeded);
    assert_eq!(defaults["accent"]["filament_color_id"], COLOR);
    assert!(defaults["accent"]["filament_custom_hex"].is_null());
    assert!(defaults["accent"]["spoolman_spool_id"].is_null());
    let projects_after: i64 = database(&seeded.server)
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(projects, projects_after);
    assert_eq!(seeded.untouched().1, 0);

    let (again, error) = json_call(
        &seeded.server,
        &seeded.headers,
        TOOL,
        json!({
            "plan_id": seeded.build,
            "assignment": {
                "role": "Accent",
                "inventory_kind": "substitute",
                "color_hex": "#112233",
                "substitution_confirmed": true,
            }
        }),
    )
    .await;
    assert!(!error, "{again}");
    let (replaced, error) = confirm(
        &seeded.server,
        &seeded.headers,
        again["action"]["id"].as_str().unwrap(),
    )
    .await;
    assert!(!error && replaced["ok"] == true, "{replaced}");
    let brief = seeded.brief();
    assert_eq!(brief["role_filaments"].as_array().unwrap().len(), 1);
    assert_eq!(brief["role_filaments"][0]["inventory_kind"], "substitute");
    let defaults = role_defaults(&seeded);
    assert!(defaults["accent"]["filament_color_id"].is_null());
    assert_eq!(defaults["accent"]["filament_custom_hex"], "#112233");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_assign_role_filament_refusals_write_nothing() {
    let seeded = seeded().await;
    seeded
        .refuses_propose(
            TOOL,
            vec![
                (
                    json!({ "assignment": catalog("#c0afa1") }),
                    "invalid_plan_id",
                ),
                (
                    json!({ "plan_id": seeded.build, "assignment": { "role": "accent" } }),
                    "invalid_filament_assignment",
                ),
                (
                    json!({
                        "plan_id": seeded.build,
                        "assignment": {
                            "role": "accent",
                            "inventory_kind": "custom",
                            "color_hex": "#112233",
                            "substitution_confirmed": false,
                        }
                    }),
                    "substitution_unconfirmed",
                ),
                (
                    json!({
                        "plan_id": seeded.build,
                        "assignment": {
                            "role": "accent",
                            "inventory_kind": "spoolman",
                            "inventory_id": "spoolman:1",
                            "color_hex": "#112233",
                        }
                    }),
                    "spoolman_module_off",
                ),
            ],
        )
        .await;

    let (proposal, error) = json_call(
        &seeded.server,
        &seeded.headers,
        TOOL,
        json!({ "plan_id": seeded.build, "assignment": catalog("#000000") }),
    )
    .await;
    assert!(!error, "{proposal}");
    let id = proposal["action"]["id"].as_str().unwrap().to_owned();
    let before = seeded.state();
    let (refused, error) = confirm(&seeded.server, &seeded.headers, &id).await;
    assert!(error, "{refused}");
    assert_eq!(refused["code"], "catalog_color_mismatch");
    assert_eq!(before, seeded.state());
    assert!(
        pending(&seeded.server, &seeded.headers)
            .await
            .iter()
            .any(|action| action["id"] == id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_assign_role_filament_catalog_and_spoolman_refusals_write_nothing() {
    let seeded = seeded().await;
    let cases = [
        (catalog_id("missing"), "catalog_filament_not_found"),
        (
            json!({
                "role": "accent",
                "inventory_kind": "catalog",
                "inventory_id": COLOR,
                "requested_name": "Not the catalog name",
                "color_hex": "#c0afa1",
            }),
            "catalog_name_mismatch",
        ),
        (
            json!({
                "role": "accent",
                "inventory_kind": "catalog",
                "inventory_id": COLOR,
                "requested_brand": "Not the product line",
                "color_hex": "#c0afa1",
            }),
            "catalog_brand_mismatch",
        ),
    ];
    for (assignment, code) in cases {
        refused_confirm(&seeded, assignment, code).await;
    }

    assert_eq!(
        seeded.brief()["role_filaments"].as_array().unwrap().len(),
        0
    );
}

/// Spoolman `sm-1` as the test router's inventory holds it.
const INTEGRATIONS: &str = r#"[{"id":"sm-1","type":"spoolman","name":"Spoolman","config":{"base_url":"http://127.0.0.1:7912","enabled":true}},{"id":"sm-off","type":"spoolman","name":"Off","config":{"base_url":"http://127.0.0.1:7913","enabled":false}},{"id":"sm-down","type":"spoolman","name":"Down","config":{"base_url":"http://127.0.0.1:7914"}}]"#;

fn spoolman(filament: &str, hex: &str, spool: Option<&str>) -> Value {
    json!({
        "role": "accent",
        "inventory_kind": "spoolman",
        "inventory_id": filament,
        "color_hex": hex,
        "spoolman_spool_id": spool,
    })
}

async fn spoolman_on() -> Seeded {
    let seeded = seeded().await;
    execute(
        &seeded.server,
        &format!(
            "UPDATE app_settings SET value='{{\"orders\":true,\"spoolman\":true}}' WHERE key='feature_toggles';
             INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'integrations', '{INTEGRATIONS}');"
        ),
    );
    seeded
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_assign_role_filament_checks_a_spoolman_filament_and_spool_against_spoolman() {
    let seeded = spoolman_on().await;
    let filament = "spoolman:sm-1:filament:3";
    let cases = [
        // Not a Spoolman filament id, or one Spoolman does not hold.
        (
            spoolman("spoolman:1", "#112233", None),
            "spoolman_filament_not_found",
        ),
        (
            spoolman("spoolman:sm-1:filament:9", "#112233", None),
            "spoolman_filament_not_found",
        ),
        (
            spoolman(filament, "#445566", None),
            "spoolman_color_mismatch",
        ),
        // Spool 8 holds filament 4, and spool 9 does not exist.
        (
            spoolman(filament, "#112233", Some("spoolman:sm-1:spool:8")),
            "spoolman_spool_not_found",
        ),
        (
            spoolman(filament, "#112233", Some("spoolman:sm-1:spool:9")),
            "spoolman_spool_not_found",
        ),
        // Unknown, disabled, and unreachable integrations cannot be checked.
        (
            spoolman("spoolman:sm-9:filament:3", "#112233", None),
            "spoolman_unavailable",
        ),
        (
            spoolman("spoolman:sm-off:filament:3", "#112233", None),
            "spoolman_unavailable",
        ),
        (
            spoolman("spoolman:sm-down:filament:3", "#112233", None),
            "spoolman_unavailable",
        ),
    ];
    for (assignment, code) in cases {
        refused_confirm(&seeded, assignment, code).await;
    }

    let (proposal, error) = json_call(
        &seeded.server,
        &seeded.headers,
        TOOL,
        json!({
            "plan_id": seeded.build,
            "assignment": spoolman(filament, "#112233", Some(" spoolman:sm-1:spool:7 ")),
        }),
    )
    .await;
    assert!(!error, "{proposal}");
    let (applied, error) = confirm(
        &seeded.server,
        &seeded.headers,
        proposal["action"]["id"].as_str().unwrap(),
    )
    .await;
    assert!(!error && applied["ok"] == true, "{applied}");
    assert_eq!(
        role_defaults(&seeded)["accent"],
        json!({
            "filament_color_id": filament,
            "filament_custom_hex": null,
            "spoolman_spool_id": "spoolman:sm-1:spool:7",
        })
    );
    let brief = seeded.brief();
    assert_eq!(brief["role_filaments"][0]["inventory_id"], filament);
    // The brief keeps its stored shape: the pin lives on the default and Parts.
    assert!(
        brief["role_filaments"][0]
            .get("spoolman_spool_id")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_spoolman_proposal_applies_with_a_ui_session() {
    let seeded = spoolman_on().await;
    let (proposal, error) = json_call(
        &seeded.server,
        &seeded.headers,
        TOOL,
        json!({
            "plan_id": seeded.build,
            "assignment": spoolman("spoolman:sm-1:filament:3", "#112233", Some("spoolman:sm-1:spool:7")),
        }),
    )
    .await;
    assert!(!error, "{proposal}");
    let id = proposal["action"]["id"].as_str().unwrap();
    let (status, result) =
        super::session_keys::ui(&seeded.server, Method::POST, &format!("/{id}/apply")).await;
    assert_eq!((status, result), (200, json!("applied")));
    assert_eq!(
        role_defaults(&seeded)["accent"]["spoolman_spool_id"],
        "spoolman:sm-1:spool:7"
    );
}

/// Spoolman on for the user and off in the Build's Plan override, as
/// Checkoff reads it: no Spoolman assignment proposes or confirms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_assign_role_filament_honours_the_builds_spoolman_override() {
    let seeded = spoolman_on().await;
    let plan_off = |seeded: &Seeded| {
        execute(
            &seeded.server,
            &format!(
                "REPLACE INTO app_settings (tenant_id, key, value) \
                 VALUES ('default', 'feature_toggles:plan:{}', '{{\"spoolman\":false}}');",
                seeded.build
            ),
        )
    };
    let args = json!({
        "plan_id": seeded.build,
        "assignment": spoolman("spoolman:sm-1:filament:3", "#112233", Some("spoolman:sm-1:spool:7")),
    });
    plan_off(&seeded);
    seeded
        .refuses_propose(TOOL, vec![(args.clone(), "spoolman_module_off")])
        .await;

    // Proposed while on, then the Build turns Spoolman off: confirm refuses,
    // writes nothing and keeps the proposal pending.
    execute(
        &seeded.server,
        &format!(
            "DELETE FROM app_settings WHERE key='feature_toggles:plan:{}';",
            seeded.build
        ),
    );
    let (proposal, error) = json_call(&seeded.server, &seeded.headers, TOOL, args).await;
    assert!(!error, "{proposal}");
    let id = proposal["action"]["id"].as_str().unwrap().to_owned();
    plan_off(&seeded);
    let before = seeded.state();
    let (refused, error) = confirm(&seeded.server, &seeded.headers, &id).await;
    assert!(error, "{refused}");
    assert_eq!(refused["code"], "spoolman_module_off");
    assert_eq!(before, seeded.state());
    assert!(
        pending(&seeded.server, &seeded.headers)
            .await
            .iter()
            .any(|action| action["id"] == id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_assign_role_filament_refuses_a_malformed_or_misplaced_spool_pin_at_propose() {
    let seeded = spoolman_on().await;
    let filament = "spoolman:sm-1:filament:3";
    let mut on_catalog = catalog("#c0afa1");
    on_catalog["spoolman_spool_id"] = json!("spoolman:sm-1:spool:7");
    seeded
        .refuses_propose(
            TOOL,
            [
                spoolman(filament, "#112233", Some("7")),
                spoolman(filament, "#112233", Some("spoolman:sm-1:spool:0")),
                // The pin's integration must be the filament's.
                spoolman(filament, "#112233", Some("spoolman:sm-2:spool:7")),
                on_catalog,
            ]
            .into_iter()
            .map(|assignment| {
                (
                    json!({ "plan_id": seeded.build, "assignment": assignment }),
                    "invalid_filament_assignment",
                )
            })
            .collect(),
        )
        .await;
}

fn role_defaults(seeded: &Seeded) -> Value {
    serde_json::from_str(
        &database(&seeded.server)
            .query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [format!("role_filaments_{}", seeded.build)],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap()
}

async fn refused_confirm(seeded: &Seeded, assignment: Value, code: &str) {
    let before = seeded.state();
    let (proposal, error) = json_call(
        &seeded.server,
        &seeded.headers,
        TOOL,
        json!({ "plan_id": seeded.build, "assignment": assignment }),
    )
    .await;
    assert!(!error, "{proposal}");
    let (refused, error) = confirm(
        &seeded.server,
        &seeded.headers,
        proposal["action"]["id"].as_str().unwrap(),
    )
    .await;
    assert!(error, "{refused}");
    assert_eq!(refused["code"], code, "{refused}");
    assert_eq!(before, seeded.state());
}

fn catalog_id(id: &str) -> Value {
    json!({
        "role": "accent",
        "inventory_kind": "catalog",
        "inventory_id": id,
        "color_hex": "#c0afa1",
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_assign_role_filament_without_a_key_is_refused() {
    let seeded = seeded().await;
    seeded
        .unkeyed_proposes_nothing(
            TOOL,
            json!({ "plan_id": seeded.build, "assignment": catalog("#c0afa1") }),
        )
        .await;
}
