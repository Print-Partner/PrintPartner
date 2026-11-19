use pp_drivers::probe::{PrinterProbeRequest, probe};
use pp_net::outbound::{HttpGuard, SystemResolver};
use pp_testkit::moonraker::RecordedPrinter;
use std::{sync::Arc, time::Duration};

fn guard() -> HttpGuard {
    HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(8), 64 * 1024).unwrap()
}

#[tokio::test]
async fn probe_classifies_recorded_printers_with_one_read_and_no_addresses_in_results() {
    const MOON: &[u8] =
        br#"{"result":{"klippy_state":"ready","components":["file_manager","webcam"]}}"#;
    const PRUSA: &[u8] = br#"{"printer_model":"MK4"}"#;
    const PRIVATE: &[u8] = b"private printer text";
    for (kind, status, body, expected) in [
        ("moonraker", 200, MOON, (true, false, true)),
        ("prusalink", 200, PRUSA, (true, false, true)),
        ("prusalink", 401, PRIVATE, (true, true, true)),
        ("moonraker", 403, PRIVATE, (true, true, true)),
        ("moonraker", 200, PRIVATE, (true, false, false)),
        ("moonraker", 404, PRIVATE, (true, false, false)),
        ("moonraker", 302, MOON, (true, false, false)),
        ("unsupported", 200, PRIVATE, (false, false, false)),
    ] {
        let printer = RecordedPrinter::start(vec![(status, body)]).unwrap();
        let result = probe(
            guard(),
            PrinterProbeRequest {
                kind: kind.into(),
                address: printer.url().into(),
            },
        )
        .await;
        assert_eq!(
            (
                result.reachable,
                result.credentials_needed,
                result.kind.is_some()
            ),
            expected
        );
        assert!(!result.reason.contains(printer.url()));
        assert!(!result.reason.contains("private printer text"));
        assert_eq!(printer.calls(), usize::from(kind != "unsupported"));
        for line in printer.request_lines() {
            assert!(line.starts_with("GET "));
        }
        if kind == "moonraker" && status == 200 && result.kind.is_some() {
            assert_eq!(result.capabilities, ["status", "files", "cameras"]);
        }
    }
}

#[tokio::test]
async fn probe_refuses_metadata_addresses_and_reports_closed_loopback_as_unreachable() {
    let printer = RecordedPrinter::start(vec![(200, b"unused")]).unwrap();
    let closed = printer.url().to_owned();
    drop(printer);
    for address in [
        closed.as_str(),
        "http://169.254.169.254",
        "http://[::]",
        "http://user:secret@127.0.0.1",
    ] {
        let result = probe(
            guard(),
            PrinterProbeRequest {
                kind: "moonraker".into(),
                address: address.into(),
            },
        )
        .await;
        assert!(!result.reachable, "{address}");
        assert!(!result.credentials_needed);
        assert!(!result.reason.contains("secret"));
    }
}

#[tokio::test(start_paused = true)]
async fn probe_deadline_expires_before_the_longer_transport_deadline() {
    struct Hung(Arc<tokio::sync::Notify>);
    impl pp_net::outbound::Resolver for Hung {
        fn resolve(&self, _: String) -> pp_net::outbound::Resolution {
            self.0.notify_one();
            Box::pin(std::future::pending())
        }
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let guard = HttpGuard::new(
        Arc::new(Hung(entered.clone())),
        Duration::from_secs(30),
        1024,
    )
    .unwrap();
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(probe(
        guard,
        PrinterProbeRequest {
            kind: "moonraker".into(),
            address: "http://fixture.invalid".into(),
        },
    ));
    entered.notified().await;
    tokio::time::advance(Duration::from_secs(7)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    let result = task.await.unwrap();
    assert!(!result.reachable);
    assert_eq!(result.reason, "Printer probe timed out.");
    assert_eq!(started.elapsed(), Duration::from_secs(8));
}

#[tokio::test]
async fn saved_connection_edge_cases_match_node_while_unsaved_probe_stays_strict() {
    use pp_testkit::moonraker::RoutedPrinter;
    use serde::Deserialize;
    use serde_json::Value;
    #[derive(Deserialize)]
    struct Case {
        name: String,
        kind: String,
        routes: std::collections::BTreeMap<String, (u16, Value)>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../../fixtures/drivers/probe/connection-cases.json"
    ))
    .unwrap();
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/drivers/probe/node-connections.json"
    ))
    .unwrap();
    for case in cases {
        let routes = || {
            case.routes
                .iter()
                .map(|(path, (status, body))| {
                    (path.clone(), *status, serde_json::to_vec(body).unwrap())
                })
                .collect()
        };
        let printer = RoutedPrinter::start(routes()).unwrap();
        let legacy = match case.kind.as_str() {
            "moonraker" => {
                pp_drivers::moonraker_driver::Driver::new(guard(), printer.url(), None)
                    .test_connection()
                    .await
            }
            _ => {
                pp_drivers::prusalink::Driver::new(
                    guard(),
                    printer.url(),
                    "fixture-user",
                    Some("synthetic-fixture-password"),
                    None,
                )
                .test_connection()
                .await
            }
        };
        assert_eq!(
            serde_json::to_value(legacy).unwrap(),
            oracle[&case.name]["result"],
            "{}",
            case.name
        );
        let requests: Vec<String> = printer
            .request_lines()
            .iter()
            .map(|line| {
                line.split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(
            serde_json::to_value(requests).unwrap(),
            oracle[&case.name]["requests"]
        );
        let unsaved = RoutedPrinter::start(routes()).unwrap();
        let result = probe(
            guard(),
            PrinterProbeRequest {
                kind: case.kind,
                address: unsaved.url().into(),
            },
        )
        .await;
        assert!(result.reachable);
        assert!(result.kind.is_none());
        assert!(!result.credentials_needed);
        assert!(result.capabilities.is_empty());
        assert_eq!(
            unsaved.request_lines().len(),
            1,
            "unsaved probe must not take the saved adapter's fallback path"
        );
    }
}
