//! Bambu LAN status-only driver (T-23, GRE-1244). Every read here goes to the
//! loopback TLS broker in `pp_testkit::bambu`, with a certificate generated per
//! test. The status mapping replays `fixtures/drivers/bambu/driver/prints.json`
//! against `node-oracle-status.json` from the Node adapter. Nothing here talks
//! to a real printer.
use pp_api_types::printer_status::{PrinterHostState as State, PrinterHostStatus as Status};
use pp_drivers::bambu::{
    CERTIFICATE_CHANGED, CLOSED, Config, Driver, TIMED_OUT, Trust, status_from_print,
};
use pp_net::outbound::{HttpGuard, Resolution, Resolver, SystemResolver};
use pp_testkit::bambu::{Broker, Identity, Reply, Script, publish};
use serde_json::{Value, json};
use std::{collections::BTreeMap, net::IpAddr, sync::Arc, time::Duration};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/drivers/bambu"
);
const SERIAL: &str = "FIXTURE01";
const CODE: &str = "synthetic-code";
const PUSHALL: &str = r#"{"pushing":{"sequence_id":"0","command":"pushall"}}"#;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).unwrap()
}

fn guard() -> HttpGuard {
    HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(5), 1024).unwrap()
}

fn driver(host: &str, port: u16) -> Driver {
    let config = Config::from_stored(&json!({
        "host": host, "port": port, "serial": SERIAL, "access_code": CODE,
    }))
    .unwrap();
    Driver::new(guard(), config).with_timeouts(Duration::from_secs(2), Duration::from_secs(3))
}

/// The exchange Node's adapter has with a printer, as the broker logs it.
fn exchange() -> Vec<String> {
    vec![
        "TCP".into(),
        "TLS".into(),
        "CONNECT bblp access code".into(),
        format!("SUBSCRIBE device/{SERIAL}/report"),
        format!("PUBLISH device/{SERIAL}/request {PUSHALL}"),
        "DISCONNECT".into(),
    ]
}

