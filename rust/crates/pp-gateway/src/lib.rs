use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderValue, Request, Response, StatusCode},
    response::IntoResponse,
    serve::Listener,
};
use http_body_util::BodyExt;
use pp_compat::CompatHandle;
pub use pp_drivers::moonraker::{
    HTTPS_UNAVAILABLE, Host as MoonrakerHost, HostKind, MAX_HOSTS, PrinterHostState,
    PrinterHostStatus, PrinterObservations, REDIRECT_UNAVAILABLE, Transition,
};
use pp_drivers::moonraker::{Host, Route, Watchers};
use pp_net::target;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

mod assets;
pub mod ownership;
pub mod printer_host;
mod rate_limit;
mod rust_only;
mod stall;

use ownership::{Owner, cutover::OperationOwners};

/// The web UI build in the gateway's asset root. Only a Rust-only runtime
/// that serves the replacement UI (`web/apps/ui`) sends the replacement
/// Content-Security-Policy (`replacement-csp.txt`); every other runtime keeps
/// the legacy policy. See docs/network-sharing.md#content-security-policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServedUi {
    /// The old `web/apps/web` build.
    Legacy,
    /// The replacement `web/apps/ui` build.
    Replacement,
}

/// The status of a runtime with no Node child, where the Rust writer owns the
/// data directory. `pp-core` owns the sender; the gateway reads it for
/// `/__runtime`. It has no child `generation` or `pid` because there is no child.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RustStatus {
    Starting,
    /// The writer holds the data directory and the gateway is serving.
    Ready,
    Stopping,
    Stopped,
}

pub const MUTATION_DRAIN: Duration = Duration::from_secs(5);

pub struct LaunchTarget(String);
impl LaunchTarget {
    pub fn into_url(self) -> String {
        self.0
    }
}

#[derive(Clone)]
pub struct ShutdownRequested {
    token: CancellationToken,
}
impl ShutdownRequested {
    pub async fn wait(self) {
        self.token.cancelled_owned().await;
    }
}

/// Bootstrap tokens live 60 seconds. At most this many wait at once, so a
/// burst of tray clicks cannot grow the set; the oldest token goes first.
const BOOTSTRAP_TTL: Duration = Duration::from_secs(60);
const PENDING_BOOTSTRAPS: usize = 4;

struct Sessions {
    draining: bool,
    active: usize,
    /// SHA-256 of each unredeemed bootstrap token with its deadline.
    bootstraps: Vec<([u8; 32], tokio::time::Instant)>,
    sessions: HashSet<[u8; 32]>,
}

impl Sessions {
    fn begin_drain(&mut self) {
        self.draining = true;
        self.bootstraps.clear();
        self.sessions.clear();
    }

    /// Store a fresh 256-bit token and return it. None while draining.
    fn mint_bootstrap(&mut self) -> Option<String> {
        if self.draining {
            return None;
        }
        let now = tokio::time::Instant::now();
        self.bootstraps.retain(|(_, deadline)| now < *deadline);
        if self.bootstraps.len() >= PENDING_BOOTSTRAPS {
            self.bootstraps.remove(0);
        }
        let token = hex::encode(rand::random::<[u8; 32]>());
        self.bootstraps.push((digest(&token), now + BOOTSTRAP_TTL));
        Some(token)
    }

    /// Remove the token if it is pending and unexpired. Callers hold the
    /// session mutex, so two requests with one token cannot both redeem it.
    fn redeem_bootstrap(&mut self, token: &str) -> bool {
        let now = tokio::time::Instant::now();
        self.bootstraps.retain(|(_, deadline)| now < *deadline);
        let hash = digest(token);
        match self
            .bootstraps
            .iter()
            .position(|(pending, _)| *pending == hash)
        {
            Some(index) => {
                self.bootstraps.remove(index);
                true
            }
            None => false,
        }
    }
}

/// Mints browser bootstrap URLs for the gateway's own origin and port. The
/// desktop tray uses it for Open in browser; a WebGL fallback can reuse it.
#[derive(Clone)]
pub struct BootstrapMinter {
    origin: String,
    access: Arc<Mutex<Sessions>>,
}
impl BootstrapMinter {
    pub fn mint(&self) -> Result<LaunchTarget> {
        let token = self
            .access
            .lock()
            .map_err(|_| anyhow::anyhow!("Session mutex poisoned"))?
            .mint_bootstrap()
            .context("Runtime is stopping")?;
        Ok(LaunchTarget(format!(
            "{}/__desktop/bootstrap?token={token}",
            self.origin
        )))
    }
}

#[derive(Deserialize)]
struct Manifest {
    routes: Vec<Operation>,
    denied: Vec<DeniedOperation>,
}
#[derive(Deserialize)]
struct DeniedOperation {
    method: String,
    path: String,
    reason: String,
}
#[derive(Deserialize)]
struct Operation {
    method: String,
    path: String,
    #[serde(rename = "owner")]
    registry_owner: String,
    #[serde(skip, default = "node_owner")]
    owner: Owner,
    effect: EffectClass,
}
fn node_owner() -> Owner {
    Owner::Node
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum EffectClass {
    Observation,
    LocalCommit,
    ExternalEffect,
}

/// Why a Rust-only host read produced no snapshot.
#[derive(Debug, PartialEq)]
pub enum HostReadError {
    /// Storage was busy or stopping: valid pollers survive the grace period.
    Unavailable,
    /// The stored integrations setting is malformed: fail closed now.
    Invalid,
}

/// The Moonraker hosts a runtime with no Node child polls. `pp-core` reads
/// them from storage, so the gateway never depends on `pp-storage`. The
/// rows follow Node's `/__desktop/moonraker-hosts` projection.
pub trait MoonrakerHosts: Send + Sync + 'static {
    fn read(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<MoonrakerHost>, HostReadError>> + Send>,
    >;
}

/// What the watchers know about one integration id.
#[derive(Debug, PartialEq)]
pub enum StatusLookup {
    Status(PrinterHostStatus),
    /// Not a Moonraker id in the last valid snapshot.
    NotWatched,
    /// A Moonraker id with no current status: no valid snapshot, or its
    /// poller is stopped (sleep, shutdown, malformed settings).
    Unavailable,
}

/// The status lookup a Rust-only runtime's authenticated product route uses.
/// It holds the gateway weakly, so the product router inside the gateway
/// does not keep the gateway alive.
#[derive(Clone)]
pub struct MoonrakerStatus(std::sync::Weak<GatewayState>);
impl MoonrakerStatus {
    /// The watchers' read-only observation port, while the gateway lives.
    pub fn observations(&self) -> Option<PrinterObservations> {
        Some(self.0.upgrade()?.observations.clone())
    }
    /// Runs `observer` on the watchers' task set, so gateway shutdown cancels
    /// the token it gets and joins it. `false` once the gateway is gone.
    pub fn spawn_observer<F>(
        &self,
        observer: impl FnOnce(PrinterObservations, CancellationToken) -> F,
    ) -> bool
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let Some(state) = self.0.upgrade() else {
            return false;
        };
        let observing = observer(state.observations.clone(), state.watcher_cancelled.clone());
        state.watcher_tasks.spawn(observing);
        true
    }
    /// The cached status, after one shared snapshot refresh when the id is
    /// not known yet. Never polls the printer on demand.
    pub async fn lookup(&self, id: &str) -> StatusLookup {
        let Some(state) = self.0.upgrade() else {
            return StatusLookup::Unavailable;
        };
        let mut route = state.watchers.lock().await.route(id);
        if route_needs_snapshot(&route) && !state.watcher_cancelled.is_cancelled() {
            let refreshing = state.clone();
            let _ = state
                .watcher_tasks
                .spawn(async move { refresh_watchers(&refreshing).await })
                .await;
            route = state.watchers.lock().await.route(id);
        }
        match route {
            Route::Cached(status) => StatusLookup::Status(status),
            Route::Node => StatusLookup::NotWatched,
            Route::NoSnapshot | Route::Unavailable => StatusLookup::Unavailable,
        }
    }
}

type ProductRoutes = Box<dyn FnOnce(MoonrakerStatus) -> axum::Router>;

struct GatewayState {
    watchers: tokio::sync::Mutex<Watchers>,
    /// Set only for a Rust-only runtime: the snapshot source in place of Node.
    hosts: Option<Arc<dyn MoonrakerHosts>>,
    observations: PrinterObservations,
    snapshot_refresh: tokio::sync::Mutex<()>,
    snapshot_revision: AtomicU64,
    /// One in-flight Node snapshot. Concurrent status misses join it instead of
    /// each waiting out the queue deadline and reading the snapshot again.
    snapshot_flight: SnapshotFlight,
    watcher_cancelled: CancellationToken,
    watcher_tasks: TaskTracker,
    monitoring_suspended: tokio::sync::watch::Sender<bool>,
    origin: String,
    host: String,
    network: bool,
    cookie_name: String,
    assets: PathBuf,
    /// The replacement policy only on a Rust-only runtime serving `ServedUi::Replacement`.
    asset_policy: assets::Policy,
    compat: CompatHandle,
    rate_limits: Option<tokio::sync::Mutex<rate_limit::Budgets>>,
    access: Arc<Mutex<Sessions>>,
    routes: Vec<Operation>,
    denied: Vec<DeniedOperation>,
    relays: TaskTracker,
    cancelled: CancellationToken,
    shutdown_requested: CancellationToken,
    /// Routes `pp-core` composed for a Rust-only runtime. When present, every
    /// admitted request that is not a gateway or asset path goes here, and
    /// none reaches the compatibility proxy. Logout goes here first too.
    product: Mutex<Option<axum::Router>>,
    /// Set only for a Rust-only runtime: `/__runtime` reports it and marks the
    /// compatibility backend `not_used`.
    rust_status: Option<tokio::sync::watch::Receiver<RustStatus>>,
    /// Proxy attempts: requests that missed every gateway, asset and product
    /// route and entered the compatibility branch.
    compat_forwards: AtomicU64,
}

