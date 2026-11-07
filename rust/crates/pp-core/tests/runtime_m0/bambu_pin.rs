//! GRE-1245: the Rust-only launcher's Bambu Test connection pins the
//! printer's certificate, and only a deliberate reset replaces the pin.
//! GRE-1176: the status watcher connects only once a pin is stored, and only
//! to that certificate. Every read goes to the loopback TLS broker in
//! `pp_testkit::bambu`; nothing here talks to a real printer.
use super::farm_reads::settings_rows;
use super::ticket_t_4_watcher_proof::{
    proof_data, run_in_child, seed_settings, shut_down, signed_in, start_rust_only,
};
use super::{request, temporary};
use axum::http::StatusCode;
use pp_testkit::bambu::{Broker, Identity, idle_printer};
use serde_json::{Value, json};

const SERIAL: &str = "FIXTURE01";
const CODE: &str = "synthetic-code";
const PIN: &str = "/api/v1/integrations/bambu-1/tls-pin";
const RESET: &str = "/api/v1/integrations/bambu-1/tls-pin/reset";
const TEST: &str = "/api/v1/integrations/bambu-1/test";
const STATUS: &str = "/api/v1/integrations/bambu-1/status";
const CHANGED: &str = "Printer certificate changed. If you replaced or reset the printer, reset its certificate in Settings.";

#[tokio::test]
async fn ticket_t_23_rust_only_runtime_pins_on_test_connection() {
    let data = temporary("bambu-pin");
    run_in_child(
        "bambu_pin::ticket_t_23_rust_only_runtime_pins_in_child",
        &data,
    )
    .await;
    let _ = std::fs::remove_dir_all(data);
}

async fn call(origin: &str, cookie: &str, method: &str, path: &str) -> (StatusCode, Value) {
    let (code, _, body) = request(origin, path, method, Some(cookie), None, None, None).await;
    (code, serde_json::from_slice(&body).unwrap_or_default())
}

fn prefix(fingerprint: &[u8; 32]) -> Value {
    json!(hex::encode(&fingerprint[..6]))
}

/// A broker for one synthetic printer, seeded as integration `bambu-1` with
/// the Bambu feature on.
async fn seeded_printer(data: &std::path::Path) -> (Broker, Identity) {
    let (broker, printer) = idle_printer(CODE, SERIAL).await.unwrap();
    seed_settings(
        data,
        &[
            (
                "integrations",
                json!([{"id": "bambu-1", "type": "bambu", "config": {
                    "host": "127.0.0.1", "port": broker.addr.port(),
                    "serial": SERIAL, "access_code": CODE,
                }}]),
            ),
            ("feature_toggles", json!({"bambu": true})),
            (
                "printer.fleet",
                json!([{"id": "p-x1c", "name": "X1C", "integration_id": "bambu-1"}]),
            ),
        ],
    );
    (broker, printer)
}

/// Polls the status route until the watcher reports `message`.
async fn watched(origin: &str, cookie: &str, message: &str) {
    let mut last = Value::Null;
    for _ in 0..100 {
        let (code, status) = call(origin, cookie, "GET", STATUS).await;
        if code == StatusCode::OK && status["message"] == message {
            return;
        }
        last = status;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the watcher never reported {message}: {last}");
}

fn connects(log: &[String]) -> usize {
    log.iter()
        .filter(|line| line.starts_with("CONNECT"))
        .count()
}

/// Every PUBLISH the printer saw was pushall, and every CONNECT carried the
/// right access code.
fn only_status_traffic(log: &[String]) {
    for line in log {
        assert!(
            !line.starts_with("PUBLISH ")
                || line.ends_with(r#"{"pushing":{"sequence_id":"0","command":"pushall"}}"#),
            "the printer saw {line}"
        );
        assert!(!line.contains("wrong access code"), "{line}");
    }
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_23_rust_only_runtime_pins_in_child() {
    let data = proof_data();
    let (broker, printer) = seeded_printer(&data).await;
    let settings = settings_rows(&data);
    let (runtime, origin, desktop) = start_rust_only(&data).await;

    for (method, path) in [("GET", PIN), ("POST", RESET), ("POST", TEST)] {
        let (code, body) = call(&origin, &desktop, method, path).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{path}: {body}");
    }
    assert!(
        broker.log().is_empty(),
        "an unauthenticated request reached the printer"
    );

    let cookie = signed_in(&origin, &desktop, 1).await;
    let pin = || call(&origin, &cookie, "GET", PIN);
    let test = || call(&origin, &cookie, "POST", TEST);
    let reset = || call(&origin, &cookie, "POST", RESET);
    assert_eq!(pin().await, (StatusCode::OK, json!({"pinned": false})));
    // With no pin the watcher shows why and never connects.
    watched(
        &origin,
        &cookie,
        "Certificate not pinned yet. Run Test connection in Settings.",
    )
    .await;
    assert!(broker.log().is_empty(), "{:?}", broker.log());
    assert_eq!(
        call(
            &origin,
            &cookie,
            "GET",
            "/api/v1/printers/p-x1c/capabilities"
        )
        .await,
        (
            StatusCode::OK,
            json!({"files": false, "cameras": false, "status": true})
        )
    );

    let connected = (
        StatusCode::OK,
        json!({"ok": true, "message": "Connected (LAN MQTT · Idle)"}),
    );
    assert_eq!(test().await, connected);
    let (code, pinned) = pin().await;
    assert_eq!(
        (code, &pinned["pinned"], &pinned["sha256_prefix"]),
        (StatusCode::OK, &json!(true), &prefix(&printer.fingerprint))
    );
    assert!(
        pinned["pinned_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z')),
        "{pinned}"
    );
    // The stored pin starts the watcher, which reads the printer's status.
    watched(&origin, &cookie, "Idle").await;

    let replaced = Identity::generate().unwrap();
    let before = broker.log().len();
    broker.present(replaced.clone());
    assert_eq!(
        test().await,
        (StatusCode::OK, json!({"ok": false, "message": CHANGED}))
    );
    assert_eq!(pin().await.1["sha256_prefix"], prefix(&printer.fingerprint));
    assert_eq!(
        connects(&broker.log()[before..]),
        0,
        "the changed certificate never got the access code"
    );

    assert_eq!(reset().await, (StatusCode::OK, json!({"reset": true})));
    assert_eq!(pin().await, (StatusCode::OK, json!({"pinned": false})));
    assert_eq!(test().await, connected);
    assert_eq!(
        pin().await.1["sha256_prefix"],
        prefix(&replaced.fingerprint)
    );
    watched(&origin, &cookie, "Idle").await;
    let (code, problem) = call(&origin, &cookie, "GET", "/api/v1/integrations/gone/tls-pin").await;
    assert_eq!(
        (code, &problem["detail"]),
        (StatusCode::NOT_FOUND, &json!("Integration not found"))
    );

    shut_down(runtime).await;
    let log = broker.log();
    assert!(connects(&log) >= 2, "{log:?}");
    only_status_traffic(&log);
    assert_eq!(settings_rows(&data), settings, "pinning changed settings");
}
