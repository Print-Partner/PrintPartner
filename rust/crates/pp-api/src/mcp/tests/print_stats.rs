//! `get_print_stats` (GRE-1352 part 2) through the MCP router with an API
//! key, the Farm Module on. In the last 8 hours printer A had two plates, one
//! completed with 120 g and one failed, and printer B finished one print no
//! Plan claims. Twenty hours ago A had one more plate and a Checkoff result.
//! The fixture's Build 1 has an Accepted Plan; Build 3, which the plates
//! were sent for, has none, and Build 4 is archived.
use super::reads::{call, session};
use super::*;
use pp_storage::read_model::farm::iso_time;
use std::time::SystemTime;

fn ago(hours: u64) -> String {
    iso_time(SystemTime::now() - Duration::from_secs(hours * 3600))
}

fn seed() -> String {
    let setting = |key: &str, value: Value| {
        format!(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','{key}','{}') \
             ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value;",
            value.to_string().replace('\'', "''")
        )
    };
    let job = |id: &str, status: &str, grams: &str, at: &str| {
        format!(
            "INSERT INTO print_jobs(id,tenant_id,profile_id,printer_id,status,filament_consumed_g,at) \
             VALUES('{id}','default',3,'p-a','{status}',{grams},'{at}');"
        )
    };
    [
        // Four of Build 1's units are printed.
        "UPDATE print_progress SET completed=1 WHERE id IN (1,2,3,4);".into(),
        "INSERT INTO build_profiles(tenant_id,id,name) VALUES('default',3,'Empty Build');".into(),
        "INSERT INTO build_profiles(tenant_id,id,name,archived_at) \
         VALUES('default',4,'Archived Build','2026-11-01T00:00:00.000Z');"
            .into(),
        setting("feature_toggles", json!({ "farm": true })),
        setting(
            "printer.fleet",
            json!([{ "id": "p-a", "name": "Printer A" }, { "id": "p-b", "name": "Printer B" }]),
        ),
        setting(
            "printer.checkoff_links",
            json!([{ "id": "link-1", "profile_id": 3, "printer_id": "p-a",
                     "integration_id": "moon-a", "filename": "old.gcode", "state": "verified",
                     "host_outcome": "success", "units": [], "created_at": ago(21),
                     "completed_at": ago(20) }]),
        ),
        setting(
            "printer.unattributed_prints",
            json!([{ "id": "print-1", "integration_id": "moon-b", "printer_id": "p-b",
                     "filename": "stray.gcode", "completed_at": ago(2),
                     "gcode_objects": [], "candidates": [] }]),
        ),
        job("job-1", "completed", "120", &ago(2)),
        job("job-2", "failed", "NULL", &ago(1)),
        job("job-3", "completed", "40.5", &ago(20)),
    ]
    .concat()
}

async fn keyed() -> (McpServer, Vec<(String, String)>) {
    let server = McpServer::seeded(&seed()).await;
    // Build 1's Accepted Plan reads its Source snapshot from here.
    std::fs::create_dir_all(server._directory.join("repos/1/revisions/fixture")).unwrap();
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let headers = session(&server, &bearer).await;
    (server, headers)
}

async fn stats(server: &McpServer, headers: &[(String, String)], args: Value) -> Value {
    let (text, error) = call(server, headers, "get_print_stats", args).await;
    assert!(!error, "{text}");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_print_stats_counts_the_window() {
    let (server, headers) = keyed().await;
    let database = server._directory.join("print-partner.db");
    let before = pp_testkit::mcp_planning::state(&database).unwrap();

    let stats_8h = stats(&server, &headers, json!({})).await;
    assert_eq!(stats_8h["window_hours"], json!(8));
    assert!(stats_8h["since"].is_string());
    assert_eq!(
        [
            &stats_8h["plates_sent"],
            &stats_8h["plates_completed"],
            &stats_8h["plates_failed"],
            &stats_8h["completion_rate"],
            &stats_8h["filament_consumed_g"],
            &stats_8h["empty_window"],
        ],
        [
            &json!(2),
            &json!(1),
            &json!(1),
            &json!(0.5),
            &json!(120),
            &json!(false)
        ]
    );
    assert_eq!(
        stats_8h["by_printer"],
        json!([{ "printer_id": "p-a", "printer_name": "Printer A", "plates_sent": 2,
                 "plates_completed": 1, "plates_failed": 1, "completion_rate": 0.5,
                 "filament_consumed_g": 120 }])
    );
    assert_eq!(
        stats_8h["prints_seen_on_printers"],
        json!({ "total": 1, "attached_to_plan": 0, "not_attached_to_plan": 1, "by_printer": [
            { "printer_id": "p-b", "printer_name": "Printer B", "seen": 1, "completed": 1,
              "failed": 0, "cancelled": 0 } ] })
    );

    // Active plans: Build 1 as get_remaining counts it, Build 3 empty, Build 4
    // archived and Build 2 another installation's, both absent.
    let plans = stats_8h["active_plans"].as_array().unwrap();
    let ids: Vec<_> = plans.iter().map(|plan| plan["plan_id"].clone()).collect();
    assert_eq!(
        ids,
        vec![json!(3), json!(1)],
        "by name: Empty Build, Fixture Build"
    );
    let (remaining, error) =
        call(&server, &headers, "get_remaining", json!({ "plan_id": 1 })).await;
    assert!(!error, "{remaining}");
    let remaining: Value = serde_json::from_str(&remaining).unwrap();
    let fixture = &plans[1]["accepted_progress"];
    assert_eq!(fixture["kind"], "ready");
    for field in ["total_units", "remaining_units", "printed_units"] {
        assert_eq!(fixture[field], remaining[field], "{field}");
    }
    assert_eq!(plans[0]["accepted_progress"], json!({ "kind": "empty" }));

    // A wider window takes in the older plate and the Checkoff result.
    let stats_24h = stats(&server, &headers, json!({ "hours": 24 })).await;
    assert_eq!(stats_24h["window_hours"], json!(24));
    assert_eq!(stats_24h["plates_sent"], json!(3));
    assert_eq!(
        stats_24h["filament_consumed_g"],
        json!(160.5),
        "fractional grams add up"
    );
    assert_eq!(stats_24h["prints_seen_on_printers"]["total"], json!(2));
    assert_eq!(
        stats_24h["prints_seen_on_printers"]["attached_to_plan"],
        json!(1)
    );

    // A window with nothing in it.
    let quiet = stats(&server, &headers, json!({ "hours": 0.25 })).await;
    assert_eq!(
        [
            &quiet["plates_sent"],
            &quiet["prints_seen_on_printers"]["total"],
            &quiet["empty_window"]
        ],
        [&json!(0), &json!(0), &json!(true)]
    );
    assert_eq!(quiet["completion_rate"], Value::Null);

    assert_eq!(
        before,
        pp_testkit::mcp_planning::state(&database).unwrap(),
        "nothing written"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_print_stats_refuses_a_bad_window_with_a_code() {
    let (server, headers) = keyed().await;
    for hours in [json!(0), json!(-2), json!("8"), json!(2161), json!(true)] {
        let (text, error) = call(
            &server,
            &headers,
            "get_print_stats",
            json!({ "hours": hours.clone() }),
        )
        .await;
        assert!(error, "{hours}: {text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["code"],
            "invalid_hours",
            "{hours}"
        );
    }
}
