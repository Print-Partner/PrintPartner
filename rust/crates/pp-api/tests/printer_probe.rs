use pp_api::{
    admin::AdminHttpConfig,
    printer_host::probe::{
        PrinterProbePort, PrinterProbeRequest, PrinterProbeResult, printer_probe_router,
    },
};
use pp_storage::{Limits, WriterOwner};
use pp_testkit::Installation;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
#[path = "support/auth_policy.rs"]
mod auth_policy;
#[path = "support/loopback.rs"]
mod loopback;

#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
    release: Arc<tokio::sync::Notify>,
}
impl PrinterProbePort for Fake {
    fn probe(
        &self,
        request: PrinterProbeRequest,
    ) -> Pin<Box<dyn Future<Output = PrinterProbeResult> + Send>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.kind == "wait" {
            self.entered.notify_one();
            return Box::pin(std::future::pending());
        }
        if request.kind == "hold" {
            self.entered.notify_one();
        }
        let release = self.release.clone();
        Box::pin(async move {
            if request.kind == "hold" {
                release.notified().await;
            }
            PrinterProbeResult {
                kind: Some("moonraker".into()),
                reachable: true,
                credentials_needed: false,
                capabilities: vec!["status".into()],
                reason: "Printer responded successfully.".into(),
            }
        })
    }
}

fn fixture() -> (Installation, WriterOwner, Arc<Fake>) {
    let data = Installation::with_authenticated_fixture().unwrap();
    let owner = WriterOwner::open(data.path(), Limits::default()).unwrap().0;
    (data, owner, Arc::default())
}

fn router(owner: &WriterOwner, fake: Arc<Fake>, flag: Option<&str>) -> axum::Router {
    printer_probe_router(
        AdminHttpConfig::from_startup(loopback::ORIGIN, "default", flag).unwrap(),
        owner
            .accepted_reads_with_policy(auth_policy::policy())
            .unwrap(),
        fake,
    )
}

const COOKIE: Option<&str> = Some("pp_session=required-unit-fixture-secret");
const PATH: &str = "/api/v2/printers/probe";
const BODY: &str = r#"{"kind":"moonraker","address":"http://fixture.invalid"}"#;

#[tokio::test]
async fn invalid_probe_bodies_return_coded_problems_without_calling_the_port() {
    let (_data, owner, fake) = fixture();
    let app = router(&owner, fake.clone(), Some("1"));
    for (content_type, body, status) in [
        (Some("application/json"), b"{".to_vec(), 400),
        (Some("application/json"), b"{}".to_vec(), 422),
        (
            Some("application/json"),
            BODY.replace("}", ",\"extra\":true}").into_bytes(),
            422,
        ),
        (None, BODY.as_bytes().to_vec(), 415),
        (Some("application/json"), vec![b' '; 4097], 413),
    ] {
        let answer = loopback::send_as(&app, "POST", PATH, content_type, body, COOKIE).await;
        assert_eq!(answer.0, status);
        assert_eq!(
            pp_api::problem::without_envelope(answer.1)["code"],
            "invalid_printer_probe"
        );
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn busy_probe_refusals_leave_the_admitted_probe_quota_available() {
    let (_data, owner, fake) = fixture();
    let app = router(&owner, fake.clone(), Some("1"));
    let active_app = app.clone();
    let active = tokio::spawn(async move {
        loopback::send(
            &active_app,
            "POST",
            PATH,
            Some(&BODY.replace("moonraker", "hold")),
            COOKIE,
        )
        .await
    });
    fake.entered.notified().await;
    for _ in 0..11 {
        let refused = loopback::send(&app, "POST", PATH, Some(BODY), COOKIE).await;
        assert_eq!(refused.0, 429);
        assert_eq!(
            pp_api::problem::without_envelope(refused.1)["code"],
            "printer_probe_busy"
        );
    }
    fake.release.notify_one();
    assert_eq!(active.await.unwrap().0, 200);
    for _ in 0..9 {
        assert_eq!(
            loopback::send(&app, "POST", PATH, Some(BODY), COOKIE)
                .await
                .0,
            200
        );
    }
    assert_eq!(
        loopback::send(&app, "POST", PATH, Some(BODY), COOKIE)
            .await
            .0,
        429
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 10);
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn authenticated_probe_does_not_write_and_limits_requests_before_network_work() {
    let (data, owner, fake) = fixture();
    let router = |flag| router(&owner, fake.clone(), flag);
    let body = Some(BODY);
    let cookie = COOKIE;
    let path = PATH;
    assert_eq!(
        loopback::send(&router(None), "POST", path, body, cookie)
            .await
            .0,
        404
    );
    let app = router(Some("1"));
    let before = [
        data.database_path(),
        data.path().join("print-partner.db-wal"),
    ]
    .map(|p| std::fs::read(p).unwrap_or_default());
    assert_eq!(loopback::send(&app, "POST", path, body, None).await.0, 401);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    for _ in 0..10 {
        let answer = loopback::send(&app, "POST", path, body, cookie).await;
        assert_eq!(answer.0, 200);
        let answer = pp_api::problem::without_envelope(answer.1);
        assert_eq!(answer["reachable"], true);
        assert_eq!(answer["credentials_needed"], false);
        assert!(!answer.to_string().contains("fixture.invalid"));
    }
    assert_eq!(
        loopback::send(&app, "POST", path, body, cookie).await.0,
        429
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 10);
    let after = [
        data.database_path(),
        data.path().join("print-partner.db-wal"),
    ]
    .map(|p| std::fs::read(p).unwrap_or_default());
    assert_eq!(before, after);
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn probe_deadline_uses_eight_seconds_of_paused_time() {
    let (_data, owner, fake) = fixture();
    let app = router(&owner, fake.clone(), Some("true"));
    let request = tokio::spawn(async move {
        loopback::send(
            &app,
            "POST",
            PATH,
            Some(r#"{"kind":"wait","address":"http://fixture.invalid"}"#),
            COOKIE,
        )
        .await
    });
    // Authenticate against the real writer before pausing the clock.
    fake.entered.notified().await;
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    tokio::time::advance(std::time::Duration::from_secs(7)).await;
    assert!(!request.is_finished());
    // A timer created before pause can round up to the next millisecond tick.
    tokio::time::advance(std::time::Duration::from_millis(1001)).await;
    let timed = request.await.unwrap();
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(8001));
    tokio::time::resume();
    assert_eq!(timed.0, 200);
    assert_eq!(
        pp_api::problem::without_envelope(timed.1)["reason"],
        "Printer probe timed out."
    );
    owner.shutdown().unwrap();
}
