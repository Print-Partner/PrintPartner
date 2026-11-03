#[cfg(feature = "desktop_rebuild")]
pub mod production;

use anyhow::{Context, Result, bail};
use pp_compat::{Bundle, CompatHandle, MeasuredBundle, SpawnSpec, Supervisor};
use pp_gateway::ownership::{
    cutover::{
        self, CutoverPlan, CutoverReceipt, CutoverRefusal, DrainEvidence, Evidence, OperationOwners,
    },
    manifest::{OwnershipIndex, admit_bundled_manifest},
};
use pp_gateway::{Gateway, LaunchTarget};
use pp_storage::lease::StorageLease;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
mod desktop_settings;
mod drain;
mod origin;
pub mod perf_launch;
pub mod perf_seed;
mod preferences;
mod rebuild;
mod state_file;

pub use desktop_settings::{DesktopSetting, DesktopSettings};
pub use origin::OriginChange;
use origin::bind_origin;
pub use preferences::{BrowserPreference, BrowserPreferences, PrinterPollInterval, Theme};

#[cfg(feature = "desktop_rebuild")]
pub mod layout;

#[cfg(feature = "desktop_rebuild")]
pub mod profile_interpreter;

#[cfg(feature = "desktop_rebuild")]
pub mod review_observations;

#[cfg(feature = "desktop_rebuild")]
pub mod rebuild_api;

#[cfg(feature = "desktop_rebuild")]
pub mod source_acquisition;

#[cfg(feature = "desktop_rebuild")]
pub mod uploads;

#[cfg(test)]
mod origin_tests;

pub use rebuild::DesktopRebuild;

pub use drain::{ShutdownGate, TrackedShutdownWork};
pub use pp_compat::drain::{
    GroupStopReport, SIGTERM_GRACE, drive_shutdown_group, process_alive, process_group_absent,
    reap_process_group, reject_and_arm_eof, restarts_allowed,
};
pub use pp_gateway::MUTATION_DRAIN;

pub struct DesktopLaunch {
    pub data_dir: PathBuf,
    pub bundle: Bundle,
    pub assets: PathBuf,
}

pub use pp_compat::SupervisorConfig;
pub use pp_compat::{Bundle as VerifiedBundle, Status as CoreStatus};
/// The shared scrub policy for log files, the Logs view and opt-in Sentry.
pub use pp_compat::{CoreLog, redact};
pub use pp_compat::{log_folder, recent_diagnostics, recent_logs};
pub use pp_gateway::LaunchTarget as DesktopLaunchTarget;

pub struct CoreHandle {
    compat: CompatHandle,
    monitoring: pp_gateway::MonitoringControl,
    bootstrap: pp_gateway::BootstrapMinter,
    gate: ShutdownGate,
    preferences: preferences::Client,
}
impl Clone for CoreHandle {
    fn clone(&self) -> Self {
        Self {
            compat: self.compat.clone(),
            monitoring: self.monitoring.clone(),
            bootstrap: self.bootstrap.clone(),
            gate: self.gate.clone(),
            preferences: self.preferences.clone(),
        }
    }
}
impl CoreHandle {
    pub async fn browser_preferences(&self) -> Result<BrowserPreferences> {
        self.preferences.read().await
    }

    pub async fn set_browser_preference(
        &self,
        field: BrowserPreference,
    ) -> Result<BrowserPreferences> {
        self.preferences.update(field).await
    }

    /// Desktop settings apply the next time Print Partner starts.
    pub async fn desktop_settings(&self) -> Result<DesktopSettings> {
        self.preferences.read_desktop().await
    }

    pub async fn set_desktop_setting(&self, field: DesktopSetting) -> Result<DesktopSettings> {
        self.preferences.update_desktop(field).await
    }

    /// Power adapters can cancel/join status sockets before acknowledging sleep.
    /// Hide-to-tray deliberately leaves monitoring running.
    pub async fn suspend_monitoring(&self) {
        self.monitoring.suspend().await;
    }
    pub fn resume_monitoring(&self) {
        self.monitoring.resume();
    }

