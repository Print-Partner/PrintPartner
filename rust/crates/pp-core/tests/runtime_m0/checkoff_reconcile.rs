//! GRE-1897: in the Rust-only launcher, a recorded Moonraker print of a sent
//! file that goes from printing to `complete` makes that send's `watching`
//! Checkoff link `awaiting_verify` with no page action. The printer only ever
//! gets status reads: nothing is sent to it. The observer's ordering and
//! retries are unit-tested in `pp_core::checkoff_reconcile`.
use super::watcher_proof::{
    SUCCESS, proof_data, run_in_child, seed_settings, shut_down, signed_in, start_rust_only,
};
use super::{request, temporary};
use pp_testkit::moonraker::RecordedPrinter;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const COMPLETE: &[u8] = br#"{"result":{"status":{"print_stats":{"state":"complete","filename":"bracket.gcode"},"virtual_sdcard":{"progress":1.0}}}}"#;

#[tokio::test]
async fn gre_1897_a_finished_moonraker_print_awaits_verify_in_the_rust_only_runtime() {
    let data = temporary("checkoff-reconcile");
    run_in_child(
        "checkoff_reconcile::gre_1897_finished_print_in_child",
        &data,
    )
    .await;
    let _ = std::fs::remove_dir_all(data);
}

/// The stored `link-sent`, read while the runtime runs.
fn link(data: &Path) -> Value {
    let connection = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let sql =
        "SELECT value FROM app_settings WHERE tenant_id='default' AND key='printer.checkoff_links'";
    let raw: String = connection.query_row(sql, [], |row| row.get(0)).unwrap();
    serde_json::from_str::<Value>(&raw).unwrap()[0].clone()
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn gre_1897_finished_print_in_child() {
    let data = proof_data();
    let printer =
        RecordedPrinter::start(vec![(200, SUCCESS), (200, SUCCESS), (200, COMPLETE)]).unwrap();
    let created = pp_storage::read_model::farm::iso_time(std::time::SystemTime::now());
    let sent = json!({
        "id": "link-sent", "profile_id": 1, "integration_id": "moon-1", "printer_id": "p-moon",
        "filename": "bracket.gcode", "state": "watching", "started": true,
        "created_at": created, "units": [{"part_id": 1, "unit_index": 0}],
    });
    let host = json!({"id": "moon-1", "name": "Recorded", "type": "moonraker",
                      "config": {"base_url": printer.url(), "api_key": "recorded-key"}});
    let settings = [
        ("integrations", json!([host])),
        ("printer.checkoff_links", json!([sent])),
    ];
    seed_settings(&data, &settings);

    let (runtime, _, _) = start_rust_only(&data).await;
    let finished = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let link = link(&data);
            if link["state"] != "watching" {
                return link;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the finished print's link never moved on");
    shut_down(runtime).await;
    let outcome = (
        &finished["state"],
        &finished["host_outcome"],
        &finished["last_progress"],
    );
    assert_eq!(
        outcome,
        (&json!("awaiting_verify"), &json!("success"), &json!(43)),
        "{finished}"
    );
    let lines = printer.request_lines();
    let reads = |line: &String| line.starts_with("GET /printer/objects/query?print_stats");
    assert!(lines.iter().all(reads), "{lines:?}");
}

#[tokio::test]
async fn gre_1897_a_finish_replayed_after_offline_moves_no_second_started_link() {
    let data = temporary("checkoff-replay");
    run_in_child(
        "checkoff_reconcile::gre_1897_replayed_finish_in_child",
        &data,
    )
    .await;
    let _ = std::fs::remove_dir_all(data);
}

/// Each stored link's state, by ID.
fn states(data: &Path) -> Vec<(String, String)> {
    let connection = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let sql =
        "SELECT value FROM app_settings WHERE tenant_id='default' AND key='printer.checkoff_links'";
    let raw: String = connection.query_row(sql, [], |row| row.get(0)).unwrap();
    let mut links: Vec<(String, String)> = serde_json::from_str::<Vec<Value>>(&raw)
        .unwrap()
        .iter()
        .map(|link| {
            (
                link["id"].as_str().unwrap().into(),
                link["state"].as_str().unwrap().into(),
            )
        })
        .collect();
    links.sort();
    links
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn gre_1897_replayed_finish_in_child() {
    let data = proof_data();
    let offline: &[u8] = br#"{"error":{"code":503,"message":"Klippy not connected"}}"#;
    // The printer shows one finish, drops one poll, then shows the same finish
    // again until the test ends.
    let printer =
        RecordedPrinter::start(vec![(200, COMPLETE), (503, offline), (200, COMPLETE)]).unwrap();
    let created = pp_storage::read_model::farm::iso_time(std::time::SystemTime::now());
    let send = |id: &str, saw_active: bool| {
        json!({
            "id": id, "profile_id": 1, "integration_id": "moon-1", "printer_id": "p-moon",
            "filename": "bracket.gcode", "state": "watching", "started": true,
            "saw_active": saw_active, "created_at": created,
            "units": [{"part_id": 1, "unit_index": 0}],
        })
    };
    let host = json!({"id": "moon-1", "name": "Recorded", "type": "moonraker",
                      "config": {"base_url": printer.url(), "api_key": "recorded-key"}});
    let settings = [
        ("integrations", json!([host])),
        (
            "printer.fleet",
            json!([{"id": "p-moon", "name": "Micron", "integration_id": "moon-1"}]),
        ),
        ("feature_toggles", json!({"farm": true})),
        (
            "printer.checkoff_links",
            json!([send("link-older", false), send("link-current", true)]),
        ),
    ];
    seed_settings(&data, &settings);

    let (runtime, origin, desktop) = start_rust_only(&data).await;
    tokio::time::timeout(Duration::from_secs(60), async {
        while printer.calls() < 5 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the printer never showed the finish again after the dropped poll");
    // The observer writes each status as soon as it is read.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let cookie = signed_in(&origin, &desktop, 1).await;
    let farm = request(
        &origin,
        "/api/v2/printer-checkoff",
        "GET",
        Some(&cookie),
        None,
        None,
        None,
    )
    .await;
    let stored = states(&data);
    shut_down(runtime).await;

    assert_eq!(farm.0, axum::http::StatusCode::OK);
    let farm: Value = serde_json::from_slice(&farm.2).unwrap();
    let mut shown: Vec<(String, String)> = farm["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().into(),
                r["state"].as_str().unwrap().into(),
            )
        })
        .collect();
    shown.sort();
    let expected = [
        ("link-current".to_owned(), "awaiting_verify".to_owned()),
        ("link-older".to_owned(), "watching".to_owned()),
    ];
    assert_eq!(stored, expected, "stored links");
    assert_eq!(
        shown, expected,
        "the farm read that feeds Today offers one bed"
    );
    let lines = printer.request_lines();
    let reads = |line: &String| line.starts_with("GET /printer/objects/query?print_stats");
    assert!(lines.iter().all(reads), "{lines:?}");
}
