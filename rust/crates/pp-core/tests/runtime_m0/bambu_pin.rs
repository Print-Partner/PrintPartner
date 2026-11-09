//! GRE-1245: the Rust-only launcher's Bambu Test connection pins the
//! printer's certificate, and only a deliberate reset replaces the pin. Every
//! read goes to the loopback TLS broker in `pp_testkit::bambu`; nothing here
//! talks to a real printer.
use super::farm_reads::settings_rows;
use super::ticket_t_4_watcher_proof::{
    proof_data, run_in_child, seed_settings, shut_down, signed_in, start_rust_only,
};
use super::{request, temporary};
use axum::http::StatusCode;
use pp_testkit::bambu::{Broker, Identity, idle_printer};
use serde_json::{Value, json};
use std::{net::SocketAddr, time::Duration};

const SERIAL: &str = "FIXTURE01";
const CODE: &str = "synthetic-code";
const PIN: &str = "/api/v1/integrations/bambu-1/tls-pin";
const RESET: &str = "/api/v1/integrations/bambu-1/tls-pin/reset";
const TEST: &str = "/api/v1/integrations/bambu-1/test";
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

/// Seeds one Bambu integration per `(id, port)`, all on loopback, with the
/// Bambu feature on.
fn seed_bambu(data: &std::path::Path, printers: &[(&str, u16)]) {
    let integrations: Vec<Value> = printers
        .iter()
        .map(|(id, port)| {
            json!({"id": id, "type": "bambu", "config": {
                "host": "127.0.0.1", "port": port,
                "serial": SERIAL, "access_code": CODE,
            }})
        })
        .collect();
    seed_settings(
        data,
        &[
            ("integrations", Value::Array(integrations)),
            ("feature_toggles", json!({"bambu": true})),
        ],
    );
}

/// A broker for one synthetic printer, seeded as integration `bambu-1` with
/// the Bambu feature on.
async fn seeded_printer(data: &std::path::Path) -> (Broker, Identity) {
    let (broker, printer) = idle_printer(CODE, SERIAL).await.unwrap();
    seed_bambu(data, &[("bambu-1", broker.addr.port())]);
    (broker, printer)
}