    /// A fresh single-use 60-second bootstrap URL for the default browser.
    /// Fails once the runtime starts stopping.
    pub fn browser_launch_target(&self) -> Result<DesktopLaunchTarget> {
        self.bootstrap.mint()
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<pp_compat::Status> {
        self.compat.status.clone()
    }
    pub async fn recover_compat(&self) -> Result<()> {
        self.compat.recover().await
    }

    pub fn track_shutdown_work(&self) -> Result<TrackedShutdownWork, &'static str> {
        self.gate.track()
    }
}

pub struct CoreRuntime {
    desktop_rebuild: DesktopRebuild,
    rust_only: bool,
    stop: Option<tokio::sync::oneshot::Sender<Option<CutoverPlan>>>,
    completion: Option<tokio::sync::oneshot::Receiver<(ShutdownReceipt, Option<CutoverReceipt>)>>,
    ownership: Option<Arc<OwnershipIndex>>,
    owners: OperationOwners,
    data_dir: PathBuf,
    evidence_unsaved: bool,
    worker: Option<std::thread::JoinHandle<()>>,
    origin: String,
    runtime_dir: PathBuf,
    origin_change: Option<OriginChange>,
    handle: CoreHandle,
    launch: Option<LaunchTarget>,
    shutdown_requested: pp_gateway::ShutdownRequested,
}
struct Resources {
    owners: OperationOwners,
    origin_change: Option<OriginChange>,
    gateway: Gateway,
    compat: CompatHandle,
    backend: Backend,
    gate: ShutdownGate,
}

/// Who owns the data directory's operations for this run.
enum Backend {
    /// The supervised Node child, with the lease pp-core holds for it.
    Node {
        supervisor: Supervisor,
        owner: StorageLease,
    },
    /// Rust only: no Node child. The writer holds the lease.
    #[cfg(feature = "desktop_rebuild")]
    Rust(Arc<std::sync::Mutex<pp_storage::WriterOwner>>),
}

impl Resources {
    /// `None` only once the Rust writer has shut down.
    fn with_lease<R>(&self, use_lease: impl FnOnce(&StorageLease) -> R) -> Option<R> {
        match &self.backend {
            Backend::Node { owner, .. } => Some(use_lease(owner)),
            #[cfg(feature = "desktop_rebuild")]
            Backend::Rust(writer) => writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .lease()
                .map(use_lease),
        }
    }
}

/// Which runtimes a start may choose between.
#[derive(Clone, Copy)]
enum Selection {
    /// Always the Node-backed runtime.
    Node,
    /// The shipped launchers: Rust only when both rebuild gates are on.
    Launcher,
}

#[derive(Serialize, Debug)]
pub struct ShutdownReceipt {
    pub proof_class: &'static str,
    pub tracked_idle: bool,
    pub gateway_idle: bool,
    pub compat_exit: Option<pp_compat::CompatExit>,
    pub compat_reaped: bool,
    pub storage_released: bool,
    pub gateway_stopped: bool,
    /// Removal of the standalone Node compatibility reservation.
    pub marker_removed: bool,
    pub runtime_removed: bool,
    pub errors: Vec<&'static str>,
}

impl ShutdownReceipt {
    /// Before storage release, with the drain timeouts recorded.
    fn drained(
        tracked_idle: bool,
        gateway_idle: bool,
        compat_exit: Option<pp_compat::CompatExit>,
        gateway_stopped: bool,
    ) -> Self {
        let mut receipt = Self {
            proof_class: "headless_unsigned",
            tracked_idle,
            gateway_idle,
            compat_exit,
            compat_reaped: compat_exit.is_some(),
            gateway_stopped,
            storage_released: false,
            marker_removed: false,
            runtime_removed: false,
            errors: Vec::new(),
        };
        if !tracked_idle {
            receipt.errors.push("tracked_drain_timed_out");
        }
        if !gateway_idle {
            receipt.errors.push("gateway_drain_timed_out");
        }
        receipt
    }

