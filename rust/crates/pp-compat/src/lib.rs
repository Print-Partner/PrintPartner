mod account_pin;
pub use account_pin::DesktopAccountPin;
pub mod drain;
mod logs;
pub use logs::{CoreLog, log_folder, recent_diagnostics, recent_logs};
#[cfg(unix)]
mod parent_death;
pub mod redact;
pub mod settings;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(windows)]
mod windows_security;
/// The supervised child process.
pub use platform::Child;
use platform::ParentDeathGuard;
mod release;
use anyhow::{Context, Result, bail, ensure};
use axum::{
    body::Body,
    http::{HeaderValue, Request, Response},
};
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
pub use release::{MeasuredBundle, verify_bundle};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncWriteExt,
    process::{ChildStdin, Command},
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::{
    sync::CancellationToken,
    task::{TaskTracker, task_tracker::TaskTrackerToken},
};

#[derive(Clone)]
pub struct Bundle {
    pub node: PathBuf,
    pub entry: PathBuf,
    pub web_root: PathBuf,
    pub runtime_version: String,
    pub commit: String,
}

impl Bundle {
    /// The community manifests Node reads: `manifest-apply.js` sits in
    /// `services/` next to `entry` and joins `../data/manifests`.
    pub fn community_manifests_dir(&self) -> Option<PathBuf> {
        // ast-grep-ignore: no-writer-family-literal -- Node's shipped community manifests, read only; Rust never writes them.
        self.entry.parent().map(|dist| dist.join("data/manifests"))
    }
}

static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Start `command` while no compat spawn holds inheritable handles. On
/// Windows a child created with handle inheritance gets every inheritable
/// handle in the process, and Node's pipe ends are inheritable for the length
/// of its `CreateProcessW` call; a child that keeps them would hide stdin EOF
/// from Node and the log readers. Every other child the process starts goes
/// through here.
pub fn spawn_child(command: &mut Command) -> std::io::Result<tokio::process::Child> {
    let _spawning = SPAWN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    command.spawn()
}

#[cfg(test)]
pub type BeforeSetup = Box<dyn Fn(&Path, u32) -> Result<()> + Send + Sync>;

pub struct SpawnSpec {
    pub account_pin: DesktopAccountPin,
    pub bundle: MeasuredBundle,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    #[cfg(test)]
    pub setup_padding: String,
    /// The exclusive printpartner.lock descriptor, inherited by the supervised child.
    pub storage_lock: std::fs::File,
    pub port: u16,
    pub gateway_rate_limit: bool,
    /// User settings for the child's environment. Only `Owner::DesktopSettings` names.
    pub settings: Vec<(&'static str, String)>,
    #[cfg(test)]
    pub before_setup: BeforeSetup,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Starting,
    Ready { generation: String, pid: u32 },
    Backoff { attempt: usize },
    Guarded,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone)]
pub struct Endpoint {
    channel: Arc<platform::Channel>,
    generation: String,
    key: [u8; 32],
    connections: Arc<Connections>,
}

struct Connections {
    closed: Mutex<bool>,
    tasks: TaskTracker,
    cancelled: CancellationToken,
}
impl Connections {
    fn new() -> Self {
        Self {
            closed: Mutex::new(false),
            tasks: TaskTracker::new(),
            cancelled: CancellationToken::new(),
        }
    }
    fn admit(&self) -> Result<TaskTrackerToken> {
        let closed = self.closed.lock().expect("Connection gate poisoned");
        anyhow::ensure!(!*closed, "Compatibility generation is stopping");
        Ok(self.tasks.token())
    }
    fn close(&self) {
        *self.closed.lock().expect("Connection gate poisoned") = true;
        self.cancelled.cancel();
        self.tasks.close();
    }
    async fn join(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(2), self.tasks.wait())
            .await
            .context("Compatibility connection join timed out")
    }
}

#[derive(Serialize, Deserialize)]
struct Principal<'a> {
    version: u8,
    generation: &'a str,
    tenant: &'a str,
    actor: &'a str,
    method: &'a str,
    target: &'a str,
    issued_at: u64,
}

impl Endpoint {
    /// Authenticate a quota debit from this private, measured Node generation.
    pub fn rate_limit_bucket(&self, encoded: &str, signature: &str) -> Option<bool> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Debit {
            generation: String,
            bucket: String,
            issued_at: u64,
        }
        if encoded.len() > 1024 || signature.len() != 64 {
            return None;
        }
        let mut verifier = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
        verifier.update(encoded.as_bytes());
        verifier.verify_slice(&hex::decode(signature).ok()?).ok()?;
        let debit: Debit = serde_json::from_slice(&hex::decode(encoded).ok()?).ok()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis() as u64;
        if debit.generation != self.generation || now.abs_diff(debit.issued_at) > 5000 {
            return None;
        }
        match debit.bucket.as_str() {
            "app" => Some(false),
            "media" => Some(true),
            _ => None,
        }
    }

    /// Socket endpoint with no child. Tests drop publication while a refresh is blocked on it.
    pub fn bound_to(socket: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            channel: Arc::new(platform::Channel::unbound(socket.into())),
            generation: "test-generation".into(),
            key: [9; 32],
            connections: Arc::new(Connections::new()),
        })
    }

    pub async fn forward(&self, mut request: Request<Body>) -> Result<Response<Body>> {
        let admitted = self.connections.admit()?;
        let target = request
            .uri()
            .path_and_query()
            .context("Missing request target")?
            .as_str()
            .to_owned();
        let assertion = Principal {
            version: 1,
            generation: &self.generation,
            tenant: "default",
            actor: "desktop-owner",
            method: request.method().as_str(),
            target: &target,
            issued_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        };
        let encoded = hex::encode(serde_json::to_vec(&assertion)?);
        let mut signer = Hmac::<Sha256>::new_from_slice(&self.key)?;
        signer.update(encoded.as_bytes());
        let signature = hex::encode(signer.finalize().into_bytes());
        request
            .headers_mut()
            .insert("x-pp-principal", HeaderValue::from_str(&encoded)?);
        request
            .headers_mut()
            .insert("x-pp-signature", HeaderValue::from_str(&signature)?);
        request
            .headers_mut()
            .insert("host", HeaderValue::from_static("desktop-compat"));
        let stream = tokio::select! {
            biased;
            _ = self.connections.cancelled.cancelled() => bail!("Compatibility connection stopped"),
            stream = self.channel.connect() => stream.context("Compatibility socket unavailable")?,
        };
        let (mut sender, connection) = tokio::select! {
            biased;
            _ = self.connections.cancelled.cancelled() => bail!("Compatibility connection stopped"),
            handshake = hyper::client::conn::http1::handshake(TokioIo::new(stream)) => handshake?,
        };
        let cancelled = self.connections.cancelled.clone();
        tokio::spawn(async move {
            let _admitted = admitted;
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => {},
                _ = connection.with_upgrades() => {},
            }
        });
        let response = tokio::select! {
            biased;
            _ = self.connections.cancelled.cancelled() => bail!("Compatibility dispatch outcome unknown"),
            response = sender.send_request(request) => response.context("Compatibility dispatch outcome unknown")?,
        };
        Ok(response.map(Body::new))
    }

    async fn ready(&self, bundle: &Bundle, cancelled: &CancellationToken) -> bool {
        let probe = async {
            let response = self
                .forward(Request::builder().uri("/health").body(Body::empty())?)
                .await?;
            if !response.status().is_success() {
                bail!("Health denied");
            }
            let bytes = http_body_util::Limited::new(response.into_body(), 65536)
                .collect()
                .await
                .map_err(|_| anyhow::anyhow!("Invalid health response body"))?
                .to_bytes();
            let health: serde_json::Value = serde_json::from_slice(&bytes)?;
            if health["ok"] != true
                || health["version"] != bundle.runtime_version
                || health["release"]["commit"] != bundle.commit
            {
                bail!("Release or database health mismatch");
            }
            Ok::<(), anyhow::Error>(())
        };
        tokio::select! {
            biased;
            _ = cancelled.cancelled() => false,
            result = tokio::time::timeout(Duration::from_secs(2), probe) => {
                matches!(result, Ok(Ok(())))
            }
        }
    }
}

#[derive(Clone)]
pub struct CompatHandle {
    endpoint: watch::Receiver<Option<Arc<Endpoint>>>,
    pub status: watch::Receiver<Status>,
    recover: mpsc::Sender<()>,
}

impl CompatHandle {
    /// No child endpoint is published. Node-owned routes must fail closed.
    pub fn unpublished() -> Self {
        let (_endpoints, endpoint) = watch::channel(None);
        let (_states, status) = watch::channel(Status::Starting);
        let (recover, _recovery) = mpsc::channel(1);
        Self {
            endpoint,
            status,
            recover,
        }
    }

    /// Caller replaces the published child. `None` is an unpublished backend.
    pub fn replaceable(
        endpoint: Option<Arc<Endpoint>>,
    ) -> (Self, watch::Sender<Option<Arc<Endpoint>>>) {
        let (tx, rx) = watch::channel(endpoint);
        let (_states, status) = watch::channel(Status::Starting);
        let (recover, _recovery) = mpsc::channel(1);
        (
            Self {
                endpoint: rx,
                status,
                recover,
            },
            tx,
        )
    }