/// A loopback port that hands its first connection to `first` and every
/// later one to `then`: one printer address behind which the certificate
/// changes between two Test connections.
async fn forwarder(first: SocketAddr, then: SocketAddr) -> (u16, tokio::task::JoinHandle<()>) {
    let front = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let front_port = front.local_addr().unwrap().port();
    let relay = async move {
        let mut targets = std::iter::once(first).chain(std::iter::repeat(then));
        while let (Ok((mut inbound, _)), Some(to)) = (front.accept().await, targets.next()) {
            tokio::spawn(async move {
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(to).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    };
    (front_port, tokio::spawn(relay))
}

/// Waits until `broker` holds a Test connection's pushall reply.
async fn held(broker: &Broker) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !broker.log().iter().any(|line| line == "HELD") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the Test connection reached the printer");
}

/// How long a request must stay blocked behind a held Test connection. The
/// held test stays within the driver's 10-second status timeout.
const BLOCKED: Duration = Duration::from_secs(2);

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

    let replaced = Identity::generate().unwrap();
    broker.present(replaced.clone());
    assert_eq!(
        test().await,
        (StatusCode::OK, json!({"ok": false, "message": CHANGED}))
    );
    assert_eq!(pin().await.1["sha256_prefix"], prefix(&printer.fingerprint));
    assert_eq!(
        connects(&broker.log()),
        1,
        "the changed certificate never got the access code"
    );

    assert_eq!(reset().await, (StatusCode::OK, json!({"reset": true})));
    assert_eq!(pin().await, (StatusCode::OK, json!({"pinned": false})));
    assert_eq!(test().await, connected);
    assert_eq!(
        pin().await.1["sha256_prefix"],
        prefix(&replaced.fingerprint)
    );
    let (code, problem) = call(&origin, &cookie, "GET", "/api/v1/integrations/gone/tls-pin").await;
    assert_eq!(
        (code, &problem["detail"]),
        (StatusCode::NOT_FOUND, &json!("Integration not found"))
    );

    shut_down(runtime).await;
    let log = broker.log();
    assert_eq!(connects(&log), 2, "{log:?}");
    only_status_traffic(&log);
    assert_eq!(settings_rows(&data), settings, "pinning changed settings");
}

#[tokio::test]
async fn ticket_t_23_rust_only_runtime_serializes_pin_races() {
    let data = temporary("bambu-pin-race");
    run_in_child(
        "bambu_pin::ticket_t_23_rust_only_runtime_serializes_pin_races_in_child",
        &data,
    )
    .await;
    let _ = std::fs::remove_dir_all(data);
}

/// Two first-use Test connections race on one integration whose address
/// reaches certificate X first and certificate Y after; a reset races a
/// first-use Test connection on another. Each held test keeps the pin lock.
#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn ticket_t_23_rust_only_runtime_serializes_pin_races_in_child() {
    let data = proof_data();
    let (winner, x) = idle_printer(CODE, SERIAL).await.unwrap();
    let (loser, _) = idle_printer(CODE, SERIAL).await.unwrap();
    let (shared, forwarding) = forwarder(winner.addr, loser.addr).await;
    let (resetting, _) = idle_printer(CODE, SERIAL).await.unwrap();
    seed_bambu(
        &data,
        &[("bambu-1", shared), ("bambu-2", resetting.addr.port())],
    );
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 1).await;
    let spawn = |method: &'static str, path: &'static str| {
        let (origin, cookie) = (origin.clone(), cookie.clone());
        tokio::spawn(async move { call(&origin, &cookie, method, path).await })
    };
    let connected = (
        StatusCode::OK,
        json!({"ok": true, "message": "Connected (LAN MQTT · Idle)"}),
    );

    // Two first-use Test connections: the second waits for the first's pin
    // and then refuses Y without sending it the access code.
    winner.hold();
    let first = spawn("POST", TEST);
    held(&winner).await;
    let mut second = spawn("POST", TEST);
    assert!(
        tokio::time::timeout(BLOCKED, &mut second).await.is_err(),
        "a second Test connection ran while the first held the pin lock"
    );
    assert!(loser.log().is_empty(), "{:?}", loser.log());
    winner.release();
    assert_eq!(first.await.unwrap(), connected);
    assert_eq!(
        second.await.unwrap(),
        (StatusCode::OK, json!({"ok": false, "message": CHANGED}))
    );
    assert_eq!(
        call(&origin, &cookie, "GET", PIN).await.1["sha256_prefix"],
        prefix(&x.fingerprint)
    );
    assert_eq!(connects(&winner.log()), 1, "{:?}", winner.log());
    assert_eq!(connects(&loser.log()), 0, "{:?}", loser.log());
    assert!(
        loser.log().iter().any(|line| line == "TCP"),
        "the second Test connection reached Y: {:?}",
        loser.log()
    );

    // A reset during a first-use Test connection waits for it, so the pin
    // that test stores does not outlive the reset.
    const PIN_2: &str = "/api/v1/integrations/bambu-2/tls-pin";
    resetting.hold();
    let test = spawn("POST", "/api/v1/integrations/bambu-2/test");
    held(&resetting).await;
    let mut reset = spawn("POST", "/api/v1/integrations/bambu-2/tls-pin/reset");
    assert!(
        tokio::time::timeout(BLOCKED, &mut reset).await.is_err(),
        "a reset ran while a Test connection held the pin lock"
    );
    resetting.release();
    assert_eq!(test.await.unwrap(), connected);
    assert_eq!(
        reset.await.unwrap(),
        (StatusCode::OK, json!({"reset": true}))
    );
    assert_eq!(
        call(&origin, &cookie, "GET", PIN_2).await,
        (StatusCode::OK, json!({"pinned": false}))
    );

    shut_down(runtime).await;
    forwarding.abort();
    for log in [winner.log(), loser.log(), resetting.log()] {
        only_status_traffic(&log);
    }
}