    fn failed(category: &'static str) -> Self {
        Self {
            proof_class: "headless_unsigned",
            tracked_idle: false,
            gateway_idle: false,
            compat_exit: None,
            compat_reaped: false,
            storage_released: false,
            gateway_stopped: false,
            marker_removed: false,
            runtime_removed: false,
            errors: vec![category],
        }
    }
    pub fn complete(&self) -> bool {
        self.tracked_idle
            && self.gateway_idle
            && self.compat_reaped
            && self.storage_released
            && self.gateway_stopped
            && self.errors.is_empty()
    }
}

async fn start_resources(
    input: DesktopLaunch,
    compat_config: SupervisorConfig,
    ownership: Option<&OwnershipIndex>,
    desktop_rebuild: DesktopRebuild,
) -> Result<Resources> {
    let (assets, bundle, data_dir) = admit_release(input).await?;
    let mut owner = StorageLease::acquire(&data_dir)?;
    let account_pin =
        pp_compat::DesktopAccountPin::capture(owner.data_dir(), desktop_rebuild.enabled())?;
    let (owners, boot) = cutover::recover(owner.data_dir(), ownership)
        .map_err(|error| anyhow::anyhow!("Operation cutover recovery failed: {error:?}"))?;
    if !boot.evidence_saved {
        eprintln!("cutover_evidence_unsaved");
        tracing::warn!("cutover_evidence_unsaved");
    }
    pp_gateway::ownership::boot_bundled_source_sync(owner.data_dir())
        .map_err(|error| anyhow::anyhow!("Source Sync ownership admission failed: {error}"))?;
    let (listener, origin_change) = bind_origin(&owner).await?;
    // An unreadable settings file keeps the app starting without those settings. The
    // file stays untouched, and CoreHandle::desktop_settings reports the error.
    let settings = desktop_settings::read(&owner)
        .map(|settings| settings.env())
        .unwrap_or_else(|error| {
            tracing::warn!(error = %error, "desktop_settings_unreadable");
            Vec::new()
        });
    let supervisor = Supervisor::start_with_config(
        SpawnSpec {
            account_pin,
            bundle,
            data_dir: owner.data_dir().to_owned(),
            runtime_dir: owner.runtime_dir().to_owned(),
            storage_lock: owner.clone_file()?,
            port: listener.local_addr()?.port(),
            gateway_rate_limit: owners.owner_of("http.GET /legal/summary")
                == pp_gateway::ownership::Owner::Rust,
            settings,
        },
        compat_config,
    )?;
    owner.retain_on_drop();
    let mut status = supervisor.handle.status.clone();
    // Release verification finished above, so this budget covers only child readiness.
    let ready = tokio::time::timeout(Duration::from_secs(32), async {
        loop {
            let current = status.borrow_and_update().clone();
            match current {
                CoreStatus::Ready { .. } => return Ok(()),
                CoreStatus::Guarded | CoreStatus::Stopped | CoreStatus::Failed => {
                    bail!("Compatibility child stopped before readiness: {current:?}");
                }
                CoreStatus::Starting | CoreStatus::Backoff { .. } | CoreStatus::Stopping => {}
            }
            status
                .changed()
                .await
                .context("Compatibility supervisor exited before readiness")?;
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(anyhow::anyhow!(
            "Compatibility readiness timed out after 32s; last status {:?}",
            *status.borrow()
        ))
    });
    if let Err(error) = ready {
        if supervisor.shutdown().await.is_ok() {
            owner.allow_drop_cleanup();
        } else {
            owner.retain_until_process_exit();
        }
        return Err(error);
    }
    let compat = supervisor.handle.clone();
    let gateway = match Gateway::start(listener, assets, compat.clone(), &owners) {
        Ok(gateway) => gateway,
        Err(error) => {
            if supervisor.shutdown().await.is_ok() {
                owner.allow_drop_cleanup();
            } else {
                owner.retain_until_process_exit();
            }
            return Err(error);
        }
    };
    Ok(Resources {
        owners,
        origin_change,
        gateway,
        compat,
        backend: Backend::Node { supervisor, owner },
        gate: ShutdownGate::new(),
    })
}

/// Verify the release and prepare the data directory, retrying a failed
/// release first. Returns the canonical assets, the measured bundle and the
/// prepared directory.
async fn admit_release(input: DesktopLaunch) -> Result<(PathBuf, MeasuredBundle, PathBuf)> {
    let assets = input
        .assets
        .canonicalize()
        .context("React assets unavailable")?;
    // Measure the release once, off the runtime and before the readiness budget starts.
    let verified_assets = assets.clone();
    let bundle = tokio::task::spawn_blocking(move || {
        pp_compat::verify_bundle(input.bundle, &verified_assets)
    })
    .await
    .context("Release verification task failed")??;
    let data_dir = pp_storage::lease::prepare_directory(&input.data_dir)?;
    if let Some(released) = StorageLease::retry_failed_release_if_present(&data_dir)? {
        anyhow::ensure!(
            released.marker_removed
                && released.runtime_removed
                && released.lock_released
                && !released.ownership_retained,
            "Storage cleanup still incomplete; ownership retained"
        );
    }
    Ok((assets, bundle, data_dir))
}

/// Start without a Node child: the Rust writer owns the data directory and the
/// gateway answers admitted requests from the `pp-api` routers. Use disposable
/// directories only; refusing real ones is separate work.
#[cfg(feature = "desktop_rebuild")]
async fn start_rust_resources(
    input: DesktopLaunch,
    desktop_rebuild: DesktopRebuild,
) -> Result<Resources> {
    let (assets, bundle, data_dir) = admit_release(input).await?;
    let community_manifests = bundle.community_manifests_dir();
    let writer = tokio::task::spawn_blocking(move || {
        pp_storage::WriterOwner::open_with_job_feature_admission(
            &data_dir,
            pp_storage::Limits::default(),
        )
    })
    .await
    .context("Storage open task failed")??
    .0;
    let (listener, origin_change) =
        bind_origin(writer.lease().context("Storage lease unavailable")?).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let writer = Arc::new(std::sync::Mutex::new(writer));
    let product = rebuild_api::product_router(
        desktop_rebuild,
        Arc::clone(&writer),
        &origin,
        community_manifests.as_deref(),
    )?
    .context("Rust product routes need the rebuild setting")?;
    let owners = OperationOwners::node_only();
    let compat = CompatHandle::unpublished();
    let gateway =
        Gateway::start_with_product(listener, assets, compat.clone(), &owners, Some(product))?;
    Ok(Resources {
        owners,
        origin_change,
        gateway,
        compat,
        backend: Backend::Rust(writer),
        gate: ShutdownGate::new(),
    })
}

impl CoreRuntime {
    /// Start the Node-backed runtime, whatever the rebuild setting.
    pub async fn start(input: DesktopLaunch) -> Result<Self> {
        Self::start_with_compat_config(input, SupervisorConfig::default()).await
    }

    /// The start `pp-server` and `pp-desktop` use. With the `desktop_rebuild`
    /// feature compiled in and `PP_DESKTOP_REBUILD` exactly `1` or `true`, no
    /// Node child starts: the Rust writer owns the data directory and the
    /// gateway serves the `pp-api` routers. Otherwise this is [`Self::start`].
    pub async fn start_for_launcher(input: DesktopLaunch) -> Result<Self> {
        Self::start_selected(input, SupervisorConfig::default(), Selection::Launcher).await
    }

    /// Start with a per-instance compatibility timing policy.
    pub async fn start_with_compat_config(
        input: DesktopLaunch,
        compat_config: SupervisorConfig,
    ) -> Result<Self> {
        Self::start_selected(input, compat_config, Selection::Node).await
    }

    async fn start_selected(
        input: DesktopLaunch,
        compat_config: SupervisorConfig,
        selection: Selection,
    ) -> Result<Self> {
        let desktop_rebuild = DesktopRebuild::from_env();
        let rust_only = matches!(selection, Selection::Launcher) && desktop_rebuild.enabled();
        let ownership = (desktop_rebuild.enabled() && !rust_only)
            .then(admit_bundled_manifest)
            .transpose()
            .map_err(|error| anyhow::anyhow!("Operation ownership admission failed: {error}"))?
            .map(Arc::new);
        let worker_ownership = ownership.clone();
        let (started, startup) = tokio::sync::oneshot::channel();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let (completed, completion) = tokio::sync::oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("pp-core-owner".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = started.send(Err(anyhow::Error::from(error)));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let resources = if rust_only {
                        #[cfg(feature = "desktop_rebuild")]
                        { start_rust_resources(input, desktop_rebuild).await }
                        #[cfg(not(feature = "desktop_rebuild"))]
                        { unreachable!("Rust-only runtime requires desktop_rebuild") }
                    } else {
                        start_resources(input, compat_config, worker_ownership.as_deref(), desktop_rebuild).await
                    };
                    let mut resources = match resources {
                        Ok(resources) => resources,
                        Err(error) => {
                            let _ = started.send(Err(error));
                            return;
                        }
                    };
                    let requested_by_gateway = resources.gateway.shutdown_requested();
                    let requested_by_adapter = requested_by_gateway.clone();
                    let (preferences, mut commands) = preferences::Client::channel();
                    let directories = resources.with_lease(|lease| {
                        (lease.data_dir().to_owned(), lease.runtime_dir().to_owned())
                    });
                    let public = resources.gateway.take_launch_target().and_then(|launch| {
                        let (data_dir, runtime_dir) =
                            directories.context("Storage lease unavailable")?;
                        Ok((
                            resources.gateway.origin().to_owned(),
                            CoreHandle {
                                compat: resources.compat.clone(),
                                monitoring: resources.gateway.monitoring_control(),
                                bootstrap: resources.gateway.bootstrap_minter(),
                                gate: resources.gate.clone(),
                                preferences,
                            },
                            launch,
                            requested_by_adapter,
                            resources.origin_change.clone(),
                            resources.owners.clone(),
                            data_dir,
                            runtime_dir,
                        ))
                    });
                    let mut plan = None;
                    if started.send(public).is_ok() {
                        let gateway_stopped = requested_by_gateway.wait();
                        tokio::pin!(gateway_stopped);
                        loop {
                            tokio::select! {
                                biased;
                                received = &mut stopped => { plan = received.ok().flatten(); break; },
                                _ = &mut gateway_stopped => {
                                    stopped.close();
                                    plan = stopped.try_recv().ok().flatten();
                                    break;
                                },
                                command = commands.recv() => match command {
                                    Some(command) => {
                                        resources.with_lease(|lease| command.execute(lease));
                                    }
                                    None => break,
                                }
                            }
                        }
                    }
                    preferences::settle(&mut commands, |command| {
                        resources.with_lease(|lease| command.execute(lease));
                    })
                    .await;
                    let receipt = stop_resources(resources, plan).await;
                    let _ = completed.send(receipt);
                });
            })
            .context("Core owner thread unavailable")?;
        let (
            origin,
            handle,
            launch,
            shutdown_requested,
            origin_change,
            owners,
            data_dir,
            runtime_dir,
        ) = startup.await.context("Core owner startup failed")??;
        Ok(Self {
            desktop_rebuild,
            rust_only,
            ownership,
            owners,
            data_dir,
            evidence_unsaved: false,
            stop: Some(stop),
            completion: Some(completion),
            worker: Some(worker),
            origin,
            runtime_dir,
            origin_change,
            handle,
            launch: Some(launch),
            shutdown_requested,
        })
    }
    pub fn desktop_rebuild(&self) -> DesktopRebuild {
        self.desktop_rebuild
    }

    /// True when no Node child runs and Rust owns the data directory.
    pub fn rust_only(&self) -> bool {
        self.rust_only
    }

    pub fn origin_change(&self) -> Option<&OriginChange> {
        self.origin_change.as_ref()
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn handle(&self) -> CoreHandle {
        self.handle.clone()
    }
    pub fn take_launch_target(&mut self) -> Result<LaunchTarget> {
        self.launch
            .take()
            .context("Launch target already transferred")
    }
    pub fn shutdown_requested(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let requested = self.shutdown_requested.clone();
        async move { requested.wait().await }
    }
    pub async fn shutdown(mut self) -> ShutdownReceipt {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(None);
        }
        self.finish_shutdown().await.0
    }

    pub async fn shutdown_with_cutover(
        mut self,
        operation: &str,
        parity: &str,
    ) -> Result<(ShutdownReceipt, CutoverReceipt), Box<(Self, CutoverRefusal)>> {
        let planned = cutover::plan(self.ownership.as_deref(), &self.owners, operation, parity);
        let refusal = match planned {
            Err(refusal) => refusal,
            Ok(plan) => {
                if self
                    .stop
                    .take()
                    .is_some_and(|stop| stop.send(Some(plan)).is_ok())
                {
                    let (mut shutdown, cutover) = self.finish_shutdown().await;
                    let cutover = cutover.unwrap_or_else(|| {
                        let unproved = CutoverReceipt::CommitUnproved {
                            operation: operation.to_owned(),
                        };
                        if cutover::append_evidence(&self.data_dir, &Evidence::Cutover(&unproved))
                            .is_err()
                        {
                            shutdown.errors.push("cutover_evidence_unsaved");
                        }
                        unproved
                    });
                    return Ok((shutdown, cutover));
                }
                CutoverRefusal::ShutdownInProgress
            }
        };
        if cutover::append_evidence(
            &self.data_dir,
            &Evidence::Refused {
                operation,
                refusal: &refusal,
            },
        )
        .is_err()
        {
            self.evidence_unsaved = true;
        }
        Err(Box::new((self, refusal)))
    }

    async fn finish_shutdown(&mut self) -> (ShutdownReceipt, Option<CutoverReceipt>) {
        let (mut receipt, cutover) =
            match self.completion.take().expect("Completion consumed").await {
                Ok(receipt) => receipt,
                Err(_) => (ShutdownReceipt::failed("core_owner_failed"), None),
            };
        if self.evidence_unsaved {
            receipt.errors.push("cutover_evidence_unsaved");
        }
        if let Some(worker) = self.worker.take()
            && !matches!(
                tokio::task::spawn_blocking(move || worker.join()).await,
                Ok(Ok(()))
            )
        {
            receipt.errors.push("core_owner_join_failed");
        }
        (receipt, cutover)
    }
}

