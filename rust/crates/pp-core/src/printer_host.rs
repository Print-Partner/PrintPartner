//! Printer-host reads in the Rust-only runtime (GRE-360, GRE-1044). The
//! `pp-api` routes resolve a printer through storage's fleet and integration
//! settings and read Moonraker and PrusaLink hosts through the gateway's drivers. Every read is
//! user-initiated and read-only: no upload, start or Checkoff change. The
//! print-file download and camera view stream the printer's reply through. The only
//! resend is PrusaLink's single Digest resend after a 401 (see
//! `pp_gateway::printer_host`).
use pp_api::printer_host::{Answer, Capabilities, HostAnswer, HostStream, PrinterHostPort};
use pp_gateway::printer_host::{BambuPin, HostReads, HostTarget, Opened};
use pp_storage::{
    SettingsClient, features,
    printer_hosts::{self, PrinterLink, StoredIntegration},
    tls_pins::{self, Pin},
};
use serde_json::{Value, json};
use std::{sync::atomic::AtomicBool, time::Duration};

const READER_WAIT: Duration = Duration::from_millis(500);
/// Types Node has an adapter for that this runtime cannot read yet.
const UNPORTED: [&str; 4] = [
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

pub(crate) enum Host {
    Read(HostTarget),
    /// Bambu: a status-only target. It is never `Read`, so file, camera and
    /// upload paths refuse it (GRE-362).
    Status,
    Unported,
}

pub(crate) fn host(integration: StoredIntegration) -> Option<Host> {
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
        "bambu" => Some(Host::Status),
        kind if UNPORTED.contains(&kind) => Some(Host::Unported),
        _ => None,
    }
}

/// The printer's linked host, or the answer that ends the request.
async fn linked<T>(settings: &SettingsClient, printer: String) -> Result<Host, HostAnswer<T>> {
    linked_watched(settings, printer, false)
        .await
        .map(|(host, _)| host)
}

/// Whether the watchers poll this PrusaLink row: storage took it (within the
/// cap, not on a Moonraker id) and it has an address.
fn status_watched(
    integration: &StoredIntegration,
    hosts: Option<printer_hosts::WatchedHosts>,
) -> bool {
    hosts.is_some_and(|hosts| {
        hosts
            .prusalink
            .iter()
            .any(|row| row.id == integration.id && !row.base_url.trim().is_empty())
    })
}

/// The printer's linked host and, when `status` asks, whether the status
/// watchers take it. Storage skips a PrusaLink row past the host cap or with
/// an id a Moonraker row has (`printer_hosts::watched`), so its status is not
/// served here. Only capabilities ask, so file and camera reads read the
/// settings once.
async fn linked_watched<T>(
    settings: &SettingsClient,
    printer: String,
    status: bool,
) -> Result<(Host, bool), HostAnswer<T>> {
    let client = settings.clone();
    let link = tokio::task::spawn_blocking(move || {
        let link = printer_hosts::printer_link(&client, READER_WAIT, &printer)?;
        let watched = match &link {
            PrinterLink::Linked(integration) if status && integration.kind == "prusalink" => {
                status_watched(integration, printer_hosts::read(&client, READER_WAIT)?)
            }
            _ => true,
        };
        anyhow::Ok((link, watched))
    })
    .await;
    match link {
        Ok(Ok((PrinterLink::Linked(integration), watched))) => host(integration)
            .map(|host| (host, watched))
            .ok_or(HostAnswer::Unavailable),
        Ok(Ok((PrinterLink::PrinterNotFound, _))) => Err(HostAnswer::NotFound),
        Ok(Ok((PrinterLink::NotLinked, _))) => Err(HostAnswer::NotLinked),
        Ok(Ok((PrinterLink::NotAvailable, _))) => Err(HostAnswer::Unavailable),
        Ok(Err(_)) | Err(_) => Err(HostAnswer::Busy),
    }
}

/// The readable host linked to `printer`, or the answer that ends the request.
async fn readable<T>(
    settings: &SettingsClient,
    printer: String,
) -> Result<HostTarget, HostAnswer<T>> {
    match linked(settings, printer).await? {
        Host::Read(target) => Ok(target),
        Host::Status | Host::Unported => Err(HostAnswer::Unsupported),
    }
}

fn answer<T>(result: anyhow::Result<T>) -> HostAnswer<T> {
    result.map_or_else(
        |error| HostAnswer::Failed(error.to_string()),
        HostAnswer::Ok,
    )
}

