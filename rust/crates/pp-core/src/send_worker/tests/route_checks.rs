//! GRE-1267: the Send now and Send ready checks in the real [`QueueTargets`].
//! Its status port comes from a running gateway whose watchers poll loopback
//! Moonraker and PrusaLink simulators, so `wait_for_idle` reads what the
//! watchers really observed.
use super::*;
use pp_gateway::{Gateway, RustStatus, ServedUi, SignOut};
use pp_testkit::moonraker::RecordedPrinter;

const WAIT: &str = "wait for Idle or force dispatch";
const MOONRAKER_PRINTING: &[u8] =
    br#"{"result":{"status":{"print_stats":{"state":"printing","filename":"a.gcode"}}}}"#;
const MOONRAKER_IDLE: &[u8] = br#"{"result":{"status":{"print_stats":{"state":"standby"}}}}"#;

/// A gateway started over `farm`'s settings, and the queue checks built on
/// the status port it handed out.
async fn watched(farm: &Farm) -> (Gateway, Arc<QueueTargets>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hosts = Arc::new(crate::printer_status::StoredMoonrakerHosts::new(
        farm.owner.client(),
    ));
    let (_, ready) = tokio::sync::watch::channel(RustStatus::Ready);
    let (port, status) = std::sync::mpsc::channel();
    let gateway = Gateway::start_rust_only(
        listener,
        ServedUi::Legacy(farm.root.path().to_path_buf()),
        move |status: MoonrakerStatus| {
            port.send(status).unwrap();
            axum::Router::new()
        },
        ready,
        hosts,
        SignOut::EndsRuntime,
    )
    .unwrap();
    let targets = QueueTargets {
        settings: farm.owner.client(),
        exports: farm.exports(),
        status: status.recv().unwrap(),
        wake: Arc::new(Notify::new()),
    };
    (gateway, Arc::new(targets))
}

/// Replaces the farm's printers and hosts: one printer `p-{id}` per host.
fn link(farm: &Farm, hosts: &[Value]) {
    let printers: Vec<Value> = hosts
        .iter()
        .map(|host| json!({"id": format!("p-{}", host["id"].as_str().unwrap()), "integration_id": host["id"]}))
        .collect();
    for (key, value) in [
        ("printer.fleet", json!(printers)),
        ("integrations", json!(hosts)),
    ] {
        farm.database()
            .execute(
                "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key=?2",
                rusqlite::params![value.to_string(), key],
            )
            .unwrap();
    }
}

fn host(id: &str, kind: &str, config: Value) -> Value {
    json!({"id": id, "name": format!("{id} host"), "type": kind, "config": config})
}

/// A queue item for `printer` whose sliced file is staged inside
/// `printer-uploads`, as Add to queue leaves it.
fn staged_item(farm: &Farm, id: &str, printer: &str) -> Value {
    let path = farm
        .exports()
        .join("printer-uploads")
        .join(id)
        .join("a.gcode");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"G28\n").unwrap();
    item(printer, "a.gcode", &path)
}

fn item(printer: &str, filename: &str, path: &Path) -> Value {
    json!({"filename": filename, "artifact_path": path.to_string_lossy(), "printer_id": printer})
}

/// Send now (`idle` false) or Send ready (`idle` true), through the routes'
/// checks.
async fn prepare(targets: &Arc<QueueTargets>, item: &Value, idle: bool) -> Prepared {
    let (targets, item) = (Arc::clone(targets), item.clone());
    tokio::task::spawn_blocking(move || targets.prepare(&item, idle))
        .await
        .unwrap()
}

fn refused(status: StatusCode, detail: &str) -> Prepared {
    Prepared::Refused {
        status,
        detail: detail.to_owned(),
    }
}

fn ready(host: &str) -> Prepared {
    Prepared::Ready {
        host_name: format!("{host} host"),
    }
}

