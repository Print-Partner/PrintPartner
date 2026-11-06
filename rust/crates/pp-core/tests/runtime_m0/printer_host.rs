//! GRE-360 and GRE-1044: the Rust-only launcher serves the Moonraker and
//! PrusaLink printer-host reads (capabilities, print-file browse, cameras and
//! the connection test) and the streamed print-file download and camera view
//! (GRE-1174) from stored settings through the gateway's drivers, behind an
//! account session. Results match the Node adapters' oracles for
//! the same recorded traffic, and the printers see only read-only GETs: no
//! upload and no print start.
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
const PRUSALINK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/drivers/prusalink"
);

#[tokio::test]
async fn ticket_t_21_ticket_t_22_rust_only_runtime_serves_host_reads() {
    let data = temporary("printer-host");
    run_in_child(
        "printer_host::ticket_t_21_ticket_t_22_rust_only_runtime_host_reads_in_child",
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
    let mut routes: Vec<_> = routes["routes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(target, name)| (target.clone(), 200, fixture(name.as_str().unwrap())))
        .collect();
    routes.push((
        "/server/files/gcodes/plates/bracket.gcode".into(),
        200,
        b"G28\n".to_vec(),
    ));
    routes.push((
        "/webcam/?action=stream".into(),
        200,
        b"mjpeg-frame".to_vec(),
    ));
    let oracle: Value = serde_json::from_slice(&fixture("node-oracle.json")).unwrap();
    (
        RoutedPrinter::start(routes).unwrap(),
        oracle["recorded"].clone(),
    )
}

/// A recorded PrusaLink from its `routes.json` plus `/api/v1/info` and
/// `/api/v1/status`, and the Node adapter's read results for that traffic.
/// It answers without a Digest challenge, so the driver sends no credentials.
fn recorded_prusalink() -> (RoutedPrinter, Value) {
    let read = |name: &str| std::fs::read(Path::new(PRUSALINK).join(name)).unwrap();
    let routes: Value = serde_json::from_slice(&read("driver/routes.json")).unwrap();
    let mut routes: Vec<_> = routes["routes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(target, name)| {
            let body = read(&format!("driver/{}", name.as_str().unwrap()));
            (target.clone(), 200, body)
        })
        .collect();
    routes.push(("/api/v1/info".into(), 200, read("info.json")));
    routes.push(("/api/v1/status".into(), 200, read("status-printing.json")));
    let metadata = json!({"refs": {"download": "/api/v1/files/usb/BENCHY~1.BGC/raw"}});
    routes.push((
        "/api/v1/files/usb/BENCHY~1.BGC".into(),
        200,
        metadata.to_string().into_bytes(),
    ));
    routes.push((
        "/api/v1/cameras/cam1/snap".into(),
        200,
        b"jpeg-snapshot".to_vec(),
    ));
    let oracle: Value = serde_json::from_slice(&read("driver/node-oracle-reads.json")).unwrap();
    (
        RoutedPrinter::start(routes).unwrap(),
        oracle["recorded"].clone(),
    )
}

fn seed(data: &Path, url: &str, prusalink: &str) {
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
                     "config": {"base_url": prusalink, "username": "fixture-user",
                                "password": "synthetic-fixture-password"}},
                    {"id": "bambu-1", "type": "bambu", "config": {}},
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
                    {"id": "p-bambu", "name": "P1S", "integration_id": "bambu-1"},
                    {"id": "p-odd", "name": "Odd", "integration_id": "odd-1"},
                    {"id": "p-gone", "name": "Gone", "integration_id": "removed-1"},
                    {"id": 7, "name": "Annex K3"}
                ]),
            ),
        ],
    );
}

async fn get(origin: &str, cookie: &str, path: String) -> (StatusCode, Value) {
    call(origin, Some(cookie), "GET", &path).await
}

