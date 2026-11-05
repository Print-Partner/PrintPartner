//! GRE-449 / GRE-335: the Rust-only launcher start polls the Moonraker hosts
//! stored in the `integrations` setting and serves their status through the
//! gateway, with no Node child, no Node fallback and no Checkoff or queue
//! writes. The child process holds the explicit `PP_DESKTOP_REBUILD` value.
use super::{desktop_cookie, launch, request, temporary};
use axum::http::StatusCode;
use pp_core::CoreRuntime;
use pp_storage::{Limits, Setting, SettingCommand, WriterOwner};
use pp_testkit::moonraker::RecordedPrinter;
use serde_json::{Value, json};
use std::{path::Path, sync::atomic::AtomicBool, time::Duration};

pub(super) const SUCCESS: &[u8] =
    include_bytes!("../../../../../fixtures/drivers/moonraker/moonraker-success.json");

#[tokio::test]
async fn ticket_t_4_watcher_proof_rust_only_runtime_polls_stored_hosts() {
    let data = temporary("watcher-proof");
    let stdout = run_in_child(
        "ticket_t_4_watcher_proof::ticket_t_4_watcher_proof_rust_only_runtime_in_child",
        &data,
    )
    .await;
    for line in stdout
        .lines()
        .filter(|line| line.starts_with("ticket_t_4_watcher_proof"))
    {
        println!("{line}");
    }
    let _ = std::fs::remove_dir_all(data);
}

/// Runs the ignored test `test` in a child process with `PP_DESKTOP_REBUILD=1`
/// and `PP_PROOF_DATA` naming `data`, and returns its standard output.
pub(super) async fn run_in_child(test: &str, data: &Path) -> String {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--ignored", "--exact", test, "--nocapture"])
        .env("PP_PROOF_DATA", data)
        .env("PP_DESKTOP_REBUILD", "1")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(240), child.output())
        .await
        .expect("the proof exceeded four minutes")
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("1 passed"), "the child ran no test");
    stdout
}

/// The data directory `run_in_child` passed to this child.
pub(super) fn proof_data() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("PP_PROOF_DATA").unwrap())
}

/// Stores each `(key, value)` setting for tenant `default` before any start.
pub(super) fn seed_settings(data: &Path, settings: &[(&str, Value)]) {
    let data = pp_storage::lease::prepare_directory(data).unwrap();
    let (writer, _) =
        WriterOwner::open_with_job_feature_admission(&data, Limits::default()).unwrap();
    for (key, value) in settings {
        let setting = Setting {
            tenant: "default".into(),
            key: (*key).into(),
            value: value.to_string(),
        };
        let reply = writer
            .client()
            .submit(
                SettingCommand::Set(setting),
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(reply.recv().unwrap().unwrap());
    }
    writer.shutdown().unwrap();
}

/// Starts the Rust-only launcher on `data` and returns it with its origin and
/// a desktop cookie.
pub(super) async fn start_rust_only(data: &Path) -> (CoreRuntime, String, String) {
    let mut runtime = CoreRuntime::start_for_launcher(launch(data.to_owned()))
        .await
        .unwrap();
    assert!(runtime.rust_only());
    let origin = runtime.origin().to_owned();
    let desktop = desktop_cookie(&mut runtime).await;
    (runtime, origin, desktop)
}

/// Shuts the runtime down within ten seconds and returns once it is complete.
pub(super) async fn shut_down(runtime: CoreRuntime) {
    let receipt = tokio::time::timeout(Duration::from_secs(10), runtime.shutdown())
        .await
        .expect("shutdown exceeded ten seconds");
    assert!(receipt.complete(), "{receipt:?}");
}

/// Every row of the Checkoff, progress, job and queue tables that exist.
pub(super) fn production_rows(data: &Path) -> Vec<(String, Vec<String>)> {
    let connection = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables.retain(|name| {
        [
            "progress",
            "checkoff",
            "send",
            "queue",
            "print_job",
            "required_unit",
            "plate",
            "assembl",
        ]
        .iter()
        .any(|part| name.contains(part))
    });
    assert!(tables.iter().any(|name| name == "print_jobs"), "{tables:?}");
    tables
        .into_iter()
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get_ref(column).map(|value| format!("{value:?}")))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|values| values.join(" | "))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

