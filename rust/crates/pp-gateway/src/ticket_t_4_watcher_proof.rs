#![cfg(test)]
//! GRE-449 / GRE-335 / GRE-364: the Rust-only gateway polls the hosts `pp-core` reads
//! from storage, answers the status route from the watchers and never reaches
//! Node. Recorded Moonraker bodies replay from loopback simulators.
use super::*;
use crate::scratch::Scratch;
use pp_testkit::moonraker::RecordedPrinter;
use std::net::SocketAddr;

const SUCCESS: &[u8] =
    include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-success.json");
const ERROR: &[u8] = include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-error.json");
const OFFLINE: &[u8] =
    include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-offline.json");

/// A host source the test rewrites, standing in for the stored setting.
#[derive(Clone)]
struct Hosts(Arc<Mutex<Result<Vec<Host>, HostReadError>>>);
impl Hosts {
    fn set(&self, hosts: Result<Vec<Host>, HostReadError>) {
        *self.0.lock().unwrap() = hosts;
    }
}
impl MoonrakerHosts for Hosts {
    fn read(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Host>, HostReadError>> + Send>>
    {
        let hosts = match &*self.0.lock().unwrap() {
            Ok(hosts) => Ok(hosts.clone()),
            Err(HostReadError::Invalid) => Err(HostReadError::Invalid),
            Err(HostReadError::Unavailable) => Err(HostReadError::Unavailable),
        };
        Box::pin(async move { hosts })
    }
}

fn only_status_gets(printer: &RecordedPrinter) {
    let lines = printer.request_lines();
    let query = format!("GET {} HTTP/1.1", pp_drivers::moonraker::QUERY);
    assert!(
        !lines.is_empty() && lines.iter().all(|line| *line == query),
        "{lines:?}"
    );
}

fn moonraker(id: &str, url: &str) -> Host {
    Host {
        id: id.into(),
        base_url: url.into(),
        api_key: Some("recorded-key".into()),
        enabled: true,
        kind: Default::default(),
    }
}

/// Stands in for pp-core's authenticated route: answers from the lookup.
fn product(lookup: MoonrakerStatus) -> axum::Router {
    axum::Router::new().route(
        "/api/v1/integrations/{id}/status",
        axum::routing::get(
            move |axum::extract::Path(id): axum::extract::Path<String>| {
                let lookup = lookup.clone();
                async move {
                    match lookup.lookup(&id).await {
                        StatusLookup::Status(status) => axum::Json(status).into_response(),
                        StatusLookup::NotWatched => {
                            (StatusCode::NOT_FOUND, "rust product").into_response()
                        }
                        StatusLookup::Unavailable => {
                            (StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response()
                        }
                    }
                }
            },
        ),
    )
}

async fn started(hosts: &Hosts) -> (Gateway, String, Scratch) {
    let root = Scratch::new("pp-watcher-proof-");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_status, receiver) = tokio::sync::watch::channel(RustStatus::Ready);
    let mut gateway = Gateway::start_rust_only(
        listener,
        root.to_path_buf(),
        ServedUi::Legacy,
        product,
        receiver,
        Arc::new(hosts.clone()),
    )
    .unwrap();
    let cookie = gateway.test_session().await;
    (gateway, cookie, root)
}

async fn status(
    gateway: &Gateway,
    cookie: Option<&str>,
    id: &str,
    method: &str,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/v1/integrations/{id}/status"))
        .header("host", &gateway.state.host);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let mut request = request.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:9".parse::<SocketAddr>().unwrap()));
    let response = handle(State(gateway.state.clone()), request).await;
    let code = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (code, String::from_utf8(body.to_vec()).unwrap())
}

async fn until<F, Fut>(label: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(15), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

async fn state_is(gateway: &Gateway, cookie: &str, id: &str, state: &str) -> bool {
    let (code, body) = status(gateway, Some(cookie), id, "GET").await;
    code == StatusCode::OK
        && serde_json::from_str::<serde_json::Value>(&body).unwrap()["state"] == state
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_rust_only_serves_recorded_status_without_node() {
    let recorded = RecordedPrinter::start(vec![
        (200, SUCCESS),
        (200, ERROR),
        (503, OFFLINE),
        (200, SUCCESS),
    ])
    .unwrap();
    let hosts = Hosts(Arc::new(Mutex::new(Ok(vec![moonraker(
        "m1",
        recorded.url(),
    )]))));
    let (gateway, cookie, _root) = started(&hosts).await;
    let mut observations = gateway.printer_observations();

    // No window or client drives polling: the observation port fills from the
    // gateway's own watcher task before any status request.
    let printing = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(status) = observations
                .latest("m1")
                .filter(|s| s.state == PrinterHostState::Printing)
            {
                return status;
            }
            observations.changed().await.unwrap();
        }
    })
    .await
    .expect("watcher never observed the recorded success");
    // The existing PrinterHostStatus wire shape, exactly as the recorded body maps.
    assert_eq!(
        serde_json::to_value(&printing).unwrap(),
        serde_json::json!({"state": "printing", "progress": 43.0, "filename": "bracket.gcode",
            "message": "Printing bracket.gcode", "ip_address": "127.0.0.1",
            "nozzle_temperature_c": 210.2, "nozzle_target_c": 215.0,
            "bed_temperature_c": 59.8, "bed_target_c": 60.0})
    );
    // Recorded error, then offline with backoff, then recovery, through the route.
    until("recorded error", || {
        state_is(&gateway, &cookie, "m1", "error")
    })
    .await;
    until("recorded offline", || {
        state_is(&gateway, &cookie, "m1", "offline")
    })
    .await;
    let (_, body) = status(&gateway, Some(&cookie), "m1", "GET").await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"state": "offline", "message": "Moonraker returned HTTP 503"})
    );
    until("recovery", || state_is(&gateway, &cookie, "m1", "printing")).await;
    let (code, body) = status(&gateway, Some(&cookie), "m1", "HEAD").await;
    assert_eq!((code, body.as_str()), (StatusCode::OK, ""));

    // Gateway admission still applies: no desktop session, no status.
    assert_ne!(status(&gateway, None, "m1", "GET").await.0, StatusCode::OK);
    // A non-Moonraker id reaches the Rust product routes, never Node.
    assert_eq!(
        status(&gateway, Some(&cookie), "prusa", "GET").await,
        (StatusCode::NOT_FOUND, "rust product".into())
    );
    assert_eq!(gateway.compat_forwards(), 0);
    only_status_gets(&recorded);

    // Shutdown cancels the poller promptly; the printer sees nothing after.
    tokio::time::timeout(Duration::from_secs(5), gateway.stop())
        .await
        .expect("gateway stop exceeded five seconds")
        .unwrap();
    let after = recorded.calls();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(recorded.calls(), after);
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_rust_only_replacement_removal_and_suspend() {
    let first = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
    let second = RecordedPrinter::start(vec![(200, ERROR)]).unwrap();
    let hosts = Hosts(Arc::new(Mutex::new(Ok(vec![moonraker("m1", first.url())]))));
    let (gateway, cookie, _root) = started(&hosts).await;
    let observations = gateway.printer_observations();
    until("first host", || {
        state_is(&gateway, &cookie, "m1", "printing")
    })
    .await;

    // Configuration replacement moves the one poller to the new endpoint.
    hosts.set(Ok(vec![moonraker("m1", second.url())]));
    until("replacement", || state_is(&gateway, &cookie, "m1", "error")).await;
    let first_calls = first.calls();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(first.calls(), first_calls, "replaced poller kept polling");
    assert_eq!(
        gateway
            .state
            .watchers
            .lock()
            .await
            .status("m1")
            .unwrap()
            .state,
        PrinterHostState::Error
    );

    // Sleep: suspend stops every poller and clears observations; resume restarts one.
    let monitoring = gateway.monitoring_control();
    monitoring.suspend().await;
    assert!(observations.all().is_empty());
    let suspended = second.calls();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(second.calls(), suspended, "suspended poller kept polling");
    monitoring.resume();
    until("resume", || async { second.calls() > suspended }).await;
    until("resumed status", || {
        state_is(&gateway, &cookie, "m1", "error")
    })
    .await;

    // Removal cancels the poller and drops the id from the snapshot.
    hosts.set(Ok(vec![]));
    until("removal", || async { observations.latest("m1").is_none() }).await;
    let removed = second.calls();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(second.calls(), removed, "removed poller kept polling");
    assert_eq!(
        status(&gateway, Some(&cookie), "m1", "GET").await,
        (StatusCode::NOT_FOUND, "rust product".into())
    );

    // A malformed stored setting fails closed with no Node fallback.
    hosts.set(Ok(vec![moonraker("m1", second.url())]));
    until("re-add", || state_is(&gateway, &cookie, "m1", "error")).await;
    hosts.set(Err(HostReadError::Invalid));
    until("invalid snapshot", || async {
        status(&gateway, Some(&cookie), "m1", "GET").await.0 == StatusCode::SERVICE_UNAVAILABLE
    })
    .await;
    assert!(observations.all().is_empty());
    assert_eq!(gateway.compat_forwards(), 0);
    only_status_gets(&first);
    only_status_gets(&second);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_rust_only_https_host_reports_offline() {
    let hosts = Hosts(Arc::new(Mutex::new(Ok(vec![moonraker(
        "tls",
        "https://printer.test",
    )]))));
    let (gateway, cookie, _root) = started(&hosts).await;
    until("https status", || {
        state_is(&gateway, &cookie, "tls", "offline")
    })
    .await;
    let (_, body) = status(&gateway, Some(&cookie), "tls", "GET").await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"state": "offline",
            "message": pp_drivers::moonraker::HTTPS_UNAVAILABLE})
    );
    assert_eq!(gateway.compat_forwards(), 0);
    gateway.stop().await.unwrap();
}

