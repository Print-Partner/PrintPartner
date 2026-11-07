use anyhow::Context;
use pp_drivers::moonraker::{Host, Route, Watchers};
use pp_net::outbound::{HttpGuard, Resolution, Resolver};
use serde_json::{Value, json};
use std::{future::Future, net::IpAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const BOUND: Duration = Duration::from_secs(2);
const NETWORK_DEADLINE: Duration = Duration::from_secs(5);
const SUCCESS: &[u8] =
    include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-success.json");
const ERROR: &[u8] = include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-error.json");

async fn bounded<T>(label: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .unwrap_or_else(|_| panic!("Timed out waiting for {label}"))
}

struct Lookup {
    host: String,
    answer: oneshot::Sender<Vec<IpAddr>>,
    dropped: oneshot::Receiver<()>,
}

struct DropAck(Option<oneshot::Sender<()>>);

impl Drop for DropAck {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

struct ScriptedResolver(mpsc::UnboundedSender<Lookup>);

impl Resolver for ScriptedResolver {
    fn resolve(&self, host: String) -> Resolution {
        let requests = self.0.clone();
        Box::pin(async move {
            let (answer, receiver) = oneshot::channel();
            let (ack, dropped) = oneshot::channel();
            let _drop_ack = DropAck(Some(ack));
            requests
                .send(Lookup {
                    host,
                    answer,
                    dropped,
                })
                .map_err(|_| anyhow::anyhow!("DNS script closed"))?;
            receiver.await.context("DNS answer abandoned")
        })
    }
}

fn watchers() -> (Watchers, mpsc::UnboundedReceiver<Lookup>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let guard = HttpGuard::new(
        Arc::new(ScriptedResolver(sender)),
        NETWORK_DEADLINE,
        2 * 1024 * 1024,
    )
    .unwrap();
    (
        Watchers::with_guard(guard, Duration::from_millis(20)),
        receiver,
    )
}

async fn lookup(receiver: &mut mpsc::UnboundedReceiver<Lookup>, expected: &str) -> Lookup {
    let request = bounded("DNS lookup start", receiver.recv()).await.unwrap();
    assert_eq!(request.host, expected);
    println!("DNS lookup started for {}", request.host);
    request
}

fn allow(request: Lookup) {
    request
        .answer
        .send(vec!["127.0.0.1".parse().unwrap()])
        .unwrap();
}

struct Request {
    wire: String,
    reply: oneshot::Sender<&'static [u8]>,
}

#[derive(Debug, Default)]
struct WireLog {
    connections: usize,
    requests: Vec<String>,
}

struct Simulator {
    port: u16,
    requests: mpsc::UnboundedReceiver<Request>,
    cancel: CancellationToken,
    task: Option<JoinHandle<WireLog>>,
}

impl Simulator {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, requests) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let stopped = cancel.clone();
        let task = tokio::spawn(async move {
            let mut log = WireLog::default();
            loop {
                let serve = async {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    log.connections += 1;
                    let mut bytes = Vec::new();
                    while !bytes.ends_with(b"\r\n\r\n") {
                        assert!(bytes.len() < 8192, "Oversized simulator request");
                        let byte = socket.read_u8().await.unwrap();
                        bytes.push(byte);
                    }
                    let wire = String::from_utf8(bytes).unwrap();
                    log.requests.push(wire.clone());
                    let (reply, response) = oneshot::channel();
                    sender.send(Request { wire, reply }).unwrap();
                    let body = response.await.unwrap();
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    socket.write_all(headers.as_bytes()).await.unwrap();
                    socket.write_all(body).await.unwrap();
                    socket.shutdown().await.unwrap();
                };
                tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    _ = serve => {},
                }
            }
            log
        });
        Self {
            port,
            requests,
            cancel,
            task: Some(task),
        }
    }

    fn host(&self, hostname: &str, key: &str) -> Host {
        Host {
            id: "printer".into(),
            base_url: format!("http://{hostname}:{}", self.port),
            api_key: Some(key.into()),
            enabled: true,
            kind: Default::default(),
        }
    }

    async fn respond(&mut self, hostname: &str, credential: &str, body: &'static [u8]) {
        let request = bounded("simulator HTTP request", self.requests.recv())
            .await
            .unwrap();
        let mut lines = request.wire.split("\r\n");
        assert_eq!(
            lines.next(),
            Some(
                "GET /printer/objects/query?print_stats&virtual_sdcard&display_status&extruder&heater_bed HTTP/1.1"
            )
        );
        let mut headers: Vec<_> = lines.filter(|line| !line.is_empty()).collect();
        headers.sort_unstable();
        let authority = format!("host: {hostname}:{}", self.port);
        let mut expected = vec!["connection: close", authority.as_str(), credential];
        expected.sort_unstable();
        assert_eq!(headers, expected);
        println!("Observed request {:?}", request.wire);
        request.reply.send(body).unwrap();
    }

    async fn finish(mut self, expected_requests: usize) {
        self.cancel.cancel();
        let log = bounded("simulator task join", self.task.as_mut().unwrap())
            .await
            .unwrap();
        self.task.take();
        assert_eq!(log.connections, expected_requests, "{log:?}");
        assert_eq!(log.requests.len(), expected_requests, "{log:?}");
        assert!(self.requests.recv().await.is_none());
        println!("Simulator joined and listener dropped. {log:?}");
    }
}