/// Native power adapters call suspend before sleep and resume after wake.
/// Window visibility must never call this control.
#[derive(Clone)]
pub struct MonitoringControl(Arc<GatewayState>);
impl MonitoringControl {
    pub async fn suspend(&self) {
        self.0.monitoring_suspended.send_replace(true);
        self.0.watchers.lock().await.stop().await;
    }
    pub fn resume(&self) {
        self.0.monitoring_suspended.send_replace(false);
    }
}

pub struct Gateway {
    state: Arc<GatewayState>,
    stop: CancellationToken,
    task: JoinHandle<std::io::Result<()>>,
    connections: TaskTracker,
    launch: Option<LaunchTarget>,
}

impl Gateway {
    pub fn start(
        listener: TcpListener,
        assets: PathBuf,
        compat: CompatHandle,
        owners: &OperationOwners,
    ) -> Result<Self> {
        Self::start_with_product(listener, assets, compat, owners, None)
    }

    /// Start with routes `pp-core` composed from `pp-api`, kept as a sibling
    /// router so the gateway never depends on `pp-api`. With `product` set,
    /// admitted requests other than gateway and asset paths are answered by it
    /// and never forwarded to the compatibility backend.
    pub fn start_with_product(
        listener: TcpListener,
        assets: PathBuf,
        compat: CompatHandle,
        owners: &OperationOwners,
        product: Option<axum::Router>,
    ) -> Result<Self> {
        let product = product.map(|routes| Box::new(move |_| routes) as ProductRoutes);
        Self::start_inner(
            listener,
            assets,
            (ServedUi::Legacy, None),
            compat,
            owners,
            product,
            None,
        )
    }

    fn start_inner(
        listener: TcpListener,
        assets: PathBuf,
        (ui, replacement_assets): (ServedUi, Option<pp_compat::AssetSnapshot>),
        compat: CompatHandle,
        owners: &OperationOwners,
        product: Option<ProductRoutes>,
        rust_only: Option<(
            tokio::sync::watch::Receiver<RustStatus>,
            Arc<dyn MoonrakerHosts>,
            bool,
        )>,
    ) -> Result<Self> {
        let (rust_status, hosts, network) = match rust_only {
            Some((status, hosts, network)) => (Some(status), Some(hosts), network),
            None => (None, None, false),
        };
        let mut address = listener.local_addr()?;
        anyhow::ensure!(
            network || address.ip().is_loopback(),
            "Desktop gateway requires a loopback listener"
        );
        if network {
            eprintln!("Listening on {address}");
            address.set_ip(if address.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        let host = address.to_string();
        let origin = format!("http://{host}");
        let mut manifest: Manifest = serde_json::from_str(include_str!("../operations.json"))?;
        let mut seen = HashSet::new();
        for route in &mut manifest.routes {
            anyhow::ensure!(
                route.registry_owner == "compat"
                    && route.path.starts_with('/')
                    && seen.insert(format!("http.{} {}", route.method, route.path)),
                "Invalid operation ownership manifest"
            );
            route.owner = owners.owner_of(&format!("http.{} {}", route.method, route.path));
        }
        ownership::cutover::admit_bundled_node_fences()
            .map_err(|error| anyhow::anyhow!("Node fence admission failed: {error}"))?;
        for participant in owners.rust_participants() {
            anyhow::ensure!(
                seen.contains(participant)
                    || (!participant.starts_with("http.")
                        && ownership::cutover::node_fences(participant)),
                "Rust-owned participant {participant} is not a registered route"
            );
        }
        // The desktop lease data directory already holds the persistent receipt.
        // This repeats the bundled manifest check before routes are served.
        ownership::admit_bundled_source_sync()
            .map_err(|error| anyhow::anyhow!("Source Sync ownership admission failed: {error}"))?;
        sort_routes(&mut manifest.routes);
        let mut sessions = Sessions {
            draining: false,
            active: 0,
            bootstraps: Vec::new(),
            sessions: HashSet::new(),
        };
        let token = sessions
            .mint_bootstrap()
            .context("Desktop bootstrap unavailable")?;
        let launch = Some(LaunchTarget(format!(
            "{origin}/__desktop/bootstrap?token={token}"
        )));
        let watchers = match hosts {
            Some(_) => Watchers::new()?.without_node_fallback(),
            None => Watchers::new()?,
        };
        let observations = watchers.observations();
        let asset_policy = if rust_status.is_some() && ui == ServedUi::Replacement {
            assets::Policy::replacement(
                &origin,
                replacement_assets.context("Replacement UI requires verified bytes")?,
            )
        } else {
            assets::Policy::Legacy
        };
        let state = Arc::new(GatewayState {
            watchers: tokio::sync::Mutex::new(watchers),
            hosts,
            observations,
            snapshot_refresh: tokio::sync::Mutex::new(()),
            snapshot_revision: AtomicU64::new(0),
            snapshot_flight: SnapshotFlight::new(),
            watcher_cancelled: CancellationToken::new(),
            watcher_tasks: TaskTracker::new(),
            monitoring_suspended: tokio::sync::watch::channel(false).0,
            origin,
            host,
            network,
            cookie_name: format!("pp_desktop_{}", address.port()),
            assets,
            asset_policy,
            compat,
            rate_limits: (owners.owner_of("http.GET /legal/summary") == Owner::Rust)
                .then(|| tokio::sync::Mutex::new(rate_limit::Budgets::default())),
            access: Arc::new(Mutex::new(sessions)),
            routes: manifest.routes,
            denied: manifest.denied,
            relays: TaskTracker::new(),
            cancelled: CancellationToken::new(),
            shutdown_requested: CancellationToken::new(),
            product: Mutex::new(None),
            rust_status,
            compat_forwards: AtomicU64::new(0),
        });
        if let Some(product) = product {
            let routes = product(MoonrakerStatus(Arc::downgrade(&state)));
            *state
                .product
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(routes);
        }
        let monitoring = state.clone();
        state.watcher_tasks.spawn(async move {
            let mut suspension = monitoring.monitoring_suspended.subscribe();
            loop {
                if *suspension.borrow_and_update() {
                    monitoring.watchers.lock().await.stop().await;
                    tokio::select! { biased;
                        _=monitoring.watcher_cancelled.cancelled()=>break,
                        _=suspension.changed()=>continue,
                    }
                }
                tokio::select! { biased;
                    _ = monitoring.watcher_cancelled.cancelled() => break,
                    _ = suspension.changed() => continue,
                    _ = refresh_watchers(&monitoring) => {},
                }
                tokio::select! { biased;
                    _ = monitoring.watcher_cancelled.cancelled() => break,
                    _ = suspension.changed() => continue,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
            }
            monitoring.watchers.lock().await.stop().await;
        });
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let connections = TaskTracker::new();
        let accepted = connections.clone();
        let gateway = state.clone();
        let task = tokio::spawn(async move {
            let mut listener = listener;
            loop {
                let (stream, peer) = tokio::select! {
                    biased;
                    _ = stopping.cancelled() => break,
                    next = Listener::accept(&mut listener) => next,
                };
                let state = gateway.clone();
                let cancelled = stopping.clone();
                accepted.spawn(async move {
                    let service = hyper::service::service_fn(move |request| {
                        let state = state.clone();
                        let mut request = request.map(Body::new);
                        request.extensions_mut().insert(ConnectInfo(peer));
                        async move {
                            Ok::<_, std::convert::Infallible>(handle(State(state), request).await)
                        }
                    });
                    let mut http = hyper::server::conn::http1::Builder::new();
                    http.max_buf_size(32 * 1024);
                    let connection = http
                        .serve_connection(
                            hyper_util::rt::TokioIo::new(stall::StallGuard::new(stream)),
                            service,
                        )
                        .with_upgrades();
                    tokio::select! {
                        biased;
                        _ = cancelled.cancelled() => {},
                        _ = connection => {},
                    }
                });
            }
            Ok(())
        });
        Ok(Self {
            state,
            stop,
            task,
            connections,
            launch,
        })
    }
    pub fn origin(&self) -> &str {
        &self.state.origin
    }
    pub fn take_launch_target(&mut self) -> Result<LaunchTarget> {
        self.launch
            .take()
            .context("Launch target already transferred")
    }
    pub fn monitoring_control(&self) -> MonitoringControl {
        MonitoringControl(self.state.clone())
    }
    /// Read-only port over the Moonraker statuses the watchers observe, for
    /// in-process consumers. It cannot send, retry or change Checkoff.
    pub fn printer_observations(&self) -> PrinterObservations {
        self.state.observations.clone()
    }
    pub fn bootstrap_minter(&self) -> BootstrapMinter {
        BootstrapMinter {
            origin: self.state.origin.clone(),
            access: self.state.access.clone(),
        }
    }
    pub fn shutdown_requested(&self) -> ShutdownRequested {
        ShutdownRequested {
            token: self.state.shutdown_requested.clone(),
        }
    }
    /// Proxy attempts since start, counted at the route-miss path: requests
    /// that no gateway, asset or Rust product route answered. Rust-only mode
    /// keeps this at 0.
    pub fn compat_forwards(&self) -> u64 {
        self.state.compat_forwards.load(Ordering::Relaxed)
    }
    pub async fn drain(&self) {
        self.drain_for(MUTATION_DRAIN).await;
    }

    pub fn begin_drain(&self) {
        self.state.watcher_cancelled.cancel();
        self.state
            .access
            .lock()
            .expect("Session mutex poisoned")
            .begin_drain();
    }

    pub async fn wait_idle(&self, bound: Duration) -> bool {
        let started = tokio::time::Instant::now();
        loop {
            if self.inflight() == 0 {
                return true;
            }
            let left = bound.saturating_sub(started.elapsed());
            if left.is_zero() {
                return self.inflight() == 0;
            }
            tokio::time::sleep(left.min(Duration::from_millis(10))).await;
        }
    }

    pub fn finish_drain(&self) {
        self.state.watcher_cancelled.cancel();
        self.state.cancelled.cancel();
        self.state.relays.close();
    }

    pub async fn drain_for(&self, bound: Duration) {
        self.begin_drain();
        let _ = self.wait_idle(bound).await;
        self.finish_drain();
    }

    fn inflight(&self) -> usize {
        self.state
            .access
            .lock()
            .expect("Session mutex poisoned")
            .active
    }
    pub async fn stop(self) -> Result<()> {
        self.state.watcher_cancelled.cancel();
        self.state.watcher_tasks.close();
        self.state.watcher_tasks.wait().await;
        self.stop.cancel();
        let mut task = self.task;
        let listener = match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
            Ok(result) => result
                .context("Gateway join failed")?
                .context("Gateway stop failed"),
            Err(_) => {
                task.abort();
                match task.await {
                    Err(error) if error.is_cancelled() => Ok(()),
                    Ok(Ok(())) => Ok(()),
                    _ => anyhow::bail!("Gateway abort failed"),
                }
            }
        };
        self.state.watcher_cancelled.cancel();
        self.state.cancelled.cancel();
        self.state.relays.close();
        listener?;
        self.connections.close();
        tokio::time::timeout(Duration::from_secs(1), self.connections.wait())
            .await
            .context("HTTP connection join timed out")?;
        tokio::time::timeout(Duration::from_secs(1), self.state.relays.wait())
            .await
            .context("Upgrade relay join timed out")?;
        // Release the product routes' storage handles once no connection can use them.
        self.state
            .product
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        anyhow::ensure!(
            self.state
                .access
                .lock()
                .expect("Session mutex poisoned")
                .active
                == 0,
            "Requests remain after gateway shutdown"
        );
        Ok(())
    }
}

struct Admission {
    access: Arc<Mutex<Sessions>>,
    released: Arc<AtomicBool>,
    _ended: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Admission {
    /// Stops counting this request once `drain` is cancelled, even if its
    /// body is never polled again. Only for read-only streams, which must not
    /// hold the shutdown drain open.
    fn release_at(&mut self, drain: CancellationToken) {
        let (ended, done) = tokio::sync::oneshot::channel::<()>();
        self._ended = Some(ended);
        let access = self.access.clone();
        let released = self.released.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = drain.cancelled() => release(&access, &released),
                _ = done => {}
            }
        });
    }