    pub fn endpoint(&self) -> Option<Arc<Endpoint>> {
        self.endpoint.borrow().clone()
    }
    pub async fn recover(&self) -> Result<()> {
        let status = self.status.borrow();
        if !matches!(*status, Status::Guarded) {
            bail!("Recovery is available only after the crash guard stops the child");
        }
        // Keep the status read lock through admission so Starting can drain
        // every request admitted against this guard before spawning again.
        let result = self.recover.try_send(()).map_err(|error| match error {
            mpsc::error::TrySendError::Full(()) => anyhow::anyhow!("Recovery already requested"),
            mpsc::error::TrySendError::Closed(()) => anyhow::anyhow!("Supervisor stopped"),
        });
        drop(status);
        result
    }
}

pub struct Supervisor {
    #[cfg(test)]
    health_probes_completed: watch::Receiver<usize>,
    pub handle: CompatHandle,
    stop: CancellationToken,
    restarts_disarmed: CancellationToken,
    task: Option<JoinHandle<Result<CompatExit>>>,
}

fn restarts_blocked(cancelled: &CancellationToken, disarmed: &CancellationToken) -> bool {
    !drain::restarts_allowed(cancelled.is_cancelled() || disarmed.is_cancelled())
}

fn startup_probe_publishes(blocked: bool, probe_ready: bool) -> bool {
    probe_ready && !blocked
}

struct Process {
    child: Child,
    pid: u32,
    stdin: Option<ChildStdin>,
    endpoint: Arc<Endpoint>,
    logs: Vec<JoinHandle<()>>,
    parent_death: Option<ParentDeathGuard>,
}

#[derive(Debug)]
enum StopOutcome {
    Observed(ExitStatus),
    Forced(ExitStatus),
}

impl StopOutcome {
    fn exit(&self) -> CompatExit {
        match self {
            Self::Observed(_) => CompatExit::Graceful,
            Self::Forced(_) => CompatExit::Forced,
        }
    }
}

/// Evidence from the last compatibility child when supervision ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatExit {
    /// A planned stop exited without parent-forced termination, or no child ever started.
    Graceful,
    /// The parent SIGKILLed the leader or surviving process-group members.
    Forced,
    /// The last child was reaped after an unplanned exit or failed readiness/health.
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopIntent {
    PlannedShutdown,
    Restart,
}

#[derive(Default)]
struct CleanupErrors {
    first: Option<anyhow::Error>,
}

impl CleanupErrors {
    fn capture<T>(&mut self, result: Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                if self.first.is_none() {
                    self.first = Some(error);
                }
                None
            }
        }
    }

    fn finish(self) -> Result<()> {
        match self.first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn validate_stop_outcome(outcome: StopOutcome, intent: StopIntent) -> Result<()> {
    match (outcome, intent) {
        (StopOutcome::Observed(_) | StopOutcome::Forced(_), StopIntent::Restart) => Ok(()),
        (StopOutcome::Forced(status), StopIntent::PlannedShutdown)
            if platform::stopped_by_parent(&status) || status.success() =>
        {
            Ok(())
        }
        (StopOutcome::Forced(status), StopIntent::PlannedShutdown) => {
            bail!("Compatibility child required parent-forced termination: {status}")
        }
        (StopOutcome::Observed(status), StopIntent::PlannedShutdown)
            if status.success() || platform::stopped_by_parent(&status) =>
        {
            Ok(())
        }
        (StopOutcome::Observed(status), StopIntent::PlannedShutdown) => {
            bail!("Compatibility child exited unexpectedly during shutdown: {status}")
        }
    }
}

fn classify_stop_outcome(
    outcome: StopOutcome,
    cancelled: &CancellationToken,
) -> Result<StopIntent> {
    let intent = if cancelled.is_cancelled() {
        StopIntent::PlannedShutdown
    } else {
        StopIntent::Restart
    };
    validate_stop_outcome(outcome, intent)?;
    Ok(intent)
}

enum SetupWrite {
    Sent,
    Cancelled,
}

async fn write_setup_or_cancel(
    stdin: &mut ChildStdin,
    setup: serde_json::Value,
    cancelled: &CancellationToken,
) -> std::io::Result<SetupWrite> {
    let bytes = format!("{setup}\n");
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Ok(SetupWrite::Cancelled),
        result = stdin.write_all(bytes.as_bytes()) => result.map(|()| SetupWrite::Sent),
    }
}