/// GRE-364: a wake reconciles against the stored hosts as they are now. The
/// host moved (a network change) while asleep; the asleep status answers
/// unavailable instead of a stale value, and only the new address is polled.
#[tokio::test]
async fn ticket_t_25_wake_reconciles_hosts_changed_during_sleep() {
    let before = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
    let after = RecordedPrinter::start(vec![(200, ERROR)]).unwrap();
    let hosts = Hosts(Arc::new(Mutex::new(Ok(vec![moonraker(
        "m1",
        before.url(),
    )]))));
    let (gateway, cookie, _root) = started(&hosts).await;
    let observations = gateway.printer_observations();
    until("first status", || {
        state_is(&gateway, &cookie, "m1", "printing")
    })
    .await;

    let monitoring = gateway.monitoring_control();
    monitoring.suspend().await;
    assert!(observations.all().is_empty());
    assert_eq!(
        status(&gateway, Some(&cookie), "m1", "GET").await.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "an asleep host is not shown with a stale status"
    );
    hosts.set(Ok(vec![moonraker("m1", after.url())]));
    let asleep = before.calls();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(before.calls(), asleep, "nothing polls while asleep");
    assert_eq!(after.calls(), 0, "nothing polls while asleep");

    monitoring.resume();
    until("reconciled status", || {
        state_is(&gateway, &cookie, "m1", "error")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        before.calls(),
        asleep,
        "the old address is never polled again"
    );
    assert_eq!(gateway.compat_forwards(), 0);
    only_status_gets(&before);
    only_status_gets(&after);
    gateway.stop().await.unwrap();
}