    fn acquire(access: &Arc<Mutex<Sessions>>, cookie: Option<&str>) -> Result<Self, StatusCode> {
        Self::enter(access, |state| {
            cookie.is_some_and(|value| state.sessions.contains(&digest(value)))
        })
    }

    /// Counts a request that proves its own identity downstream, so the
    /// shutdown drain still waits for it. Only MCP uses this: its guard in
    /// `pp-api` requires an API key and refuses every cookie.
    fn keyed(access: &Arc<Mutex<Sessions>>) -> Result<Self, StatusCode> {
        Self::enter(access, |_| true)
    }

    fn enter(
        access: &Arc<Mutex<Sessions>>,
        admitted: impl FnOnce(&Sessions) -> bool,
    ) -> Result<Self, StatusCode> {
        let mut state = access.lock().expect("Session mutex poisoned");
        if state.draining {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        if !admitted(&state) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        state.active += 1;
        Ok(Self {
            access: access.clone(),
            released: Arc::new(AtomicBool::new(false)),
            _ended: None,
        })
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        release(&self.access, &self.released);
    }
}
fn release(access: &Mutex<Sessions>, released: &AtomicBool) {
    if !released.swap(true, Ordering::AcqRel) {
        access.lock().expect("Session mutex poisoned").active -= 1;
    }
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
fn error(status: StatusCode, detail: &'static str) -> Response<Body> {
    (
        status,
        [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
        ],
        axum::Json(serde_json::json!({"detail":detail})),
    )
        .into_response()
}

/// A browser that follows a stale link gets a page that says what to do next.
/// Other clients keep the JSON error.
fn bootstrap_rejected(request: &Request<Body>) -> Response<Body> {
    const DETAIL: &str = "Bootstrap expired or consumed";
    let navigation = header(request, "sec-fetch-mode") == Some("navigate")
        || header(request, "accept").is_some_and(|value| value.contains("text/html"));
    if !navigation {
        return error(StatusCode::UNAUTHORIZED, DETAIL);
    }
    (
        StatusCode::UNAUTHORIZED,
        [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("content-type", "text/html; charset=utf-8"),
            ("content-security-policy", "default-src 'none'"),
        ],
        BOOTSTRAP_REJECTED_PAGE,
    )
        .into_response()
}

const BOOTSTRAP_REJECTED_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Print Partner sign-in link expired</title></head><body><h1>This sign-in link has expired</h1><p>Each sign-in link works once, within 60 seconds.</p><p>Choose Open in browser from the Print Partner tray menu to get a new link. On a server without a desktop, run <code>pp-server bootstrap-url --data &lt;data directory&gt;</code> as the account that runs it.</p></body></html>";

fn node_unavailable() -> Response<Body> {
    let mut response = Response::new(Body::from(
        r#"{"type":"about:blank","title":"Service Unavailable","status":503,"detail":"Compatibility backend unavailable"}"#,
    ));
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        HeaderValue::from_static("application/problem+json"),
    );
    headers.insert("retry-after", HeaderValue::from_static("1"));
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    response
}
fn header<'a>(request: &'a Request<Body>, name: &str) -> Option<&'a str> {
    request.headers().get(name)?.to_str().ok()
}
fn desktop_cookie<'a>(request: &'a Request<Body>, expected_name: &str) -> Option<&'a str> {
    let mut selected = None;
    for cookies in request.headers().get_all("cookie").iter() {
        for pair in cookies.to_str().ok()?.split(';') {
            let Some((name, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if name == expected_name {
                if selected.is_some() {
                    return None;
                }
                selected = Some(value);
            }
        }
    }
    selected
}
fn sort_routes(routes: &mut [Operation]) {
    fn rank(path: &str) -> impl Iterator<Item = u8> + '_ {
        path.split('/')
            .map(|part| {
                if part == "*" {
                    0
                } else if part.starts_with(':') {
                    1
                } else {
                    2
                }
            })
            .chain(std::iter::once(3))
    }
    routes.sort_by(|a, b| rank(&b.path).cmp(rank(&a.path)));
}

fn matches_path(pattern: &str, path: &str) -> bool {
    let mut actual = path.split('/');
    for segment in pattern.split('/') {
        if segment == "*" {
            return actual.next().is_some();
        }
        let Some(value) = actual.next() else {
            return false;
        };
        if segment.starts_with(':') {
            if value.is_empty() {
                return false;
            }
        } else if segment != value {
            return false;
        }
    }
    actual.next().is_none()
}

