//! GRE-1125: the Rust-only launcher serves the farm reads from storage and
//! the status watchers' observation port, behind an account session, with
//! distinct not-set-up, manual and unreachable reasons and no writes. Since
//! GRE-1175 a PrusaLink printer is watched too, so one with no reachable host
//! reads as unreachable instead of manual.
use super::watcher_proof::{
    SUCCESS, production_rows, proof_data, run_in_child, seed_settings, shut_down, signed_in,
    start_rust_only,
};
use super::{mcp_gateway_support::bearer, request, temporary};
use axum::http::StatusCode;
use pp_testkit::moonraker::RecordedPrinter;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const ROUTES: [&str; 5] = [
    "/api/v2/printers",
    "/api/v2/printer-checkoff",
    "/api/v2/printer-checkoff/unattributed",
    "/api/v2/printer-send-queue",
    "/api/v2/printer-checkoff/unattributed/suggestions",
];

#[tokio::test]
async fn farm_reads_rust_only_runtime_serves_observed_printers() {
    let data = temporary("farm-reads");
    run_in_child("farm_reads::farm_reads_rust_only_runtime_in_child", &data).await;
    let _ = std::fs::remove_dir_all(data);
}

pub(super) fn settings_rows(data: &Path) -> Option<String> {
    let path = data.join("print-partner.db");
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    rusqlite::Connection::open_with_flags(path, flags)
        .unwrap()
        .query_row(
            "SELECT group_concat(tenant_id || char(9) || key || char(9) || value, char(10))
             FROM (SELECT * FROM app_settings ORDER BY tenant_id, key)",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// The status and JSON body of an authenticated farm read.
async fn read(origin: &str, cookie: &str, path: &str) -> (StatusCode, Value) {
    let response = request(origin, path, "GET", Some(cookie), None, None, None).await;
    let body = serde_json::from_slice(&response.2).unwrap_or_default();
    (response.0, body)
}

/// Printers for every reason, one result, one unmatched print and one send.
fn seed_farm(data: &Path, url: &str) {
    seed_settings(
        data,
        &[
            (
                "integrations",
                json!([
                    {"id": "moon-1", "name": "Recorded", "type": "moonraker",
                     "config": {"base_url": url, "api_key": "recorded-key"}},
                    {"id": "tls-1", "type": "moonraker",
                     "config": {"base_url": "https://printer.invalid"}},
                    {"id": "off-1", "type": "moonraker",
                     "config": {"base_url": "http://127.0.0.1:9", "enabled": false}},
                    {"id": "prusa-1", "type": "prusalink",
                     "config": {"base_url": "http://127.0.0.1:9"}},
                    {"id": "dead-1", "type": "moonraker",
                     "config": {"base_url": "http://127.0.0.1:9"}}
                ]),
            ),
            (
                "printer.fleet",
                json!([
                    {"id": "p-moon", "name": "Micron", "integration_id": "moon-1"},
                    {"id": "p-tls", "name": "TLS", "integration_id": "tls-1"},
                    {"id": "p-off", "name": "Off", "integration_id": "off-1"},
                    {"id": "p-prusa", "name": "MK4", "integration_id": "prusa-1"},
                    {"id": "p-dead", "name": "Dead", "integration_id": "dead-1"},
                    {"id": "p-gone", "name": "Gone", "integration_id": "removed-1"},
                    {"id": "p-none", "name": "Annex K3"}
                ]),
            ),
            (
                "printer.checkoff_links",
                json!([{"id": "link-1", "profile_id": 1, "printer_id": "p-moon",
                    "integration_id": "moon-1", "filename": "bracket.gcode",
                    "state": "watching", "units": [], "saw_active": true,
                    "created_at": "2026-11-05T00:00:00.000Z"}]),
            ),
            (
                "printer.unattributed_prints",
                json!([{"id": "print-1", "integration_id": "prusa-1", "printer_id": "p-prusa",
                    "host_name": "MK4", "filename": "stray.gcode",
                    "completed_at": "2026-11-05T01:00:00.000Z", "gcode_objects": [],
                    "candidates": []}]),
            ),
            ("feature_toggles", json!({"farm": true})),
            (
                "printer.send_queue",
                json!([{"id": "send-1", "filename": "next.gcode",
                    "artifact_path": "/disposable/exports/next.gcode", "printer_id": "p-moon",
                    "state": "queued", "start": false, "wait_for_idle": true,
                    "created_at": "2026-11-05T01:00:00.000Z",
                    "updated_at": "2026-11-05T01:00:00.000Z"}]),
            ),
        ],
    );
}

/// One MCP POST with the session's headers: the JSON-RPC message the
/// streamable HTTP answer carries, and the session id it names.
async fn mcp_post(
    client: &reqwest::Client,
    origin: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (Option<String>, Value) {
    let mut request = client
        .post(format!("{origin}/api/v1/mcp"))
        .header("accept", "application/json, text/event-stream")
        .header("origin", origin)
        .json(&body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request.send().await.unwrap();
    let session = response
        .headers()
        .get("mcp-session-id")
        .map(|value| value.to_str().unwrap().to_owned());
    let mut text = String::new();
    while let Ok(Ok(Some(chunk))) =
        tokio::time::timeout(Duration::from_secs(5), response.chunk()).await
    {
        text.push_str(std::str::from_utf8(&chunk).unwrap());
        if text.contains("\"result\"") {
            break;
        }
    }
    let message = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .find_map(|data| serde_json::from_str(data.trim()).ok())
        .unwrap_or(Value::Null);
    (session, message)
}

/// `get_farm_status` over MCP with an API key, through the runtime's own
/// watchers: the observations bound into MCP when the gateway started.
async fn farm_status_over_mcp(origin: &str, cookie: &str) -> Value {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let bearer = bearer(&client, origin, cookie).await;
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "farm-reads", "version": "0"}}});
    let auth = [("authorization", bearer.as_str())];
    let (session, _) = mcp_post(&client, origin, &auth, initialize).await;
    let session = session.expect("an MCP session");
    let headers = [
        ("authorization", bearer.as_str()),
        ("mcp-session-id", session.as_str()),
        ("mcp-protocol-version", "2025-06-18"),
    ];
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    mcp_post(&client, origin, &headers, initialized).await;
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "get_farm_status", "arguments": {}}});
    let (_, message) = mcp_post(&client, origin, &headers, call).await;
    let result = &message["result"];
    assert_ne!(result["isError"], true, "{message}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// Polls `GET /api/v2/printers` until the watched printers have a status.
async fn observed_printers(origin: &str, cookie: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (code, body) = read(origin, cookie, ROUTES[0]).await;
            assert_eq!(code, StatusCode::OK, "{body}");
            let observed = |id: &str| {
                body["printers"].as_array().unwrap().iter().any(|p| {
                    p["id"] == id
                        && p["reason"] != "awaiting_status"
                        && p["state"]["execution"] != "unknown"
                })
            };
            if ["p-moon", "p-dead", "p-prusa"].into_iter().all(observed) {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the runtime never served observed printers")
}

/// Every reason is distinct, and no address or key reaches the body.
fn check_printers(printers: &Value) {
    let summary: Vec<_> = printers["printers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            json!([
                p["id"],
                p["state"]["execution"],
                p["state"]["display"],
                p["reason"]
            ])
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            json!(["p-moon", "printing", "printing", null]),
            json!(["p-tls", "manual", "manual", "monitoring_not_supported"]),
            json!(["p-off", "manual", "manual", "monitoring_disabled"]),
            json!(["p-prusa", "unreachable", "unreachable", "unreachable"]),
            json!(["p-dead", "unreachable", "unreachable", "unreachable"]),
            json!(["p-gone", "not_set_up", "not_set_up", "integration_missing"]),
            json!(["p-none", "not_set_up", "not_set_up", "not_set_up"]),
        ]
    );
    let moon = &printers["printers"][0];
    assert_eq!(moon["state"]["bed_clear"], "needs_clearing");
    assert_eq!(moon["progress_percent"], 43);
    assert_eq!(
        moon["integration"],
        json!({"id": "moon-1", "kind": "moonraker"})
    );
    let heard = moon["last_heard_at"].as_str().expect("last heard time");
    assert!(heard.ends_with('Z') && heard.len() == 24, "{heard}");
    assert_eq!(printers["printers"][4]["last_heard_at"], Value::Null);
    assert_eq!(
        printers["printers"][3]["state"]["bed_clear"],
        "needs_clearing"
    );
    let text = printers.to_string();
    assert!(
        !text.contains("127.0.0.1") && !text.contains("recorded-key"),
        "{text}"
    );
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn farm_reads_rust_only_runtime_in_child() {
    let data = proof_data();
    let printer = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
    seed_farm(&data, printer.url());

    let (runtime, origin, desktop) = start_rust_only(&data).await;
    // Opening the writer moved the seeded queue into `printer_send_queue`
    // (GRE-1208); the reads below must leave that state as they found it.
    let rows = production_rows(&data);
    let settings = settings_rows(&data);
    assert!(
        !settings
            .as_deref()
            .unwrap_or("")
            .contains("printer.send_queue")
    );
    assert!(
        rows.iter()
            .any(|(table, values)| table == "printer_send_queue" && values.len() == 1),
        "{rows:?}"
    );
    for path in ROUTES {
        let (code, body) = read(&origin, &desktop, path).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{path}: {body}");
    }
    let cookie = signed_in(&origin, &desktop, 1).await;
    check_printers(&observed_printers(&origin, &cookie).await);

    // MCP reads the same watchers (GRE-1352): the observations bound into it
    // when the gateway started, with no request of its own to a printer.
    let status = farm_status_over_mcp(&origin, &cookie).await;
    let moon = &status["printers"][0];
    assert_eq!(
        (&moon["id"], &moon["state"], &moon["progress"]),
        (&json!("p-moon"), &json!("printing"), &json!(43)),
        "{status}"
    );
    assert_eq!(
        moon["active_job"].as_str().map(str::trim),
        Some("bracket.gcode")
    );
    assert_eq!(status["printers"][6]["not_connected_reason"], "not_set_up");
    assert_eq!(status["printer_count"], 7);

    let (code, results) = read(&origin, &cookie, ROUTES[1]).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(results["results"][0]["state"], "watching");
    let (code, prints) = read(&origin, &cookie, ROUTES[2]).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(prints["prints"][0]["id"], "print-1");
    let (code, queue) = read(&origin, &cookie, ROUTES[3]).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(queue["items"][0]["state"], "queued");
    assert!(!queue.to_string().contains("/disposable/exports"));
    let (code, suggestions) = read(&origin, &cookie, ROUTES[4]).await;
    assert_eq!(code, StatusCode::OK, "{suggestions}");
    let row = &suggestions["suggestions"][0];
    assert_eq!(
        (
            &row["print_id"],
            &row["confidence"],
            &row["starts_checked"],
            &row["build"]
        ),
        (
            &json!("print-1"),
            &json!("none"),
            &json!(false),
            &Value::Null
        ),
        "{suggestions}"
    );

    shut_down(runtime).await;
    for line in &printer.request_lines() {
        assert!(
            line.starts_with("GET /printer/objects/query?"),
            "monitoring sent {line}"
        );
    }
    assert_eq!(
        production_rows(&data),
        rows,
        "farm reads changed production rows"
    );
    // The API key MCP was called with is the one setting the test wrote.
    let without_keys = |rows: Option<String>| -> Vec<String> {
        rows.unwrap_or_default()
            .lines()
            .filter(|line| !line.starts_with("default\tapi_keys_v1\t"))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        without_keys(settings_rows(&data)),
        without_keys(settings),
        "farm reads changed settings"
    );
    println!(
        "farm_reads: {} status requests, all status GETs",
        printer.calls()
    );
}
