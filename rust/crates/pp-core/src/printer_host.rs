//! Printer-host reads in the Rust-only runtime (GRE-360). The `pp-api`
//! routes resolve a printer through storage's fleet and integration settings
//! and read Moonraker hosts through the gateway's driver. Every read is
//! user-initiated and read-only: no upload, start, retry or Checkoff change.
use pp_api::printer_host::{Answer, Capabilities, HostAnswer, PrinterHostPort};
use pp_gateway::printer_host::{MoonrakerReads, MoonrakerTarget};
use pp_storage::{
    SettingsClient,
    printer_hosts::{self, PrinterLink, StoredIntegration},
};
use serde_json::{Value, json};
use std::time::Duration;

const READER_WAIT: Duration = Duration::from_millis(500);
const MOONRAKER: &str = "moonraker";
/// Types Node has an adapter for that this runtime cannot read yet.
const UNPORTED: [&str; 6] = [
    "prusalink",
    "bambu",
    "spoolman",
    "slicer-sidecar",
    "ai-assistant",
    "home-assistant",
];

pub(crate) struct StoredPrinterHosts {
    settings: SettingsClient,
    reads: MoonrakerReads,
}

impl StoredPrinterHosts {
    pub(crate) fn new(settings: SettingsClient, reads: MoonrakerReads) -> Self {
        Self { settings, reads }
    }
}

enum Host {
    Moonraker(MoonrakerTarget),
    Unported,
}

fn host(integration: StoredIntegration) -> Option<Host> {
    match integration.kind.as_str() {
        MOONRAKER => Some(Host::Moonraker(MoonrakerTarget {
            base_url: integration.base_url,
            api_key: integration.api_key,
        })),
        kind if UNPORTED.contains(&kind) => Some(Host::Unported),
        _ => None,
    }
}

/// The printer's linked host, or the answer that ends the request.
async fn linked<T>(settings: &SettingsClient, printer: String) -> Result<Host, HostAnswer<T>> {
    let client = settings.clone();
    let link = tokio::task::spawn_blocking(move || {
        printer_hosts::printer_link(&client, READER_WAIT, &printer)
    })
    .await;
    match link {
        Ok(Ok(PrinterLink::Linked(integration))) => {
            host(integration).ok_or(HostAnswer::Unavailable)
        }
        Ok(Ok(PrinterLink::PrinterNotFound)) => Err(HostAnswer::NotFound),
        Ok(Ok(PrinterLink::NotLinked)) => Err(HostAnswer::NotLinked),
        Ok(Ok(PrinterLink::NotAvailable)) => Err(HostAnswer::Unavailable),
        Ok(Err(_)) | Err(_) => Err(HostAnswer::Busy),
    }
}

fn failed(error: anyhow::Error) -> HostAnswer<Value> {
    HostAnswer::Failed(error.to_string())
}

impl PrinterHostPort for StoredPrinterHosts {
    fn capabilities(&self, printer: String) -> Answer<Capabilities> {
        let settings = self.settings.clone();
        Box::pin(async move {
            match linked(&settings, printer).await {
                Ok(Host::Moonraker(_)) => HostAnswer::Ok(Capabilities {
                    files: true,
                    cameras: true,
                    status: true,
                }),
                Ok(Host::Unported) => HostAnswer::Unsupported,
                Err(answer) => answer,
            }
        })
    }

    fn files(&self, printer: String, path: String) -> Answer<Value> {
        let (settings, reads) = (self.settings.clone(), self.reads.clone());
        Box::pin(async move {
            match linked(&settings, printer).await {
                Ok(Host::Moonraker(target)) => reads
                    .browse(&target, &path)
                    .await
                    .map_or_else(failed, HostAnswer::Ok),
                Ok(Host::Unported) => HostAnswer::Unsupported,
                Err(answer) => answer,
            }
        })
    }

    fn cameras(&self, printer: String) -> Answer<Value> {
        let (settings, reads) = (self.settings.clone(), self.reads.clone());
        Box::pin(async move {
            match linked(&settings, printer).await {
                Ok(Host::Moonraker(target)) => reads
                    .cameras(&target)
                    .await
                    .map_or_else(failed, HostAnswer::Ok),
                Ok(Host::Unported) => HostAnswer::Unsupported,
                Err(answer) => answer,
            }
        })
    }

    fn test(&self, integration: String) -> Answer<Value> {
        let (settings, reads) = (self.settings.clone(), self.reads.clone());
        Box::pin(async move {
            let client = settings.clone();
            let stored = tokio::task::spawn_blocking(move || {
                printer_hosts::integration(&client, READER_WAIT, &integration)
            })
            .await;
            let stored = match stored {
                Ok(Ok(Some(stored))) => stored,
                Ok(Ok(None)) => return HostAnswer::NotFound,
                Ok(Err(_)) | Err(_) => return HostAnswer::Busy,
            };
            let kind = stored.kind.clone();
            match host(stored) {
                Some(Host::Moonraker(target)) => HostAnswer::Ok(reads.test(&target).await),
                Some(Host::Unported) => HostAnswer::Unsupported,
                None => HostAnswer::Ok(json!({
                    "ok": false,
                    "message": format!("Unknown integration type: {kind}"),
                })),
            }
        })
    }
}