fn is_spa_client_path(path: &str) -> bool {
    if matches!(
        path,
        "/" | "/login"
            | "/setup"
            | "/forgot-password"
            | "/reset-password"
            | "/sources"
            | "/build"
            | "/builds"
            | "/review"
            | "/checkoff"
            | "/settings"
            | "/help"
            | "/plan"
            | "/plans"
            | "/plate"
            | "/print"
            | "/printers"
            | "/library"
            | "/parts"
            | "/progress"
            | "/export"
            | "/production"
            | "/board"
    ) {
        return true;
    }
    let path_bytes = path.as_bytes();
    if path_bytes
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"/board/"))
    {
        let bytes = &path_bytes[7..];
        return bytes.len() == 36
            && bytes[8] == b'-'
            && bytes[13] == b'-'
            && matches!(bytes[14], b'1'..=b'8')
            && bytes[18] == b'-'
            && matches!(bytes[19].to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b')
            && bytes[23] == b'-'
            && bytes.iter().enumerate().all(|(index, byte)| {
                matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit()
            });
    }
    path.strip_prefix("/plans/")
        .and_then(|value| value.strip_suffix("/studio"))
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

fn legal_summary_cors(response: &mut Response<Body>, origin: Option<&HeaderValue>) {
    let headers = response.headers_mut();
    headers.append("vary", HeaderValue::from_static("Origin"));
    headers.insert(
        "access-control-allow-credentials",
        HeaderValue::from_static("true"),
    );
    if let Some(origin) = origin {
        headers.insert("access-control-allow-origin", origin.clone());
    }
}

async fn handle(
    State(state): State<Arc<GatewayState>>,
    mut request: Request<Body>,
) -> Response<Body> {
    let (expected_origin, expected_host) = match rust_only::authority(&state, &request) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::FORBIDDEN, "Invalid network authority"),
    };
    let mut hosts = request.headers().get_all("host").iter();
    if hosts.next().map(HeaderValue::as_bytes) != Some(expected_host.as_bytes())
        || hosts.next().is_some()
    {
        return error(StatusCode::BAD_REQUEST, "Invalid Host");
    }
    let target = match target::CanonicalTarget::parse(request.uri()) {
        Ok(target) => target,
        Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid request target"),
    };
    *request.uri_mut() = target.uri;
    let path = target.path;
    if path == "/__desktop/rate-limit" && request.method() == "POST" {
        let bucket = state.compat.endpoint().and_then(|endpoint| {
            endpoint.rate_limit_bucket(
                header(&request, "x-pp-rate-debit")?,
                header(&request, "x-pp-rate-signature")?,
            )
        });
        return match (&state.rate_limits, bucket) {
            (Some(budgets), Some(media)) => {
                axum::Json(budgets.lock().await.consume(media)).into_response()
            }
            _ => error(StatusCode::FORBIDDEN, "Private quota debit required"),
        };
    }
    // With the Rust product routes mounted, MCP answers from their key-only
    // guard. MCP clients are not browsers and send no Origin; the guard
    // refuses an unsafe request without one unless it carries an API key.
    // An explicit network listener never routes MCP: LAN peers stay out until
    // MCP trust classes exist, whatever key they present.
    let mcp = if (path == "/mcp" || path == "/api/v1/mcp") && !state.network {
        product_router(&state)
    } else {
        None
    };
    let mut origins = request.headers().get_all("origin").iter();
    let origin = origins.next();
    let upgrade = request.headers().contains_key("upgrade");
    let unsafe_method = !matches!(request.method().as_str(), "GET" | "HEAD" | "OPTIONS");
    if origins.next().is_some()
        || origin.is_some_and(|value| value.as_bytes() != expected_origin.as_bytes())
        || (((unsafe_method && mcp.is_none()) || upgrade) && origin.is_none())
    {
        return error(StatusCode::FORBIDDEN, "Invalid Origin");
    }
    if let Some(product) = mcp {
        // The desktop cookie is not MCP authorization: the request is only
        // counted for the drain, and the guard checks loopback, Host, Origin,
        // the MCP rate limit and the API key. A POST answers as an event
        // stream while its tool runs, so only the GET notification stream
        // stops counting when the drain begins.
        let Ok(admission) = Admission::keyed(&state.access) else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "Runtime is stopping");
        };
        let streams = if request.method() == "GET" {
            Streams::Release
        } else {
            Streams::Count
        };
        return product_response(&state, product, request, admission, streams).await;
    }
    if path == "/__desktop/bootstrap" && request.method() == "GET" {
        let token = request
            .uri()
            .query()
            .and_then(|query| query.strip_prefix("token="));
        let mut access = state.access.lock().expect("Session mutex poisoned");
        if access.draining {
            return error(StatusCode::SERVICE_UNAVAILABLE, "Runtime is stopping");
        }
        if !token.is_some_and(|token| access.redeem_bootstrap(token)) {
            drop(access);
            return bootstrap_rejected(&request);
        }
        let session = hex::encode(rand::random::<[u8; 32]>());
        access.sessions.insert(digest(&session));
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header("location", "/")
            .header("cache-control", "no-store")
            .header("referrer-policy", "no-referrer")
            .header(
                "set-cookie",
                format!(
                    "{}={session}; HttpOnly; SameSite=Strict; Path=/",
                    state.cookie_name
                ),
            )
            .body(Body::empty())
            .expect("Static response");
    }
    let cookie = desktop_cookie(&request, &state.cookie_name);
    let admission = match Admission::acquire(&state.access, cookie) {
        Ok(admission) => admission,
        Err(StatusCode::SERVICE_UNAVAILABLE) => {
            return error(StatusCode::SERVICE_UNAVAILABLE, "Runtime is stopping");
        }
        Err(_) => return error(StatusCode::UNAUTHORIZED, "Desktop session required"),
    };
    if path == "/__runtime" && request.method() == "GET" {
        let gateway = serde_json::json!({"active_requests":state.access.lock().expect("Session mutex poisoned").active.saturating_sub(1),"active_upgrades":state.relays.len()});
        let body = match &state.rust_status {
            Some(rust) => {
                serde_json::json!({"proof_class":"headless_unsigned","rust":*rust.borrow(),"compat":{"state":"not_used"},"gateway":gateway})
            }
            None => {
                serde_json::json!({"proof_class":"headless_unsigned","compat":*state.compat.status.borrow(),"gateway":gateway})
            }
        };
        return axum::Json(body).into_response();
    }
    if path == "/auth/logout" && request.method() == "POST" {
        // Rust revokes its own session before the gateway ends the desktop one.
        let revoked = match product_router(&state) {
            Some(product) => Some(
                buffered(
                    product_response(&state, product, request, admission, Streams::Release).await,
                )
                .await,
            ),
            None => None,
        };
        state
            .access
            .lock()
            .expect("Session mutex poisoned")
            .sessions
            .clear();
        state.watcher_cancelled.cancel();
        state.cancelled.cancel();
        state.shutdown_requested.cancel();
        return revoked
            .unwrap_or_else(|| axum::Json(serde_json::json!({"ok":true})).into_response());
    }
    // Without the Rust product routes, or on an explicit network listener,
    // nothing here serves MCP.
    if path == "/mcp" || path == "/api/v1/mcp" {
        return error(
            StatusCode::FORBIDDEN,
            "MCP is disabled in this foundation; API-key authorization is required before enabling it",
        );
    }
    let document = matches!(request.method().as_str(), "GET" | "HEAD")
        && (header(&request, "sec-fetch-mode") == Some("navigate")
            || header(&request, "accept").is_some_and(|v| v.contains("text/html")));
    let spa = state.asset_policy.is_spa_client_path(&path);
    if (document && spa)
        || path.starts_with("/assets/")
        || path.starts_with("/icons/")
        || matches!(
            path.as_str(),
            "/favicon.ico" | "/logo.png" | "/manifest.json"
        )
    {
        let relative = if document && spa {
            "index.html"
        } else {
            path.trim_start_matches('/')
        };
        let response = assets::serve(
            &state.assets,
            relative,
            document && spa,
            &state.asset_policy,
            request.method(),
            request.headers(),
        )
        .await;
        return response.map(|body| {
            Body::new(TrackedBody {
                body,
                _admission: admission,
                cancelled: Box::pin(state.cancelled.clone().cancelled_owned()),
            })
        });
    }

    if let Some(product) = product_router(&state) {
        return product_response(&state, product, request, admission, Streams::Release).await;
    }
    // Route miss: no gateway, asset or Rust product route answered, so the
    // request enters the compatibility branch. Counted here, before any early
    // return, so no path into that branch goes uncounted.
    state.compat_forwards.fetch_add(1, Ordering::Relaxed);
    if let Some(denied) = state
        .denied
        .iter()
        .find(|route| route.method == request.method().as_str() && matches_path(&route.path, &path))
    {
        return (
            StatusCode::NOT_IMPLEMENTED,
            axum::Json(
                serde_json::json!({"detail":denied.reason,"code":"desktop_feature_unavailable"}),
            ),
        )
            .into_response();
    }
    let operation = state.routes.iter().find(|route| {
        route.method == request.method().as_str() && matches_path(&route.path, &path)
    });
    let Some(operation) = operation else {
        return error(StatusCode::NOT_FOUND, "Operation not registered");
    };
    let mut fetch_sites = request.headers().get_all("sec-fetch-site").iter();
    let same_origin_browser_get = request.method() == "GET"
        && origin.is_none()
        && fetch_sites
            .next()
            .is_some_and(|value| value.as_bytes() == b"same-origin")
        && fetch_sites.next().is_none();
    if operation.effect != EffectClass::Observation && origin.is_none() && !same_origin_browser_get
    {
        return error(
            StatusCode::FORBIDDEN,
            "Origin required for operation with effects",
        );
    }
    if operation.owner == Owner::Rust {
        if matches!(path.as_str(), "/legal/summary" | "/api/v1/legal/summary")
            && matches!(request.method().as_str(), "GET" | "HEAD")
        {
            let quota = if let Some(budgets) = &state.rate_limits {
                Some(budgets.lock().await.consume(false))
            } else {
                None
            };
            if let Some(mut response) = quota
                .as_ref()
                .and_then(|quota| quota.rejection(request.method() == "HEAD"))
            {
                legal_summary_cors(&mut response, origin);
                return response.map(|body| {
                    Body::new(TrackedBody {
                        body,
                        _admission: admission,
                        cancelled: Box::pin(state.cancelled.clone().cancelled_owned()),
                    })
                });
            }
            let document =
                include_str!("../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md");
            let body = if request.method() == "HEAD" {
                Body::empty()
            } else {
                Body::from(document)
            };
            let mut response = Response::new(body);
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            response
                .headers_mut()
                .insert("content-length", HeaderValue::from(document.len()));
            response
                .headers_mut()
                .insert("cache-control", HeaderValue::from_static("no-store"));
            if let Some(quota) = quota {
                quota.apply(&mut response);
            }
            legal_summary_cors(&mut response, origin);
            return response.map(|body| {
                Body::new(TrackedBody {
                    body,
                    _admission: admission,
                    cancelled: Box::pin(state.cancelled.clone().cancelled_owned()),
                })
            });
        }
        let mut response = (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "detail": "Operation is owned by the Rust runtime and has no handler yet",
                "code": "rust_owner_unavailable",
            })),
        )
            .into_response();
        if request.method() == "HEAD" {
            *response.body_mut() = Body::empty();
        }
        return response;
    }
    if matches!(request.method().as_str(), "GET" | "HEAD")
        && let Some(id) = moonraker_status_id(&path)
    {
        // The last valid snapshot proves an id is not Moonraker (or is an HTTPS/redirect
        // host left on Node until GRE-359), so those forward to Node without waiting.
        let mut route = state.watchers.lock().await.route(&id);
        if route_needs_snapshot(&route) {
            // No published child can produce a snapshot. This is the unready
            // compat route, not a monitoring outage on a live backend.
            if state.compat.endpoint().is_none() {
                return node_unavailable();
            }
            // The regression test waits here, while this read still sees a child
            // and the snapshot flight is blocked.
            #[cfg(test)]
            ticket_t_2::note_published_refresh();
            // Detached so a client disconnect cannot cut a reconcile short.
            // Concurrent misses share that one refresh; a recorded snapshot then
            // routes confirmed non-Moonraker ids without another queue wait.
            let refreshing = state.clone();
            let _ = state
                .watcher_tasks
                .spawn(async move { refresh_watchers(&refreshing).await })
                .await;
            route = state.watchers.lock().await.route(&id);
            if route_needs_snapshot(&route) {
                // The child can exit during the refresh. That is still an unready
                // compat route, not a monitoring outage on a live backend.
                if state.compat.endpoint().is_none() {
                    return node_unavailable();
                }
                // Never fall back to on-demand Node polling for a known HTTP Moonraker
                // id, nor guess when no valid snapshot exists.
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Printer monitoring unavailable",
                );
            }
        }
        if let Route::Cached(status) = route {
            return status_response(status, request.method());
        }
    }
    let integrations_changed = matches!(request.method().as_str(), "POST" | "PATCH" | "DELETE")
        && (path == "/api/v1/integrations"
            || path
                .strip_prefix("/api/v1/integrations/")
                .is_some_and(|tail| !tail.contains('/')));
    let Some(endpoint) = state.compat.endpoint() else {
        return node_unavailable();
    };
    let untrusted: Vec<_> = request
        .headers()
        .keys()
        .filter(|name| {
            let name = name.as_str();
            name.starts_with("x-pp-")
                || name.starts_with("x-forwarded-")
                || matches!(
                    name,
                    "forwarded" | "authorization" | "cookie" | "x-print-partner-api-key"
                )
        })
        .cloned()
        .collect();
    for name in untrusted {
        request.headers_mut().remove(name);
    }
    let inbound_upgrade = if upgrade {
        Some(hyper::upgrade::on(&mut request))
    } else {
        None
    };
    let result = tokio::select! {
        biased;
        _ = state.cancelled.cancelled() => return error(StatusCode::SERVICE_UNAVAILABLE, "Runtime stopped; operation outcome may be unknown"),
        result = endpoint.forward(request) => result,
    };
    match result {
        Ok(mut response) => {
            if integrations_changed && response.status().is_success() {
                // Older reads may not restore settings after a committed mutation.
                state.snapshot_revision.fetch_add(1, Ordering::SeqCst);
                let cause = if path == "/api/v1/integrations" {
                    SnapshotCause::Create
                } else {
                    SnapshotCause::Update
                };
                // Read committed configuration; failure invalidates old monitoring.
                // Finish reconciliation even if the requesting client disconnects.
                let refreshing = state.clone();
                let _ = state
                    .watcher_tasks
                    .spawn(async move {
                        refreshing
                            .snapshot_flight
                            .fresh(&refreshing.watcher_cancelled, || {
                                apply_snapshot(&refreshing, cause)
                            })
                            .await
                    })
                    .await;
            }
            if response.status() == StatusCode::SWITCHING_PROTOCOLS
                && let Some(inbound) = inbound_upgrade
            {
                let outbound = hyper::upgrade::on(&mut response);
                if state.cancelled.is_cancelled() {
                    return error(StatusCode::SERVICE_UNAVAILABLE, "Runtime is stopping");
                }
                let cancelled = state.cancelled.clone();
                state.relays.spawn(async move {
                    let _admission = admission;
                    tokio::select! {
                        _ = cancelled.cancelled() => {},
                        _ = async {
                            if let (Ok(a), Ok(b)) = tokio::join!(inbound, outbound) {
                                let _ = tokio::io::copy_bidirectional(
                                    &mut hyper_util::rt::TokioIo::new(a),
                                    &mut hyper_util::rt::TokioIo::new(b),
                                ).await;
                            }
                        } => {},
                    }
                });
                return response;
            }

            response
                .headers_mut()
                .insert("cache-control", HeaderValue::from_static("no-store"));
            response.map(|body| {
                Body::new(TrackedBody {
                    body,
                    _admission: admission,
                    cancelled: Box::pin(state.cancelled.clone().cancelled_owned()),
                })
            })
        }
        Err(_) => error(
            StatusCode::BAD_GATEWAY,
            "Compatibility response unavailable; operation outcome may be unknown",
        ),
    }
}