fn stream(opened: Option<Opened>) -> Option<HostStream> {
    opened.map(|opened| HostStream {
        status: opened.status,
        content_type: opened.content_type,
        content_length: opened.content_length,
        body: opened.body,
    })
}

/// A streamed read: a print file at a path, or one camera's view.
enum Open {
    File(String),
    Camera(String),
}

impl StoredPrinterHosts {
    /// A print-file browse of `path`, or the camera list when `path` is `None`.
    fn read(&self, printer: String, path: Option<String>) -> Answer<Value> {
        let (settings, reads) = (self.settings.clone(), self.reads.clone());
        Box::pin(async move {
            let target = match readable(&settings, printer).await {
                Ok(target) => target,
                Err(answer) => return answer,
            };
            answer(match path {
                Some(path) => reads.browse(&target, &path).await,
                None => reads.cameras(&target).await,
            })
        })
    }

    fn open(&self, printer: String, open: Open) -> Answer<Option<HostStream>> {
        let (settings, reads) = (self.settings.clone(), self.reads.clone());
        Box::pin(async move {
            let target = match readable(&settings, printer).await {
                Ok(target) => target,
                Err(answer) => return answer,
            };
            answer(
                match open {
                    Open::File(path) => reads.open_file(&target, &path).await,
                    Open::Camera(id) => reads.open_camera(&target, &id).await,
                }
                .map(stream),
            )
        })
    }
}

