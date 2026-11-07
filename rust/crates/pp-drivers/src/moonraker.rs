//! One cancellable, bounded status-only task per enabled host. No storage/send APIs.
//! Hosts are Moonraker unless a host says it is PrusaLink (GRE-1175), which
//! the watcher polls through the PrusaLink driver's status read.
use anyhow::Result;
use pp_api_types::printer_status::{PrinterHostState as State, PrinterHostStatus as Status};
use pp_net::outbound::{HttpGuard, SystemResolver};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};

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
    /// Node's host snapshot lists only Moonraker hosts and has no such field.
    #[serde(skip, default)]
    pub kind: HostKind,
}
/// Which status read a watched host gets.
#[derive(Clone, Default, PartialEq, Eq)]
pub enum HostKind {
    #[default]
    Moonraker,
    /// Polled with the PrusaLink driver's Digest-authenticated status read.
    PrusaLink {
        username: String,
        password: Option<String>,
    },
}
/// A PrusaLink status read is a Digest probe plus one or two GETs, so it
/// runs at the web client's default status cadence rather than Moonraker's.
pub const PRUSALINK_INTERVAL: Duration = Duration::from_secs(5);
/// Shown for an HTTPS host when no Node runtime can serve it (until GRE-359).
pub const HTTPS_UNAVAILABLE: &str = "HTTPS printer monitoring is not available yet";
/// Shown when a host redirects and no Node runtime can follow it; polling stops.
pub const REDIRECT_UNAVAILABLE: &str = "Moonraker redirected; printer monitoring stopped";
/// The latest status of each watched host, keyed by integration id.
pub type Observed = Arc<BTreeMap<String, Status>>;
/// When each watched host last answered a status poll.
type Heard = Arc<Mutex<HashMap<String, SystemTime>>>;
/// An unanswered spell shorter than this that ends in the same state is a
/// missed poll, not a new job: no plate prints in five minutes.
const OUTAGE_GRACE: Duration = Duration::from_secs(300);
/// Each watched host's last answered state, when it entered it, and when its
/// current unanswered spell began.
type Entered = Arc<Mutex<HashMap<String, Spell>>>;
#[derive(Clone)]
struct Spell {
    state: State,
    since: SystemTime,
    unanswered: Option<SystemTime>,
}
/// Status changes kept for a reader that falls behind `PrinterObservations::transitions`.
pub const TRANSITION_CAPACITY: usize = 1024;
/// One status change. `seq` counts changes in publish order, so a reader can
/// skip the ones a [`PrinterObservations::snapshot`] already holds.
#[derive(Clone, Debug)]
pub struct Transition {
    pub seq: u64,
    pub id: String,
    pub status: Status,
}
/// The senders behind [`PrinterObservations`]: the newest map, every change
/// in order, and when each host entered its current state.
#[derive(Clone)]
struct Port {
    observed: watch::Sender<Observed>,
    transitions: broadcast::Sender<Transition>,
    published: Arc<AtomicU64>,
    entered: Entered,
}
/// Read-only port over what the watchers observed. Pollers publish into a
/// `watch` channel that keeps only the newest map, so a slow reader never
/// holds a poller back. It carries status only: no send, retry or Checkoff API.
/// A host leaves the map when its poller stops, so an absent id is not idle.
#[derive(Clone)]
pub struct PrinterObservations {
    observed: watch::Receiver<Observed>,
    heard: Heard,
    entered: Entered,
    transitions: broadcast::Sender<Transition>,
    published: Arc<AtomicU64>,
}
impl PrinterObservations {
    pub fn latest(&self, id: &str) -> Option<Status> {
        self.observed.borrow().get(id).cloned()
    }
    pub fn all(&self) -> Observed {
        self.observed.borrow().clone()
    }
    /// When the host last answered a status poll; kept while it is offline
    /// and dropped with its poller. A repeated identical status changes this
    /// time without waking `changed`.
    pub fn last_heard(&self, id: &str) -> Option<SystemTime> {
        lock(&self.heard).get(id).copied()
    }
    /// When the host entered its last answered state, as this process saw it.
    /// A changed temperature or progress at the same state does not move it,
    /// and neither does an `offline` or `unknown` spell shorter than
    /// [`OUTAGE_GRACE`] that ends in the same state: a missed poll is not a
    /// new job. A longer outage could hide one, so it starts the state afresh.
    pub fn state_since(&self, id: &str) -> Option<SystemTime> {
        lock(&self.entered).get(id).map(|spell| spell.since)
    }
    /// Every status change in publish order, including ones the newest-only
    /// map coalesced away. A reader more than [`TRANSITION_CAPACITY`] changes
    /// behind gets `Lagged`. Removals are not transitions: read `all`.
    pub fn transitions(&self) -> broadcast::Receiver<Transition> {
        self.transitions.subscribe()
    }
    /// The newest map and the `seq` of the last transition it includes, read
    /// together: a later transition has a higher `seq`.
    pub fn snapshot(&self) -> (Observed, u64) {
        let map = self.observed.borrow();
        (map.clone(), self.published.load(Ordering::SeqCst))
    }
    /// Waits for a map newer than the last one this handle returned. `None`
    /// once the watchers are gone.
    pub async fn changed(&mut self) -> Option<Observed> {
        self.observed.changed().await.ok()?;
        Some(self.observed.borrow_and_update().clone())
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
    prusalink_interval: Duration,
    port: Port,
    heard: Heard,
    /// False in the Rust-only runtime: no Node serves HTTPS or redirecting
    /// hosts, so they report offline instead of routing to Node.
    node_fallback: bool,
}
impl Watchers {
    pub fn new() -> Result<Self> {
        let mut watchers = Self::with_guard(
            HttpGuard::new(
                Arc::new(SystemResolver),
                Duration::from_secs(5),
                2 * 1024 * 1024,
            )?,
            Duration::from_secs(2),
        );
        watchers.prusalink_interval = PRUSALINK_INTERVAL;
        Ok(watchers)
    }
    pub fn with_guard(guard: HttpGuard, interval: Duration) -> Self {
        assert!(!interval.is_zero());
        Self {
            hosts: HashMap::new(),
            known: None,
            stale_since: None,
            guard,
            interval,
            prusalink_interval: interval,
            port: Port {
                observed: watch::channel(Observed::default()).0,
                entered: Entered::default(),
                transitions: broadcast::channel(TRANSITION_CAPACITY).0,
                published: Arc::default(),
            },
            heard: Heard::default(),
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
        PrinterObservations {
            observed: self.port.observed.subscribe(),
            heard: self.heard.clone(),
            entered: self.port.entered.clone(),
            transitions: self.port.transitions.clone(),
            published: self.port.published.clone(),
        }
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
    /// How often a host of `kind` is polled while it answers.
    fn interval_for(&self, kind: &HostKind) -> Duration {
        match kind {
            HostKind::Moonraker => self.interval,
            HostKind::PrusaLink { .. } => self.prusalink_interval,
        }
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
            publish(&status, &self.port, &id, initial);
            let task = if host.enabled && !needs_node(&host.base_url) {
                let config = host.clone();
                let stopped = cancel.clone();
                let current = status.clone();
                let port = self.port.clone();
                let heard = self.heard.clone();
                let guard = self.guard.clone();
                let interval = self.interval_for(&config.kind);
                Some(tokio::spawn(async move {
                    let mut failures = 0u32;
                    loop {
                        let response = tokio::select! { biased; _=stopped.cancelled()=>break, response=poll(&guard,&config)=>response };
                        let delay = match response {
                            Ok(value) => {
                                failures = 0;
                                lock(&heard).insert(config.id.clone(), SystemTime::now());
                                publish(&current, &port, &config.id, Slot::Status(value));
                                interval
                            }
                            Err(Failure::Redirect) => {
                                publish(&current, &port, &config.id, redirected);
                                break;
                            }
                            Err(failure) => {
                                failures = failures.saturating_add(1);
                                let offline = failure.status();
                                publish(&current, &port, &config.id, Slot::Status(offline));
                                backoff(interval, failures)
                            }
                        };
                        let delay = jitter(delay, rand::random());
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
        lock(&self.heard).remove(id);
        lock(&self.port.entered).remove(id);
        self.port.observed.send_if_modified(|observed| {
            observed.contains_key(id) && Arc::make_mut(observed).remove(id).is_some()
        });
    }
}
/// Store a slot value and mirror it into the observation map. Node-routed
/// hosts have no Rust observation. The slot is written first and stays locked
/// until the map is published, so a reader woken by the port already sees the
/// same value through `route`.
fn publish(slot: &Mutex<Slot>, port: &Port, id: &str, value: Slot) {
    let mut current = lock(slot);
    *current = value;
    port.observed.send_if_modified(|map| match &*current {
        Slot::Status(status) if map.get(id) != Some(status) => {
            enter(&port.entered, id, status.state.clone(), SystemTime::now());
            Arc::make_mut(map).insert(id.to_owned(), status.clone());
            let seq = port.published.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = port.transitions.send(Transition {
                seq,
                id: id.to_owned(),
                status: status.clone(),
            });
            true
        }
        Slot::Status(_) => false,
        Slot::Node => map.contains_key(id) && Arc::make_mut(map).remove(id).is_some(),
    });
}
/// Records `state` for `id` at `now` unless the host is already in it. An
/// `offline` or `unknown` report only marks when the unanswered spell began,
/// or fills in a host never answered; see [`PrinterObservations::state_since`].
fn enter(entered: &Entered, id: &str, state: State, now: SystemTime) {
    let mut entered = lock(entered);
    let passing = matches!(state, State::Offline | State::Unknown);
    match entered.get_mut(id) {
        Some(spell) if passing => {
            spell.unanswered.get_or_insert(now);
        }
        Some(spell)
            if spell.state == state
                && spell.unanswered.is_none_or(|began| {
                    now.duration_since(began).unwrap_or_default() < OUTAGE_GRACE
                }) =>
        {
            spell.unanswered = None;
        }
        _ => {
            entered.insert(
                id.to_owned(),
                Spell {
                    state,
                    since: now,
                    unanswered: None,
                },
            );
        }
    }
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
fn lock<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
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
/// A fixed set of Moonraker messages: printer/transport error text is never
/// echoed. A PrusaLink offline status is the driver's, as Node's adapter
/// reports it.
enum Failure {
    Redirect,
    Http(u16),
    Unavailable,
    Offline(Box<Status>),
}
impl Failure {
    fn status(self) -> Status {
        match self {
            Failure::Http(status) => Status::offline(&format!("Moonraker returned HTTP {status}")),
            Failure::Offline(status) => *status,
            _ => Status::offline("Moonraker status unavailable"),
        }
    }
}
/// Up to a fifth shorter, so hosts started together (after a wake or a
/// network change) spread out instead of polling in lockstep. Never longer:
/// the interval and the 60 s backoff cap stay upper bounds.
fn jitter(delay: Duration, unit: f64) -> Duration {
    delay.mul_f64(1.0 - JITTER * unit.clamp(0.0, 1.0))
}
const JITTER: f64 = 0.2;
fn backoff(interval: Duration, failures: u32) -> Duration {
    interval
        .saturating_mul(1u32 << failures.min(5))
        .min(Duration::from_secs(60))
}
async fn poll(guard: &HttpGuard, host: &Host) -> Result<Status, Failure> {
    if let HostKind::PrusaLink { username, password } = &host.kind {
        let driver = crate::prusalink::Driver::new(
            guard.clone(),
            &host.base_url,
            username,
            password.as_deref(),
            None,
        );
        // An offline status is an unanswered poll: it backs off and does not
        // count as heard. Anything else, an idle 204 included, answered.
        let status = driver.status().await;
        return if status.state == State::Offline {
            Err(Failure::Offline(Box::new(status)))
        } else {
            Ok(status)
        };
    }
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
            kind: Default::default(),
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
    #[test]
    fn farm_reads_state_since_survives_a_missed_poll_but_not_a_new_state() {
        let entered = Entered::default();
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let since = || lock(&entered).get("host").map(|spell| spell.since);
        enter(&entered, "host", State::Offline, at(1));
        assert_eq!(
            since(),
            Some(at(1)),
            "a host never answered is offline since then"
        );
        enter(&entered, "host", State::Complete, at(2));
        enter(&entered, "host", State::Offline, at(3));
        enter(&entered, "host", State::Unknown, at(4));
        enter(&entered, "host", State::Complete, at(5));
        assert_eq!(
            since(),
            Some(at(2)),
            "a missed poll is not a new completion"
        );
        enter(&entered, "host", State::Printing, at(6));
        enter(&entered, "host", State::Complete, at(7));
        assert_eq!(since(), Some(at(7)), "a new job completes again");
        enter(&entered, "host", State::Offline, at(8));
        enter(&entered, "host", State::Complete, at(8 + 300));
        assert_eq!(
            since(),
            Some(at(308)),
            "an outage long enough to hide a reprint starts complete afresh"
        );
    }
    #[tokio::test]
    async fn printer_stream_transitions_keep_every_change_in_order() {
        const ERROR: &[u8] =
            include_bytes!("../../../../fixtures/drivers/moonraker/moonraker-error.json");
        let replies = vec![
            (200, SUCCESS),
            (200, ERROR),
            (503, &b"{}"[..]),
            (200, SUCCESS),
        ];
        let printer = RecordedPrinter::start(replies).unwrap();
        let mut watchers = watchers_every(Duration::from_millis(5));
        let mut transitions = watchers.observations().transitions();
        watchers
            .replace(vec![host(printer.url().into())])
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while printer.calls() < 6 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        watchers.stop().await;
        let mut states = Vec::new();
        let mut last = 0;
        while let Ok(transition) = transitions.try_recv() {
            assert_eq!(transition.id, "host");
            assert_eq!(transition.seq, last + 1, "seq counts every change");
            last = transition.seq;
            states.push(transition.status.state);
        }
        assert_eq!(watchers.observations().snapshot().1, last);
        assert_eq!(
            states,
            [
                State::Unknown,
                State::Printing,
                State::Error,
                State::Offline,
                State::Printing
            ]
        );
    }
    #[tokio::test]
    async fn farm_reads_last_heard_survives_offline_and_leaves_with_the_poller() {
        let printer = RecordedPrinter::start(vec![(200, SUCCESS), (503, b"{}")]).unwrap();
        let mut watchers = watchers_every(Duration::from_millis(50));
        let mut observations = watchers.observations();
        watchers
            .replace(vec![host(printer.url().into())])
            .await
            .unwrap();
        next_state(&mut observations, "host", State::Printing).await;
        let heard = observations
            .last_heard("host")
            .expect("heard after a reply");
        let printing_since = observations.state_since("host").expect("entered printing");
        next_state(&mut observations, "host", State::Offline).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(observations.last_heard("host"), Some(heard));
        assert_eq!(
            observations.state_since("host"),
            Some(printing_since),
            "offline does not move the answered state"
        );
        watchers.replace(vec![]).await.unwrap();
        assert_eq!(observations.last_heard("host"), None);
        assert_eq!(observations.state_since("host"), None);
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

    /// Every request sent was a status query. A poll cancelled after its
    /// connection opened but before it wrote anything (replace, remove or
    /// stop) leaves an empty line in the log: nothing was sent on it.
    fn sent_only_status_queries(printer: &RecordedPrinter) {
        let lines = printer.request_lines();
        let sent: Vec<_> = lines.iter().filter(|line| !line.is_empty()).collect();
        assert!(!sent.is_empty());
        assert!(
            sent.iter()
                .all(|line| **line == format!("GET {QUERY} HTTP/1.1")),
            "{lines:?}"
        );
    }
    fn live_workers(watchers: &Watchers) -> usize {
        watchers
            .hosts
            .values()
            .filter(|p| p.task.as_ref().is_some_and(|task| !task.is_finished()))
            .count()
    }
    async fn frozen(printer: &RecordedPrinter) -> bool {
        let before = printer.calls();
        tokio::time::sleep(Duration::from_millis(150)).await;
        printer.calls() == before
    }
    async fn polled(printer: &RecordedPrinter) {
        let before = printer.calls();
        tokio::time::timeout(Duration::from_secs(5), async {
            while printer.calls() <= before {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("printer was not polled");
    }
    fn named(id: &str, url: &str, enabled: bool) -> Host {
        Host {
            id: id.into(),
            enabled,
            ..host(url.into())
        }
    }
    #[tokio::test]
    async fn ticket_t_25_one_tracked_worker_per_enabled_host() {
        let a = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
        let b = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
        let c = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
        let moved = RecordedPrinter::start(vec![(200, SUCCESS)]).unwrap();
        let mut watchers = watchers_every(Duration::from_millis(20));
        let hosts = vec![
            named("a", a.url(), true),
            named("b", b.url(), true),
            named("c", c.url(), false),
        ];
        for _ in 0..10 {
            watchers.replace(hosts.clone()).await.unwrap();
        }
        assert_eq!(live_workers(&watchers), 2, "one worker per enabled host");
        polled(&a).await;
        polled(&b).await;
        assert_eq!(c.calls(), 0, "a disabled host has no worker");

        // Replacing b's endpoint joins the old worker before the new one starts.
        watchers
            .replace(vec![
                named("a", a.url(), true),
                named("b", moved.url(), true),
                named("c", c.url(), false),
            ])
            .await
            .unwrap();
        assert_eq!(live_workers(&watchers), 2);
        assert!(frozen(&b).await, "the replaced worker kept polling");
        polled(&moved).await;

        // Removing a cancels and joins its worker.
        watchers
            .replace(vec![named("b", moved.url(), true)])
            .await
            .unwrap();
        assert_eq!(live_workers(&watchers), 1);
        assert!(frozen(&a).await, "the removed worker kept polling");
        assert_eq!(watchers.hosts.len(), 1);

        // Shutdown joins the last worker.
        watchers.stop().await;
        assert!(watchers.hosts.is_empty());
        assert!(frozen(&moved).await);
        for printer in [&a, &b, &moved] {
            sent_only_status_queries(printer);
        }
    }
    #[test]
    fn ticket_t_25_jitter_spreads_polls_without_exceeding_the_bounds() {
        let interval = Duration::from_secs(2);
        assert_eq!(jitter(interval, 0.0), interval);
        assert_eq!(jitter(interval, 1.0), interval.mul_f64(0.8));
        assert_eq!(jitter(interval, 7.0), interval.mul_f64(0.8));
        let cap = backoff(interval, 100);
        let samples: HashSet<_> = (0..200).map(|_| jitter(cap, rand::random())).collect();
        assert!(samples.len() > 1, "jitter is not constant");
        assert!(
            samples
                .iter()
                .all(|delay| *delay <= cap && *delay >= cap.mul_f64(0.8)),
            "jitter never lengthens a delay past the 60 s cap"
        );
    }
    #[tokio::test]
    async fn ticket_t_25_offline_idle_and_stopped_are_distinct() {
        const STANDBY: &[u8] = br#"{"result":{"status":{"print_stats":{"state":"standby"}}}}"#;
        let idle = RecordedPrinter::start(vec![(200, STANDBY)]).unwrap();
        let unreachable = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let refused = format!("http://{}", unreachable.local_addr().unwrap());
        drop(unreachable);
        let mut watchers = watchers_every(Duration::from_millis(20));
        let mut observations = watchers.observations();
        watchers
            .replace(vec![
                named("idle", idle.url(), true),
                named("down", &refused, true),
            ])
            .await
            .unwrap();
        next_state(&mut observations, "idle", State::Idle).await;
        let down = next_state(&mut observations, "down", State::Offline).await;
        assert_eq!(
            down.message.as_deref(),
            Some("Moonraker status unavailable")
        );

        // A stopped (asleep) host is neither idle nor offline: it has no status
        // and the route reports it unavailable, never a stale cached value.
        watchers.stop().await;
        assert_eq!(watchers.route("idle"), Route::Unavailable);
        assert_eq!(watchers.route("down"), Route::Unavailable);
        assert!(observations.latest("idle").is_none());
        assert!(watchers.status("idle").is_none());
        sent_only_status_queries(&idle);
    }
    #[tokio::test]
    async fn ticket_t_25_reconnect_after_refused_connections_recovers() {
        let unreachable = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = unreachable.local_addr().unwrap();
        drop(unreachable);
        let mut watchers = watchers_every(Duration::from_millis(20));
        let mut observations = watchers.observations();
        watchers
            .replace(vec![host(format!("http://{address}"))])
            .await
            .unwrap();
        next_state(&mut observations, "host", State::Offline).await;
        assert_eq!(
            live_workers(&watchers),
            1,
            "a failing host keeps one worker"
        );
        // Let the failures pile up to the 640 ms backoff (40+80+160+320+640 ms).
        tokio::time::sleep(Duration::from_millis(1300)).await;

        // The printer comes back on the same address: the same worker reconnects.
        let listener = TcpListener::bind(address).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = [0; 4096];
                let len = socket.read(&mut bytes).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&bytes[..len]);
                assert!(request.starts_with(&format!("GET {QUERY} HTTP/1.1")));
                seen.fetch_add(1, Ordering::SeqCst);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    SUCCESS.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(SUCCESS).await;
            }
        });
        next_state(&mut observations, "host", State::Printing).await;
        assert_eq!(live_workers(&watchers), 1);
        // After a success the worker polls at the plain 20 ms interval again,
        // not at the 640 ms backoff the failures reached: three more polls at
        // that backoff would take over 1.5 s.
        let recovered = calls.load(Ordering::SeqCst);
        tokio::time::timeout(Duration::from_millis(400), async {
            while calls.load(Ordering::SeqCst) < recovered + 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("polling stayed at the backed-off delay after recovery");
        watchers.stop().await;
        server.abort();
    }
    #[tokio::test]
    async fn ticket_t_25_lagging_subscribers_stay_bounded_and_never_block_publishing() {
        let watchers = watchers_every(Duration::from_secs(60));
        let mut observations = watchers.observations();
        let mut lagging = observations.transitions();
        let slot = Mutex::new(Slot::Node);
        let changes = 3 * TRANSITION_CAPACITY as u64;
        // Nobody reads while every change publishes: publish never waits.
        tokio::time::timeout(Duration::from_secs(5), async {
            for i in 0..changes {
                publish(
                    &slot,
                    &watchers.port,
                    "host",
                    Slot::Status(Status::offline(&i.to_string())),
                );
            }
        })
        .await
        .expect("publishing waited on a subscriber");
        match lagging.try_recv() {
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                assert_eq!(skipped, changes - TRANSITION_CAPACITY as u64)
            }
            other => panic!("expected Lagged, got {other:?}"),
        }
        let first = lagging.try_recv().unwrap();
        assert_eq!(first.seq, changes - TRANSITION_CAPACITY as u64 + 1);
        let mut retained = 1;
        while lagging.try_recv().is_ok() {
            retained += 1;
        }
        assert_eq!(
            retained, TRANSITION_CAPACITY,
            "the channel keeps only its capacity"
        );
        // The newest-only map coalesces every change into one wake-up.
        let newest = observations.changed().await.unwrap();
        assert_eq!(newest.len(), 1);
        assert_eq!(
            newest["host"].message.as_deref(),
            Some((changes - 1).to_string().as_str())
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), observations.changed())
                .await
                .is_err(),
            "a slow reader wakes once, not once per change"
        );
        assert_eq!(lock(&watchers.port.entered).len(), 1);
    }
}