fn moonraker_status_id(path: &str) -> Option<String> {
    let tail = path.strip_prefix("/api/v1/integrations/")?;
    let id = tail.strip_suffix("/status")?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    pp_net::target::decode_segment(id).ok()
}
fn status_response(status: PrinterHostStatus, method: &axum::http::Method) -> Response<Body> {
    let mut response = axum::Json(status).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    if method == "HEAD" {
        // Keep GET's representation length even though HEAD has no body.
        if let Some(length) = http_body::Body::size_hint(response.body()).exact() {
            response
                .headers_mut()
                .insert("content-length", HeaderValue::from(length));
        }
        *response.body_mut() = Body::empty();
    }
    response
}
fn route_needs_snapshot(route: &Route) -> bool {
    matches!(route, Route::NoSnapshot | Route::Unavailable)
}
/// Joiners wait out one in-flight read. The cap is the old queue deadline plus
/// that read's own budget, so a shared 1.2s snapshot does not fail the third caller.
const SNAPSHOT_FLIGHT_WAIT: Duration = Duration::from_secs(4);
struct SnapshotFlight {
    slot: Mutex<Option<tokio::sync::watch::Receiver<Option<bool>>>>,
}
struct FlightCleanup<'a> {
    slot: &'a Mutex<Option<tokio::sync::watch::Receiver<Option<bool>>>>,
    tx: tokio::sync::watch::Sender<Option<bool>>,
    published: bool,
}
impl Drop for FlightCleanup<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = self.tx.send(Some(false));
        }
        *flight_slot(self.slot) = None;
    }
}
fn flight_slot(
    mutex: &Mutex<Option<tokio::sync::watch::Receiver<Option<bool>>>>,
) -> std::sync::MutexGuard<'_, Option<tokio::sync::watch::Receiver<Option<bool>>>> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl SnapshotFlight {
    fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }
    /// Mutations must read again after any pre-commit flight, never reuse its result.
    async fn fresh<F, Fut>(&self, cancelled: &CancellationToken, lead: F) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let pending = { flight_slot(&self.slot).clone() };
        if let Some(flight) = pending {
            let _ = wait_shared_snapshot(flight, cancelled).await;
        }
        if cancelled.is_cancelled() {
            return false;
        }
        lead().await
    }

    /// The first caller runs `lead`. Everyone already waiting shares that result.
    async fn share<F, Fut>(&self, cancelled: &CancellationToken, lead: F) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let (tx, rx) = tokio::sync::watch::channel(None);
        // Drop the guard before any await. A std mutex guard is not Send.
        let joined = {
            let mut slot = flight_slot(&self.slot);
            if let Some(flight) = slot.clone() {
                Some(flight)
            } else {
                *slot = Some(rx);
                None
            }
        };
        if let Some(flight) = joined {
            return wait_shared_snapshot(flight, cancelled).await;
        }
        let mut cleanup = FlightCleanup {
            slot: &self.slot,
            tx,
            published: false,
        };
        let result = lead().await;
        let _ = cleanup.tx.send(Some(result));
        cleanup.published = true;
        result
    }
}
async fn wait_shared_snapshot(
    mut flight: tokio::sync::watch::Receiver<Option<bool>>,
    cancelled: &CancellationToken,
) -> bool {
    let deadline = tokio::time::Instant::now() + SNAPSHOT_FLIGHT_WAIT;
    loop {
        if let Some(done) = *flight.borrow() {
            return done;
        }
        tokio::select! {
            biased;
            _ = cancelled.cancelled() => return false,
            _ = tokio::time::sleep_until(deadline) => return false,
            changed = flight.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
        }
    }
}
async fn snapshot_refresh_guard<'a>(
    mutex: &'a tokio::sync::Mutex<()>,
    cancelled: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Option<(tokio::sync::MutexGuard<'a, ()>, tokio::time::Instant)> {
    tokio::select! { biased;
        _ = cancelled.cancelled() => None,
        guard = tokio::time::timeout_at(deadline, mutex.lock()) => guard.ok().map(|guard|
            (guard, tokio::time::Instant::now() + Duration::from_secs(2))),
    }
}
/// Slow/unavailable Node keeps last-known-good pollers this long before stopping them.
const SNAPSHOT_GRACE: Duration = Duration::from_secs(10);
#[derive(Clone, Copy)]
enum SnapshotCause {
    Periodic,
    Create,
    Update,
}

