//! GRE-1176: the status watchers keep one long-lived MQTT session per
//! enabled, pinned Bambu host, under the same lifecycle as the Moonraker and
//! PrusaLink pollers. Simulated broker only; the broker log shows every packet
//! the watcher sent.
use pp_drivers::{
    bambu::{CERTIFICATE_CHANGED, Config, Fingerprint, status_from_print},
    moonraker::{Host, HostKind, PrinterHostState, PrinterObservations, Watchers},
};
use pp_net::outbound::{HttpGuard, SystemResolver};
use pp_testkit::bambu::{Broker, Identity, Reply, Script};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const CODE: &str = "synthetic-code";
const SERIAL: &str = "FIXTURE01";
const PUSHALL: &str =
    r#"PUBLISH device/FIXTURE01/request {"pushing":{"sequence_id":"0","command":"pushall"}}"#;

fn printing() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/drivers/bambu/pushall-printing.json"
    ))
    .unwrap()
}

fn watchers() -> Watchers {
    let guard = HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(2), 1 << 20).unwrap();
    Watchers::with_guard(guard, Duration::from_millis(20)).without_node_fallback()
}

fn bambu(id: &str, broker: &Broker, pin: Result<Fingerprint, String>, enabled: bool) -> Host {
    let config = json!({"host": "127.0.0.1", "port": broker.addr.port(), "serial": SERIAL, "access_code": CODE});
    Host {
        id: id.into(),
        base_url: "127.0.0.1".into(),
        api_key: None,
        enabled,
        kind: HostKind::Bambu {
            config: Config::from_stored(&config),
            pin,
        },
    }
}

/// A broker that answers pushall with the recorded printing report, then
/// `after`.
async fn printer(after: Vec<Reply>) -> (Broker, Identity) {
    let identity = Identity::generate().unwrap();
    let mut script = Script::reporting(CODE, SERIAL, &printing());
    script.replies.extend(after);
    (
        Broker::start(identity.clone(), script).await.unwrap(),
        identity,
    )
}

fn change(print: Value) -> Reply {
    Reply::Publish {
        topic: format!("device/{SERIAL}/report"),
        payload: json!({ "print": print }).to_string().into_bytes(),
    }
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

fn count(broker: &Broker, line: &str) -> usize {
    broker.log().iter().filter(|seen| *seen == line).count()
}

/// The watcher sent only what a status read needs.
fn status_only(broker: &Broker) {
    for line in broker.log() {
        assert!(
            [
                "TCP",
                "TLS",
                "CONNECT bblp access code",
                "SUBSCRIBE device/FIXTURE01/report",
                PUSHALL
            ]
            .contains(&line.as_str()),
            "the printer saw {line}"
        );
    }
}

fn full_report() -> Value {
    serde_json::from_slice::<Value>(&printing()).unwrap()["print"].clone()
}

#[tokio::test]
async fn ticket_t_25_bambu_watcher_merges_changes_into_the_full_report() {
    let (broker, identity) = printer(vec![change(json!({"mc_percent": 77}))]).await;
    let mut watchers = watchers();
    let observations = watchers.observations();
    watchers
        .replace(vec![bambu("x1c", &broker, Ok(identity.fingerprint), true)])
        .await
        .unwrap();

    let full = status_from_print(&full_report());
    assert_eq!(full.state, PrinterHostState::Printing);
    until("the change is merged", || {
        observations
            .latest("x1c")
            .is_some_and(|status| status.progress == Some(77.0))
    })
    .await;
    let merged = observations.latest("x1c").unwrap();
    assert_eq!(
        (merged.state, &merged.filename, merged.message.as_deref()),
        (
            PrinterHostState::Printing,
            &full.filename,
            full.message.as_deref()
        ),
        "a change keeps every field it does not carry"
    );
    assert!(observations.last_heard("x1c").is_some());

    watchers.stop().await;
    assert!(observations.latest("x1c").is_none());
    assert_eq!(count(&broker, PUSHALL), 1);
    status_only(&broker);
}

#[tokio::test]
async fn ticket_t_25_bambu_watcher_never_pins_and_never_trusts_a_changed_certificate() {
    let (broker, identity) = printer(Vec::new()).await;
    let other = Identity::generate().unwrap();
    let mut watchers = watchers();
    let observations = watchers.observations();
    let unpinned = "Certificate not pinned yet. Run Test connection in Settings.";
    watchers
        .replace(vec![
            bambu("unpinned", &broker, Err(unpinned.into()), true),
            bambu("changed", &broker, Ok(other.fingerprint), true),
            bambu("off", &broker, Ok(identity.fingerprint), false),
        ])
        .await
        .unwrap();

    let message = |id: &str| observations.latest(id).and_then(|status| status.message);
    assert_eq!(message("unpinned").as_deref(), Some(unpinned));
    assert_eq!(message("off").as_deref(), Some("Integration disabled"));
    until("the changed certificate is refused", || {
        message("changed").as_deref() == Some(CERTIFICATE_CHANGED)
    })
    .await;
    watchers.stop().await;

    let log = broker.log();
    assert!(
        log.iter()
            .all(|line| line == "TCP" || line == "TLS refused"),
        "only the changed host connected, and its TLS handshake failed before CONNECT: {log:?}"
    );
}

#[tokio::test]
async fn ticket_t_25_bambu_reconnect_resumes_without_a_second_pushall() {
    let (broker, identity) = printer(vec![Reply::Close]).await;
    let mut watchers = watchers();
    let observations = watchers.observations();
    let mut changes = observations.transitions();
    watchers
        .replace(vec![bambu("x1c", &broker, Ok(identity.fingerprint), true)])
        .await
        .unwrap();

    // Printing, offline when the printer closes the session, then printing
    // again from the held report once the watcher reconnects.
    let mut states = Vec::new();
    until("the reconnected session reports the held status", || {
        while let Ok(change) = changes.try_recv() {
            states.push(change.status.state);
        }
        states.ends_with(&[
            PrinterHostState::Printing,
            PrinterHostState::Offline,
            PrinterHostState::Printing,
        ])
    })
    .await;
    assert_eq!(count(&broker, "CONNECT bblp access code"), 2);
    assert_eq!(
        count(&broker, PUSHALL),
        1,
        "pushall is sent at most every five minutes"
    );
    watchers.stop().await;
    status_only(&broker);
}

#[tokio::test]
async fn ticket_t_25_bambu_suspend_ends_the_session_and_resume_starts_one() {
    let (broker, identity) = printer(Vec::new()).await;
    let mut watchers = watchers();
    let observations: PrinterObservations = watchers.observations();
    let hosts = vec![bambu("x1c", &broker, Ok(identity.fingerprint), true)];
    for _ in 0..5 {
        watchers.replace(hosts.clone()).await.unwrap();
    }
    until("the printer reports", || {
        observations.last_heard("x1c").is_some()
    })
    .await;
    assert_eq!(
        count(&broker, "TCP"),
        1,
        "a repeated snapshot keeps the session"
    );

    // Suspend is an empty snapshot; resume is the hosts again.
    watchers.replace(Vec::new()).await.unwrap();
    assert!(observations.latest("x1c").is_none());
    let suspended = broker.log().len();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        broker.log().len(),
        suspended,
        "a suspended watcher stays silent"
    );

    watchers.replace(hosts).await.unwrap();
    until("the resumed watcher reports", || {
        observations
            .latest("x1c")
            .is_some_and(|status| status.state == PrinterHostState::Printing)
    })
    .await;
    assert_eq!(count(&broker, "TCP"), 2);
    watchers.stop().await;
    status_only(&broker);
}
