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

    let brief = seeded.brief();
    assert!(brief["draft_id"].is_null());
    assert_eq!(brief["requirements"][0]["status"], "satisfied");
    assert_eq!(brief["requirements"][1]["status"], "unverified");
    assert_eq!(brief["role_filaments"].as_array().unwrap().len(), 1);
    assert_eq!(brief["role_filaments"][0]["inventory_id"], COLOR);
    let defaults: Value = serde_json::from_str(
        &database(&seeded.server)
            .query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [format!("role_filaments_{}", seeded.build)],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(defaults["Accent"]["filament_color_id"], COLOR);
    assert!(defaults["Accent"]["filament_custom_hex"].is_null());
    assert!(defaults["Accent"]["spoolman_spool_id"].is_null());
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
    let defaults: Value = serde_json::from_str(
        &database(&seeded.server)
            .query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [format!("role_filaments_{}", seeded.build)],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert!(defaults["Accent"]["filament_color_id"].is_null());
    assert_eq!(defaults["Accent"]["filament_custom_hex"], "#112233");
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

    execute(
        &seeded.server,
        "UPDATE app_settings SET value='{\"orders\":true,\"spoolman\":true}' WHERE key='feature_toggles';",
    );
    let (proposal, error) = json_call(
        &seeded.server,
        &seeded.headers,
        TOOL,
        json!({
            "plan_id": seeded.build,
            "assignment": {
                "role": "accent",
                "inventory_kind": "spoolman",
                "inventory_id": "spoolman:1",
                "color_hex": "#112233",
            }
        }),
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
    assert_eq!(refused["code"], "spoolman_unavailable");
    assert_eq!(
        seeded.brief()["role_filaments"].as_array().unwrap().len(),
        0
    );
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
