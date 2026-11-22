//! GRE-1897: in the Rust-only launcher, a recorded Moonraker print of a sent
//! file that goes from printing to `complete` makes that send's `watching`
//! Checkoff link `awaiting_verify` with no page action, and leaves a send of
//! another file watching. A restart that sees the same finish changes
//! nothing. The printer only ever gets status reads: nothing is sent to it.
use super::temporary;
use super::watcher_proof::{
    SUCCESS, proof_data, run_in_child, seed_settings, shut_down, start_rust_only,
};
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

fn sent(id: &str, filename: &str, created_at: &str) -> Value {
    json!({
        "id": id, "profile_id": 1, "integration_id": "moon-1", "printer_id": "p-moon",
        "host_name": "Recorded", "filename": filename, "remote_path": filename,
        "upload_job_id": format!("job-{id}"), "state": "watching", "started": true,
        "created_at": created_at, "units": [{"part_id": 1, "unit_index": 0}],
    })
}

fn links(data: &Path) -> Vec<Value> {
    let connection = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let raw: String = connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='printer.checkoff_links'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str::<Value>(&raw)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn link(data: &Path, id: &str) -> Value {
    links(data)
        .into_iter()
        .find(|link| link["id"] == id)
        .unwrap()
}

/// Waits until the printer has answered `polls` more status reads.
async fn polled(printer: &RecordedPrinter, polls: usize) {
    let target = printer.calls() + polls;
    tokio::time::timeout(Duration::from_secs(20), async {
        while printer.calls() < target {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the watcher stopped polling");
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn gre_1897_finished_print_in_child() {
    let data = proof_data();
    let printer =
        RecordedPrinter::start(vec![(200, SUCCESS), (200, SUCCESS), (200, COMPLETE)]).unwrap();
    let created = pp_storage::read_model::farm::iso_time(std::time::SystemTime::now());
    seed_settings(
        &data,
        &[
            (
                "integrations",
                json!([{"id": "moon-1", "name": "Recorded", "type": "moonraker",
                        "config": {"base_url": printer.url(), "api_key": "recorded-key"}}]),
            ),
            (
                "printer.checkoff_links",
                json!([
                    sent("link-sent", "bracket.gcode", &created),
                    sent("link-other", "other.gcode", &created),
                ]),
            ),
        ],
    );

    let (runtime, _, _) = start_rust_only(&data).await;
    let finished = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let link = link(&data, "link-sent");
            if link["state"] != "watching" {
                return link;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the finished print's link never moved on");
    assert_eq!(
        (
            &finished["state"],
            &finished["host_outcome"],
            &finished["saw_active"]
        ),
        (&json!("awaiting_verify"), &json!("success"), &json!(true)),
        "{finished}"
    );
    assert_eq!(finished["last_progress"], 43, "{finished}");
    let completed_at = finished["completed_at"].clone();
    assert!(completed_at.as_str().is_some_and(|at| !at.is_empty()));
    polled(&printer, 2).await;
    assert_eq!(link(&data, "link-sent"), finished, "a repeated finish");
    assert_eq!(link(&data, "link-other")["state"], "watching");
    shut_down(runtime).await;

    let (runtime, _, _) = start_rust_only(&data).await;
    polled(&printer, 3).await;
    assert_eq!(
        link(&data, "link-sent"),
        finished,
        "a restart that sees the same finish changes nothing"
    );
    assert_eq!(link(&data, "link-other")["state"], "watching");
    shut_down(runtime).await;

    let lines = printer.request_lines();
    assert!(
        lines
            .iter()
            .all(|line| line.starts_with("GET /printer/objects/query?print_stats")),
        "{lines:?}"
    );
    println!("gre_1897 link-sent awaiting_verify at {completed_at}");
}
