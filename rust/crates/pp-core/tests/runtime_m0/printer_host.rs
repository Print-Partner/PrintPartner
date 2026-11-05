//! GRE-360: the Rust-only launcher serves the Moonraker printer-host reads
//! (capabilities, print-file browse, cameras and the connection test) from
//! stored settings through the gateway's driver, behind an account session.
//! Results match the Node adapter's oracle for the same recorded traffic, and
//! the printer sees only read-only GETs: no upload and no print start.
use super::farm_reads::settings_rows;
use super::ticket_t_4_watcher_proof::{
    production_rows, proof_data, run_in_child, seed_settings, shut_down, signed_in, start_rust_only,
};
use super::{request, temporary};
use axum::http::StatusCode;
use pp_testkit::moonraker::RoutedPrinter;
use serde_json::{Value, json};
use std::path::Path;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/drivers/moonraker/driver"
);

#[tokio::test]
async fn ticket_t_21_rust_only_runtime_serves_moonraker_host_reads() {
    let data = temporary("printer-host");
    run_in_child(
        "printer_host::ticket_t_21_rust_only_runtime_host_reads_in_child",
        &data,
    )
    .await;
    let _ = std::fs::remove_dir_all(data);
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

/// The recorded Moonraker from `routes.json`, and the Node adapter's results
/// for the same traffic from `node-oracle.json`.
fn recorded() -> (RoutedPrinter, Value) {
    let routes: Value = serde_json::from_slice(&fixture("routes.json")).unwrap();
    let routes = routes["routes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(target, name)| (target.clone(), 200, fixture(name.as_str().unwrap())))
        .collect();
    let oracle: Value = serde_json::from_slice(&fixture("node-oracle.json")).unwrap();
    (
        RoutedPrinter::start(routes).unwrap(),
        oracle["recorded"].clone(),
    )
}

fn seed(data: &Path, url: &str) {
    seed_settings(
        data,
        &[
            (
                "integrations",
                json!([
                    {"id": "moon-1", "type": "moonraker",
                     "config": {"base_url": url, "api_key": "recorded-key"}},
                    {"id": "off-1", "type": "moonraker",
                     "config": {"base_url": url, "enabled": false}},
                    {"id": "down-1", "type": "moonraker",
                     "config": {"base_url": "http://127.0.0.1:9"}},
                    {"id": "prusa-1", "type": "prusalink",
                     "config": {"base_url": "http://127.0.0.1:9"}},
                    {"id": "odd-1", "type": "mystery", "config": {}}
                ]),
            ),
            (
                "printer.fleet",
                json!([
                    {"id": "p-moon", "name": "Micron", "integration_id": " moon-1 "},
                    {"id": "p-off", "name": "Off", "integration_id": "off-1"},
                    {"id": "p-down", "name": "Down", "integration_id": "down-1"},
                    {"id": "p-prusa", "name": "MK4", "integration_id": "prusa-1"},
                    {"id": "p-odd", "name": "Odd", "integration_id": "odd-1"},
                    {"id": "p-gone", "name": "Gone", "integration_id": "removed-1"},
                    {"id": 7, "name": "Annex K3"}
                ]),
            ),
        ],
    );
}

async fn call(origin: &str, cookie: Option<&str>, method: &str, path: &str) -> (StatusCode, Value) {
    let (code, _, body) = request(origin, path, method, cookie, None, None, None).await;
    (code, serde_json::from_slice(&body).unwrap_or_default())
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_21_rust_only_runtime_host_reads_in_child() {
    let data = proof_data();
    let (printer, oracle) = recorded();
    seed(&data, printer.url());
    let rows = production_rows(&data);
    let settings = settings_rows(&data);
    let (runtime, origin, desktop) = start_rust_only(&data).await;

    for (method, path) in [
        ("GET", "/api/v1/printers/p-moon/capabilities"),
        ("GET", "/api/v1/printers/p-moon/files"),
        ("GET", "/api/v1/printers/p-moon/cameras"),
        ("POST", "/api/v1/integrations/moon-1/test"),
    ] {
        let (code, body) = call(&origin, Some(&desktop), method, path).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{path}: {body}");
    }
    let cookie = signed_in(&origin, &desktop, 1).await;
    let get = |path: String| {
        let (origin, cookie) = (origin.clone(), cookie.clone());
        async move { call(&origin, Some(&cookie), "GET", &path).await }
    };

    let all = json!({"files": true, "cameras": true, "status": true});
    let none = json!({"files": false, "cameras": false, "status": false});
    for (printer, expected) in [
        ("p-moon", &all),
        ("p-off", &none),
        ("p-down", &all),
        ("p-prusa", &none),
        ("p-odd", &none),
        ("p-gone", &none),
        ("7", &none),
    ] {
        let read = get(format!("/api/v1/printers/{printer}/capabilities")).await;
        assert_eq!(read, (StatusCode::OK, expected.clone()), "{printer}");
    }
    let (code, problem) = get("/api/v1/printers/p-missing/capabilities".into()).await;
    assert_eq!(
        (code, &problem["detail"]),
        (StatusCode::NOT_FOUND, &json!("Printer not found"))
    );

    let root = get("/api/v1/printers/p-moon/files".into()).await;
    assert_eq!(root, (StatusCode::OK, oracle["browse"][""]["ok"].clone()));
    let plates = get("/api/v1/printers/p-moon/files?path=%2Fplates%2F".into()).await;
    assert_eq!(
        plates,
        (StatusCode::OK, oracle["browse"]["/plates/"]["ok"].clone())
    );
    for (path, code, detail) in [
        (
            "p-moon/files?path=..%2Fescape",
            400,
            "path is not a valid printer storage path",
        ),
        ("7/files", 409, "Printer is not linked to a host"),
        ("p-off/files", 503, "Printer host is unavailable"),
        ("p-odd/files", 503, "Printer host is unavailable"),
        (
            "p-prusa/files",
            501,
            "Printer host does not support file browsing",
        ),
    ] {
        let (status, problem) = get(format!("/api/v1/printers/{path}")).await;
        assert_eq!(
            (status.as_u16(), problem["detail"].as_str()),
            (code, Some(detail)),
            "{path}"
        );
    }
    let (status, _) = get("/api/v1/printers/p-down/files".into()).await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "an unreachable host is an upstream failure"
    );

    let cameras = get("/api/v1/printers/p-moon/cameras".into()).await;
    assert_eq!(
        cameras,
        (StatusCode::OK, json!({"cameras": oracle["cameras"]["ok"]}))
    );
    let unported = get("/api/v1/printers/p-prusa/cameras".into()).await;
    assert_eq!(unported, (StatusCode::OK, json!({"cameras": []})));

    let test = |id: &str| {
        let (origin, cookie, path) = (
            origin.clone(),
            cookie.clone(),
            format!("/api/v1/integrations/{id}/test"),
        );
        async move { call(&origin, Some(&cookie), "POST", &path).await }
    };
    assert_eq!(
        test("moon-1").await,
        (StatusCode::OK, oracle["test_connection"].clone())
    );
    let (code, problem) = test("removed-1").await;
    assert_eq!(
        (code, &problem["detail"]),
        (StatusCode::NOT_FOUND, &json!("Integration not found"))
    );
    assert_eq!(test("prusa-1").await.0, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        test("odd-1").await,
        (
            StatusCode::OK,
            json!({"ok": false, "message": "Unknown integration type: mystery"})
        )
    );

    shut_down(runtime).await;
    let lines = printer.request_lines();
    for target in [
        "/server/info",
        "/server/webcams/list",
        "/server/files/directory?path=gcodes&extended=true",
    ] {
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("GET {target} HTTP/1.1")),
            "{target}: {lines:?}"
        );
    }
    for line in &lines {
        assert!(line.starts_with("GET "), "the printer saw {line}");
        assert!(
            !line.contains("/print/") && !line.contains("/upload"),
            "the printer saw {line}"
        );
    }
    assert_eq!(
        production_rows(&data),
        rows,
        "host reads changed production rows"
    );
    assert_eq!(
        settings_rows(&data),
        settings,
        "host reads changed settings"
    );
    println!(
        "ticket_t_21: {} printer requests, all read-only GETs",
        lines.len()
    );
}
