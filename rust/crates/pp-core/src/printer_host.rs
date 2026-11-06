//! Printer-host reads in the Rust-only runtime (GRE-360, GRE-1044). The
//! `pp-api` routes resolve a printer through storage's fleet and integration
//! settings and read Moonraker and PrusaLink hosts through the gateway's drivers. Every read is
//! user-initiated and read-only: no upload, start or Checkoff change. The only
//! resend is PrusaLink's single Digest resend after a 401 (see
//! `pp_gateway::printer_host`).
use pp_api::printer_host::{Answer, Capabilities, HostAnswer, PrinterHostPort};
use pp_gateway::printer_host::{HostReads, HostTarget};
use pp_storage::{
    SettingsClient,
    printer_hosts::{self, PrinterLink, StoredIntegration},
};
use serde_json::{Value, json};
use std::time::Duration;

const READER_WAIT: Duration = Duration::from_millis(500);
/// Types Node has an adapter for that this runtime cannot read yet.
const UNPORTED: [&str; 5] = [
    "bambu",
    "spoolman",
    "slicer_sidecar",
    "ai_assistant",
    "home_assistant",
];

pub(crate) struct StoredPrinterHosts {
    settings: SettingsClient,
    reads: HostReads,
}

impl StoredPrinterHosts {
    pub(crate) fn new(settings: SettingsClient, reads: HostReads) -> Self {
        Self { settings, reads }
    }
}

enum Host {
    Read(HostTarget),
    Unported,
}

fn host(integration: StoredIntegration) -> Option<Host> {
    match integration.kind.as_str() {
        "moonraker" => Some(Host::Read(HostTarget::Moonraker {
            base_url: integration.base_url,
            api_key: integration.api_key,
        })),
        "prusalink" => Some(Host::Read(HostTarget::PrusaLink {
            base_url: integration.base_url,
            username: integration.username,
            password: integration.password,
            storage: integration.storage,
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

impl StoredPrinterHosts {
    /// A print-file browse of `path`, or the camera list when `path` is `None`.
    fn read(&self, printer: String, path: Option<String>) -> Answer<Value> {
        let (settings, reads) = (self.settings.clone(), self.reads.clone());
        Box::pin(async move {
            let target = match linked(&settings, printer).await {
                Ok(Host::Read(target)) => target,
                Ok(Host::Unported) => return HostAnswer::Unsupported,
                Err(answer) => return answer,
            };
            match path {
                Some(path) => reads.browse(&target, &path).await,
                None => reads.cameras(&target).await,
            }
            .map_or_else(failed, HostAnswer::Ok)
        })
    }
}

impl PrinterHostPort for StoredPrinterHosts {
    fn capabilities(&self, printer: String) -> Answer<Capabilities> {
        let settings = self.settings.clone();
        Box::pin(async move {
            match linked(&settings, printer).await {
                // Only Moonraker has a status watcher here so far.
                Ok(Host::Read(target)) => HostAnswer::Ok(Capabilities {
                    files: true,
                    cameras: true,
                    status: matches!(target, HostTarget::Moonraker { .. }),
                }),
                Ok(Host::Unported) => HostAnswer::Unsupported,
                Err(answer) => answer,
            }
        })
    }

    fn files(&self, printer: String, path: String) -> Answer<Value> {
        self.read(printer, Some(path))
    }

    fn cameras(&self, printer: String) -> Answer<Value> {
        self.read(printer, None)
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
                Some(Host::Read(target)) => HostAnswer::Ok(reads.test(&target).await),
                Some(Host::Unported) => HostAnswer::Unsupported,
                None => HostAnswer::Ok(json!({
                    "ok": false,
                    "message": format!("Unknown integration type: {kind}"),
                })),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(kind: &str) -> StoredIntegration {
        StoredIntegration {
            id: "i".into(),
            kind: kind.into(),
            base_url: "http://p.test".into(),
            api_key: None,
            username: String::new(),
            password: None,
            storage: None,
            enabled: true,
        }
    }

    /// Every type Node's adapter registry (`integrations/registry.ts`) knows
    /// is read here (Moonraker, PrusaLink) or unported; anything else is unknown.
    #[test]
    fn ticket_t_21_host_types_follow_nodes_registry() {
        assert!(matches!(
            host(stored("moonraker")),
            Some(Host::Read(HostTarget::Moonraker { .. }))
        ));
        assert!(matches!(
            host(stored("prusalink")),
            Some(Host::Read(HostTarget::PrusaLink { .. }))
        ));
        for kind in [
            "bambu",
            "spoolman",
            "slicer_sidecar",
            "ai_assistant",
            "home_assistant",
        ] {
            assert!(matches!(host(stored(kind)), Some(Host::Unported)), "{kind}");
        }
        for kind in ["", "slicer-sidecar", "Moonraker", "mystery"] {
            assert!(host(stored(kind)).is_none(), "{kind}");
        }
    }
}
