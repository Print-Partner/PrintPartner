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

struct Fake(AtomicUsize);
impl PrinterProbePort for Fake {
    fn probe(
        &self,
        request: PrinterProbeRequest,
    ) -> Pin<Box<dyn Future<Output = PrinterProbeResult> + Send>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if request.kind == "wait" {
            return Box::pin(std::future::pending());
        }
        Box::pin(async {
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

#[tokio::test]
async fn authenticated_probe_does_not_write_and_limits_requests_before_network_work() {
    let data = Installation::with_authenticated_fixture().unwrap();
    let owner = WriterOwner::open(data.path(), Limits::default()).unwrap().0;
    let fake = Arc::new(Fake(AtomicUsize::new(0)));
    let router = |flag| {
        printer_probe_router(
            AdminHttpConfig::from_startup(loopback::ORIGIN, "default", flag).unwrap(),
            owner
                .accepted_reads_with_policy(auth_policy::policy())
                .unwrap(),
            fake.clone(),
        )
    };
    let body = Some(r#"{"kind":"moonraker","address":"http://fixture.invalid"}"#);
    let cookie = Some("pp_session=required-unit-fixture-secret");
    let path = "/api/v2/printers/probe";
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
    assert_eq!(fake.0.load(Ordering::SeqCst), 0);
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
    assert_eq!(fake.0.load(Ordering::SeqCst), 10);
    let after = [
        data.database_path(),
        data.path().join("print-partner.db-wal"),
    ]
    .map(|p| std::fs::read(p).unwrap_or_default());
    assert_eq!(before, after);
    let timed = loopback::send(
        &router(Some("true")),
        "POST",
        path,
        Some(r#"{"kind":"wait","address":"http://fixture.invalid"}"#),
        cookie,
    )
    .await;
    assert_eq!(timed.0, 200);
    assert_eq!(
        pp_api::problem::without_envelope(timed.1)["reason"],
        "Printer probe timed out."
    );
    drop(app);
    owner.shutdown().unwrap();
}
