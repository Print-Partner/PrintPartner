//! Moonraker status in the Rust-only runtime. `pp-gateway` owns the watchers;
//! these adapters feed them the hosts in storage's `integrations` setting and
//! expose their cached statuses to the authenticated `pp-api` route. Neither
//! adapter writes: monitoring never changes Checkoff or dispatches a send.
use anyhow::Result;
use axum::Router;
use pp_api::{
    admin::AdminHttpConfig,
    printer_status::{PrinterStatusPort, StatusLookup, printer_status_router},
};
use pp_gateway::{HostReadError, MoonrakerHost, MoonrakerHosts, MoonrakerStatus};
use pp_storage::{SettingsClient, WriterOwner, printer_hosts, read_model::ReadClient};
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

/// The status route's configuration and credential reader, prepared before
/// the gateway starts and mounted once it hands over its status lookup.
pub struct StatusRoutes {
    config: AdminHttpConfig,
    client: ReadClient,
}

impl StatusRoutes {
    pub fn new(owner: &WriterOwner, origin: &str) -> Result<Self> {
        Ok(Self {
            config: AdminHttpConfig::from_startup(
                origin,
                crate::rebuild_api::DESKTOP_TENANT,
                Some("1"),
            )?,
            client: owner.accepted_reads_with_policy(crate::rebuild_api::desktop_policy())?,
        })
    }

    pub fn mount(self, status: MoonrakerStatus) -> Router {
        printer_status_router(self.config, self.client, Arc::new(WatchedStatus(status)))
    }
}
