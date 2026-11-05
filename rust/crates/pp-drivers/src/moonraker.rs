//! One cancellable, bounded status-only task per enabled host. No storage/send APIs.
use anyhow::Result;
use pp_api_types::printer_status::{PrinterHostState as State, PrinterHostStatus as Status};
use pp_net::outbound::{HttpGuard, SystemResolver};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};

pub use pp_api_types::printer_status::{PrinterHostState, PrinterHostStatus};
use tokio_util::sync::CancellationToken;

pub const QUERY: &str =
    "/printer/objects/query?print_stats&virtual_sdcard&display_status&extruder&heater_bed";
pub const MAX_HOSTS: usize = 128;
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_CONFIG_BYTES: usize = 4096;
// JSON can escape every input byte as six bytes (\u00XX), plus keys/punctuation.
pub const MAX_SNAPSHOT_BYTES: usize =
    MAX_HOSTS * (6 * (MAX_ID_BYTES + 2 * MAX_CONFIG_BYTES) + 128) + 2;
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub id: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub enabled: bool,
}
/// Shown for an HTTPS host when no Node runtime can serve it (until GRE-359).
pub const HTTPS_UNAVAILABLE: &str = "HTTPS printer monitoring is not available yet";
/// Shown when a host redirects and no Node runtime can follow it; polling stops.
pub const REDIRECT_UNAVAILABLE: &str = "Moonraker redirected; printer monitoring stopped";
/// The latest status of each watched host, keyed by integration id.
pub type Observed = Arc<BTreeMap<String, Status>>;
/// Read-only port over what the watchers observed. Pollers publish into a
/// `watch` channel that keeps only the newest map, so a slow reader never
/// holds a poller back. It carries status only: no send, retry or Checkoff API.
/// A host leaves the map when its poller stops, so an absent id is not idle.
#[derive(Clone)]
pub struct PrinterObservations(watch::Receiver<Observed>);
impl PrinterObservations {
    pub fn latest(&self, id: &str) -> Option<Status> {
        self.0.borrow().get(id).cloned()
    }
    pub fn all(&self) -> Observed {
        self.0.borrow().clone()
    }
    /// Waits for a map newer than the last one this handle returned. `None`
    /// once the watchers are gone.
    pub async fn changed(&mut self) -> Option<Observed> {
        self.0.changed().await.ok()?;
        Some(self.0.borrow_and_update().clone())
    }
}
/// `Node` hands the host back to Node's on-demand path (HTTPS/redirects) until GRE-359.
#[derive(Clone)]
enum Slot {
    Status(Status),
    Node,
}
/// How the gateway answers `/integrations/:id/status`.
#[derive(Debug, PartialEq)]
pub enum Route {
    Cached(Status),
    /// Not a Moonraker id in the last valid snapshot, or a Node-fallback host.
    Node,
    /// A Moonraker id from the last valid snapshot whose poller is stopped.
    Unavailable,
    /// No valid snapshot has been applied yet (or the last one was invalid).
    NoSnapshot,
}
struct Poller {
    host: Host,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
    status: Arc<Mutex<Slot>>,
}
impl Drop for Poller {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
pub struct Watchers {
    hosts: HashMap<String, Poller>,
    /// Moonraker ids of the last valid snapshot; kept across transient stops.
    known: Option<HashSet<String>>,
    stale_since: Option<tokio::time::Instant>,
    guard: HttpGuard,
    interval: Duration,
    observed: watch::Sender<Observed>,
    /// False in the Rust-only runtime: no Node serves HTTPS or redirecting
    /// hosts, so they report offline instead of routing to Node.
    node_fallback: bool,
}
impl Watchers {
    pub fn new() -> Result<Self> {
        Ok(Self::with_guard(
            HttpGuard::new(
                Arc::new(SystemResolver),
                Duration::from_secs(5),
                2 * 1024 * 1024,
            )?,
            Duration::from_secs(2),
        ))
    }
    pub fn with_guard(guard: HttpGuard, interval: Duration) -> Self {
        assert!(!interval.is_zero());
        Self {
            hosts: HashMap::new(),
            known: None,
            stale_since: None,
            guard,
            interval,
            observed: watch::channel(Observed::default()).0,
            node_fallback: true,
        }
    }
    /// For a runtime with no Node child: HTTPS and redirecting Moonraker hosts
    /// report a fixed offline status, and only non-Moonraker ids route to Node.
    pub fn without_node_fallback(mut self) -> Self {
        self.node_fallback = false;
        self
    }
    pub fn observations(&self) -> PrinterObservations {
        PrinterObservations(self.observed.subscribe())
    }
    pub fn status(&self, id: &str) -> Option<Status> {
        match self.route(id) {
            Route::Cached(status) => Some(status),
            _ => None,
        }
    }
    pub fn route(&self, id: &str) -> Route {
        if let Some(poller) = self.hosts.get(id) {
            return match &*lock(&poller.status) {
                Slot::Status(status) => Route::Cached(status.clone()),
                Slot::Node => Route::Node,
            };
        }
        match &self.known {
            None => Route::NoSnapshot,
            Some(known) if known.contains(id) => Route::Unavailable,
            Some(_) => Route::Node,
        }
    }
    /// Node was slow/unavailable: keep last-known-good pollers for `grace`, then stop
    /// the plain-HTTP pollers. HTTPS/redirect hosts stay on Node's on-demand path, and
    /// the id set is kept so non-Moonraker ids still route straight to Node.
    pub async fn snapshot_unavailable(&mut self, grace: Duration) {
        let now = tokio::time::Instant::now();
        let since = *self.stale_since.get_or_insert(now);
        if now.duration_since(since) < grace {
            return;
        }
        let polled: Vec<_> = self
            .hosts
            .iter()
            .filter(|(_, p)| !matches!(*lock(&p.status), Slot::Node))
            .map(|(id, _)| id.clone())
            .collect();
        for id in polled {
            self.retire(&id).await;
        }
    }
    /// More hosts than the cap: start nothing new, but record every id as Moonraker so a
    /// newly added host fails closed instead of falling back to Node. Existing pollers
    /// get the same grace as an unavailable snapshot.
    pub async fn over_cap(&mut self, hosts: &[Host], grace: Duration) {
        let Some(ids) = valid_ids(hosts) else {
            self.invalidate().await;
            return;
        };
        self.known = Some(ids);
        self.snapshot_unavailable(grace).await;
    }
    /// An invalid or denied snapshot fails closed: stop pollers and forget the id set.
    pub async fn invalidate(&mut self) {
        self.stop().await;
        self.known = None;
    }
    /// The caller serializes snapshots; join removed/replaced tasks before spawning.
    /// An empty snapshot is also the sleep/shutdown/backend-loss cancellation path.
    pub async fn replace(&mut self, hosts: Vec<Host>) -> Result<()> {
        let Some(ids) = valid_ids(&hosts).filter(|_| hosts.len() <= MAX_HOSTS) else {
            self.invalidate().await;
            anyhow::bail!("Invalid host snapshot");
        };
        // Enabled unusable or oversized rows stay known without a polling slot,
        // so only those ids fail closed. Disabled rows need no usable endpoint.
        let wanted: HashMap<_, _> = hosts
            .into_iter()
            .filter(|h| {
                (!h.enabled || !h.base_url.trim().is_empty())
                    && h.base_url.len() <= MAX_CONFIG_BYTES
                    && h.api_key
                        .as_ref()
                        .is_none_or(|key| key.len() <= MAX_CONFIG_BYTES)
            })
            .map(|h| (h.id.clone(), h))
            .collect();
        self.known = Some(ids);
        self.stale_since = None;
        let removed: Vec<_> = self
            .hosts
            .iter()
            .filter(|(id, p)| wanted.get(*id) != Some(&p.host))
            .map(|(id, _)| id.clone())
            .collect();
        for id in removed {
            self.retire(&id).await;
        }
        for (id, host) in wanted {
            if self.hosts.contains_key(&id) {
                continue;
            }
            let cancel = CancellationToken::new();
            let fallback = |message: &str| {
                if self.node_fallback {
                    Slot::Node
                } else {
                    Slot::Status(Status::offline(message))
                }
            };
            let initial = if !host.enabled {
                Slot::Status(Status::offline("Integration disabled"))
            } else if needs_node(&host.base_url) {
                fallback(HTTPS_UNAVAILABLE)
            } else {
                Slot::Status(Status::unknown("Awaiting status"))
            };
            let redirected = fallback(REDIRECT_UNAVAILABLE);
            let status = Arc::new(Mutex::new(Slot::Node));
            publish(&status, &self.observed, &id, initial);
            let task = if host.enabled && !needs_node(&host.base_url) {
                let config = host.clone();
                let stopped = cancel.clone();
                let current = status.clone();
                let observed = self.observed.clone();
                let guard = self.guard.clone();
                let interval = self.interval;
                Some(tokio::spawn(async move {
                    let mut failures = 0u32;
                    loop {
                        let response = tokio::select! { biased; _=stopped.cancelled()=>break, response=poll(&guard,&config)=>response };
                        let delay = match response {
                            Ok(value) => {
                                failures = 0;
                                publish(&current, &observed, &config.id, Slot::Status(value));
                                interval
                            }
                            Err(Failure::Redirect) => {
                                publish(&current, &observed, &config.id, redirected);
                                break;
                            }
                            Err(failure) => {
                                failures = failures.saturating_add(1);
                                let offline = Status::offline(&failure.message());
                                publish(&current, &observed, &config.id, Slot::Status(offline));
                                backoff(interval, failures)
                            }
                        };
                        tokio::select! { biased; _=stopped.cancelled()=>break, _=tokio::time::sleep(delay)=>{} }
                    }
                }))
            } else {
                None
            };
            self.hosts.insert(
                id,
                Poller {
                    host,
                    cancel,
                    task,
                    status,
                },
            );
        }
        Ok(())
    }
    /// Cancel and join every poller. The last valid id set is kept.
    pub async fn stop(&mut self) {
        let ids: Vec<_> = self.hosts.keys().cloned().collect();
        for id in ids {
            self.retire(&id).await;
        }
    }
    /// Cancel and join one poller, then drop its observation: a stopped host
    /// must not look live to observation readers.
    async fn retire(&mut self, id: &str) {
        if let Some(mut poller) = self.hosts.remove(id) {
            poller.cancel.cancel();
            if let Some(task) = poller.task.take() {
                let _ = task.await;
            }
        }
        self.observed.send_if_modified(|observed| {
            observed.contains_key(id) && Arc::make_mut(observed).remove(id).is_some()
        });
    }
}
/// Store a slot value and mirror it into the observation map. Node-routed
/// hosts have no Rust observation. The slot is written first and stays locked
/// until the map is published, so a reader woken by the port already sees the
/// same value through `route`.
fn publish(slot: &Mutex<Slot>, observed: &watch::Sender<Observed>, id: &str, value: Slot) {
    let mut current = lock(slot);
    *current = value;
    observed.send_if_modified(|map| match &*current {
        Slot::Status(status) if map.get(id) != Some(status) => {
            Arc::make_mut(map).insert(id.to_owned(), status.clone());
            true
        }
        Slot::Status(_) => false,
        Slot::Node => map.contains_key(id) && Arc::make_mut(map).remove(id).is_some(),
    });
}
/// Non-empty, bounded and unique ids, or `None` when the snapshot is unusable.
fn valid_ids(hosts: &[Host]) -> Option<HashSet<String>> {
    let ids: HashSet<_> = hosts
        .iter()
        .filter(|h| !h.id.is_empty() && h.id.len() <= MAX_ID_BYTES)
        .map(|h| h.id.clone())
        .collect();
    (ids.len() == hosts.len()).then_some(ids)
}
/// A slot only ever holds a whole value, so a poisoned lock is still consistent.
fn lock(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}
/// The status guard is plain HTTP without redirects; Node's moonrakerFetch serves
/// HTTPS and follows checked redirects, so those hosts stay on Node until GRE-359.
fn needs_node(base_url: &str) -> bool {
    base_url
        .trim()
        .get(..6)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https:"))
}
/// A fixed set of messages: printer/transport error text is never echoed.
enum Failure {
    Redirect,
    Http(u16),
    Unavailable,
}
impl Failure {
    fn message(&self) -> String {
        match self {
            Failure::Http(status) => format!("Moonraker returned HTTP {status}"),
            _ => "Moonraker status unavailable".into(),
        }
    }
}
fn backoff(interval: Duration, failures: u32) -> Duration {
    interval
        .saturating_mul(1u32 << failures.min(5))
        .min(Duration::from_secs(60))
}
async fn poll(guard: &HttpGuard, host: &Host) -> Result<Status, Failure> {
    // Accept only an origin plus optional provider path; never append to a query.
    let base = host.base_url.trim().trim_end_matches('/');
    if base.contains('?') || base.contains('#') {
        return Err(Failure::Unavailable);
    }
    let reply = guard
        .get(
            &format!("{base}{QUERY}"),
            host.api_key.as_deref().map(str::trim),
        )
        .await
        .map_err(|_| Failure::Unavailable)?;
    match reply.status {
        200 => recorded_status(&reply.body, base).map_err(|_| Failure::Unavailable),
        300..=399 => Err(Failure::Redirect),
        status => Err(Failure::Http(status)),
    }
}
pub fn recorded_status(body: &[u8], base: &str) -> Result<Status> {
    let value: Value = serde_json::from_slice(body)?;
    let status = value
        .get("result")
        .and_then(|v| v.get("status"))
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Missing status"))?;
    let stats = status.get("print_stats");
    let raw = stats.and_then(|v| v.get("state")).and_then(Value::as_str);
    let state = match raw.map(str::to_lowercase).as_deref() {
        Some("printing") => State::Printing,
        Some("paused") => State::Paused,
        Some("complete") => State::Complete,
        Some("error") => State::Error,
        Some("standby" | "cancelled") => State::Idle,
        _ => State::Unknown,
    };
    let mut result = Status::offline("");
    result.state = state;
    result.filename = stats
        .and_then(|v| v.get("filename"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    result.message = match result.state {
        State::Printing => result
            .filename
            .as_ref()
            .map(|f| format!("Printing {f}"))
            .or_else(|| raw.map(str::to_owned)),
        State::Complete => Some(
            result
                .filename
                .as_ref()
                .map_or_else(|| "Complete".into(), |f| format!("Complete · {f}")),
        ),
        State::Idle => Some("Idle".into()),
        _ => raw.map(str::to_owned),
    };
    // Node reports WHATWG `URL.hostname`; the url crate implements the same parser,
    // so case and IP-literal canonicalization match.
    result.ip_address = url::Url::parse(base)?.host_str().map(str::to_owned);
    let number = |object: &str, field: &str| {
        status
            .get(object)
            .and_then(|v| v.get(field))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
    };
    if matches!(result.state, State::Printing | State::Paused) {
        let progress = status
            .get("virtual_sdcard")
            .and_then(|v| v.get("progress"))
            .filter(|v| !v.is_null())
            .or_else(|| status.get("display_status").and_then(|v| v.get("progress")));
        result.progress = progress
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .map(|v| (v * 100.0).clamp(0.0, 100.0).round());
    }
    result.nozzle_temperature_c = number("extruder", "temperature");
    result.nozzle_target_c = number("extruder", "target");
    result.bed_temperature_c = number("heater_bed", "temperature");
    result.bed_target_c = number("heater_bed", "target");
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_t_4_maximum_escaped_snapshot_fits_transport_bound() {
        let hosts: Vec<_> = (0..MAX_HOSTS)
            .map(|i| {
                serde_json::json!({
                    "id": format!("{i:03}{}", "\u{0001}".repeat(MAX_ID_BYTES - 3)),
                    "base_url": "\u{0001}".repeat(MAX_CONFIG_BYTES),
                    "api_key": "\u{0001}".repeat(MAX_CONFIG_BYTES), "enabled": false
                })
            })
            .collect();
        let bytes = serde_json::to_vec(&hosts).unwrap();
        assert!(bytes.len() > 1024 * 1024);
        assert!(bytes.len() <= MAX_SNAPSHOT_BYTES);
        let hosts: Vec<Host> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(hosts.len(), MAX_HOSTS);
    }
    use pp_testkit::moonraker::RecordedPrinter;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    const SUCCESS: &[u8] =
        include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-success.json");
    // Recorded body mapping is pinned against Node by tests/ticket_t_4_watcher_proof_parity.rs.
    #[test]
    fn ticket_t_4_offline_wire_shape() {
        assert_eq!(
            serde_json::to_value(Status::offline("Moonraker status unavailable")).unwrap(),
            serde_json::json!({"state":"offline","message":"Moonraker status unavailable"})
        );
    }
    async fn simulator(stall: bool) -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = [0; 4096];
                let len = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..len]);
                assert!(request.starts_with(&format!("GET {QUERY} HTTP/1.1")));
                assert!(request.contains("x-api-key: fixture-key"));
                seen.fetch_add(1, Ordering::SeqCst);
                if stall {
                    let mut end = [0; 1];
                    assert_eq!(socket.read(&mut end).await.unwrap(), 0);
                } else {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        SUCCESS.len()
                    );
                    socket.write_all(head.as_bytes()).await.unwrap();
                    socket.write_all(SUCCESS).await.unwrap();
                }
            }
        });
        (url, calls, task)
    }
    fn host(url: String) -> Host {
        Host {
            id: "host".into(),
            base_url: url,
            api_key: Some("fixture-key".into()),
            enabled: true,
        }
    }
    async fn wait_calls(calls: &AtomicUsize, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn ticket_t_4_hidden_window_no_duplicate_and_replacement_removal() {
        let (url, calls, server) = simulator(false).await;
        let guard = HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 4096).unwrap();
        let mut watchers = Watchers::with_guard(guard, Duration::from_millis(40));
        let config = host(url.clone());
        watchers.replace(vec![config.clone()]).await.unwrap();
        // No UI client is connected; status updates solely from the owner task.
        wait_calls(&calls, 2).await;
        for _ in 0..20 {
            watchers.replace(vec![config.clone()]).await.unwrap();
        }
        assert_eq!(watchers.hosts.len(), 1);
        assert_eq!(watchers.status("host").unwrap().state, State::Printing);
        let mut disabled = config.clone();
        disabled.enabled = false;
        watchers.replace(vec![disabled]).await.unwrap();
        let before = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), before);
        assert_eq!(watchers.status("host").unwrap().state, State::Offline);
        watchers.replace(vec![config.clone()]).await.unwrap();
        wait_calls(&calls, before + 1).await;
        // Sleep/shutdown use the same cancellation path, and resume has one task.
        watchers.stop().await;
        watchers.replace(vec![config]).await.unwrap();
        wait_calls(&calls, before + 2).await;
        watchers.replace(vec![]).await.unwrap();
        let before = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), before);
        assert!(watchers.status("host").is_none());
        server.abort();
    }
    #[tokio::test]
    async fn ticket_t_4_prompt_inflight_cancellation_and_invalid_snapshot() {
        let (url, calls, server) = simulator(true).await;
        let mut watchers = Watchers::new().unwrap();
        watchers.replace(vec![host(url.clone())]).await.unwrap();
        wait_calls(&calls, 1).await;
        tokio::time::timeout(Duration::from_millis(200), watchers.replace(vec![]))
            .await
            .unwrap()
            .unwrap();
        assert!(watchers.hosts.is_empty());
        watchers.replace(vec![host(url.clone())]).await.unwrap();
        wait_calls(&calls, 2).await;
        assert!(
            watchers
                .replace(vec![host(url.clone()), host(url)])
                .await
                .is_err()
        );
        assert!(watchers.hosts.is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn ticket_t_4_http_errors_backoff_and_offline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = [0; 4096];
                let _ = socket.read(&mut data).await;
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
        let mut watchers = Watchers::with_guard(
            HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 4096).unwrap(),
            Duration::from_millis(50),
        );
        watchers.replace(vec![host(url)]).await.unwrap();
        wait_calls(&calls, 1).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(watchers.status("host").unwrap().state, State::Offline);
        assert_eq!(
            watchers.status("host").unwrap().message.as_deref(),
            Some("Moonraker returned HTTP 503")
        );
        assert!(calls.load(Ordering::SeqCst) <= 3);
        assert_eq!(backoff(Duration::from_secs(2), 1), Duration::from_secs(4));
        assert_eq!(
            backoff(Duration::from_secs(2), 100),
            Duration::from_secs(60)
        );
        watchers.stop().await;
        server.abort();
    }
    #[tokio::test]
    async fn gre_335_last_valid_snapshot_routes_other_ids_to_node() {
        let mut watchers = Watchers::new().unwrap();
        assert_eq!(watchers.route("prusa"), Route::NoSnapshot);
        let mut disabled = host("http://printer.test".into());
        disabled.enabled = false;
        watchers.replace(vec![disabled.clone()]).await.unwrap();
        assert!(matches!(watchers.route("host"), Route::Cached(_)));
        assert_eq!(watchers.route("prusa"), Route::Node);
        // Node slow/unavailable: last-known-good pollers survive the grace period.
        watchers
            .snapshot_unavailable(Duration::from_millis(100))
            .await;
        assert!(matches!(watchers.route("host"), Route::Cached(_)));
        tokio::time::sleep(Duration::from_millis(120)).await;
        watchers
            .snapshot_unavailable(Duration::from_millis(100))
            .await;
        assert_eq!(watchers.route("host"), Route::Unavailable);
        assert_eq!(watchers.route("prusa"), Route::Node);
        // A valid snapshot restarts pollers and resets the grace window.
        watchers.replace(vec![disabled.clone()]).await.unwrap();
        watchers
            .snapshot_unavailable(Duration::from_millis(100))
            .await;
        assert!(matches!(watchers.route("host"), Route::Cached(_)));
        // Invalid snapshots fail closed and forget the id set.
        assert!(
            watchers
                .replace(vec![disabled.clone(), disabled])
                .await
                .is_err()
        );
        assert_eq!(watchers.route("host"), Route::NoSnapshot);
        assert_eq!(watchers.route("prusa"), Route::NoSnapshot);
    }
    #[tokio::test]
    async fn gre_335_awaiting_status_is_unknown() {
        let (url, calls, server) = simulator(true).await;
        let mut watchers = Watchers::new().unwrap();
        watchers.replace(vec![host(url)]).await.unwrap();
        wait_calls(&calls, 1).await;
        assert_eq!(
            serde_json::to_value(watchers.status("host").unwrap()).unwrap(),
            serde_json::json!({"state":"unknown","message":"Awaiting status"})
        );
        watchers.stop().await;
        server.abort();
    }
    #[tokio::test]
    async fn gre_335_https_and_redirect_hosts_fall_back_to_node() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = [0; 4096];
                let _ = socket.read(&mut data).await;
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: /moonraker/\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await;
            }
        });
        let mut watchers = Watchers::with_guard(
            HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 4096).unwrap(),
            Duration::from_millis(20),
        );
        let mut https = host(format!(" HTTPS://{address}"));
        https.id = "tls".into();
        watchers
            .replace(vec![https, host(format!("http://{address}"))])
            .await
            .unwrap();
        // HTTPS never reaches the plain-HTTP guard; Node serves it on demand.
        assert_eq!(watchers.route("tls"), Route::Node);
        wait_calls(&calls, 1).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while watchers.route("host") != Route::Node {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // The redirecting host's poller stops instead of retrying the redirect.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Past the snapshot grace period both hosts stay on Node's path.
        watchers.snapshot_unavailable(Duration::ZERO).await;
        assert_eq!(watchers.route("tls"), Route::Node);
        assert_eq!(watchers.route("host"), Route::Node);
        watchers.stop().await;
        server.abort();
    }
    #[tokio::test]
    async fn gre_335_over_cap_snapshot_fails_new_moonraker_ids_closed() {
        let mut watchers = Watchers::new().unwrap();
        let mut known = host("http://printer.test".into());
        known.enabled = false;
        watchers.replace(vec![known.clone()]).await.unwrap();
        // A 129th host arrives with the over-cap snapshot: it must not route to Node.
        let over: Vec<_> = (0..=MAX_HOSTS)
            .map(|i| Host {
                id: if i == 0 {
                    "host".into()
                } else {
                    format!("new-{i}")
                },
                enabled: i != 0,
                ..known.clone()
            })
            .collect();
        watchers.over_cap(&over, Duration::from_secs(60)).await;
        assert!(matches!(watchers.route("host"), Route::Cached(_)));
        assert_eq!(watchers.route("new-128"), Route::Unavailable);
        assert_eq!(watchers.route("prusa"), Route::Node);
        assert_eq!(watchers.hosts.len(), 1, "no poller started over the cap");
        // Past grace the old poller stops too; other printers still reach Node.
        watchers.over_cap(&over, Duration::ZERO).await;
        assert_eq!(watchers.route("host"), Route::Unavailable);
        assert_eq!(watchers.route("new-128"), Route::Unavailable);
        assert_eq!(watchers.route("prusa"), Route::Node);
    }
    #[tokio::test]
    async fn disabled_empty_endpoints_cache_offline_without_polling() {
        let mut watchers = Watchers::new().unwrap();
        for endpoint in ["", "   "] {
            let mut disabled = host(endpoint.into());
            disabled.enabled = false;
            watchers.replace(vec![disabled]).await.unwrap();
            assert_eq!(
                watchers.route("host"),
                Route::Cached(Status::offline("Integration disabled"))
            );
            assert!(watchers.hosts["host"].task.is_none());
        }
        watchers.replace(vec![host(String::new())]).await.unwrap();
        assert_eq!(watchers.route("host"), Route::Unavailable);
        assert!(watchers.hosts.is_empty());
    }

    fn only_status_queries(printer: &RecordedPrinter) {
        let lines = printer.request_lines();
        assert!(!lines.is_empty());
        assert!(
            lines
                .iter()
                .all(|line| *line == format!("GET {QUERY} HTTP/1.1")),
            "{lines:?}"
        );
    }
    async fn next_state(observations: &mut PrinterObservations, id: &str, state: State) -> Status {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = observations.latest(id).filter(|s| s.state == state) {
                    return status;
                }
                observations.changed().await.expect("watchers dropped");
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{id} never reached {state:?}"))
    }
    fn watchers_every(interval: Duration) -> Watchers {
        let guard = HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 4096).unwrap();
        Watchers::with_guard(guard, interval)
    }
    #[tokio::test]
    async fn ticket_t_4_watcher_proof_observation_port_replays_recorded_responses() {
        const ERROR: &[u8] =
            include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-error.json");
        const OFFLINE: &[u8] =
            include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-offline.json");
        let printer = RecordedPrinter::start(vec![
            (200, SUCCESS),
            (200, ERROR),
            (503, OFFLINE),
            (200, SUCCESS),
        ])
        .unwrap();
        // The port keeps only the newest map, so each recorded reply must stay
        // current long enough for this reader to see it: 250 ms between polls.
        let mut watchers = watchers_every(Duration::from_millis(250));
        let mut observations = watchers.observations();
        assert!(observations.all().is_empty());
        let config = host(printer.url().into());
        watchers.replace(vec![config.clone()]).await.unwrap();
        let printing = next_state(&mut observations, "host", State::Printing).await;
        // The slot is written before the port notifies, so both agree.
        assert_eq!(watchers.status("host"), Some(printing.clone()));
        assert_eq!(printing.filename.as_deref(), Some("bracket.gcode"));
        next_state(&mut observations, "host", State::Error).await;
        let offline = next_state(&mut observations, "host", State::Offline).await;
        assert_eq!(
            offline.message.as_deref(),
            Some("Moonraker returned HTTP 503")
        );
        // Recovery after one failure resets the backoff and is observed too.
        next_state(&mut observations, "host", State::Printing).await;
        // Disabling replaces the poller; the port shows the disabled status.
        let mut disabled = config.clone();
        disabled.enabled = false;
        watchers.replace(vec![disabled]).await.unwrap();
        assert_eq!(
            observations.latest("host"),
            Some(Status::offline("Integration disabled"))
        );
        let before = printer.calls();
        // Removal drops the observation, so a stopped host never looks live.
        watchers.replace(vec![]).await.unwrap();
        assert!(observations.all().is_empty());
        watchers.replace(vec![config]).await.unwrap();
        next_state(&mut observations, "host", State::Printing).await;
        watchers.stop().await;
        assert!(observations.all().is_empty());
        let stopped = printer.calls();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(printer.calls(), stopped);
        assert!(stopped > before);
        only_status_queries(&printer);
        // Dropping the watchers ends the port once unseen maps are read.
        drop(watchers);
        tokio::time::timeout(Duration::from_secs(1), async {
            while observations.changed().await.is_some() {}
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn ticket_t_4_watcher_proof_without_node_https_and_redirect_report_offline() {
        let printer = RecordedPrinter::start(vec![(302, b"")]).unwrap();
        let mut watchers = watchers_every(Duration::from_millis(20)).without_node_fallback();
        let observations = watchers.observations();
        let mut https = host(" HTTPS://printer.test".into());
        https.id = "tls".into();
        watchers
            .replace(vec![https, host(printer.url().into())])
            .await
            .unwrap();
        // No Node can serve HTTPS: a fixed offline status, and no request at all.
        let https_offline = Status::offline(HTTPS_UNAVAILABLE);
        assert_eq!(watchers.route("tls"), Route::Cached(https_offline.clone()));
        assert_eq!(observations.latest("tls"), Some(https_offline));
        let redirected = Route::Cached(Status::offline(REDIRECT_UNAVAILABLE));
        tokio::time::timeout(Duration::from_secs(1), async {
            while watchers.route("host") != redirected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // The redirect is never followed or retried.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(printer.calls(), 1);
        only_status_queries(&printer);
        // Only ids outside the snapshot are someone else's.
        assert_eq!(watchers.route("prusa"), Route::Node);
        watchers.stop().await;
        assert_eq!(watchers.route("tls"), Route::Unavailable);
    }

    #[tokio::test]
    async fn gre_335_oversized_row_only_fails_its_own_id() {
        let mut watchers = Watchers::new().unwrap();
        let mut healthy = host("http://printer.test".into());
        healthy.enabled = false;
        let oversized = Host {
            id: "big".into(),
            base_url: "h".repeat(MAX_CONFIG_BYTES + 1),
            ..healthy.clone()
        };
        let big_key = Host {
            id: "key".into(),
            api_key: Some("k".repeat(MAX_CONFIG_BYTES + 1)),
            ..healthy.clone()
        };
        let projected = Host {
            id: "projected".into(),
            base_url: String::new(),
            enabled: true,
            ..healthy.clone()
        };
        watchers
            .replace(vec![healthy, oversized, big_key, projected])
            .await
            .unwrap();
        assert!(matches!(watchers.route("host"), Route::Cached(_)));
        assert_eq!(watchers.route("big"), Route::Unavailable);
        assert_eq!(watchers.route("key"), Route::Unavailable);
        assert_eq!(watchers.route("projected"), Route::Unavailable);
        assert_eq!(watchers.route("prusa"), Route::Node);
    }
}