impl Drop for Simulator {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

fn printing(hostname: &str) -> Value {
    json!({
        "state": "printing", "progress": 43.0, "filename": "bracket.gcode",
        "message": "Printing bracket.gcode", "ip_address": hostname,
        "nozzle_temperature_c": 210.2, "nozzle_target_c": 215.0,
        "bed_temperature_c": 59.8, "bed_target_c": 60.0
    })
}

fn printer_error(hostname: &str) -> Value {
    json!({
        "state": "error", "filename": "bracket.gcode",
        "message": "error", "ip_address": hostname
    })
}

async fn status(watchers: &Watchers, expected: Value) {
    tokio::time::timeout(BOUND, async {
        loop {
            if let Route::Cached(actual) = watchers.route("printer")
                && serde_json::to_value(actual).unwrap() == expected
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Expected status {expected}, observed route {:?}",
            watchers.route("printer")
        )
    });
    let Route::Cached(actual) = watchers.route("printer") else {
        panic!("Expected a cached status");
    };
    assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    println!("Exact public status {expected}");
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_mixed_dns_rebinding_goes_offline_without_second_request() {
    let mut server = Simulator::start().await;
    let (mut watchers, mut dns) = watchers();
    watchers
        .replace(vec![server.host("printer.test", " status-key ")])
        .await
        .unwrap();
    allow(lookup(&mut dns, "printer.test").await);
    server
        .respond("printer.test", "x-api-key: status-key", SUCCESS)
        .await;
    status(&watchers, printing("printer.test")).await;

    let unsafe_lookup = lookup(&mut dns, "printer.test").await;
    unsafe_lookup
        .answer
        .send(vec![
            "127.0.0.1".parse().unwrap(),
            "169.254.169.254".parse().unwrap(),
        ])
        .unwrap();
    status(
        &watchers,
        json!({"state": "offline", "message": "Moonraker status unavailable"}),
    )
    .await;
    bounded("stop after prohibited DNS", watchers.stop()).await;
    assert_eq!(watchers.route("printer"), Route::Unavailable);
    server.finish(1).await;
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_allowed_dns_control_makes_second_request() {
    let mut server = Simulator::start().await;
    let (mut watchers, mut dns) = watchers();
    watchers
        .replace(vec![server.host("printer.test", "a.b.c")])
        .await
        .unwrap();
    allow(lookup(&mut dns, "printer.test").await);
    server
        .respond("printer.test", "authorization: Bearer a.b.c", SUCCESS)
        .await;
    status(&watchers, printing("printer.test")).await;
    allow(lookup(&mut dns, "printer.test").await);
    server
        .respond("printer.test", "authorization: Bearer a.b.c", ERROR)
        .await;
    status(&watchers, printer_error("printer.test")).await;
    bounded("stop allowed DNS control", watchers.stop()).await;
    server.finish(2).await;
}

enum Cancel {
    Remove,
    Stop,
}

async fn cancel_pending_lookup(action: Cancel) {
    let mut server = Simulator::start().await;
    let (mut watchers, mut dns) = watchers();
    let host = server.host("printer.test", "status-key");
    watchers.replace(vec![host.clone()]).await.unwrap();
    let pending = lookup(&mut dns, "printer.test").await;
    bounded(
        "pending DNS cancellation and drop before five-second deadline",
        async {
            match action {
                Cancel::Remove => watchers.replace(Vec::new()).await.unwrap(),
                Cancel::Stop => watchers.stop().await,
            }
            pending.dropped.await.unwrap();
        },
    )
    .await;
    println!("Pending DNS future drop acknowledged within the two-second bound");
    match action {
        Cancel::Remove => assert_eq!(watchers.route("printer"), Route::Node),
        Cancel::Stop => assert_eq!(watchers.route("printer"), Route::Unavailable),
    }
    assert!(
        pending
            .answer
            .send(vec!["127.0.0.1".parse().unwrap()])
            .is_err()
    );
    watchers.replace(vec![host]).await.unwrap();
    allow(lookup(&mut dns, "printer.test").await);
    server
        .respond("printer.test", "x-api-key: status-key", SUCCESS)
        .await;
    status(&watchers, printing("printer.test")).await;
    bounded("stop resumed poller", watchers.stop()).await;
    server.finish(1).await;
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_remove_drops_pending_dns_and_resumes() {
    cancel_pending_lookup(Cancel::Remove).await;
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_stop_drops_pending_dns_and_resumes() {
    cancel_pending_lookup(Cancel::Stop).await;
}

#[tokio::test]
async fn ticket_t_4_watcher_proof_same_id_replacement_drops_old_dns_and_ignores_release() {
    let mut old_server = Simulator::start().await;
    let mut new_server = Simulator::start().await;
    let (mut watchers, mut dns) = watchers();
    watchers
        .replace(vec![old_server.host("old.test", "old-key")])
        .await
        .unwrap();
    allow(lookup(&mut dns, "old.test").await);
    old_server
        .respond("old.test", "x-api-key: old-key", SUCCESS)
        .await;
    status(&watchers, printing("old.test")).await;
    let mut pending = lookup(&mut dns, "old.test").await;
    bounded(
        "replace pending old host",
        watchers.replace(vec![new_server.host("new.test", "new-key")]),
    )
    .await
    .unwrap();
    assert_eq!(pending.dropped.try_recv(), Ok(()));
    println!("Old DNS future dropped before replacement response");
    allow(lookup(&mut dns, "new.test").await);
    new_server
        .respond("new.test", "x-api-key: new-key", ERROR)
        .await;
    status(&watchers, printer_error("new.test")).await;

    assert!(
        pending
            .answer
            .send(vec!["127.0.0.1".parse().unwrap()])
            .is_err()
    );
    println!("Late old DNS answer rejected because the receiver was dropped");
    allow(lookup(&mut dns, "new.test").await);
    new_server
        .respond("new.test", "x-api-key: new-key", ERROR)
        .await;
    let next = lookup(&mut dns, "new.test").await;
    status(&watchers, printer_error("new.test")).await;
    bounded("stop replacement", watchers.stop()).await;
    bounded("drop final replacement lookup", next.dropped)
        .await
        .unwrap();
    old_server.finish(1).await;
    new_server.finish(2).await;
}