async fn stop_resources(
    resources: Resources,
    plan: Option<CutoverPlan>,
) -> (ShutdownReceipt, Option<CutoverReceipt>) {
    let Resources {
        gateway,
        gate,
        backend,
        ..
    } = resources;
    match backend {
        Backend::Node { supervisor, owner } => {
            stop_node(gateway, gate, supervisor, owner, plan).await
        }
        #[cfg(feature = "desktop_rebuild")]
        Backend::Rust(writer) => (stop_rust(gateway, gate, writer).await, None),
    }
}

/// No child to reap, so `compat_reaped` holds trivially. Storage is released
/// by shutting the writer down once the gateway has dropped its routes.
#[cfg(feature = "desktop_rebuild")]
async fn stop_rust(
    gateway: Gateway,
    gate: ShutdownGate,
    writer: Arc<std::sync::Mutex<pp_storage::WriterOwner>>,
) -> ShutdownReceipt {
    let (tracked_idle, gateway_idle) = drain::reject_and_wait(
        MUTATION_DRAIN,
        &gate,
        || gateway.begin_drain(),
        |bound| gateway.wait_idle(bound),
        || {},
    )
    .await;
    gateway.finish_drain();
    let gateway_stopped = gateway.stop().await.is_ok();
    let mut receipt = ShutdownReceipt::drained(tracked_idle, gateway_idle, None, gateway_stopped);
    receipt.compat_reaped = true;
    if !gateway_stopped {
        receipt.errors.push("gateway_join_failed");
    }
    if !tracked_idle {
        // Tracked work may still use storage: keep the lock until process exit.
        std::mem::forget(writer);
        receipt.errors.push("storage_ownership_retained");
        return receipt;
    }
    let released = match Arc::try_unwrap(writer) {
        Ok(writer) => tokio::task::spawn_blocking(move || {
            writer
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .shutdown()
                .is_ok()
        })
        .await
        .unwrap_or(false),
        Err(_) => {
            receipt.errors.push("storage_owner_shared");
            false
        }
    };
    if !released {
        receipt.errors.push("storage_ownership_retained");
    }
    // `WriterOwner::shutdown` succeeds only after the lease removed its
    // marker and runtime directory and unlocked.
    receipt.storage_released = released;
    receipt.marker_removed = released;
    receipt.runtime_removed = released;
    receipt
}

