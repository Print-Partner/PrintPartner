//! Moonraker status in the Rust-only runtime. `pp-gateway` owns the watchers;
//! these adapters feed them the hosts in storage's `integrations` setting and
//! expose their cached statuses to the authenticated `pp-api` status route and
//! farm reads. No adapter writes: monitoring never changes Checkoff or
//! dispatches a send.
use anyhow::Result;
use axum::Router;
use pp_api::{
    accepted::AcceptedHttpConfig,
    admin::AdminHttpConfig,
    farm::{FarmObservations, farm_router},
    printer_status::{PrinterStatusPort, StatusLookup, printer_status_router},
};
use pp_gateway::{
    HTTPS_UNAVAILABLE, HostReadError, MoonrakerHost, MoonrakerHosts, MoonrakerStatus,
    PrinterHostState, REDIRECT_UNAVAILABLE,
};
use pp_storage::{
    SettingsClient, WriterOwner, printer_hosts,
    read_model::{
        ReadClient,
        farm::{FarmClient, HostState, IntegrationKind, Observation, PrinterIntegration, iso_time},
    },
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

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
        let Some(observations) = self.0.observations() else {
            return Observation::Unavailable;
        };
        let Some(status) = observations.latest(&integration.id) else {
            return Observation::Unavailable;
        };
        if [HTTPS_UNAVAILABLE, REDIRECT_UNAVAILABLE]
            .contains(&status.message.as_deref().unwrap_or(""))
        {
            return Observation::NotSupported;
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
            last_heard_at: observations.last_heard(&integration.id).map(iso_time),
        }
    }
}

/// The status and farm routes' configuration and storage clients, prepared
/// before the gateway starts and mounted once it hands over its status lookup.
pub struct StatusRoutes {
    config: AdminHttpConfig,
    client: ReadClient,
    farm_config: AcceptedHttpConfig,
    farm: FarmClient,
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
        })
    }

    pub fn mount(self, status: MoonrakerStatus) -> Router {
        printer_status_router(
            self.config,
            self.client,
            Arc::new(WatchedStatus(status.clone())),
        )
        .merge(farm_router(
            self.farm_config,
            self.farm,
            Arc::new(WatchedFarm(status)),
        ))
    }
}