async fn settled(broker: &Broker, want: usize) -> Vec<String> {
    for _ in 0..100 {
        let log = broker.log();
        if log.len() >= want {
            return log;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    broker.log()
}

#[test]
fn ticket_t_23_status_mapping_matches_the_node_oracle() {
    let cases: BTreeMap<String, Value> =
        serde_json::from_slice(&fixture("driver/prints.json")).unwrap();
    let oracle: BTreeMap<String, Status> =
        serde_json::from_slice(&fixture("driver/node-oracle-status.json")).unwrap();
    assert_eq!(
        cases.keys().collect::<Vec<_>>(),
        oracle.keys().collect::<Vec<_>>()
    );
    for (name, case) in &cases {
        let print = match case.get("file").and_then(Value::as_str) {
            Some(file) => {
                let report: Value =
                    serde_json::from_slice(&fixture(file.trim_start_matches("../"))).unwrap();
                report["print"].clone()
            }
            None => case["print"].clone(),
        };
        assert_eq!(&status_from_print(&print), &oracle[name], "{name}");
    }
}

#[test]
fn ticket_t_23_config_reads_node_fields_and_refuses_unsafe_serials() {
    let config = Config::from_stored(&json!({
        "ip": " 192.168.1.20 ", "port": "8884.9", "device_id": "AB_12-cd", "accessCode": "x",
    }))
    .unwrap();
    assert_eq!(
        (config.host.as_str(), config.port, config.serial.as_str()),
        ("192.168.1.20", 8884, "AB_12-cd")
    );
    let default_port = Config::from_stored(&json!({
        "host": "h", "port": 70000, "serial": "ABCDEF", "access_code": "x",
    }))
    .unwrap();
    assert_eq!(default_port.port, 8883);
    for (config, error) in [
        (
            json!({"serial": "ABCDEF", "access_code": "x"}),
            "host (printer LAN IP) is required",
        ),
        (
            json!({"host": "h", "serial": "ABCDEF", "access_code": "****"}),
            "access_code is required",
        ),
        (
            json!({"host": "h", "access_code": "x"}),
            "serial (device id) is required",
        ),
        (
            json!({"host": "h", "serial": "dev/#", "access_code": "x"}),
            "serial must be 6–64 letters/digits (no MQTT wildcards or path characters)",
        ),
        (
            json!({"host": "h", "serial": "ABC", "access_code": "x"}),
            "serial must be 6–64 letters/digits (no MQTT wildcards or path characters)",
        ),
    ] {
        assert_eq!(Config::from_stored(&config).err().as_deref(), Some(error));
    }
}

#[tokio::test]
async fn ticket_t_23_first_use_reads_status_and_reports_the_fingerprint() {
    let identity = Identity::generate().unwrap();
    let broker = Broker::start(
        identity.clone(),
        Script::reporting(CODE, SERIAL, &fixture("pushall-printing.json")),
    )
    .await
    .unwrap();
    let read = driver("127.0.0.1", broker.addr.port())
        .status(Trust::FirstUse)
        .await;
    assert_eq!(read.certificate, Some(identity.fingerprint));
    assert_eq!(read.status.state, State::Printing);
    assert_eq!(read.status.message.as_deref(), Some("Printing frame_x.3mf"));
    assert_eq!(read.status.ip_address.as_deref(), Some("127.0.0.1"));
    assert_eq!(settled(&broker, 6).await, exchange());
}

#[tokio::test]
async fn ticket_t_23_checked_dns_reaches_the_pinned_tls_connection() {
    struct Loopback;
    impl Resolver for Loopback {
        fn resolve(&self, host: String) -> Resolution {
            Box::pin(async move {
                assert_eq!(host, "printer.test");
                Ok(vec![IpAddr::from([127, 0, 0, 1])])
            })
        }
    }
    let identity = Identity::generate().unwrap();
    let broker = Broker::start(
        identity.clone(),
        Script::reporting(CODE, SERIAL, &fixture("pushall-idle.json")),
    )
    .await
    .unwrap();
    let config = Config::from_stored(&json!({
        "host": "printer.test", "port": broker.addr.port(), "serial": SERIAL, "access_code": CODE,
    }))
    .unwrap();
    let guard = HttpGuard::new(Arc::new(Loopback), Duration::from_secs(5), 1024).unwrap();
    let read = Driver::new(guard, config)
        .status(Trust::Pinned(identity.fingerprint))
        .await;
    assert_eq!(read.status.state, State::Idle);
    assert_eq!(read.status.ip_address.as_deref(), Some("printer.test"));
    assert_eq!(settled(&broker, 6).await, exchange());
}

#[tokio::test]
async fn ticket_t_23_a_changed_certificate_fails_before_mqtt_connect() {
    let pinned = Identity::generate().unwrap();
    let broker = Broker::start(
        pinned.clone(),
        Script::reporting(CODE, SERIAL, &fixture("pushall-idle.json")),
    )
    .await
    .unwrap();
    let driver = driver("127.0.0.1", broker.addr.port());
    assert_eq!(
        driver
            .status(Trust::Pinned(pinned.fingerprint))
            .await
            .status
            .state,
        State::Idle
    );
    let replaced = Identity::generate().unwrap();
    broker.present(replaced.clone());
    let read = driver.status(Trust::Pinned(pinned.fingerprint)).await;
    assert_eq!(read.status.state, State::Offline);
    assert_eq!(read.status.message.as_deref(), Some(CERTIFICATE_CHANGED));
    assert_eq!(read.certificate, Some(replaced.fingerprint));
    let log = settled(&broker, 8).await;
    assert_eq!(&log[6..], ["TCP", "TLS refused"]);
    assert_eq!(
        log.iter()
            .filter(|line| line.starts_with("CONNECT"))
            .count(),
        1
    );
}

#[tokio::test]
async fn ticket_t_23_the_guard_refuses_prohibited_hosts_before_connecting() {
    for host in ["192.0.2.10", "169.254.169.254", "[2001:db8::1]"] {
        let read = driver(host, 8883).status(Trust::FirstUse).await;
        assert_eq!(read.status.state, State::Offline, "{host}");
        assert_eq!(
            read.status.message.as_deref(),
            Some("Prohibited DNS answer"),
            "{host}"
        );
        assert_eq!(read.status.ip_address, None, "{host}");
        assert_eq!(read.certificate, None, "{host}");
    }
}

#[tokio::test]
async fn ticket_t_23_broker_failures_are_offline_statuses() {
    let identity = Identity::generate().unwrap();
    let report = fixture("pushall-idle.json");
    let topic = format!("device/{SERIAL}/report");
    let mut oversized = vec![0x30, 0x81, 0x80, 0x40];
    oversized.extend(vec![0; 16]);
    let cases: Vec<(&str, Script, &str)> = vec![
        (
            "wrong code",
            Script {
                connack: 5,
                ..Script::reporting("other", SERIAL, &report)
            },
            "Connection refused: Not authorized",
        ),
        (
            "suback refused",
            Script {
                suback: 0x80,
                ..Script::reporting(CODE, SERIAL, &report)
            },
            "Failed to subscribe to Bambu report topic",
        ),
        (
            "closed",
            Script {
                replies: vec![Reply::Close],
                ..Script::reporting(CODE, SERIAL, &report)
            },
            CLOSED,
        ),
        (
            "silent",
            Script {
                replies: vec![],
                ..Script::reporting(CODE, SERIAL, &report)
            },
            TIMED_OUT,
        ),
        (
            "oversized",
            Script {
                replies: vec![Reply::Raw(oversized)],
                ..Script::reporting(CODE, SERIAL, &report)
            },
            "Bambu MQTT packet is too large",
        ),
        (
            "unusable reports only",
            Script {
                replies: vec![
                    Reply::Publish {
                        topic: "device/OTHER1/report".into(),
                        payload: report.clone(),
                    },
                    Reply::Publish {
                        topic: topic.clone(),
                        payload: br#"{"print":{"gcode_state":"  "}}"#.to_vec(),
                    },
                    Reply::Publish {
                        topic: topic.clone(),
                        payload: b"not json".to_vec(),
                    },
                ],
                ..Script::reporting(CODE, SERIAL, &report)
            },
            TIMED_OUT,
        ),
    ];
    let broker = Broker::start(identity.clone(), Script::reporting(CODE, SERIAL, &report))
        .await
        .unwrap();
    let driver = driver("127.0.0.1", broker.addr.port());
    for (name, script, message) in cases {
        broker.script(script);
        let read = driver.status(Trust::Pinned(identity.fingerprint)).await;
        assert_eq!(read.status.state, State::Offline, "{name}");
        assert_eq!(read.status.message.as_deref(), Some(message), "{name}");
        assert!(!format!("{:?}", read.status).contains(CODE), "{name}");
    }
}

#[tokio::test]
async fn ticket_t_23_skips_unusable_reports_until_a_status_arrives() {
    let identity = Identity::generate().unwrap();
    let topic = format!("device/{SERIAL}/report");
    let script = Script {
        replies: vec![
            Reply::Publish {
                topic: topic.clone(),
                payload: br#"{"info":{}}"#.to_vec(),
            },
            Reply::Raw(publish(
                "device/OTHER1/report",
                &fixture("pushall-printing.json"),
            )),
            Reply::Publish {
                topic,
                payload: fixture("pushall-error.json"),
            },
        ],
        ..Script::reporting(CODE, SERIAL, b"")
    };
    let broker = Broker::start(identity.clone(), script).await.unwrap();
    let read = driver("127.0.0.1", broker.addr.port())
        .status(Trust::Pinned(identity.fingerprint))
        .await;
    assert_eq!(read.status.state, State::Error);
}

#[tokio::test]
async fn ticket_t_23_v1_certificates_pin_over_tls13_and_tls12() {
    let cases = [
        ("v1, TLS 1.3", Identity::generate_v1().unwrap()),
        ("v1, TLS 1.2", Identity::generate_v1().unwrap().tls12_only()),
        ("v3, TLS 1.2", Identity::generate().unwrap().tls12_only()),
    ];
    for (name, identity) in cases {
        let broker = Broker::start(
            identity.clone(),
            Script::reporting(CODE, SERIAL, &fixture("pushall-idle.json")),
        )
        .await
        .unwrap();
        let driver = driver("127.0.0.1", broker.addr.port());
        let first = driver.status(Trust::FirstUse).await;
        assert_eq!(
            first.status.state,
            State::Idle,
            "{name}: {:?}",
            first.status.message
        );
        assert_eq!(first.certificate, Some(identity.fingerprint), "{name}");
        let pinned = driver.status(Trust::Pinned(identity.fingerprint)).await;
        assert_eq!(
            pinned.status.state,
            State::Idle,
            "{name}: {:?}",
            pinned.status.message
        );
    }
}

#[tokio::test]
async fn ticket_t_23_a_matching_pin_without_the_private_key_fails_before_connect() {
    let printer = Identity::generate_v1().unwrap();
    let impostor = Identity::generate().unwrap();
    for (name, presented) in [
        ("TLS 1.3", printer.with_key_of(&impostor)),
        ("TLS 1.2", printer.with_key_of(&impostor).tls12_only()),
    ] {
        let broker = Broker::start(
            presented,
            Script::reporting(CODE, SERIAL, &fixture("pushall-idle.json")),
        )
        .await
        .unwrap();
        let read = driver("127.0.0.1", broker.addr.port())
            .status(Trust::Pinned(printer.fingerprint))
            .await;
        assert_eq!(read.status.state, State::Offline, "{name}");
        assert_eq!(
            read.status.message.as_deref(),
            Some("Bambu MQTT TLS handshake failed"),
            "{name}"
        );
        assert_eq!(read.certificate, Some(printer.fingerprint), "{name}");
        let log = settled(&broker, 2).await;
        assert!(
            !log.iter().any(|line| line.starts_with("CONNECT")),
            "{name}: {log:?}"
        );
    }
}

#[tokio::test]
async fn ticket_t_23_a_report_before_suback_answers_the_read() {
    let identity = Identity::generate().unwrap();
    let script = Script {
        retained: Some(fixture("pushall-printing.json")),
        replies: vec![],
        ..Script::reporting(CODE, SERIAL, b"")
    };
    let broker = Broker::start(identity.clone(), script).await.unwrap();
    let read = driver("127.0.0.1", broker.addr.port())
        .status(Trust::Pinned(identity.fingerprint))
        .await;
    assert_eq!(read.status.state, State::Printing);
    assert_eq!(
        settled(&broker, 5).await,
        [
            "TCP".to_owned(),
            "TLS".into(),
            "CONNECT bblp access code".into(),
            format!("SUBSCRIBE device/{SERIAL}/report"),
            "DISCONNECT".into(),
        ]
    );
}