#[derive(Clone, Copy)]
enum SnapshotError {
    /// Timeout, restart or 5xx: routine refreshes may keep valid pollers briefly.
    Unavailable,
    /// Denied (4xx) or unparseable: fail closed immediately.
    Invalid,
    /// A mutation committed while this read was in flight; discard its result.
    Superseded,
}
async fn refresh_watchers(state: &GatewayState) -> bool {
    let cancelled = state.watcher_cancelled.clone();
    state
        .snapshot_flight
        .share(&cancelled, || {
            apply_snapshot(state, SnapshotCause::Periodic)
        })
        .await
}
async fn apply_snapshot(state: &GatewayState, cause: SnapshotCause) -> bool {
    let revision = state.snapshot_revision.load(Ordering::SeqCst);
    // Serialize snapshots without blocking already-cached reads on the Node round trip.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    // Queue contention must not shorten the independent Node-read budget.
    let Some((_refresh, read_deadline)) =
        snapshot_refresh_guard(&state.snapshot_refresh, &state.watcher_cancelled, deadline).await
    else {
        if !matches!(cause, SnapshotCause::Periodic) {
            state.watchers.lock().await.invalidate().await;
        }
        return false;
    };
    let snapshot = tokio::select! { biased;
        _ = state.watcher_cancelled.cancelled() => return false,
        snapshot = tokio::time::timeout_at(read_deadline, async {
            if let Some(hosts) = &state.hosts {
                return hosts.read().await.map_err(|error| match error {
                    HostReadError::Unavailable => SnapshotError::Unavailable,
                    HostReadError::Invalid => SnapshotError::Invalid,
                });
            }
            let endpoint = state.compat.endpoint().ok_or(SnapshotError::Unavailable)?;
            let request = Request::builder()
                .uri("/__desktop/moonraker-hosts")
                .body(Body::empty())
                .map_err(|_| SnapshotError::Invalid)?;
            let response = endpoint
                .forward(request)
                .await
                .map_err(|_| SnapshotError::Unavailable)?;
            if response.status().is_client_error() {
                return Err(SnapshotError::Invalid);
            }
            if !response.status().is_success() {
                return Err(SnapshotError::Unavailable);
            }
            let body = http_body_util::Limited::new(
                response.into_body(),
                pp_drivers::moonraker::MAX_SNAPSHOT_BYTES,
            )
            .collect()
            .await
            .map_err(|_| SnapshotError::Unavailable)?
            .to_bytes();
            serde_json::from_slice::<Vec<Host>>(&body).map_err(|_| SnapshotError::Invalid)
        }) => snapshot.unwrap_or(Err(SnapshotError::Unavailable)),
    };
    let mut watchers = state.watchers.lock().await;
    if *state.monitoring_suspended.borrow() || state.watcher_cancelled.is_cancelled() {
        if matches!(cause, SnapshotCause::Periodic) {
            watchers.stop().await;
        } else {
            watchers.invalidate().await;
        }
        return false;
    }
    if state.snapshot_revision.load(Ordering::SeqCst) != revision {
        return apply_host_snapshot(&mut watchers, Err(SnapshotError::Superseded), cause).await;
    }
    apply_host_snapshot(&mut watchers, snapshot, cause).await
}

async fn apply_host_snapshot(
    watchers: &mut Watchers,
    snapshot: Result<Vec<Host>, SnapshotError>,
    cause: SnapshotCause,
) -> bool {
    match snapshot {
        Ok(hosts) if hosts.len() > pp_drivers::moonraker::MAX_HOSTS => {
            let grace = if matches!(cause, SnapshotCause::Update) {
                Duration::ZERO
            } else {
                SNAPSHOT_GRACE
            };
            watchers.over_cap(&hosts, grace).await;
            false
        }
        Ok(hosts) => watchers.replace(hosts).await.is_ok(),
        Err(SnapshotError::Unavailable) if matches!(cause, SnapshotCause::Periodic) => {
            watchers.snapshot_unavailable(SNAPSHOT_GRACE).await;
            false
        }
        Err(SnapshotError::Superseded) if matches!(cause, SnapshotCause::Periodic) => false,
        Err(_) => {
            watchers.invalidate().await;
            false
        }
    }
}

#[cfg(test)]
mod spa_tests {
    use super::is_spa_client_path;

    #[tokio::test]
    async fn ticket_t_4_queued_refresh_keeps_full_read_budget() {
        use std::time::Duration;
        use tokio::time::{Instant, timeout_at};
        let mutex = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let cancel = tokio_util::sync::CancellationToken::new();
        let (first, first_deadline) =
            super::snapshot_refresh_guard(&mutex, &cancel, Instant::now() + Duration::from_secs(2))
                .await
                .unwrap();
        let queued_mutex = mutex.clone();
        let queued = tokio::spawn(async move {
            let (_second, read_deadline) = super::snapshot_refresh_guard(
                &queued_mutex,
                &cancel,
                Instant::now() + Duration::from_secs(2),
            )
            .await
            .unwrap();
            assert!(read_deadline.duration_since(Instant::now()) > Duration::from_millis(1900));
            // This healthy 1.2s read would time out with the queue's remaining 0.8s.
            timeout_at(
                read_deadline,
                tokio::time::sleep(Duration::from_millis(1200)),
            )
            .await
            .unwrap();
        });
        timeout_at(
            first_deadline,
            tokio::time::sleep(Duration::from_millis(1200)),
        )
        .await
        .unwrap();
        drop(first);
        queued.await.unwrap();
    }

    #[tokio::test]
    async fn ticket_t_4_snapshot_queue_deadline_and_shutdown_cancellation() {
        let mutex = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let held = mutex.lock().await;
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut queued = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let mutex = mutex.clone();
            let cancel = cancel.clone();
            queued.spawn(async move {
                super::snapshot_refresh_guard(
                    &mutex,
                    &cancel,
                    tokio::time::Instant::now() + std::time::Duration::from_millis(50),
                )
                .await
                .is_none()
            });
        }
        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            while let Some(result) = queued.join_next().await {
                assert!(result.unwrap());
            }
        })
        .await
        .unwrap();
        let pending = super::snapshot_refresh_guard(
            &mutex,
            &cancel,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        );
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        cancel.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), pending)
                .await
                .unwrap()
                .is_none()
        );
        drop(held);
    }

    #[test]
    fn spa_client_paths_match_the_node_router() {
        for path in [
            "/",
            "/login",
            "/setup",
            "/forgot-password",
            "/reset-password",
            "/sources",
            "/build",
            "/builds",
            "/review",
            "/checkoff",
            "/settings",
            "/help",
            "/plan",
            "/plans",
            "/plate",
            "/print",
            "/printers",
            "/library",
            "/parts",
            "/progress",
            "/export",
            "/production",
            "/board",
            "/board/123e4567-e89b-42d3-a456-426614174000",
            "/board/123E4567-E89B-42D3-B456-426614174000",
            "/BOARD/123e4567-e89b-42d3-a456-426614174000",
            "/BoArD/123E4567-E89B-42D3-B456-426614174000",
            "/plans/1/studio",
            "/plans/000/studio",
        ] {
            assert!(is_spa_client_path(path), "expected SPA path: {path}");
        }

        for path in [
            "/unknown",
            "/api/unknown",
            "/board/not-a-uuid",
            "/board/123e4567-e89b-02d3-a456-426614174000",
            "/board/123e4567-e89b-42d3-7456-426614174000",
            "/board/123e4567-e89b-42d3-a456-426614174000/extra",
            "/plans/-1/studio",
            "/plans/1.5/studio",
            "/plans/abc/studio",
            "/plans/1/studio/extra",
        ] {
            assert!(!is_spa_client_path(path), "unexpected SPA path: {path}");
        }
    }
}