/// A print job on the watched host, so the no-write proof compares real
/// column values and not only empty tables.
fn seed_print_job(data: &Path) {
    let connection = rusqlite::Connection::open(data.join("print-partner.db")).unwrap();
    connection
        .execute(
            "INSERT INTO build_profiles (id, tenant_id, name) VALUES (1, 'default', 'Watched Build')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO print_jobs (id, tenant_id, profile_id, host_integration_id, filename, at)
             VALUES ('job-watched', 'default', 1, 'moon-1', 'bracket.gcode', '2026-11-05T00:00:00.000Z')",
            [],
        )
        .unwrap();
}

/// Sign in to the single desktop account: register on the first start, log
/// in after a restart. Returns the desktop and account cookies together.
pub(super) async fn signed_in(origin: &str, desktop: &str, start: u32) -> String {
    let path = if start == 1 {
        "/auth/register"
    } else {
        "/auth/login"
    };
    let (code, headers, body) = request(
        origin,
        path,
        "POST",
        Some(desktop),
        None,
        None,
        Some(json!({"email": "watcher@example.test", "password": "password-1234"})),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let session = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    format!("{desktop}; {session}")
}

async fn status(origin: &str, cookie: &str, id: &str) -> (StatusCode, Value) {
    let path = format!("/api/v1/integrations/{id}/status");
    let (code, _, body) = request(origin, &path, "GET", Some(cookie), None, None, None).await;
    (code, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

async fn printing(origin: &str, cookie: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (code, body) = status(origin, cookie, "moon-1").await;
            if code == StatusCode::OK && body["state"] == "printing" {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the runtime never served the recorded Moonraker status")
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_4_watcher_proof_rust_only_runtime_in_child() {
    let data = proof_data();
    let printer = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
    let url = printer.url();
    seed_settings(
        &data,
        &[(
            "integrations",
            json!([
                {"id": "moon-1", "name": "Recorded", "type": "moonraker",
                 "config": {"base_url": url, "api_key": "recorded-key"}},
                {"id": "tls-1", "name": "TLS", "type": "moonraker",
                 "config": {"base_url": "https://printer.invalid"}},
                {"id": "spool-1", "name": "Spools", "type": "spoolman",
                 "config": {"base_url": "http://127.0.0.1:9"}},
            ]),
        )],
    );
    seed_print_job(&data);
    let before = production_rows(&data);
    assert!(
        before
            .iter()
            .any(|(table, rows)| table == "print_jobs" && rows.len() == 1),
        "{before:?}"
    );

    for start in 1..=2u32 {
        let (runtime, origin, desktop) = start_rust_only(&data).await;
        // The route authenticates like every Rust route: a desktop session
        // alone is not an account session.
        let (code, body) = status(&origin, &desktop, "moon-1").await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{body}");
        let cookie = signed_in(&origin, &desktop, start).await;
        let body = printing(&origin, &cookie).await;
        assert_eq!(
            body,
            json!({"state": "printing", "progress": 43.0, "filename": "bracket.gcode",
                "message": "Printing bracket.gcode", "ip_address": "127.0.0.1",
                "nozzle_temperature_c": 210.2, "nozzle_target_c": 215.0,
                "bed_temperature_c": 59.8, "bed_target_c": 60.0})
        );
        // HTTPS waits for GRE-359; with no Node it reports offline, unpolled.
        let (code, body) = status(&origin, &cookie, "tls-1").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            body,
            json!({"state": "offline", "message": "HTTPS printer monitoring is not available yet"})
        );
        // A non-Moonraker id is not the watchers', and no Node answers it.
        let (code, body) = status(&origin, &cookie, "spool-1").await;
        assert_eq!(code, StatusCode::NOT_FOUND, "start {start}");
        assert_eq!(body["detail"], "Integration not found");
        // Polling continues with no client asking.
        let seen = printer.calls();
        tokio::time::sleep(Duration::from_millis(4500)).await;
        assert!(printer.calls() > seen, "start {start}: polling stalled");
        shut_down(runtime).await;
        let stopped = printer.calls();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(
            printer.calls(),
            stopped,
            "start {start}: poller outlived shutdown"
        );
        println!(
            "ticket_t_4_watcher_proof start {start}: {stopped} status requests, then none after shutdown"
        );
    }
    for line in &printer.request_lines() {
        assert_eq!(
            line,
            &format!(
                "GET {} HTTP/1.1",
                "/printer/objects/query?print_stats&virtual_sdcard&display_status&extruder&heater_bed"
            )
        );
    }
    assert_eq!(
        production_rows(&data),
        before,
        "monitoring changed production rows"
    );
}