async fn stop_node(
    gateway: Gateway,
    gate: ShutdownGate,
    supervisor: Supervisor,
    owner: StorageLease,
    plan: Option<CutoverPlan>,
) -> (ShutdownReceipt, Option<CutoverReceipt>) {
    let (tracked_idle, gateway_idle) = drain::reject_and_wait(
        MUTATION_DRAIN,
        &gate,
        || gateway.begin_drain(),
        |bound| gateway.wait_idle(bound),
        || supervisor.disarm_restarts(),
    )
    .await;
    gateway.finish_drain();
    let compat = supervisor.shutdown().await.ok();
    let compat_reaped = compat.is_some();
    let gateway_stopped = gateway.stop().await.is_ok();
    let mut receipt = ShutdownReceipt::drained(tracked_idle, gateway_idle, compat, gateway_stopped);
    let cutover = plan.map(|plan| {
        plan.commit(
            owner.data_dir(),
            DrainEvidence {
                tracked_idle,
                gateway_idle,
                compat,
            },
        )
    });
    if let Some(cutover) = &cutover
        && cutover::append_evidence(owner.data_dir(), &Evidence::Cutover(cutover)).is_err()
    {
        receipt.errors.push("cutover_evidence_unsaved");
    }
    if !gateway_stopped {
        receipt.errors.push("gateway_join_failed");
    }
    if !compat_reaped {
        receipt.errors.push("compat_cleanup_unproved");
    }
    if !tracked_idle || !compat_reaped {
        owner.retain_until_process_exit();
        receipt.errors.push("storage_ownership_retained");
        return (receipt, cutover);
    }
    let released = owner.release();
    receipt.marker_removed = released.marker_removed;
    receipt.runtime_removed = released.runtime_removed;
    if !released.marker_removed {
        receipt
            .errors
            .push("compatibility_reservation_cleanup_failed");
    }
    if !released.runtime_removed {
        receipt.errors.push("runtime_directory_cleanup_failed");
    }
    if released.ownership_retained {
        receipt.errors.push("storage_ownership_retained");
    }
    if !released.lock_released {
        receipt.errors.push("storage_unlock_unproved");
    }
    receipt.storage_released =
        released.marker_removed && released.runtime_removed && released.lock_released;
    (receipt, cutover)
}

impl Drop for CoreRuntime {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(None);
        }
    }
}