impl Process {
    /// The first spawn uses the verification that produced `spec.bundle`. A respawn passes
    /// the restart-disarm token, measures the release again on a blocking thread that stops
    /// between files once `cancelled` fires, and starts no child if restarts were blocked
    /// while it measured.
    async fn spawn(
        spec: &SpawnSpec,
        capture: &logs::Logs,
        cancelled: &CancellationToken,
        respawn: Option<&CancellationToken>,
    ) -> Result<Self> {
        if let Some(disarmed) = respawn {
            let bundle = Bundle::clone(&spec.bundle);
            let stop = cancelled.clone();
            tokio::task::spawn_blocking(move || release::verify_backend(&bundle, &stop))
                .await
                .context("Release verification task failed")??;
            ensure!(
                !restarts_blocked(cancelled, disarmed),
                "Compatibility restart blocked during release verification"
            );
        }
        let package_root = release::package_root(&spec.bundle)?;
        let preload = spec
            .bundle
            .entry
            .parent()
            .context("Missing backend root")?
            .join("desktop-resolution.js")
            .canonicalize()
            .context("Measured desktop preload unavailable")?;
        let mut root_argument = std::ffi::OsString::from("--pp-desktop-package-root=");
        root_argument.push(package_root);
        let generation = hex::encode(rand::random::<[u8; 16]>());
        let key = rand::random::<[u8; 32]>();
        let endpoint = Arc::new(Endpoint {
            channel: Arc::new(
                platform::Channel::bind(&spec.runtime_dir, &generation)
                    .context("Could not create the compatibility channel")?,
            ),
            generation,
            key,
            connections: Arc::new(Connections::new()),
        });
        if let Some((name, _)) = spec
            .settings
            .iter()
            .find(|(name, _)| !settings::is_desktop_setting(name))
        {
            bail!("{name} is not a desktop setting");
        }
        let mut command = Command::new(&spec.bundle.node);
        command
            .arg("--no-global-search-paths")
            .arg("--import")
            .arg(preload)
            .arg(&spec.bundle.entry)
            .arg(root_argument)
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .current_dir(&spec.bundle.web_root)
            .env_clear()
            .envs(
                platform::INHERITED_ENV
                    .iter()
                    .filter_map(|name| std::env::var_os(name).map(|value| (*name, value))),
            )
            .env("PP_COMMIT", release::manifest()?.commit)
            .envs(settings::fixed_env())
            .envs(
                spec.settings
                    .iter()
                    .map(|(name, value)| (*name, value.as_str())),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let (child_setup, mut parent_death) =
            platform::configure_child(&mut command, &spec.storage_lock)
                .context("Could not arm parent-death watcher")?;
        let mut child =
            platform::spawn(&mut command, &child_setup).context("Could not start bundled Node")?;
        if let Err(error) = platform::adopt(child_setup, &mut parent_death, &child) {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error).context("Could not arm parent-death watcher");
        }
        #[cfg(test)]
        if let Err(error) =
            (spec.before_setup)(&spec.data_dir, child.id().context("Missing child PID")?)
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error);
        }
        let mut stdin = child.stdin.take().context("Missing parent-liveness pipe")?;
        let mut setup = serde_json::json!({ "version":1, "data_dir":spec.data_dir, "socket_path":endpoint.channel.name(),
            "generation":endpoint.generation, "key":hex::encode(key), "parent_pid":std::process::id(),
            "port":spec.port, "gateway_rate_limit":spec.gateway_rate_limit });
        spec.account_pin.apply(&mut setup);
        #[cfg(test)]
        if !spec.setup_padding.is_empty() {
            setup["test_padding"] = spec.setup_padding.clone().into();
        }
        if write_setup_or_cancel(&mut stdin, setup, cancelled)
            .await
            .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
            bail!("Compatibility setup failed");
        }
        // The child receives its HMAC key on stdin; never let it echo back.
        let capture = &capture.with_secret(hex::encode(key));
        let mut logs = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let capture = capture.clone();
            logs.push(tokio::spawn(async move { capture.capture(stdout).await }));
        }
        if let Some(stderr) = child.stderr.take() {
            let capture = capture.clone();
            logs.push(tokio::spawn(async move { capture.capture(stderr).await }));
        }
        Ok(Self {
            pid: child.id().context("Missing child PID")?,
            child,
            stdin: Some(stdin),
            endpoint,
            logs,
            parent_death: Some(parent_death),
        })
    }

    #[cfg(test)]
    async fn spawn_shell(
        spec: &ShellSpec,
        capture: &logs::Logs,
        cancelled: &CancellationToken,
    ) -> Result<Self> {
        let generation = hex::encode(rand::random::<[u8; 16]>());
        let key = rand::random::<[u8; 32]>();
        let endpoint = Arc::new(Endpoint {
            channel: Arc::new(platform::Channel::bind(&spec.runtime_dir, &generation)?),
            generation,
            key,
            connections: Arc::new(Connections::new()),
        });
        let mut command = platform::test_shell(spec.script);
        command
            .env("NODE", crate::tests::node_binary())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let lease = std::fs::File::open(if cfg!(windows) { "NUL" } else { "/dev/null" })?;
        let (child_setup, mut parent_death) = platform::configure_child(&mut command, &lease)?;
        let mut child =
            platform::spawn(&mut command, &child_setup).context("Could not start test child")?;
        if let Err(error) = platform::adopt(child_setup, &mut parent_death, &child) {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error.into());
        }
        let pid = child.id().context("Missing child PID")?;
        if let Err(error) = record_test_pid(&spec.pidfile, pid) {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error);
        }
        let mut stdin = child.stdin.take().context("Missing parent-liveness pipe")?;
        let setup = serde_json::json!({
            "version": 1,
            "data_dir": spec.data_dir,
            "socket_path": endpoint.channel.name(),
            "generation": endpoint.generation,
            "key": hex::encode(key),
            "test_padding": spec.setup_padding,
            "parent_pid": std::process::id(),
            "port": spec.port
        });
        if write_setup_or_cancel(&mut stdin, setup, cancelled)
            .await
            .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
            bail!("Compatibility setup failed");
        }
        let capture = &capture.with_secret(hex::encode(key));
        let mut logs = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let capture = capture.clone();
            logs.push(tokio::spawn(async move { capture.capture(stdout).await }));
        }
        if let Some(stderr) = child.stderr.take() {
            let capture = capture.clone();
            logs.push(tokio::spawn(async move { capture.capture(stderr).await }));
        }
        Ok(Self {
            pid,
            child,
            stdin: Some(stdin),
            endpoint,
            logs,
            parent_death: Some(parent_death),
        })
    }

    async fn stop(
        mut self,
        observed: Option<ExitStatus>,
        cancelled: &CancellationToken,
        eof_watchdog: bool,
    ) -> Result<(StopIntent, CompatExit)> {
        let pid = self.pid;
        let mut errors = CleanupErrors::default();
        let mut outcome = None;
        if cancelled.is_cancelled() {
            let eof_closed = {
                let connections = Arc::clone(&self.endpoint.connections);
                let mut reject = move || connections.close();
                drain::reject_and_arm_eof(eof_watchdog, &mut self.stdin, &mut reject)
            };
            match drain::drive_shutdown_group(
                &mut self.child,
                pid,
                eof_closed,
                drain::SIGTERM_GRACE,
                observed,
            )
            .await
            {
                Ok(report) => {
                    outcome = Some(match report.exit {
                        drain::GroupExit::Observed(status) => StopOutcome::Observed(status),
                        drain::GroupExit::Forced(status) => StopOutcome::Forced(status),
                    });
                    if !report.group_absent {
                        errors.capture::<()>(Err(anyhow::anyhow!("Process group still present")));
                    }
                }
                Err(error) => {
                    errors.capture::<()>(Err(error));
                }
            }
        } else {
            self.endpoint.connections.close();
            self.stdin.take();
            outcome = observed.map(StopOutcome::Observed);
            if outcome.is_none() {
                let shutdown = cancelled.cancelled();
                tokio::pin!(shutdown);
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        match drain::drive_shutdown_group(
                            &mut self.child,
                            pid,
                            true,
                            drain::SIGTERM_GRACE,
                            None,
                        )
                        .await
                        {
                            Ok(report) => {
                                outcome = Some(match report.exit {
                                    drain::GroupExit::Observed(status) => StopOutcome::Observed(status),
                                    drain::GroupExit::Forced(status) => {
                                        StopOutcome::Forced(status)
                                    }
                                });
                                if !report.group_absent {
                                    errors.capture::<()>(Err(anyhow::anyhow!(
                                        "Process group still present"
                                    )));
                                }
                            }
                            Err(error) => {
                                errors.capture::<()>(Err(error));
                            }
                        }
                    }
                    finished = tokio::time::timeout(Duration::from_secs(10), self.child.wait()) => {
                        match finished {
                            Ok(result) => {
                                if let Some(status) =
                                    errors.capture(result.context("Child reap failed"))
                                {
                                    outcome = Some(StopOutcome::Observed(status));
                                }
                            }
                            Err(_) => {
                                drain::reap_process_group(pid);
                                errors.capture(
                                    self.child.start_kill().context("Child termination failed"),
                                );
                                match tokio::time::timeout(Duration::from_secs(2), self.child.wait())
                                    .await
                                {
                                    Ok(result) => {
                                        if let Some(status) =
                                            errors.capture(result.context("Child reap failed"))
                                        {
                                            outcome =
                                                Some(StopOutcome::Forced(status));
                                        }
                                    }
                                    Err(error) => {
                                        errors.capture::<()>(
                                            Err(error).context("Child reap timed out"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let leader_exited = matches!(outcome, Some(StopOutcome::Observed(_)));
            if drain::reap_settled_group(pid, leader_exited).await {
                outcome = outcome.map(|outcome| match outcome {
                    StopOutcome::Observed(status) | StopOutcome::Forced(status) => {
                        StopOutcome::Forced(status)
                    }
                });
            }
            errors.capture(
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if drain::process_group_absent(pid) {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .context("Process group still present"),
            );
        }
        if let Some(guard) = self.parent_death.as_mut() {
            guard.release();
        }
        errors.capture(self.endpoint.connections.join().await);
        for mut log in self.logs {
            let result = match tokio::time::timeout(Duration::from_secs(1), &mut log).await {
                Ok(result) => result.context("Log reader join failed"),
                Err(_) => {
                    log.abort();
                    let _ = log.await;
                    Err(anyhow::anyhow!("Log reader join timed out"))
                }
            };
            errors.capture(result);
        }
        errors.capture(
            self.endpoint
                .channel
                .close()
                .await
                .context("Compatibility socket cleanup failed"),
        );
        let stopped = outcome
            .map(|outcome| {
                let exit = outcome.exit();
                classify_stop_outcome(outcome, cancelled).map(|intent| (intent, exit))
            })
            .transpose()?;
        errors.finish()?;
        stopped.context("Compatibility child outcome unavailable after cleanup")
    }
}

enum Launch {
    Release(SpawnSpec),
    /// Paused-time double. Production startup always uses [`Launch::Release`].
    #[cfg(test)]
    Shell(ShellSpec),
}

#[cfg(test)]
pub(crate) struct ShellSpec {
    pub script: &'static str,
    pub bundle: Bundle,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub pidfile: PathBuf,
    pub setup_padding: String,
    pub port: u16,
}

impl Launch {
    fn data_dir(&self) -> &Path {
        match self {
            Self::Release(spec) => &spec.data_dir,
            #[cfg(test)]
            Self::Shell(spec) => &spec.data_dir,
        }
    }

    fn bundle(&self) -> &Bundle {
        match self {
            Self::Release(spec) => &spec.bundle,
            #[cfg(test)]
            Self::Shell(spec) => &spec.bundle,
        }
    }
}

async fn obtain(
    source: &Launch,
    logs: &logs::Logs,
    cancelled: &CancellationToken,
    respawn: Option<&CancellationToken>,
) -> Result<Process> {
    match source {
        Launch::Release(spec) => Process::spawn(spec, logs, cancelled, respawn).await,
        #[cfg(test)]
        Launch::Shell(spec) => Process::spawn_shell(spec, logs, cancelled).await,
    }
}

#[cfg(test)]
fn record_test_pid(path: &Path, pid: u32) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Could not record test child {}", path.display()))?;
    writeln!(file, "{pid}").context("Could not record test child")?;
    file.sync_all().context("Could not record test child")
}

pub fn backoff(attempt: usize) -> Duration {
    Duration::from_millis([500, 1000, 2000, 4000, 8000, 30_000][attempt.min(5)])
}

/// Timing policy owned by an individual compatibility supervisor.
#[derive(Clone, Copy, Debug)]
pub struct SupervisorConfig {
    /// Ready time required to reset backoff, and age beyond which crashes expire.
    pub crash_guard_window: Duration,
    /// Close compat stdin during planned shutdown. Restart cleanup always closes it.
    pub eof_watchdog: bool,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            crash_guard_window: Duration::from_secs(120),
            eof_watchdog: true,
        }
    }
}

impl Supervisor {
    pub fn start(spec: SpawnSpec) -> Result<Self> {
        Self::start_with_config(spec, SupervisorConfig::default())
    }

    /// Start with a per-instance timing policy; `start` uses production defaults.
    pub fn start_with_config(spec: SpawnSpec, config: SupervisorConfig) -> Result<Self> {
        Self::launch(Launch::Release(spec), config)
    }

    #[cfg(test)]
    pub(crate) fn launch_shell(spec: ShellSpec, config: SupervisorConfig) -> Result<Self> {
        Self::launch(Launch::Shell(spec), config)
    }

    fn launch(source: Launch, config: SupervisorConfig) -> Result<Self> {
        let logs = logs::Logs::open(source.data_dir().join("logs"))?;
        let (endpoints, endpoint) = watch::channel(None);
        let (states, status) = watch::channel(Status::Starting);
        #[cfg(test)]
        let (probe_completions, health_probes_completed) = watch::channel(0);
        let (recover, mut recovery) = mpsc::channel(1);
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let restarts_disarmed = CancellationToken::new();
        let disarmed = restarts_disarmed.clone();
        let task = tokio::spawn(async move {
            let mut crashes = std::collections::VecDeque::new();
            let mut exit = CompatExit::Graceful;
            let mut attempt = 0;
            let mut spawned = false;
            loop {
                if restarts_blocked(&cancelled, &disarmed) {
                    break;
                }
                states.send_replace(Status::Starting);
                // A concurrent caller may enqueue after the guard receives the
                // first request. Starting closes admission before draining it.
                while recovery.try_recv().is_ok() {}
                logs.event(logs::Event::Starting, 30);
                let process =
                    obtain(&source, &logs, &cancelled, spawned.then_some(&disarmed)).await;
                spawned = true;
                let mut failure = logs::FailureReason::SpawnFailed;
                let mut termination = None;
                if let Ok(mut process) = process {
                    exit = CompatExit::Interrupted;
                    failure = logs::FailureReason::ReadinessTimeout;
                    let mut ready = false;
                    let mut observed_exit = None;
                    if !restarts_blocked(&cancelled, &disarmed) {
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                        while tokio::time::Instant::now() < deadline {
                            if restarts_blocked(&cancelled, &disarmed) {
                                break;
                            }
                            if let Some(status) =
                                process.child.try_wait().context("Child status failed")?
                            {
                                observed_exit = Some(status);
                                break;
                            }
                            let probe = tokio::select! {
                                biased;
                                _ = cancelled.cancelled() => None,
                                _ = disarmed.cancelled() => None,
                                probe = tokio::time::timeout_at(deadline, process.endpoint.ready(source.bundle(), &cancelled)) => {
                                    Some(probe.unwrap_or(false))
                                }
                            };
                            match probe {
                                None => break,
                                Some(probe_ready) => {
                                    #[cfg(test)]
                                    probe_completions.send_modify(|count| *count += 1);
                                    ready = startup_probe_publishes(
                                        restarts_blocked(&cancelled, &disarmed),
                                        probe_ready,
                                    );
                                    if ready || restarts_blocked(&cancelled, &disarmed) {
                                        break;
                                    }
                                }
                            }
                            tokio::select! {
                                biased;
                                _ = cancelled.cancelled() => break,
                                _ = disarmed.cancelled() => break,
                                _ = tokio::time::sleep_until(std::cmp::min(deadline, tokio::time::Instant::now() + Duration::from_millis(250))) => {}
                            }
                        }
                    }
                    if ready && !restarts_blocked(&cancelled, &disarmed) {
                        logs.event(logs::Event::Ready, 30);
                        failure = logs::FailureReason::HealthLost;
                        let pid = process.child.id().unwrap_or_default();
                        endpoints.send_replace(Some(process.endpoint.clone()));
                        states.send_replace(Status::Ready {
                            generation: process.endpoint.generation.clone(),
                            pid,
                        });
                        let ready_since = tokio::time::Instant::now();
                        let mut failures = 0;
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancelled.cancelled() => break,
                                result = process.child.wait() => {
                                    observed_exit = Some(result.context("Child reap failed")?);
                                    break;
                                },
                                _ = tokio::time::sleep(Duration::from_secs(10)) => {
                                    if process.endpoint.ready(source.bundle(), &cancelled).await { failures = 0; } else { failures += 1; }
                                    #[cfg(test)]
                                    probe_completions.send_modify(|count| *count += 1);
                                    if failures >= 3 { break; }
                                }
                            }
                        }
                        // Preserve escalation for repeated crashes, but forget old failures
                        // once the child has stayed ready for the crash-guard window.
                        if ready_since.elapsed() >= config.crash_guard_window {
                            attempt = 0;
                        }
                    }
                    endpoints.send_replace(None);
                    states.send_replace(Status::Stopping);
                    if let Some(status) = observed_exit {
                        failure = logs::FailureReason::ChildExit;
                        termination = status
                            .code()
                            .map(logs::Termination::ExitCode)
                            .or_else(|| platform::signal(&status).map(logs::Termination::Signal));
                    }
                    let planned_stop = cancelled.is_cancelled() && observed_exit.is_none();
                    let (stop_intent, stop_exit) = match process
                        .stop(observed_exit, &cancelled, config.eof_watchdog)
                        .await
                    {
                        Ok(stopped) => stopped,
                        Err(error) => {
                            logs.event(
                                logs::Event::Failure {
                                    reason: logs::FailureReason::CleanupFailed,
                                    termination,
                                },
                                50,
                            );
                            states.send_replace(Status::Failed);
                            return Err(error);
                        }
                    };
                    logs.event(logs::Event::Reaped, 30);
                    if stop_exit == CompatExit::Forced {
                        exit = stop_exit;
                    }
                    if stop_intent == StopIntent::PlannedShutdown {
                        if planned_stop {
                            exit = stop_exit;
                        }
                        break;
                    }
                } else if cancelled.is_cancelled() {
                    break;
                }
                if restarts_blocked(&cancelled, &disarmed) {
                    break;
                }
                logs.event(
                    logs::Event::Failure {
                        reason: failure,
                        termination,
                    },
                    50,
                );
                let now = tokio::time::Instant::now();
                crashes.push_back(now);
                while crashes
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > config.crash_guard_window)
                {
                    crashes.pop_front();
                }
                if crashes.len() >= 5 {
                    logs.event(logs::Event::CrashGuard, 50);
                    states.send_replace(Status::Guarded);
                    tokio::select! {
                        biased;
                        _ = cancelled.cancelled() => break,
                        _ = disarmed.cancelled() => break,
                        _ = recovery.recv() => { crashes.clear(); attempt = 0; }
                    }
                } else {
                    states.send_replace(Status::Backoff { attempt });
                    // Drain disarms restarts before shutdown cancels this task.
                    tokio::select! {
                        biased;
                        _ = cancelled.cancelled() => break,
                        _ = disarmed.cancelled() => break,
                        _ = tokio::time::sleep(backoff(attempt)) => {}
                    }
                    attempt += 1;
                }
            }
            endpoints.send_replace(None);
            states.send_replace(Status::Stopped);
            Ok(exit)
        });
        Ok(Self {
            #[cfg(test)]
            health_probes_completed,
            handle: CompatHandle {
                endpoint,
                status,
                recover,
            },
            stop,
            restarts_disarmed,
            task: Some(task),
        })
    }

    pub fn disarm_restarts(&self) {
        self.restarts_disarmed.cancel();
    }

    pub async fn shutdown(mut self) -> Result<CompatExit> {
        self.stop.cancel();
        match self.task.take() {
            Some(task) => task.await.context("Compatibility supervisor join failed")?,
            None => Ok(CompatExit::Graceful),
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod ticket_t_2;

#[cfg(test)]
mod tests {
    pub(crate) fn node_binary() -> std::path::PathBuf {
        static NODE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        NODE.get_or_init(|| {
            let configured = std::env::var_os("PP_TEST_NODE").unwrap_or_else(|| "node".into());
            let output = std::process::Command::new(configured)
                .args(["-p", "process.execPath"])
                .output()
                .expect("Node must report its native executable");
            assert!(output.status.success());
            std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        })
        .clone()
    }

    #[cfg(unix)]
    fn pid_has_exited(pid: i32) -> bool {
        let result = unsafe { libc::kill(pid, 0) };
        (result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
            || matches!(linux_proc_state(pid as u32), Some('Z' | 'X'))
    }

    #[cfg(unix)]
    fn group_snapshot(group: u32) -> String {
        let output = match std::process::Command::new("ps")
            .args(["-e", "-o", "pid,ppid,pgid,stat,comm"])
            .output()
        {
            Ok(output) => output,
            Err(error) => return format!("ps failed to spawn: {error}"),
        };
        let group = group.to_string();
        let rows = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains("PID") || line.split_whitespace().nth(2) == Some(group.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "ps status: {}\nps stderr: {}\n{rows}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// `kill(pid, 0)` is true for a zombie. A preserved peer has to still be
    /// running: `try_wait` has not reaped it, and Linux does not report `Z`.
    #[cfg(unix)]
    fn peer_is_running(peer: &mut std::process::Child) -> bool {
        let state = linux_proc_state(peer.id());
        let unreaped = peer.try_wait().unwrap().is_none();
        unreaped && state.is_none_or(|state| state != 'Z' && state != 'X')
    }

    #[cfg(unix)]
    fn linux_proc_state(pid: u32) -> Option<char> {
        #[cfg(target_os = "linux")]
        {
            let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let close = text.rfind(')')?;
            text[close + 1..].split_whitespace().next()?.chars().next()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            None
        }
    }

    /// Windows counterparts of the parent-death tests below:
    /// `windows::tests::ticket_t_3_drain_job_object_kills_children_when_core_is_killed`
    /// and `windows::tests::ticket_t_3_drain_job_object_kills_child_when_core_exits_at_spawn`.
    #[cfg(unix)]
    #[tokio::test]
    async fn parent_death_fixture() {
        let Some(pidfile) = std::env::var_os("PP_COMPAT_DEATH_PIDFILE") else {
            return;
        };
        let lease = std::fs::File::open("/dev/null").unwrap();
        let node = node_binary();
        let mut command = tokio::process::Command::new(&node);
        command
            .arg("-e")
            .arg("const {spawn}=require('node:child_process'); const fs=require('node:fs'); const child=spawn(process.execPath,['-e','setInterval(() => {}, 1e9)'],{stdio:'ignore'}); const pidfile=process.env.PP_COMPAT_DEATH_PIDFILE+'.desc'; fs.writeFileSync(pidfile+'.tmp',String(child.pid)); fs.renameSync(pidfile+'.tmp',pidfile); setInterval(() => {}, 1e9)")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let (parent_read, _parent_death) =
            super::platform::configure_child(&mut command, &lease).unwrap();
        let mut child = command.spawn().unwrap();
        drop(parent_read);
        let pidfile = std::path::PathBuf::from(pidfile);
        let temporary = pidfile.with_extension("tmp");
        std::fs::write(&temporary, child.id().unwrap().to_string()).unwrap();
        std::fs::rename(temporary, pidfile).unwrap();
        child.wait().await.unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn separate_group_child_dies_with_force_killed_parent() {
        use std::os::unix::process::CommandExt;
        struct Fixture {
            owner: std::process::Child,
            pidfile: std::path::PathBuf,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.owner.kill();
                let _ = self.owner.wait();
                if let Ok(text) = std::fs::read_to_string(&self.pidfile)
                    && let Ok(pid) = text.parse::<i32>()
                {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
                let _ = std::fs::remove_file(&self.pidfile);
                let _ = std::fs::remove_file(self.pidfile.with_extension("desc"));
                let _ = std::fs::remove_file(self.pidfile.with_extension("desc.tmp"));
            }
        }
        let node = node_binary();
        let pidfile = std::env::temp_dir().join(format!(
            "pp-compat-death-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let owner = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["parent_death_fixture", "--nocapture"])
            .env("PP_COMPAT_DEATH_PIDFILE", &pidfile)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut fixture = Fixture {
            owner,
            pidfile: pidfile.clone(),
        };
        let owner = &mut fixture.owner;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pidfile.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !pidfile.exists() {
            let _ = owner.kill();
            owner.wait().unwrap();
            panic!("node fixture never spawned");
        }
        let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().parse().unwrap();
        let descendant_file = pidfile.with_file_name(format!(
            "{}.desc",
            pidfile.file_name().unwrap().to_string_lossy()
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !descendant_file.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let descendant: u32 = std::fs::read_to_string(&descendant_file)
            .unwrap()
            .parse()
            .unwrap();
        let group = unsafe { libc::getpgid(pid) };
        assert!(node.is_file(), "node binary missing: {}", node.display());
        #[cfg(target_os = "linux")]
        {
            let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
            assert_eq!(
                std::fs::canonicalize(&exe).unwrap_or(exe),
                std::fs::canonicalize(&node).unwrap(),
                "death subject must be the real node binary"
            );
        }
        // The peer keeps this stopped group from becoming orphaned before the
        // owner dies. The death guard must terminate the entire Node group.
        let mut peer = std::process::Command::new("sleep")
            .arg("600")
            .process_group(pid)
            .spawn()
            .unwrap();
        let peer_pid = peer.id() as i32;
        assert_eq!(unsafe { libc::getpgid(peer_pid) }, pid);
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&state.stdout).contains('T') {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "node did not stop");
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            libc::kill(-(owner.id() as i32), libc::SIGKILL);
        }
        owner.wait().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut node_gone = pid_has_exited(pid);
        while !node_gone && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            node_gone = pid_has_exited(pid);
        }
        let descendant_deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut descendant_gone = pid_has_exited(descendant as i32);
        while !descendant_gone && std::time::Instant::now() < descendant_deadline {
            std::thread::sleep(Duration::from_millis(10));
            descendant_gone = pid_has_exited(descendant as i32);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while peer_is_running(&mut peer) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let peer_alive = peer_is_running(&mut peer);
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = peer.kill();
        let _ = peer.wait();
        std::fs::remove_file(pidfile).unwrap();
        std::fs::remove_file(descendant_file).unwrap();
        assert_eq!(
            group, pid,
            "fixture must reproduce Node's separate process group"
        );
        assert!(node_gone, "stopped Node outlived its force-killed parent");
        assert!(!peer_alive, "Node group member outlived its owner");
        assert!(descendant_gone, "Node subprocess outlived its owner");
    }

    /// Windows has no watcher process; its counterpart is
    /// `windows::tests::ticket_t_3_drain_job_object_kills_child_when_core_exits_at_spawn`.
    #[cfg(unix)]
    #[test]
    fn watcher_is_isolated_before_node_can_spawn() {
        let lease = std::fs::File::open("/dev/null").unwrap();
        for _ in 0..32 {
            let mut command = tokio::process::Command::new("true");
            let (read, guard) = super::platform::configure_child(&mut command, &lease).unwrap();
            let session = unsafe { libc::getsid(guard.watcher) };
            let watcher = guard.watcher;
            drop(read);
            drop(guard);
            assert_eq!(
                session, watcher,
                "Node may spawn only after watcher isolation"
            );
        }
    }

    use super::platform::{test_shell, test_support};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    struct AttemptOnDrop(Arc<AtomicBool>);

    impl Drop for AttemptOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn cancellation_dominates_an_observed_abnormal_exit() {
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();

        assert!(
            super::classify_stop_outcome(
                super::StopOutcome::Observed(test_support::crashed()),
                &cancelled,
            )
            .is_err()
        );
    }

    #[test]
    fn startup_sleep_cancellation_classifies_as_planned_shutdown() {
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();

        assert_eq!(
            super::classify_stop_outcome(
                super::StopOutcome::Observed(test_support::exited(0)),
                &cancelled,
            )
            .unwrap(),
            super::StopIntent::PlannedShutdown
        );
    }

    #[test]
    fn observed_abnormal_exit_without_cancellation_still_restarts() {
        let active = tokio_util::sync::CancellationToken::new();

        assert_eq!(
            super::classify_stop_outcome(
                super::StopOutcome::Observed(test_support::crashed()),
                &active,
            )
            .unwrap(),
            super::StopIntent::Restart
        );
    }

    #[test]
    fn forced_exit_without_cancellation_still_restarts() {
        let active = tokio_util::sync::CancellationToken::new();

        assert_eq!(
            super::classify_stop_outcome(
                super::StopOutcome::Forced(test_support::killed()),
                &active,
            )
            .unwrap(),
            super::StopIntent::Restart
        );
    }

    /// An endpoint on a fresh runtime directory whose channel is bound, so the
    /// stop under test has a channel to close.
    struct TestEndpoint {
        runtime_dir: std::path::PathBuf,
        listener: test_support::Listener,
        connections: Arc<super::Connections>,
        endpoint: Arc<super::Endpoint>,
    }

    fn test_endpoint(label: &str) -> TestEndpoint {
        let generation = format!("{label}-{:016x}", rand::random::<u64>());
        let runtime_dir =
            std::env::temp_dir().join(format!("pp-compat-{generation}-{}", std::process::id()));
        std::fs::create_dir(&runtime_dir).unwrap();
        let (channel, listener) = test_support::channel(&runtime_dir, &generation);
        let connections = Arc::new(super::Connections::new());
        let endpoint = Arc::new(super::Endpoint {
            channel: Arc::new(channel),
            generation,
            key: [0; 32],
            connections: connections.clone(),
        });
        TestEndpoint {
            runtime_dir,
            listener,
            connections,
            endpoint,
        }
    }

    /// A child and the guard that owns its group, as `Process` holds them.
    type Spawned = (
        super::platform::Child,
        Option<super::platform::ParentDeathGuard>,
    );

    /// When `exit` is set, close the child's stdin and reap it, as a stop
    /// that finds the leader already gone sees it.
    #[cfg(any(target_os = "linux", windows))]
    async fn observe_exit(
        child: &mut super::platform::Child,
        exit: bool,
    ) -> Option<(u32, std::process::ExitStatus)> {
        if !exit {
            return None;
        }
        let pid = child.id().unwrap();
        drop(child.stdin.take());
        Some((pid, child.wait().await.unwrap()))
    }

    /// Read the child's first line of output, waiting at most 5 s.
    async fn first_line(
        stdout: tokio::process::ChildStdout,
    ) -> (tokio::io::BufReader<tokio::process::ChildStdout>, String) {
        use tokio::io::AsyncBufReadExt;
        let mut stdout = tokio::io::BufReader::new(stdout);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        (stdout, line)
    }

    fn assert_forced_after_grace(outcome: &OwnedStop) {
        assert_eq!(outcome.intent, super::StopIntent::PlannedShutdown);
        assert_eq!(outcome.exit, super::CompatExit::Forced);
        assert!(
            outcome.elapsed >= Duration::from_secs(10) && outcome.elapsed < Duration::from_secs(13),
            "{:?}",
            outcome.elapsed
        );
        assert!(outcome.rejected);
        assert!(outcome.group_absent);
    }

    #[tokio::test]
    async fn process_stop_continues_cleanup_after_log_join_error() {
        let (mut child, parent_death) =
            test_support::spawn_group(&mut test_shell(test_support::EXITS));
        let pid = child.id().unwrap();
        let observed = child.wait().await.unwrap();
        assert!(observed.success());

        let TestEndpoint {
            runtime_dir,
            listener: _listener,
            connections,
            endpoint,
        } = test_endpoint("stop-regression");
        let channel = endpoint.channel.clone();
        test_support::channel_open(&channel).await.unwrap();

        let first_log = tokio::spawn(std::future::pending::<()>());
        first_log.abort();
        let later_attempted = Arc::new(AtomicBool::new(false));
        let later_release = tokio_util::sync::CancellationToken::new();
        let (later_started, started) = tokio::sync::oneshot::channel();
        let later_log = {
            let attempted = later_attempted.clone();
            let release = later_release.clone();
            tokio::spawn(async move {
                let _attempt = AttemptOnDrop(attempted);
                let _ = later_started.send(());
                release.cancelled().await;
            })
        };
        started.await.unwrap();

        let process = super::Process {
            child,
            pid,
            stdin: None,
            endpoint,
            logs: vec![first_log, later_log],
            parent_death,
        };
        let active = tokio_util::sync::CancellationToken::new();
        let error = process
            .stop(Some(observed), &active, true)
            .await
            .unwrap_err();

        let error_message = error.to_string();
        let later_cleanup_attempted = later_attempted.load(Ordering::SeqCst);
        let channel_closed = test_support::channel_open(&channel)
            .await
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        let process_group_absent = super::drain::process_group_absent(pid);
        let connections_closed = *connections.closed.lock().unwrap();

        later_release.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !later_attempted.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(&runtime_dir).unwrap();

        assert!(error_message.contains("Log reader join failed"));
        assert!(later_cleanup_attempted);
        assert!(channel_closed);
        assert!(process_group_absent);
        assert!(connections_closed);
    }

    #[test]
    fn planned_shutdown_accepts_term_or_forced_group_exit() {
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Observed(test_support::exited(0)),
                super::StopIntent::PlannedShutdown,
            )
            .is_ok()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Observed(test_support::killed()),
                super::StopIntent::PlannedShutdown,
            )
            .is_ok()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Observed(test_support::terminated()),
                super::StopIntent::PlannedShutdown,
            )
            .is_ok()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Observed(test_support::crashed()),
                super::StopIntent::PlannedShutdown,
            )
            .is_err()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Observed(test_support::exited(7)),
                super::StopIntent::PlannedShutdown,
            )
            .is_err()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Forced(test_support::killed()),
                super::StopIntent::PlannedShutdown,
            )
            .is_ok()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Forced(test_support::terminated()),
                super::StopIntent::PlannedShutdown,
            )
            .is_ok()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Forced(test_support::crashed()),
                super::StopIntent::PlannedShutdown,
            )
            .is_err()
        );
    }

    #[test]
    fn ticket_t_3_drain_planned_shutdown_accepts_sigterm_at_the_grace_boundary() {
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Forced(test_support::terminated()),
                super::StopIntent::PlannedShutdown,
            )
            .is_ok()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Forced(test_support::crashed()),
                super::StopIntent::PlannedShutdown,
            )
            .is_err()
        );
    }

    #[test]
    fn ticket_t_3_drain_forced_stop_outcome_reports_forced_exit() {
        for status in [
            test_support::killed(),
            test_support::terminated(),
            test_support::exited(0),
        ] {
            assert_eq!(
                super::StopOutcome::Forced(status).exit(),
                super::CompatExit::Forced
            );
            assert_eq!(
                super::StopOutcome::Observed(status).exit(),
                super::CompatExit::Graceful
            );
        }
    }

    #[test]
    fn unplanned_observed_exit_preserves_recovery_path() {
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Observed(test_support::crashed()),
                super::StopIntent::Restart,
            )
            .is_ok()
        );
        assert!(
            super::validate_stop_outcome(
                super::StopOutcome::Forced(test_support::killed()),
                super::StopIntent::Restart,
            )
            .is_ok()
        );
    }

    #[test]
    fn production_crash_guard_window() {
        assert_eq!(
            super::SupervisorConfig::default().crash_guard_window,
            std::time::Duration::from_secs(120)
        );
    }

    struct OwnedStop {
        intent: super::StopIntent,
        exit: super::CompatExit,
        elapsed: Duration,
        rejected: bool,
        group_absent: bool,
    }

    async fn stop_owned_child(child: Spawned, eof_watchdog: bool) -> OwnedStop {
        stop_owned_child_at(child, eof_watchdog, None).await
    }

    async fn stop_owned_child_at(
        child: Spawned,
        eof_watchdog: bool,
        cancel_after: Option<Duration>,
    ) -> OwnedStop {
        let cancelled = tokio_util::sync::CancellationToken::new();
        if let Some(delay) = cancel_after {
            let later = cancelled.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                later.cancel();
            });
        } else {
            cancelled.cancel();
        }
        stop_owned_child_with(child, eof_watchdog, None, cancelled).await
    }

    async fn stop_owned_child_with(
        (mut child, parent_death): Spawned,
        eof_watchdog: bool,
        observed: Option<(u32, std::process::ExitStatus)>,
        cancelled: tokio_util::sync::CancellationToken,
    ) -> OwnedStop {
        let pid = observed.map(|(pid, _)| pid).or_else(|| child.id()).unwrap();
        let stdin = child.stdin.take();
        let TestEndpoint {
            runtime_dir,
            listener: _listener,
            connections,
            endpoint,
            ..
        } = test_endpoint("t3-drain");
        let process = super::Process {
            child,
            pid,
            stdin,
            endpoint,
            logs: Vec::new(),
            parent_death,
        };
        let started = std::time::Instant::now();
        let (intent, exit) = process
            .stop(observed.map(|(_, status)| status), &cancelled, eof_watchdog)
            .await
            .unwrap();
        let rejected = connections.admit().is_err();
        let group_absent = super::drain::process_group_absent(pid);
        let _ = std::fs::remove_dir_all(&runtime_dir);
        OwnedStop {
            intent,
            exit,
            elapsed: started.elapsed(),
            rejected,
            group_absent,
        }
    }

    #[cfg(target_os = "linux")]
    const SUBREAPER_FIXTURE: &str =
        "tests::ticket_t_3_drain_leader_exit_with_forced_descendant_reports_forced";

    #[cfg(target_os = "linux")]
    fn run_subreaper_fixture(test_name: &str) -> std::process::Output {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--format=pretty", "--color=never"])
            .env("PP_COMPAT_SUBREAPER_FIXTURE", "1")
            .stderr(std::process::Stdio::inherit())
            .output()
            .unwrap()
    }

    #[cfg(target_os = "linux")]
    pub(super) fn validate_subreaper_fixture(
        output: &std::process::Output,
        test_name: &str,
    ) -> Result<(), &'static str> {
        if !output.status.success() {
            return Err("isolated subreaper fixture failed");
        }
        let stdout = std::str::from_utf8(&output.stdout)
            .map_err(|_| "isolated subreaper fixture output is not UTF-8")?;
        let lines: Vec<_> = stdout.lines().filter(|line| !line.is_empty()).collect();
        let expected = format!("test {test_name} ... ok");
        let invalid = "isolated subreaper fixture did not pass exactly once";
        let ["running 1 test", result, summary] = lines.as_slice() else {
            return Err(invalid);
        };
        if *result != expected {
            return Err(invalid);
        }
        let (filtered, _) = summary
            .strip_prefix("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; ")
            .and_then(|rest| rest.split_once(" filtered out; finished in "))
            .ok_or(invalid)?;
        filtered.parse::<usize>().map_err(|_| invalid)?;
        Ok(())
    }

    /// Checks the re-exec harness of the Linux-only subreaper fixture. Windows
    /// needs no re-exec because its job sees every descendant; see
    /// `ticket_t_3_drain_leader_exit_with_live_job_member_reports_forced`.
    #[cfg(target_os = "linux")]
    #[test]
    fn subreaper_reexec_runs_exactly_one_fixture() {
        let output = run_subreaper_fixture(SUBREAPER_FIXTURE);
        assert_eq!(
            validate_subreaper_fixture(&output, SUBREAPER_FIXTURE),
            Ok(()),
            "{output:?}"
        );
        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        for malformed in [
            stdout.repeat(2),
            stdout.replace(SUBREAPER_FIXTURE, "unrelated_fixture"),
            stdout.replace("1 passed;", "2 passed;"),
            stdout.replace("0 failed;", "1 failed;"),
        ] {
            let mut invalid = output.clone();
            invalid.stdout = malformed.into_bytes();
            assert_eq!(
                validate_subreaper_fixture(&invalid, SUBREAPER_FIXTURE),
                Err("isolated subreaper fixture did not pass exactly once")
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn subreaper_reexec_wrong_name_is_failure() {
        let output = run_subreaper_fixture(&format!("{SUBREAPER_FIXTURE}_missing"));
        assert_eq!(output.status.code(), Some(0));
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 0 tests"));
        assert_eq!(
            validate_subreaper_fixture(&output, SUBREAPER_FIXTURE),
            Err("isolated subreaper fixture did not pass exactly once"),
            "{output:?}"
        );
    }

    /// Windows counterpart: `ticket_t_3_drain_leader_exit_with_live_job_member_reports_forced`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn ticket_t_3_drain_leader_exit_with_forced_descendant_reports_forced() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        if std::env::var_os("PP_COMPAT_SUBREAPER_FIXTURE").is_none() {
            let output = run_subreaper_fixture(SUBREAPER_FIXTURE);
            assert_eq!(
                validate_subreaper_fixture(&output, SUBREAPER_FIXTURE),
                Ok(()),
                "{output:?}"
            );
            return;
        }
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);

        for (planned, already_exited, cancel_on_eof) in [
            (true, false, false),
            (true, true, false),
            (false, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let mut child = tokio::process::Command::new("python3")
                .arg("-c")
                .arg(
                    r#"
import os, signal, sys
read, write = os.pipe()
pid = os.fork()
if pid == 0:
    os.close(read)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.write(write, b'1')
    os.close(write)
    while True:
        signal.pause()
os.close(write)
os.read(read, 1)
os.close(read)
signal.signal(signal.SIGTERM, signal.SIG_DFL)
print(pid, flush=True)
sys.stdin.buffer.read()
if sys.argv[1] == "wait":
    print("eof", flush=True)
    signal.pause()
"#,
                )
                .arg(if cancel_on_eof { "wait" } else { "exit" })
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .process_group(0)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut line = String::new();
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let descendant: i32 = line.trim().parse().unwrap();
            let observed = observe_exit(&mut child, already_exited).await;
            let cancelled = tokio_util::sync::CancellationToken::new();
            if planned {
                cancelled.cancel();
            }
            let on_eof = cancelled.clone();
            let eof_task = tokio::spawn(async move {
                if cancel_on_eof {
                    let mut line = String::new();
                    tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(line, "eof\n");
                    on_eof.cancel();
                }
            });
            let during_cleanup = cancelled.clone();
            let reaper = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let mut status = 0;
                        let result =
                            unsafe { libc::waitpid(descendant, &mut status, libc::WNOHANG) };
                        if result == descendant {
                            during_cleanup.cancel();
                            return status;
                        }
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap()
            });
            let stopped = stop_owned_child_with((child, None), false, observed, cancelled).await;
            eof_task.await.unwrap();
            let status = reaper.await.unwrap();
            assert!(libc::WIFSIGNALED(status));
            assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
            assert!(stopped.group_absent);
            assert!(stopped.rejected);
            assert_eq!(stopped.intent, super::StopIntent::PlannedShutdown);
            assert_eq!(
                stopped.exit,
                super::CompatExit::Forced,
                "planned={planned}, already_exited={already_exited}"
            );
        }
    }

    /// Spawns [`test_support::COOPERATIVE`] and returns once it is ready to stop.
    async fn spawn_cooperative_child()
    -> (Spawned, tokio::io::BufReader<tokio::process::ChildStdout>) {
        let mut child = test_support::spawn_group(&mut test_shell(test_support::COOPERATIVE));
        let (stdout, line) = first_line(child.0.stdout.take().unwrap()).await;
        assert_eq!(line, "ready\n");
        (child, stdout)
    }

    /// Spawns [`test_support::STUBBORN`] and returns once it ignores stop requests.
    async fn spawn_stubborn_child() -> Spawned {
        let mut child = test_support::spawn_group(&mut test_shell(test_support::STUBBORN));
        let (stdout, line) = first_line(child.0.stdout.take().unwrap()).await;
        assert_eq!(line, "ready\n");
        child.0.stdout = Some(stdout.into_inner());
        child
    }

    #[tokio::test]
    async fn ticket_t_3_drain_process_stop_sigterms_cooperative_child() {
        let (child, _stdout) = spawn_cooperative_child().await;
        let outcome = stop_owned_child(child, true).await;
        assert_eq!(outcome.intent, super::StopIntent::PlannedShutdown);
        assert_eq!(outcome.exit, super::CompatExit::Graceful);
        assert!(
            outcome.elapsed < Duration::from_secs(3),
            "{:?}",
            outcome.elapsed
        );
        assert!(outcome.rejected);
        assert!(outcome.group_absent);
    }

    /// Windows counterpart: `ticket_t_3_drain_restart_cleanup_becomes_planned_stop_when_shutdown_starts`.
    #[cfg(unix)]
    #[tokio::test]
    async fn ticket_t_3_drain_restart_cleanup_switches_to_sigterm_when_shutdown_starts() {
        use tokio::io::AsyncBufReadExt;

        let (child, mut stdout) = spawn_cooperative_child().await;
        let outcome = stop_owned_child_at(child, false, Some(Duration::from_millis(100))).await;
        assert_eq!(outcome.intent, super::StopIntent::PlannedShutdown);
        assert_eq!(outcome.exit, super::CompatExit::Graceful);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line, "terminated\n");
        assert!(
            outcome.elapsed < Duration::from_secs(3),
            "{:?}",
            outcome.elapsed
        );
        assert!(outcome.rejected);
        assert!(outcome.group_absent);
    }

    /// Restart cleanup has already closed stdin, the only stop request on
    /// Windows, so a child that ignores it is terminated with its job once the
    /// grace runs out, and the stop still counts as a planned shutdown.
    #[cfg(windows)]
    #[tokio::test]
    async fn ticket_t_3_drain_restart_cleanup_becomes_planned_stop_when_shutdown_starts() {
        let child = spawn_stubborn_child().await;
        let outcome = stop_owned_child_at(child, false, Some(Duration::from_millis(100))).await;
        assert_forced_after_grace(&outcome);
    }

    #[tokio::test]
    async fn ticket_t_3_drain_process_stop_force_kills_after_sigterm_grace() {
        let child = spawn_stubborn_child().await;
        let outcome = stop_owned_child(child, false).await;
        assert_forced_after_grace(&outcome);
    }

    #[test]
    fn ticket_t_3_drain_disarmed_restarts_block_before_cancel() {
        let cancelled = tokio_util::sync::CancellationToken::new();
        let disarmed = tokio_util::sync::CancellationToken::new();
        assert!(!super::restarts_blocked(&cancelled, &disarmed));
        disarmed.cancel();
        assert!(super::restarts_blocked(&cancelled, &disarmed));
        assert!(!cancelled.is_cancelled());
        assert!(!super::startup_probe_publishes(true, true));
        assert!(super::startup_probe_publishes(false, true));
        assert!(!super::startup_probe_publishes(false, false));
    }

    #[tokio::test]
    async fn ticket_t_3_drain_ready_probe_returns_when_shutdown_cancels() {
        let TestEndpoint {
            runtime_dir,
            listener,
            endpoint,
            ..
        } = test_endpoint("t3-ready");
        let name = endpoint.channel.name().to_path_buf();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let held = tokio::spawn(async move {
            let stream = test_support::hold_connection(listener, &name).await;
            let _ = accepted_tx.send(());
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(stream);
        });
        let bundle = super::Bundle {
            node: std::path::PathBuf::from("node"),
            entry: std::path::PathBuf::from("entry"),
            web_root: runtime_dir.clone(),
            runtime_version: "0".to_owned(),
            commit: "0".to_owned(),
        };
        let cancelled = tokio_util::sync::CancellationToken::new();
        let probe_cancel = cancelled.clone();
        let probe = tokio::spawn(async move { endpoint.ready(&bundle, &probe_cancel).await });
        accepted_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = std::time::Instant::now();
        cancelled.cancel();
        let ready = probe.await.unwrap();
        let elapsed = started.elapsed();
        assert!(!ready);
        assert!(
            elapsed < Duration::from_millis(500),
            "health probe kept running after cancel: {elapsed:?}"
        );
        held.abort();
        let _ = std::fs::remove_dir_all(&runtime_dir);
    }

    /// Windows counterpart: `ticket_t_3_drain_reaps_job_when_leader_is_already_gone`.
    #[cfg(unix)]
    #[tokio::test]
    async fn ticket_t_3_drain_reaps_group_when_leader_wait_fails() {
        use std::os::unix::process::ExitStatusExt;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let cases = if cfg!(target_os = "macos") { 50 } else { 1 };
        for case in 0..cases {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("exec /bin/sleep 300")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0)
                .kill_on_drop(true);
            let mut child = command.spawn().unwrap();
            let pid = child.id().unwrap();
            let mut member_command = tokio::process::Command::new("/bin/sh");
            member_command
                .arg("-c")
                .arg("trap '' TERM; echo ready; exec /bin/sleep 300")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .process_group(pid as i32)
                .kill_on_drop(true);
            let mut member = member_command.spawn().unwrap();
            let member_pid = member.id().unwrap();
            let mut stdout = BufReader::new(member.stdout.take().unwrap());
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(line, "ready\n");
            assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
            assert_eq!(unsafe { libc::getpgid(member_pid as i32) }, pid as i32);
            assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
            loop {
                let collected = unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), 0) };
                if collected == pid as i32 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                assert_eq!(collected, -1);
                assert_eq!(error.raw_os_error(), Some(libc::EINTR), "{error}");
            }
            let started = std::time::Instant::now();
            let joined = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(
                    super::drain::drive_shutdown_group(
                        &mut child,
                        pid,
                        false,
                        Duration::from_secs(2),
                        None,
                    ),
                    member.wait()
                )
            })
            .await;
            let (result, member_status) = match joined {
                Ok(outcomes) => outcomes,
                Err(error) => {
                    let snapshot = group_snapshot(pid);
                    let _ = member.kill().await;
                    let _ = member.wait().await;
                    panic!("case {case} timed out before cleanup: {error}\n{snapshot}");
                }
            };
            assert_eq!(member_status.unwrap().signal(), Some(libc::SIGKILL));
            let error = result
                .as_ref()
                .expect_err("leader wait was expected to fail");
            assert_eq!(error.to_string(), "Child reap failed");
            assert!(
                error.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|cause| cause.raw_os_error() == Some(libc::ECHILD))
                }),
                "{error:#}"
            );
            let group_probe = unsafe { libc::kill(-(pid as i32), 0) };
            let group_probe_error = if group_probe < 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            };
            assert!(
                group_probe < 0
                    && group_probe_error
                        .as_ref()
                        .is_some_and(|error| error.raw_os_error() == Some(libc::ESRCH)),
                "group survived a failed leader wait: pgid={pid}, kill(-pgid, 0)={group_probe}, errno={:?}, error={:?}, drain result={result:#?}\n{}",
                group_probe_error
                    .as_ref()
                    .and_then(|error| error.raw_os_error()),
                group_probe_error.as_ref().map(|error| error.to_string()),
                group_snapshot(pid)
            );
            let elapsed = started.elapsed();
            assert!(elapsed < Duration::from_secs(2), "case {case}: {elapsed:?}");
        }
        eprintln!("completed {cases} failed-leader-wait process-group cases");
    }

    /// Starts a job member that exits once the file named by its argument
    /// exists, prints the member's PID, and exits when its stdin closes.
    #[cfg(windows)]
    const SPAWN_MEMBER: &str = concat!(
        "const {spawn}=require('node:child_process'); ",
        "const member=spawn(process.execPath, ['-e', ",
        "\"const fs=require('node:fs'); console.log('up'); setInterval(() => { if (fs.existsSync(process.argv[1])) process.exit(0); }, 20)\", ",
        "process.argv[1]], {stdio: ['ignore', 'pipe', 'ignore'], detached: true}); member.unref(); ",
        "member.stdout.once('data', () => { member.stdout.destroy(); console.log(member.pid); }); ",
        "process.stdin.on('end', () => process.exit(0)); process.stdin.resume();",
    );

    #[cfg(windows)]
    async fn spawn_with_member(release: &std::path::Path) -> (Spawned, u32) {
        let mut command = test_shell(SPAWN_MEMBER);
        command.arg(release);
        let mut spawned = test_support::spawn_group(&mut command);
        let (_, line) = first_line(spawned.0.stdout.take().unwrap()).await;
        (spawned, line.trim().parse().unwrap())
    }

    /// Whether `pid` exits within 5 s. A job reports no active process as soon
    /// as its members start terminating; each process exits a moment later.
    #[cfg(windows)]
    async fn exits(pid: u32) -> bool {
        tokio::time::timeout(Duration::from_secs(5), async {
            while super::drain::process_alive(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok()
    }

    #[cfg(windows)]
    fn member_release(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "pp-compat-{label}-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    /// A job member that outlives its leader keeps the job alive, so the stop
    /// terminates the job and reports a forced exit.
    #[cfg(windows)]
    #[tokio::test]
    async fn ticket_t_3_drain_leader_exit_with_live_job_member_reports_forced() {
        for (planned, already_exited) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let ((mut child, guard), member) = spawn_with_member(&member_release("leader")).await;
            let observed = observe_exit(&mut child, already_exited).await;
            let cancelled = tokio_util::sync::CancellationToken::new();
            if planned {
                cancelled.cancel();
            }
            let stopped = stop_owned_child_with((child, guard), true, observed, cancelled).await;
            assert!(exits(member).await, "job member outlived the stop");
            assert!(stopped.group_absent);
            assert!(stopped.rejected);
            assert_eq!(
                stopped.intent,
                if planned {
                    super::StopIntent::PlannedShutdown
                } else {
                    super::StopIntent::Restart
                }
            );
            assert_eq!(
                stopped.exit,
                super::CompatExit::Forced,
                "planned={planned}, already_exited={already_exited}"
            );
        }
    }

    /// A job member that exits on its own shortly after its leader exits
    /// cleanly, like the conhost.exe of Node's hidden console, does not turn the
    /// clean exit into a forced one: the drain waits for the job to empty.
    #[cfg(windows)]
    #[tokio::test]
    async fn ticket_t_3_drain_leader_exit_with_short_lived_job_member_is_graceful() {
        for (planned, already_exited) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let release = member_release("settle");
            let ((mut child, guard), member) = spawn_with_member(&release).await;
            let leader = child.id().unwrap();
            let releaser = tokio::spawn({
                let release = release.clone();
                async move {
                    assert!(exits(leader).await, "leader did not exit on EOF");
                    std::fs::write(&release, b"").unwrap();
                }
            });
            let observed = observe_exit(&mut child, already_exited).await;
            let cancelled = tokio_util::sync::CancellationToken::new();
            if planned {
                cancelled.cancel();
            }
            let stopped = stop_owned_child_with((child, guard), true, observed, cancelled).await;
            releaser.await.unwrap();
            assert!(exits(member).await, "job member outlived the stop");
            assert!(stopped.group_absent);
            assert!(stopped.rejected);
            assert_eq!(
                stopped.exit,
                super::CompatExit::Graceful,
                "planned={planned}, already_exited={already_exited}"
            );
            assert!(
                stopped.elapsed < Duration::from_secs(3),
                "{:?}",
                stopped.elapsed
            );
            let _ = std::fs::remove_file(&release);
        }
    }

    #[cfg(windows)]
    #[test]
    fn scratch_stress_clean_exit_reporting() {
        for _ in 0..200 {
            ticket_t_3_drain_process_stop_sigterms_cooperative_child();
        }
        for _ in 0..25 {
            ticket_t_3_drain_leader_exit_with_short_lived_job_member_is_graceful();
        }
        for _ in 0..5 {
            ticket_t_3_drain_leader_exit_with_live_job_member_reports_forced();
        }
    }

    /// A leader that is already gone when the drain starts does not keep the
    /// drain from terminating the rest of its job.
    #[cfg(windows)]
    #[tokio::test]
    async fn ticket_t_3_drain_reaps_job_when_leader_is_already_gone() {
        let ((mut child, _guard), member) = spawn_with_member(&member_release("gone")).await;
        let pid = child.id().unwrap();
        child.start_kill().unwrap();
        let status = child.wait().await.unwrap();
        let started = std::time::Instant::now();
        let report = super::drain::drive_shutdown_group(
            &mut child,
            pid,
            false,
            Duration::from_secs(2),
            Some(status),
        )
        .await
        .unwrap();
        assert!(matches!(report.exit, super::drain::GroupExit::Forced(_)));
        assert!(report.group_absent);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(exits(member).await);
    }

    /// Windows counterpart: `ticket_t_3_drain_job_remains_until_every_member_exits`.
    #[cfg(target_os = "macos")]
    #[test]
    fn ticket_t_3_drain_zombie_group_remains_until_owned_child_is_reaped() {
        use std::os::unix::process::CommandExt;

        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        let observed = (|| -> std::io::Result<_> {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                    )
                };
                if result < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EINTR) {
                        return Err(error);
                    }
                } else if info.si_pid == pid as i32 {
                    let probe = unsafe { libc::kill(-(pid as i32), 0) };
                    let errno = std::io::Error::last_os_error().raw_os_error();
                    let absent = super::drain::process_group_absent(pid);
                    return Ok((probe, errno, absent));
                } else if info.si_pid != 0 {
                    return Err(std::io::Error::other(format!(
                        "waitid returned PID {} instead of {pid}",
                        info.si_pid
                    )));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "owned child did not become an observable zombie",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        if observed.is_err() {
            let _ = child.kill();
        }
        let status = child.wait();
        let (probe, errno, absent) = observed.unwrap();
        assert!(status.unwrap().success());
        assert_eq!(probe, -1);
        assert_eq!(errno, Some(libc::EPERM));
        assert!(!absent);
        let probe = unsafe { libc::kill(-(pid as i32), 0) };
        let errno = std::io::Error::last_os_error().raw_os_error();
        assert_eq!(probe, -1);
        assert_eq!(errno, Some(libc::ESRCH));
        assert!(super::drain::process_group_absent(pid));
    }

    /// The job counts as present until every member has exited, not only the
    /// leader.
    #[cfg(windows)]
    #[tokio::test]
    async fn ticket_t_3_drain_job_remains_until_every_member_exits() {
        let release = member_release("remains");
        let ((mut child, _guard), member) = spawn_with_member(&release).await;
        let pid = child.id().unwrap();
        drop(child.stdin.take());
        assert!(child.wait().await.unwrap().success());
        assert!(super::drain::process_alive(member));
        assert!(!super::drain::process_group_absent(pid));
        std::fs::write(&release, b"").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !super::drain::process_group_absent(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(exits(member).await);
        std::fs::remove_file(&release).unwrap();
    }

    #[test]
    fn product_backoff_schedule() {
        assert_eq!(
            (0..8)
                .map(|n| super::backoff(n).as_millis())
                .collect::<Vec<_>>(),
            vec![500, 1000, 2000, 4000, 8000, 30000, 30000, 30000]
        );
    }
}