/// Waits until the watchers have answered for host `id`.
async fn heard(targets: &QueueTargets, id: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !matches!(
            crate::printer_status::observed(&targets.status, id),
            Observation::Seen { .. }
        ) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the watchers never answered for {id}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1267_send_checks_refuse_every_host_that_is_not_read() {
    let printer = RecordedPrinter::start(vec![(200, MOONRAKER_IDLE)]).unwrap();
    let farm = Farm::open(printer.url(), &[]);
    // Kinds that are not Moonraker or PrusaLink, including ones no build of
    // Print Partner knows: the refusal is "not Read", not an allow-list.
    let others = ["bambu", "octoprint", "resin-vat", ""];
    let mut hosts = vec![host(
        "moon",
        "moonraker",
        json!({"base_url": printer.url()}),
    )];
    hosts.extend(others.iter().enumerate().map(|(n, kind)| {
        host(
            &format!("other-{n}"),
            kind,
            json!({"base_url": "http://127.0.0.1:9"}),
        )
    }));
    link(&farm, &hosts);
    let (gateway, targets) = watched(&farm).await;
    for (n, kind) in others.iter().enumerate() {
        let item = staged_item(&farm, &format!("q{n}"), &format!("p-other-{n}"));
        let detail = format!("Upload is not supported for {kind}");
        for idle in [false, true] {
            assert_eq!(
                prepare(&targets, &item, idle).await,
                refused(StatusCode::BAD_REQUEST, &detail),
                "{kind:?} idle={idle}"
            );
        }
    }
    let moon = staged_item(&farm, "q-moon", "p-moon");
    assert_eq!(prepare(&targets, &moon, false).await, ready("moon"));
    gateway.stop().await.unwrap();
    assert!(
        printer
            .request_lines()
            .iter()
            .all(|line| line.starts_with("GET ")),
        "the checks never send: {:?}",
        printer.request_lines()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1267_send_ready_waits_for_the_watchers_to_see_idle() {
    let printing = RecordedPrinter::start(vec![(200, MOONRAKER_PRINTING)]).unwrap();
    let idle = RecordedPrinter::start(vec![(200, MOONRAKER_IDLE)]).unwrap();
    let prusa_status = json!({"printer": {"state": "PRINTING"}}).to_string();
    let prusa = RoutedPrinter::start(vec![(
        "/api/v1/status".into(),
        200,
        prusa_status.into_bytes(),
    )])
    .unwrap();
    let farm = Farm::open(idle.url(), &[]);
    link(
        &farm,
        &[
            host("busy", "moonraker", json!({"base_url": printing.url()})),
            host("idle", "moonraker", json!({"base_url": idle.url()})),
            host(
                "mk4",
                "prusalink",
                json!({"base_url": prusa.url(), "username": "maker", "password": "not-a-real-secret"}),
            ),
            // The watchers hold an HTTPS host but never connect to it.
            host(
                "tls",
                "moonraker",
                json!({"base_url": "https://printer.invalid"}),
            ),
        ],
    );
    let (gateway, targets) = watched(&farm).await;
    for id in ["busy", "idle", "mk4"] {
        heard(&targets, id).await;
    }
    let printing_now = || {
        refused(
            StatusCode::CONFLICT,
            &format!("Printer is printing — {WAIT}"),
        )
    };
    let unavailable = || {
        refused(
            StatusCode::CONFLICT,
            &format!("Printer status is unavailable — {WAIT}"),
        )
    };
    for (id, expected) in [
        ("busy", printing_now()),
        ("mk4", printing_now()),
        ("idle", ready("idle")),
        ("tls", unavailable()),
    ] {
        let item = staged_item(&farm, &format!("q-{id}"), &format!("p-{id}"));
        assert_eq!(prepare(&targets, &item, true).await, expected, "{id}");
        // Send now dispatches without waiting for Idle.
        assert_eq!(prepare(&targets, &item, false).await, ready(id), "{id}");
    }

    // Once the watchers are gone nothing is observed, so even the idle
    // printer waits.
    gateway.stop().await.unwrap();
    let item = staged_item(&farm, "q-after", "p-idle");
    assert_eq!(prepare(&targets, &item, true).await, unavailable());
    for simulator in [&printing, &idle] {
        let lines = simulator.request_lines();
        assert!(
            lines.iter().all(|line| line.starts_with("GET ")),
            "{lines:?}"
        );
    }
    let lines = prusa.request_lines();
    assert!(
        lines.iter().all(|line| line.starts_with("GET ")),
        "{lines:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gre_1267_send_checks_take_only_a_sliced_file_staged_in_printer_uploads() {
    let printer = RecordedPrinter::start(vec![(200, MOONRAKER_IDLE)]).unwrap();
    let farm = Farm::open(printer.url(), &[]);
    link(
        &farm,
        &[host(
            "moon",
            "moonraker",
            json!({"base_url": printer.url()}),
        )],
    );
    let (gateway, targets) = watched(&farm).await;
    let invalid = refused(StatusCode::BAD_REQUEST, INVALID_ARTIFACT);

    let staged = staged_item(&farm, "q1", "p-moon");
    assert_eq!(prepare(&targets, &staged, false).await, ready("moon"));
    let path = PathBuf::from(staged["artifact_path"].as_str().unwrap());

    // Under `exports` but outside `printer-uploads`, and outside `exports`.
    let loose = farm.exports().join("a.gcode");
    std::fs::write(&loose, b"G28\n").unwrap();
    let outside = farm.root.path().join("a.gcode");
    std::fs::write(&outside, b"G28\n").unwrap();
    for path in [&loose, &outside] {
        let item = item("p-moon", "a.gcode", path);
        assert_eq!(prepare(&targets, &item, false).await, invalid, "{path:?}");
    }
    // A link inside `printer-uploads` to a file outside `exports`.
    #[cfg(unix)]
    {
        let link = path.with_file_name("b.gcode");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let item = item("p-moon", "b.gcode", &link);
        assert_eq!(prepare(&targets, &item, false).await, invalid);
    }
    // A staged directory is not a file.
    let directory = item("p-moon", "a.gcode", path.parent().unwrap());
    assert_eq!(
        prepare(&targets, &directory, false).await,
        Prepared::Missing
    );
    // Only sliced files print.
    let unsliced = item("p-moon", "a.stl", &path);
    assert_eq!(
        prepare(&targets, &unsliced, false).await,
        refused(StatusCode::BAD_REQUEST, NOT_SLICED)
    );

    // The staged file is gone: Missing, for Send now and Send ready, and
    // before the host is checked.
    std::fs::remove_file(&path).unwrap();
    for idle in [false, true] {
        assert_eq!(prepare(&targets, &staged, idle).await, Prepared::Missing);
    }
    let elsewhere = item("p-gone", "a.gcode", &path);
    assert_eq!(
        prepare(&targets, &elsewhere, false).await,
        Prepared::Missing
    );
    gateway.stop().await.unwrap();
}