async fn call(origin: &str, cookie: Option<&str>, method: &str, path: &str) -> (StatusCode, Value) {
    let (code, _, body) = request(origin, path, method, cookie, None, None, None).await;
    (code, serde_json::from_slice(&body).unwrap_or_default())
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_21_ticket_t_22_rust_only_runtime_host_reads_in_child() {
    let data = proof_data();
    let (printer, oracle) = recorded();
    let (prusa, prusa_oracle) = recorded_prusalink();
    seed(&data, printer.url(), prusa.url());
    let rows = production_rows(&data);
    let settings = settings_rows(&data);
    let (runtime, origin, desktop) = start_rust_only(&data).await;

    for (method, path) in [
        ("GET", "/api/v1/printers/p-moon/capabilities"),
        ("GET", "/api/v1/printers/p-moon/files"),
        ("GET", "/api/v1/printers/p-moon/cameras"),
        ("POST", "/api/v1/integrations/moon-1/test"),
        ("GET", "/printers/p-moon/capabilities"),
        ("GET", "/printers/p-moon/files"),
        ("GET", "/printers/p-moon/cameras"),
        (
            "GET",
            "/printers/p-moon/files/content?path=plates%2Fbracket.gcode",
        ),
        ("GET", "/printers/p-moon/cameras/view?id=camera-0"),
    ] {
        let (code, body) = call(&origin, Some(&desktop), method, path).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{path}: {body}");
    }
    let cookie = signed_in(&origin, &desktop, 1).await;
    capabilities_and_files(&origin, &cookie, &oracle).await;
    cameras_and_test(&origin, &cookie, &oracle, &prusa_oracle).await;
    streams(&origin, &cookie).await;

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
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("GET /webcam/?action=stream ")),
        "{lines:?}"
    );
    let prusa_lines = prusa.request_lines();
    for target in [
        "/api/v1/files/usb/",
        "/api/v1/files/usb/BENCHY~1.BGC/raw",
        "/api/v1/cameras/cam1/snap",
    ] {
        assert!(
            prusa_lines
                .iter()
                .any(|line| line.starts_with(&format!("GET {target} "))),
            "{target}: {prusa_lines:?}"
        );
    }
    for line in lines.iter().chain(&prusa_lines) {
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

/// Capabilities and print-file browse for every linked, unlinked and
/// unported printer, at the root and under `/api/v1`.
async fn capabilities_and_files(origin: &str, cookie: &str, oracle: &Value) {
    let get = |path: String| get(origin, cookie, path);

    let all = json!({"files": true, "cameras": true, "status": true});
    let none = json!({"files": false, "cameras": false, "status": false});
    let unwatched = json!({"files": true, "cameras": true, "status": false});
    for (printer, expected) in [
        ("p-moon", &all),
        ("p-off", &none),
        ("p-down", &all),
        ("p-prusa", &unwatched),
        ("p-bambu", &none),
        ("p-odd", &none),
        ("p-gone", &none),
        ("7", &none),
    ] {
        let read = get(format!("/api/v1/printers/{printer}/capabilities")).await;
        assert_eq!(read, (StatusCode::OK, expected.clone()), "{printer}");
    }
    // The web client calls the printer routes at the root, as Node serves them.
    let root = get("/printers/p-moon/capabilities".into()).await;
    assert_eq!(root, (StatusCode::OK, all.clone()));
    let (code, problem) = get("/api/v1/printers/p-missing/capabilities".into()).await;
    assert_eq!(
        (code, &problem["detail"]),
        (StatusCode::NOT_FOUND, &json!("Printer not found"))
    );

    let root = get("/api/v1/printers/p-moon/files".into()).await;
    assert_eq!(root, (StatusCode::OK, oracle["browse"][""]["ok"].clone()));
    let plates = get("/printers/p-moon/files?path=%2Fplates%2F".into()).await;
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
            "p-bambu/files",
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
}

/// Moonraker and PrusaLink cameras, PrusaLink browse and the connection test.
async fn cameras_and_test(origin: &str, cookie: &str, oracle: &Value, prusa_oracle: &Value) {
    let get = |path: String| get(origin, cookie, path);
    let cameras = get("/printers/p-moon/cameras".into()).await;
    assert_eq!(
        cameras,
        (StatusCode::OK, json!({"cameras": oracle["cameras"]["ok"]}))
    );
    let unported = get("/api/v1/printers/p-bambu/cameras".into()).await;
    assert_eq!(unported, (StatusCode::OK, json!({"cameras": []})));

    for path in ["", "SUB"] {
        let listing = get(format!("/api/v1/printers/p-prusa/files?path={path}")).await;
        let expected = prusa_oracle["browse"][path]["ok"].clone();
        assert!(expected.is_object(), "{path}");
        assert_eq!(listing, (StatusCode::OK, expected), "PrusaLink {path:?}");
    }
    let cameras = get("/api/v1/printers/p-prusa/cameras".into()).await;
    assert_eq!(
        cameras,
        (
            StatusCode::OK,
            json!({"cameras": prusa_oracle["cameras"]["ok"]})
        )
    );

    let test = |id: &str| {
        let (origin, cookie, path) = (
            origin.to_owned(),
            cookie.to_owned(),
            format!("/api/v1/integrations/{id}/test"),
        );
        async move { call(&origin, Some(&cookie), "POST", &path).await }
    };
    assert_eq!(
        test("moon-1").await,
        (StatusCode::OK, oracle["test_connection"].clone())
    );
    let (code, _, _) = request(
        origin,
        "/integrations/moon-1/test",
        "POST",
        Some(cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        code,
        StatusCode::NOT_FOUND,
        "Node serves the test only under /api/v1"
    );
    let (code, problem) = test("removed-1").await;
    assert_eq!(
        (code, &problem["detail"]),
        (StatusCode::NOT_FOUND, &json!("Integration not found"))
    );
    let (code, probe) = test("prusa-1").await;
    assert_eq!(
        (code, &probe["ok"]),
        (StatusCode::OK, &json!(true)),
        "{probe}"
    );
    assert!(
        probe["message"]
            .as_str()
            .unwrap()
            .starts_with("Connected ("),
        "{probe}"
    );
    assert_eq!(test("bambu-1").await.0, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        test("odd-1").await,
        (
            StatusCode::OK,
            json!({"ok": false, "message": "Unknown integration type: mystery"})
        )
    );
}

/// Status, `Content-Length`, `Cache-Control` and body of one streamed GET.
async fn stream(origin: &str, cookie: &str, path: &str) -> (u16, [Option<String>; 2], Vec<u8>) {
    let (code, headers, body) = request(origin, path, "GET", Some(cookie), None, None, None).await;
    let header = |name: &str| {
        headers
            .get(name)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    (
        code.as_u16(),
        [header("content-length"), header("cache-control")],
        body,
    )
}

/// The print-file download and camera view stream the recorded printers'
/// bytes through, and refuse what Node refuses before reading a printer body.
async fn streams(origin: &str, cookie: &str) {
    // The route sends Node's "private, no-store"; the gateway then stamps
    // every admitted reply "no-store", as for every other Rust route.
    let private = |length: usize| [Some(length.to_string()), Some("no-store".into())];
    let file = stream(
        origin,
        cookie,
        "/printers/p-moon/files/content?path=%2Fplates%2Fbracket.gcode",
    )
    .await;
    assert_eq!(file, (200, private(4), b"G28\n".to_vec()));
    let view = stream(
        origin,
        cookie,
        "/api/v1/printers/p-moon/cameras/view?id=55d3b9f7-3c2b-4f6e-9a8e-1c1f2b7a0e01",
    )
    .await;
    assert_eq!(view, (200, private(11), b"mjpeg-frame".to_vec()));
    // RoutedPrinter serves bodies as lossy UTF-8.
    let gcode = std::fs::read(Path::new(PRUSALINK).join("driver/print-file.gcode")).unwrap();
    let gcode = String::from_utf8_lossy(&gcode).into_owned().into_bytes();
    let prusa = stream(
        origin,
        cookie,
        "/printers/p-prusa/files/content?path=BENCHY~1.BGC",
    )
    .await;
    assert_eq!(prusa, (200, private(gcode.len()), gcode));
    let snap = stream(origin, cookie, "/printers/p-prusa/cameras/view?id=cam1").await;
    assert_eq!(snap, (200, private(13), b"jpeg-snapshot".to_vec()));

    for (path, code, detail) in [
        (
            "p-moon/files/content?path=plates%2Fmissing.gcode",
            404,
            "Printer file not found",
        ),
        (
            "p-moon/files/content?path=..%2Fescape",
            400,
            "path is not a valid printer storage path",
        ),
        ("p-moon/files/content", 400, "path is required"),
        (
            "p-moon/files/content?path=plates%2Fbracket.gcode&profile_id=1",
            501,
            "Print-file inspection for a Build is not available in this runtime yet",
        ),
        (
            "p-bambu/files/content?path=a.gcode",
            501,
            "Printer host does not support file browsing",
        ),
        (
            "7/files/content?path=a.gcode",
            409,
            "Printer is not linked to a host",
        ),
        (
            "p-moon/cameras/view?id=cross-origin-stream",
            404,
            "Printer camera not found",
        ),
        (
            "p-moon/cameras/view?id=disabled",
            404,
            "Printer camera not found",
        ),
        ("p-moon/cameras/view", 400, "id is required"),
        ("p-bambu/cameras/view?id=0", 404, "Printer camera not found"),
        (
            "p-prusa/cameras/view?id=cam3",
            404,
            "Printer camera not found",
        ),
    ] {
        let (status, problem) = get(origin, cookie, format!("/api/v1/printers/{path}")).await;
        assert_eq!(
            (status.as_u16(), problem["detail"].as_str()),
            (code, Some(detail)),
            "{path}"
        );
    }
}
