//! GRE-1175: the status watchers poll PrusaLink hosts through the PrusaLink
//! driver's status read, so the Rust-only runtime publishes the same status
//! Node's adapter reports on demand. Recorded printers only; every request is
//! a read-only GET.
use pp_drivers::{
    moonraker::{Host, HostKind, PrinterHostState, Watchers},
    prusalink,
};
use pp_net::outbound::{HttpGuard, SystemResolver};
use pp_testkit::moonraker::RoutedPrinter;
use std::{path::Path, sync::Arc, time::Duration};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/drivers/prusalink"
);

/// The recorded reply body of a fixture, which wraps it with its status and headers.
fn fixture(name: &str) -> Vec<u8> {
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Path::new(FIXTURES).join(name)).unwrap()).unwrap();
    recorded["body"].to_string().into_bytes()
}

fn guard() -> HttpGuard {
    HttpGuard::new(
        Arc::new(SystemResolver),
        Duration::from_secs(2),
        2 * 1024 * 1024,
    )
    .unwrap()
}

fn prusalink(id: &str, base_url: &str, enabled: bool) -> Host {
    Host {
        id: id.into(),
        base_url: base_url.into(),
        api_key: None,
        enabled,
        kind: HostKind::PrusaLink {
            username: "maker".into(),
            password: Some("synthetic-fixture-password".into()),
        },
    }
}

#[tokio::test]
async fn ticket_t_22_watchers_publish_the_prusalink_drivers_status() {
    let printing = RoutedPrinter::start(vec![
        (
            "/api/v1/status".into(),
            200,
            fixture("status-printing.json"),
        ),
        ("/api/v1/job".into(), 200, fixture("job.json")),
    ])
    .unwrap();
    let failing =
        RoutedPrinter::start(vec![("/api/v1/status".into(), 500, b"{}".to_vec())]).unwrap();
    let disabled = RoutedPrinter::start(Vec::new()).unwrap();
    let mut watchers = Watchers::with_guard(guard(), Duration::from_millis(20));
    let observations = watchers.observations();
    watchers
        .replace(vec![
            prusalink("mk4", printing.url(), true),
            prusalink("down", failing.url(), true),
            prusalink("off", disabled.url(), false),
        ])
        .await
        .unwrap();

    let expected = prusalink::Driver::new(
        guard(),
        printing.url(),
        "maker",
        Some("synthetic-fixture-password"),
        None,
    )
    .status()
    .await;
    assert_eq!(expected.state, PrinterHostState::Printing);
    tokio::time::timeout(Duration::from_secs(5), async {
        while observations.latest("mk4").as_ref() != Some(&expected)
            || observations
                .latest("down")
                .is_none_or(|s| s.state != PrinterHostState::Offline)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both PrusaLink hosts publish a status");
    assert!(observations.last_heard("mk4").is_some());

    // An offline read is the driver's own status, backs off and is not heard.
    let down = observations.latest("down").unwrap();
    assert_eq!(down.message.as_deref(), Some("PrusaLink returned HTTP 500"));
    assert!(observations.last_heard("down").is_none());
    let off = observations.latest("off").unwrap();
    assert_eq!(
        (off.state, off.message.as_deref()),
        (PrinterHostState::Offline, Some("Integration disabled"))
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    watchers.stop().await;

    let (healthy, backing_off) = (printing.request_lines(), failing.request_lines());
    assert!(
        backing_off.len() * 2 < healthy.len(),
        "a failing host backs off: {} requests against {}",
        backing_off.len(),
        healthy.len()
    );
    assert!(
        disabled.request_lines().is_empty(),
        "a disabled host is never polled"
    );
    for line in healthy.iter().chain(&backing_off) {
        assert!(
            line.starts_with("GET /api/v1/status ") || line.starts_with("GET /api/v1/job "),
            "the printer saw {line}"
        );
    }
}

#[test]
fn ticket_t_22_prusalink_hosts_poll_at_the_web_clients_cadence() {
    assert_eq!(
        pp_drivers::moonraker::PRUSALINK_INTERVAL,
        Duration::from_secs(5)
    );
}