impl PrinterHostPort for StoredPrinterHosts {
    fn capabilities(&self, printer: String) -> Answer<Capabilities> {
        let settings = self.settings.clone();
        Box::pin(async move {
            match linked_watched(&settings, printer, true).await {
                // Moonraker and PrusaLink both have status watchers.
                Ok((Host::Read(_), watched)) => HostAnswer::Ok(Capabilities {
                    files: true,
                    cameras: true,
                    status: watched,
                }),
                // Bambu status arrives with its watcher (GRE-1176).
                Ok((Host::Status | Host::Unported, _)) => HostAnswer::Unsupported,
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

    fn file(&self, printer: String, path: String) -> Answer<Option<HostStream>> {
        self.open(printer, Open::File(path))
    }

    fn camera_view(&self, printer: String, camera: String) -> Answer<Option<HostStream>> {
        self.open(printer, Open::Camera(camera))
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
            let id = stored.id.clone();
            match host(stored) {
                Some(Host::Read(target)) => HostAnswer::Ok(reads.test(&target).await),
                Some(Host::Status) => bambu_test(&settings, &reads, id).await,
                Some(Host::Unported) => HostAnswer::Unsupported,
                None => HostAnswer::Ok(json!({
                    "ok": false,
                    "message": format!("Unknown integration type: {kind}"),
                })),
            }
        })
    }

    fn tls_pin(&self, integration: String) -> Answer<Value> {
        let settings = self.settings.clone();
        Box::pin(async move {
            let read = tokio::task::spawn_blocking(move || {
                let Some(_) = printer_hosts::integration(&settings, READER_WAIT, &integration)?
                else {
                    return anyhow::Ok(None);
                };
                Ok(Some(tls_pins::read(&settings, READER_WAIT, &integration)?))
            })
            .await;
            match read {
                Ok(Ok(Some(pin))) => HostAnswer::Ok(match pin {
                    Some(pin) => json!({
                        "pinned": true,
                        "sha256_prefix": hex::encode(&pin.sha256[..6]),
                        "pinned_at": pin.pinned_at,
                    }),
                    None => json!({ "pinned": false }),
                }),
                Ok(Ok(None)) => HostAnswer::NotFound,
                Ok(Err(_)) | Err(_) => HostAnswer::Busy,
            }
        })
    }

    fn reset_tls_pin(&self, integration: String) -> Answer<Value> {
        let settings = self.settings.clone();
        Box::pin(async move {
            let reset = tokio::task::spawn_blocking(move || {
                let Some(_) = printer_hosts::integration(&settings, READER_WAIT, &integration)?
                else {
                    return anyhow::Ok(None);
                };
                let reply =
                    settings.reset_tls_pin(&integration, &AtomicBool::new(false), WRITE_WAIT)?;
                Ok(Some(reply.recv_timeout(WRITE_WAIT)??))
            })
            .await;
            match reset {
                Ok(Ok(Some(reset))) => HostAnswer::Ok(json!({ "reset": reset })),
                Ok(Ok(None)) => HostAnswer::NotFound,
                Ok(Err(_)) | Err(_) => HostAnswer::Busy,
            }
        })
    }
}

/// How long a pin write may wait for the writer.
const WRITE_WAIT: Duration = Duration::from_secs(5);

/// The user's Bambu Test connection (GRE-1245). Node's tenant-wide Bambu
/// admission first. With no pin stored, the test trusts the certificate the
/// printer presents and, once it has read the printer's status, stores that
/// certificate's pin; only this route ever writes one. A pin taken at another
/// address, or a different certificate, fails without trusting anything.
async fn bambu_test(settings: &SettingsClient, reads: &HostReads, id: String) -> HostAnswer<Value> {
    let client = settings.clone();
    let lookup = id.clone();
    let stored = tokio::task::spawn_blocking(move || {
        let enabled = features::bambu_enabled(&client, READER_WAIT)?;
        let config = printer_hosts::integration_config(&client, READER_WAIT, &lookup)?;
        let pin = tls_pins::read(&client, READER_WAIT, &lookup)?;
        anyhow::Ok((enabled, config, pin))
    })
    .await;
    let (config, pin) = match stored {
        Ok(Ok((Some(true), Some(config), pin))) => (config, pin),
        Ok(Ok((Some(false), ..))) => return HostAnswer::FeatureDisabled("bambu"),
        Ok(Ok((None, ..))) => return HostAnswer::FeatureInvalid,
        Ok(Ok((_, None, _))) => return HostAnswer::NotFound,
        Ok(Err(_)) | Err(_) => return HostAnswer::Busy,
    };
    let pin = pin.map(|pin| BambuPin {
        host: pin.host,
        port: pin.port,
        sha256: pin.sha256,
    });
    let test = reads.bambu_test(&config, pin.as_ref()).await;
    let Some(taken) = test.pin else {
        return HostAnswer::Ok(test.result);
    };
    let client = settings.clone();
    let sha256 = taken.sha256;
    let stored = tokio::task::spawn_blocking(move || {
        let pin = Pin {
            host: taken.host,
            port: taken.port,
            sha256: taken.sha256,
            pinned_at: String::new(),
        };
        let reply = client.pin_tls(&id, pin, &AtomicBool::new(false), WRITE_WAIT)?;
        reply.recv_timeout(WRITE_WAIT)??;
        tls_pins::read(&client, READER_WAIT, &id)
    })
    .await;
    match stored {
        // Another Test connection may have pinned first; only its pin counts.
        Ok(Ok(Some(stored))) if stored.sha256 == sha256 => HostAnswer::Ok(test.result),
        Ok(Ok(_)) => HostAnswer::Ok(json!({
            "ok": false,
            "message": pp_gateway::printer_host::BAMBU_CERTIFICATE_CHANGED,
        })),
        Ok(Err(_)) | Err(_) => HostAnswer::Busy,
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
        assert!(matches!(host(stored("bambu")), Some(Host::Status)));
        for kind in [
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

    /// Capabilities report status only for PrusaLink rows the watchers take:
    /// not past the host cap, and not on an id a Moonraker row already has.
    #[test]
    fn ticket_t_22_prusalink_status_capability_follows_the_watched_rows() {
        let hosts = |moonraker: usize, prusalink: &str| {
            let mut rows: Vec<Value> = (0..moonraker)
                .map(|i| json!({"id": format!("m{i}"), "type": "moonraker", "config": {"base_url": "http://m.test"}}))
                .collect();
            rows.push(
                json!({"id": prusalink, "type": "prusalink", "config": {"base_url": "http://p.test"}}),
            );
            printer_hosts::watched(Some(&Value::Array(rows).to_string()))
        };
        let prusalink = stored("prusalink");
        assert!(status_watched(&prusalink, hosts(127, "i")));
        assert!(!status_watched(&prusalink, hosts(128, "i")), "past the cap");
        assert!(!status_watched(&prusalink, None), "unusable setting");
        let shared = StoredIntegration {
            id: "m0".into(),
            ..stored("prusalink")
        };
        assert!(status_watched(&shared, hosts(0, "m0")));
        assert!(
            !status_watched(&shared, hosts(1, "m0")),
            "id a Moonraker row has"
        );
        let blank = json!([{"id": "i", "type": "prusalink", "config": {"base_url": " "}}]);
        let blank = printer_hosts::watched(Some(&blank.to_string()));
        assert!(!status_watched(&prusalink, blank), "no address");
    }
}