#[cfg(test)]
mod snapshot_membership_tests {
    use super::{
        SnapshotCause, SnapshotError, SnapshotFlight, apply_host_snapshot, route_needs_snapshot,
    };
    use pp_drivers::moonraker::{Host, Route, Watchers};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn failed_mutation_reconciliation_stops_precommit_polling() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0; 4096];
                if socket.read(&mut request).await.unwrap() == 0 {
                    continue;
                }
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
        let guard = pp_net::outbound::HttpGuard::new(
            Arc::new(pp_net::outbound::SystemResolver),
            Duration::from_millis(500),
            4096,
        )
        .unwrap();
        let mut watchers = Watchers::with_guard(guard, Duration::from_millis(20));
        for (cause, failure) in [
            (SnapshotCause::Create, SnapshotError::Unavailable),
            (SnapshotCause::Update, SnapshotError::Unavailable),
            (SnapshotCause::Update, SnapshotError::Superseded),
        ] {
            let before = calls.load(Ordering::SeqCst);
            watchers
                .replace(vec![Host {
                    id: "printer".into(),
                    base_url: endpoint.clone(),
                    api_key: None,
                    enabled: true,
                    kind: Default::default(),
                }])
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while calls.load(Ordering::SeqCst) == before {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // An ordinary timeout/5xx keeps a valid poller through grace.
            assert!(
                !apply_host_snapshot(
                    &mut watchers,
                    Err(SnapshotError::Unavailable),
                    SnapshotCause::Periodic
                )
                .await
            );
            assert!(matches!(watchers.route("printer"), Route::Cached(_)));
            // Once Node commits a mutation, that same failure cannot retain old settings.
            assert!(!apply_host_snapshot(&mut watchers, Err(failure), cause).await);
            assert_eq!(watchers.route("printer"), Route::NoSnapshot);
            let stopped = calls.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                stopped,
                "old endpoint polled after failed reconciliation"
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn over_cap_updates_stop_old_pollers_but_creation_keeps_grace() {
        let mut watchers = Watchers::new().unwrap();
        let host = Host {
            id: "printer".into(),
            base_url: "http://printer.test".into(),
            api_key: None,
            enabled: false,
            kind: Default::default(),
        };
        watchers.replace(vec![host.clone()]).await.unwrap();
        let overflow: Vec<_> = (0..=pp_drivers::moonraker::MAX_HOSTS)
            .map(|index| Host {
                id: if index == 0 {
                    "printer".into()
                } else {
                    format!("new-{index}")
                },
                ..host.clone()
            })
            .collect();
        assert!(
            !apply_host_snapshot(&mut watchers, Ok(overflow.clone()), SnapshotCause::Create).await
        );
        assert!(matches!(watchers.route("printer"), Route::Cached(_)));
        assert_eq!(watchers.route("new-128"), Route::Unavailable);
        assert!(!apply_host_snapshot(&mut watchers, Ok(overflow), SnapshotCause::Update).await);
        assert_eq!(watchers.route("printer"), Route::Unavailable);
        assert_eq!(watchers.route("new-128"), Route::Unavailable);
        assert_eq!(watchers.route("prusa"), Route::Node);
    }

    #[tokio::test]
    async fn mutation_reads_committed_configuration_after_a_precommit_flight() {
        let flight = Arc::new(SnapshotFlight::new());
        let configured = Arc::new(AtomicUsize::new(1));
        let applied = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let (captured_tx, captured_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let before_commit = {
            let (flight, configured, applied, reads, cancel) = (
                flight.clone(),
                configured.clone(),
                applied.clone(),
                reads.clone(),
                cancel.clone(),
            );
            tokio::spawn(async move {
                flight
                    .share(&cancel, || async {
                        let snapshot = configured.load(Ordering::SeqCst);
                        reads.fetch_add(1, Ordering::SeqCst);
                        captured_tx.send(snapshot).unwrap();
                        release_rx.await.unwrap();
                        applied.store(snapshot, Ordering::SeqCst);
                        true
                    })
                    .await
            })
        };
        assert_eq!(captured_rx.await.unwrap(), 1);
        configured.store(2, Ordering::SeqCst);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let after_commit = {
            let (flight, configured, applied, reads, cancel) = (
                flight.clone(),
                configured.clone(),
                applied.clone(),
                reads.clone(),
                cancel.clone(),
            );
            tokio::spawn(async move {
                started_tx.send(()).unwrap();
                flight
                    .fresh(&cancel, || async {
                        reads.fetch_add(1, Ordering::SeqCst);
                        applied.store(configured.load(Ordering::SeqCst), Ordering::SeqCst);
                        true
                    })
                    .await
            })
        };
        started_rx.await.unwrap();
        release_tx.send(()).unwrap();
        assert!(before_commit.await.unwrap());
        assert!(after_commit.await.unwrap());
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert_eq!(applied.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn encoded_status_ids_use_snapshot_membership() {
        let mut watchers = pp_drivers::moonraker::Watchers::new().unwrap();
        for (id, segment) in [
            ("café printer", "caf%c3%a9%20printer"),
            ("printer ?#", "printer%20%3f%23"),
        ] {
            watchers
                .replace(vec![pp_drivers::moonraker::Host {
                    id: id.into(),
                    base_url: "http://printer.test".into(),
                    api_key: None,
                    enabled: false,
                    kind: Default::default(),
                }])
                .await
                .unwrap();
            let uri = format!("/api/v1/integrations/{segment}/status")
                .parse()
                .unwrap();
            let canonical = pp_net::target::CanonicalTarget::parse(&uri).unwrap();
            let decoded = super::moonraker_status_id(&canonical.path).unwrap();
            assert!(
                matches!(watchers.route(&decoded), Route::Cached(_)),
                "{segment}"
            );
        }
    }

    #[tokio::test]
    async fn three_non_moonraker_misses_share_one_slow_snapshot() {
        let flight = Arc::new(SnapshotFlight::new());
        let watchers = Arc::new(Mutex::new(Watchers::new().unwrap()));
        let runs = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let mut tasks = tokio::task::JoinSet::new();
        for id in ["prusa", "bambu", "ha"] {
            let (flight, watchers, runs, cancel) = (
                flight.clone(),
                watchers.clone(),
                runs.clone(),
                cancel.clone(),
            );
            tasks.spawn(async move {
                let started = Instant::now();
                let mut route = watchers.lock().await.route(id);
                if route_needs_snapshot(&route) {
                    let (watchers, runs) = (watchers.clone(), runs.clone());
                    flight
                        .share(&cancel, || {
                            let (watchers, runs) = (watchers.clone(), runs.clone());
                            async move {
                                runs.fetch_add(1, Ordering::SeqCst);
                                tokio::time::sleep(Duration::from_millis(1200)).await;
                                let mut guard = watchers.lock().await;
                                guard
                                    .replace(vec![Host {
                                        id: "printer".into(),
                                        base_url: "http://printer.test".into(),
                                        api_key: None,
                                        enabled: false,
                                        kind: Default::default(),
                                    }])
                                    .await
                                    .unwrap();
                                true
                            }
                        })
                        .await;
                    route = watchers.lock().await.route(id);
                }
                (id, route, started.elapsed())
            });
        }
        let mut elapsed = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (id, route, took) = joined.unwrap();
            assert_eq!(route, Route::Node, "{id}");
            assert!(!route_needs_snapshot(&route), "{id}");
            elapsed.push(took);
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "snapshot reads");
        let slowest = elapsed.into_iter().max().unwrap();
        assert!(
            slowest < Duration::from_millis(2500),
            "third caller waited {slowest:?} for a shared 1.2s snapshot"
        );
        let later = Instant::now();
        let route = watchers.lock().await.route("prusa");
        assert_eq!(route, Route::Node);
        assert!(!route_needs_snapshot(&route));
        assert!(later.elapsed() < Duration::from_millis(200));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }
}

fn product_router(state: &GatewayState) -> Option<axum::Router> {
    state
        .product
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Whether a `text/event-stream` response stops counting when draining begins.
#[derive(Clone, Copy)]
enum Streams {
    /// The stream only reads, so the drain need not wait for it.
    Release,
    /// The stream carries the result of work in progress, such as an MCP tool call.
    Count,
}

/// Answer from the Rust product routes. They see the peer as `ConnectInfo`
/// and apply the composed transport and credential gates. With
/// `Streams::Release`, a `text/event-stream` body stops counting as active when
/// draining begins and ends at its next poll, even if its client has stopped
/// reading; other bodies end when the drain finishes.
async fn product_response(
    state: &GatewayState,
    product: axum::Router,
    request: Request<Body>,
    mut admission: Admission,
    streams: Streams,
) -> Response<Body> {
    let Ok(mut response) = tower::ServiceExt::oneshot(product, request).await;
    // Same dynamic-response policy as the compatibility branch: account and
    // Plan JSON must not be cached and reused without rechecking the session.
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    let cancelled = if matches!(streams, Streams::Release)
        && response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .is_some_and(|value| value == "text/event-stream")
    {
        admission.release_at(state.watcher_cancelled.clone());
        state.watcher_cancelled.clone()
    } else {
        state.cancelled.clone()
    };
    response.map(|body| {
        Body::new(TrackedBody {
            body,
            _admission: admission,
            cancelled: Box::pin(cancelled.cancelled_owned()),
        })
    })
}

/// The whole body read up front, so cancelling the runtime cannot cut it off.
async fn buffered(response: Response<Body>) -> Response<Body> {
    let (parts, body) = response.into_parts();
    match body.collect().await {
        Ok(bytes) => Response::from_parts(parts, Body::from(bytes.to_bytes())),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "Logout response failed"),
    }
}

struct TrackedBody {
    body: Body,
    _admission: Admission,
    cancelled: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}
impl http_body::Body for TrackedBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Some(Err(axum::Error::new(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Runtime stopped during response",
            )))));
        }
        std::pin::Pin::new(&mut self.body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod csp;

#[cfg(test)]
mod slice0_mount;

#[cfg(test)]
mod ticket_t_4_watcher_proof;

#[cfg(test)]
mod ticket_t_2;

#[cfg(test)]
mod ticket_t_3;

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn changed_port_rejects_old_cookie_and_accepts_fresh_bootstrap() {
        use super::*;
        use http_body_util::BodyExt;
        use hyper_util::rt::TokioIo;

        async fn exchange(
            origin: &str,
            target: &str,
            cookie: Option<&str>,
        ) -> (StatusCode, axum::http::HeaderMap) {
            let socket = tokio::net::TcpStream::connect(origin.strip_prefix("http://").unwrap())
                .await
                .unwrap();
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(socket))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            let mut request = Request::builder()
                .uri(target)
                .header("host", origin.strip_prefix("http://").unwrap())
                .header("connection", "close");
            if let Some(cookie) = cookie {
                request = request.header("cookie", cookie);
            }
            let response = sender
                .send_request(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let (head, body) = response.into_parts();
            body.collect().await.unwrap();
            drop(sender);
            connection.await.unwrap().unwrap();
            (head.status, head.headers)
        }

        let old_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut old = Gateway::start(
            old_listener,
            PathBuf::new(),
            CompatHandle::unpublished(),
            &super::OperationOwners::node_only(),
        )
        .unwrap();
        let old_origin = old.origin().to_owned();
        let target = old.take_launch_target().unwrap().into_url();
        let (status, headers) =
            exchange(&old_origin, target.strip_prefix(&old_origin).unwrap(), None).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let old_cookie = headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut new = Gateway::start(
            listener,
            PathBuf::new(),
            CompatHandle::unpublished(),
            &super::OperationOwners::node_only(),
        )
        .unwrap();
        let origin = new.origin().to_owned();
        assert_ne!(origin, old_origin);
        assert_eq!(
            exchange(&origin, "/__runtime", Some(&old_cookie)).await.0,
            StatusCode::UNAUTHORIZED
        );
        let target = new.take_launch_target().unwrap().into_url();
        let (status, headers) = exchange(
            &origin,
            target.strip_prefix(&origin).unwrap(),
            Some(&old_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let cookie = headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let wrong_name = format!(
            "{}={}",
            old_cookie.split_once('=').unwrap().0,
            cookie.split_once('=').unwrap().1
        );
        assert_eq!(
            exchange(&origin, "/__runtime", Some(&wrong_name)).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            exchange(&origin, "/__runtime", Some(cookie)).await.0,
            StatusCode::OK
        );
        assert_ne!(
            cookie.split_once('=').unwrap().0,
            old_cookie.split_once('=').unwrap().0
        );
        assert_eq!(
            exchange(
                &origin,
                "/__runtime",
                Some(&format!("{old_cookie}; {cookie}"))
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            exchange(&origin, "/__runtime", Some(&old_cookie)).await.0,
            StatusCode::UNAUTHORIZED
        );
        old.stop().await.unwrap();
        new.stop().await.unwrap();
    }

    #[test]
    fn encoded_artifact_segments_preserve_safe_names_and_reject_ambiguity() {
        for path in [
            "/sources/1/stl/caf%C3%A9%20%28sample%29.stl/mesh",
            "/exports/a%20b.zip",
            "/sources/1/stl/folder/model.stl/mesh",
            "/",
        ] {
            assert!(
                super::target::CanonicalTarget::parse(&path.parse().unwrap()).is_ok(),
                "{path}"
            );
        }
        for path in [
            "/sources/1/stl/%2e%2e/mesh",
            "/a/./b",
            "/a/../b",
            "/a//b",
            "/a%2Fb",
            "/a%2fb",
            "/a%5Cb",
            "/a\\b",
            "/a%00b",
            "/a%0Ab",
            "/a%252fb",
            "/a%252e%252eb",
            "/a%",
            "/a%2",
            "/a%zz",
            "/a%ff",
        ] {
            assert!(
                super::target::CanonicalTarget::parse(&path.parse().unwrap()).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn ticket_t_3_drain_begin_drain_rejects_admission_while_work_is_active() {
        use super::*;
        let access = Arc::new(Mutex::new(Sessions {
            draining: false,
            active: 0,
            bootstraps: vec![(
                [1; 32],
                tokio::time::Instant::now() + Duration::from_secs(60),
            )],
            sessions: HashSet::from([digest("session")]),
        }));
        let held = Admission::acquire(&access, Some("session")).unwrap();
        access.lock().unwrap().begin_drain();
        assert!(matches!(
            Admission::acquire(&access, Some("session")),
            Err(StatusCode::SERVICE_UNAVAILABLE)
        ));
        let state = access.lock().unwrap();
        assert!(state.draining);
        assert!(state.bootstraps.is_empty());
        assert!(state.sessions.is_empty());
        assert_eq!(state.active, 1);
        drop(state);
        drop(held);
        assert_eq!(access.lock().unwrap().active, 0);
    }

    fn empty_sessions() -> super::Sessions {
        super::Sessions {
            draining: false,
            active: 0,
            bootstraps: Vec::new(),
            sessions: std::collections::HashSet::new(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ticket_t_19_bootstrap_expires_after_60_seconds() {
        let mut sessions = empty_sessions();
        let token = sessions.mint_bootstrap().unwrap();
        tokio::time::advance(std::time::Duration::from_secs(59)).await;
        assert!(sessions.redeem_bootstrap(&token));
        assert!(!sessions.redeem_bootstrap(&token));
        let token = sessions.mint_bootstrap().unwrap();
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        assert!(!sessions.redeem_bootstrap(&token));
        assert!(sessions.bootstraps.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn ticket_t_19_pending_bootstraps_stay_bounded() {
        let mut sessions = empty_sessions();
        let tokens: Vec<_> = (0..=super::PENDING_BOOTSTRAPS)
            .map(|_| sessions.mint_bootstrap().unwrap())
            .collect();
        assert_eq!(sessions.bootstraps.len(), super::PENDING_BOOTSTRAPS);
        assert!(!sessions.redeem_bootstrap(&tokens[0]));
        for token in &tokens[1..] {
            assert!(sessions.redeem_bootstrap(token));
        }
        sessions.begin_drain();
        assert!(sessions.mint_bootstrap().is_none());
    }

    #[test]
    fn admission_and_drain_share_one_barrier() {
        use super::*;
        for _ in 0..16 {
            let access = Arc::new(Mutex::new(Sessions {
                draining: false,
                active: 0,
                bootstraps: Vec::new(),
                sessions: HashSet::from([digest("session")]),
            }));
            let start = Arc::new(std::sync::Barrier::new(17));
            let release = Arc::new(std::sync::Barrier::new(17));
            let workers: Vec<_> = (0..16)
                .map(|_| {
                    let access = access.clone();
                    let start = start.clone();
                    let release = release.clone();
                    std::thread::spawn(move || {
                        start.wait();
                        let admitted = Admission::acquire(&access, Some("session")).ok();
                        release.wait();
                        admitted.is_some()
                    })
                })
                .collect();
            start.wait();
            let admitted_before_drain = {
                let mut state = access.lock().unwrap();
                state.begin_drain();
                state.active
            };
            assert!(matches!(
                Admission::acquire(&access, Some("session")),
                Err(StatusCode::SERVICE_UNAVAILABLE)
            ));
            release.wait();
            let admitted = workers
                .into_iter()
                .map(|worker| usize::from(worker.join().unwrap()))
                .sum::<usize>();
            assert_eq!(admitted, admitted_before_drain);
            assert_eq!(access.lock().unwrap().active, 0);
        }
    }

    #[test]
    fn wildcard_requires_its_slash_boundary() {
        for (pattern, prefix) in [
            ("/exports/*", "/exports"),
            ("/sources/:id/stl/*", "/sources/1/stl"),
            ("/sources/:id/docs/*", "/sources/1/docs"),
        ] {
            assert!(!super::matches_path(pattern, prefix));
            for tail in ["/", "/file", "/nested/file"] {
                assert!(super::matches_path(pattern, &format!("{prefix}{tail}")));
            }
        }
        assert!(!super::matches_path(
            "/sources/:id/stl/*",
            "/sources//stl/file"
        ));
    }

    #[test]
    fn manifest_wildcards_preserve_methods_and_aliases() {
        let mut manifest: super::Manifest =
            serde_json::from_str(include_str!("../operations.json")).unwrap();
        super::sort_routes(&mut manifest.routes);
        for method in ["GET", "HEAD"] {
            for alias in ["", "/api/v1"] {
                for (pattern, prefix) in [
                    ("/exports/*", "/exports"),
                    ("/sources/:id/stl/*", "/sources/1/stl"),
                ] {
                    let pattern = format!("{alias}{pattern}");
                    let prefix = format!("{alias}{prefix}");
                    let selected = |path: &str| {
                        manifest.routes.iter().find(|route| {
                            route.method == method && super::matches_path(&route.path, path)
                        })
                    };
                    assert!(selected(&prefix).is_none());
                    for tail in ["/", "/file", "/nested/file"] {
                        assert_eq!(selected(&format!("{prefix}{tail}")).unwrap().path, pattern);
                    }
                }
                let exact = format!("{alias}/sources/:id/docs");
                let wildcard = format!("{exact}/*");
                let exact_index = manifest
                    .routes
                    .iter()
                    .position(|route| route.method == method && route.path == exact)
                    .unwrap();
                let wildcard_index = manifest
                    .routes
                    .iter()
                    .position(|route| route.method == method && route.path == wildcard)
                    .unwrap();
                assert!(exact_index < wildcard_index);
                let selected = manifest
                    .routes
                    .iter()
                    .find(|route| {
                        route.method == method
                            && super::matches_path(&route.path, &format!("{alias}/sources/1/docs"))
                    })
                    .unwrap();
                assert_eq!(selected.path, exact);
            }
        }
    }

    #[test]
    fn operation_sort_preserves_exact_static_parameter_and_wildcard_priority() {
        let mut routes: Vec<_> = ["/files/*", "/files/:id", "/files/settings", "/files"]
            .into_iter()
            .map(|path| super::Operation {
                method: "GET".into(),
                path: path.into(),
                registry_owner: "compat".into(),
                owner: super::Owner::Node,
                effect: super::EffectClass::Observation,
            })
            .collect();
        super::sort_routes(&mut routes);
        for (path, expected) in [
            ("/files", "/files"),
            ("/files/settings", "/files/settings"),
            ("/files/42", "/files/:id"),
            ("/files/42/nested", "/files/*"),
            ("/files/", "/files/*"),
        ] {
            let selected = routes
                .iter()
                .find(|route| super::matches_path(&route.path, path))
                .unwrap();
            assert_eq!(selected.path, expected);
        }
    }

    #[test]
    fn global_options_denial_remains_universal() {
        let manifest: super::Manifest =
            serde_json::from_str(include_str!("../operations.json")).unwrap();
        let denied = manifest
            .denied
            .iter()
            .find(|route| route.method == "OPTIONS" && route.path == "*")
            .unwrap();
        for path in [
            "/",
            "/exports",
            "/exports/",
            "/sources/1/stl/nested/file.stl/mesh",
        ] {
            assert!(super::matches_path(&denied.path, path));
        }
    }

    #[test]
    fn effectful_get_is_explicit() {
        let manifest: super::Manifest =
            serde_json::from_str(include_str!("../operations.json")).unwrap();
        assert!(manifest.routes.iter().any(|r| r.method == "GET"
            && r.path == "/printer-checkoff"
            && r.effect == super::EffectClass::LocalCommit));
    }

    #[tokio::test]
    async fn shutdown_request_observation_is_monotonic() {
        let token = CancellationToken::new();
        let requested = super::ShutdownRequested {
            token: token.clone(),
        };
        let first = requested.clone();
        token.cancel();
        first.wait().await;
        token.cancel();
        requested.wait().await;
    }
}

#[cfg(test)]
mod ticket_t_11;

#[cfg(test)]
#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;
#[cfg(test)]
impl Gateway {
    /// Redeem the launch bootstrap in-process; returns the desktop session cookie.
    async fn test_session(&mut self) -> String {
        let launch = self.take_launch_target().unwrap().into_url();
        let bootstrap = Request::builder()
            .uri(launch.strip_prefix(self.origin()).unwrap())
            .header("host", &self.state.host)
            .body(Body::empty())
            .unwrap();
        let response = handle(State(self.state.clone()), bootstrap).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        pp_testkit::session_cookie(response.headers()["set-cookie"].to_str().unwrap())
    }
}

/// Auto-removing test scratch, shared with pp-storage's tests.
#[cfg(test)]
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

#[cfg(test)]
#[test]
fn scratch_root_is_empty_after_lib_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_lib_tests"]);
}
