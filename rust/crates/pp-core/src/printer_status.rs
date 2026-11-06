//! Moonraker status in the Rust-only runtime. `pp-gateway` owns the watchers;
//! these adapters feed them the hosts in storage's `integrations` setting and
//! expose their cached statuses to the authenticated `pp-api` status route and
//! farm reads. No adapter writes: monitoring never changes Checkoff or
//! dispatches a send.
use crate::{printer_host::StoredPrinterHosts, send_worker::QueueTargets};
use anyhow::{Context, Result};
use axum::Router;
use pp_api::{
    accepted::AcceptedHttpConfig,
    admin::AdminHttpConfig,
    farm::{FarmObservations, farm_router},
    printer_host::printer_host_router,
    printer_status::{PrinterStatusPort, StatusLookup, printer_status_router},
    printer_stream::{HEARTBEAT, PrinterStream, printer_stream_router},
    send_queue::send_queue_router,
};
use pp_gateway::printer_host::HostReads;
use pp_gateway::{
    HTTPS_UNAVAILABLE, HostReadError, MoonrakerHost, MoonrakerHosts, MoonrakerStatus,
    PrinterHostState, PrinterHostStatus, PrinterObservations, REDIRECT_UNAVAILABLE,
};
use pp_storage::{
    SettingsClient, WriterOwner, printer_hosts,
    send_queue::writes::SendQueueClient,
    read_model::{
        ReadClient,
        farm::{
            Farm, FarmClient, HostState, IntegrationKind, Observation, PrinterIntegration,
            iso_time, read_settings,
        },
    },
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

/// Reader wait per refresh. The gateway refreshes every second and keeps
/// valid pollers through a busy read.
const READER_WAIT: Duration = Duration::from_millis(500);

pub struct StoredMoonrakerHosts {
    client: SettingsClient,
}

impl StoredMoonrakerHosts {
    pub fn new(client: SettingsClient) -> Self {
        Self { client }
    }
}

impl MoonrakerHosts for StoredMoonrakerHosts {
    fn read(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<MoonrakerHost>, HostReadError>> + Send>> {
        let client = self.client.clone();
        Box::pin(async move {
            let read =
                tokio::task::spawn_blocking(move || printer_hosts::read(&client, READER_WAIT));
            match read.await {
                Ok(Ok(Some(hosts))) => Ok(hosts
                    .into_iter()
                    .map(|host| MoonrakerHost {
                        id: host.id,
                        base_url: host.base_url,
                        api_key: host.api_key,
                        enabled: host.enabled,
                    })
                    .collect()),
                Ok(Ok(None)) => Err(HostReadError::Invalid),
                Ok(Err(_)) | Err(_) => Err(HostReadError::Unavailable),
            }
        })
    }
}

struct WatchedStatus(MoonrakerStatus);

impl PrinterStatusPort for WatchedStatus {
    fn lookup(&self, id: String) -> Pin<Box<dyn Future<Output = StatusLookup> + Send>> {
        let status = self.0.clone();
        Box::pin(async move {
            match status.lookup(&id).await {
                pp_gateway::StatusLookup::Status(status) => serde_json::to_value(status)
                    .map_or(StatusLookup::Unavailable, StatusLookup::Status),
                pp_gateway::StatusLookup::NotWatched => StatusLookup::NotWatched,
                pp_gateway::StatusLookup::Unavailable => StatusLookup::Unavailable,
            }
        })
    }
}

/// Farm reads answer from the watchers' observation port. Only Moonraker has
/// a Rust watcher; its HTTPS and redirecting hosts cannot be monitored here.
pub(crate) struct WatchedFarm(pub(crate) MoonrakerStatus);

impl FarmObservations for WatchedFarm {
    fn observe(&self, integration: &PrinterIntegration) -> Observation {
        if integration.kind != IntegrationKind::Moonraker {
            return Observation::NotWatched;
        }
        observed(&self.0, &integration.id)
    }
}

/// What the watchers last observed for Moonraker integration `id`.
pub(crate) fn observed(status: &MoonrakerStatus, id: &str) -> Observation {
    let Some(observations) = status.observations() else {
        return Observation::Unavailable;
    };
    observe(&observations, id, observations.latest(id))
}

/// One Moonraker integration's observation, from `status` (the latest map
/// entry for farm reads, the stream's own ordered copy for `/ws/printers`).
fn observe(
    observations: &PrinterObservations,
    id: &str,
    status: Option<PrinterHostStatus>,
) -> Observation {
    let Some(status) = status else {
        return Observation::Unavailable;
    };
    if [HTTPS_UNAVAILABLE, REDIRECT_UNAVAILABLE].contains(&status.message.as_deref().unwrap_or(""))
    {
        return Observation::NotSupported;
    }
    let last_heard = observations.last_heard(id);
    // The watcher's "Awaiting status" placeholder: unknown, never answered.
    if status.state == PrinterHostState::Unknown && last_heard.is_none() {
        return Observation::Unavailable;
    }
    Observation::Seen {
        state: match status.state {
            PrinterHostState::Idle => HostState::Idle,
            PrinterHostState::Printing => HostState::Printing,
            PrinterHostState::Paused => HostState::Paused,
            PrinterHostState::Complete => HostState::Complete,
            PrinterHostState::Error => HostState::Error,
            PrinterHostState::Offline => HostState::Offline,
            PrinterHostState::Unknown => HostState::Unknown,
        },
        progress_percent: status
            .progress
            .filter(|progress| progress.is_finite())
            .map(|progress| progress.clamp(0.0, 100.0).round() as u8),
        filename: status.filename,
        last_heard_at: last_heard.map(iso_time),
        state_since: observations.state_since(id).map(iso_time),
    }
}

/// Feeds `/ws/printers` from the watchers' ordered transitions. It keeps its
/// own copy of each host's status, advanced one transition at a time, so a
/// state the printer passed through is published even when the newest-only
/// map already moved on. It starts from, and after falling behind resyncs to,
/// one map snapshot and skips transitions that snapshot already holds. Every
/// second it re-reads the farm settings and drops hosts whose pollers stopped.
/// It writes nothing and never polls a printer. On cancellation it stops the
/// stream and waits for its sockets to close.
async fn produce(
    settings: SettingsClient,
    stream: PrinterStream,
    observations: PrinterObservations,
    cancelled: CancellationToken,
) {
    let resync = || {
        let transitions = observations.transitions();
        let (map, mark) = observations.snapshot();
        (transitions, (*map).clone(), mark)
    };
    let (mut transitions, mut seen, mut mark) = resync();
    let mut farm: Option<Farm> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            () = cancelled.cancelled() => break,
            transition = transitions.recv() => match transition {
                Ok(transition) if transition.seq > mark => {
                    seen.insert(transition.id, transition.status);
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => (transitions, seen, mark) = resync(),
                Err(RecvError::Closed) => break,
            },
            _ = tick.tick() => {
                let client = settings.clone();
                if let Ok(Ok(read)) =
                    tokio::task::spawn_blocking(move || read_settings(&client, READER_WAIT)).await
                {
                    farm = Some(read);
                }
                let live = observations.all();
                seen.retain(|id, _| live.contains_key(id));
            }
        }
        let Some(farm) = &farm else {
            continue;
        };
        stream.publish(farm.stream_printers(|integration| {
            if integration.kind != IntegrationKind::Moonraker {
                return Observation::NotWatched;
            }
            observe(
                &observations,
                &integration.id,
                seen.get(&integration.id).cloned(),
            )
        }));
    }
    stream.stop().await;
}

/// The status and farm routes' configuration and storage clients, prepared
/// before the gateway starts and mounted once it hands over its status lookup.
pub struct StatusRoutes {
    config: AdminHttpConfig,
    client: ReadClient,
    farm_config: AcceptedHttpConfig,
    farm: FarmClient,
    stream_config: AcceptedHttpConfig,
    settings: SettingsClient,
    reads: HostReads,
    send_config: AcceptedHttpConfig,
    send_queue: SendQueueClient,
    exports: std::path::PathBuf,
}

impl StatusRoutes {
    pub fn new(owner: &WriterOwner, origin: &str) -> Result<Self> {
        let tenant = crate::rebuild_api::DESKTOP_TENANT;
        let policy = crate::rebuild_api::desktop_policy();
        Ok(Self {
            config: AdminHttpConfig::from_startup(origin, tenant, Some("1"))?,
            client: owner.accepted_reads_with_policy(policy)?,
            farm_config: AcceptedHttpConfig::from_startup(origin, tenant, Some("1"))?,
            farm: owner.farm_reads(policy)?,
            stream_config: AcceptedHttpConfig::from_startup(origin, tenant, Some("1"))?,
            settings: owner.client(),
            reads: HostReads::new()?,
            send_config: AcceptedHttpConfig::from_startup(origin, tenant, Some("1"))?,
            send_queue: owner.send_queue(policy)?,
            exports: owner
                .lease()
                .context("Storage lease unavailable")?
                .data_dir()
                .join("exports"),
        })
    }

    pub fn mount(self, status: MoonrakerStatus) -> Router {
        let stream = PrinterStream::new(HEARTBEAT);
        let producer = stream.clone();
        let settings = self.settings;
        let hosts = StoredPrinterHosts::new(settings.clone(), self.reads);
        let targets = QueueTargets {
            settings: settings.clone(),
            exports: self.exports,
            status: status.clone(),
        };
        status.spawn_observer(move |observations, cancelled| {
            produce(settings, producer, observations, cancelled)
        });
        printer_status_router(
            self.config.clone(),
            self.client.clone(),
            Arc::new(WatchedStatus(status.clone())),
        )
        .merge(printer_host_router(
            self.config,
            self.client,
            Arc::new(hosts),
        ))
        .merge(farm_router(
            self.farm_config,
            self.farm.clone(),
            Arc::new(WatchedFarm(status)),
        ))
        .merge(printer_stream_router(self.stream_config, self.farm, stream))
        .merge(send_queue_router(
            self.send_config,
            self.send_queue,
            Arc::new(targets),
        ))
    }
}
