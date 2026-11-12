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

#[tokio::test]
async fn dns_that_never_finishes_is_bounded_by_the_probe_deadline() {
    struct Hung;
    impl pp_net::outbound::Resolver for Hung {
        fn resolve(&self, _: String) -> pp_net::outbound::Resolution {
            Box::pin(std::future::pending())
        }
    }
    let guard = HttpGuard::new(Arc::new(Hung), Duration::from_millis(10), 1024).unwrap();
    let start = std::time::Instant::now();
    let result = probe(
        guard,
        PrinterProbeRequest {
            kind: "moonraker".into(),
            address: "http://fixture.invalid".into(),
        },
    )
    .await;
    assert!(!result.reachable);
    assert!(start.elapsed() < Duration::from_secs(9));
}
