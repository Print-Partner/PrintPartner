//! Moonraker and PrusaLink status in the Rust-only runtime. `pp-gateway` owns
//! the watchers; these adapters feed them the hosts in storage's `integrations` setting and
//! expose their cached statuses to the authenticated `pp-api` status route and
//! farm reads. No adapter writes: monitoring never changes Checkoff or
//! dispatches a send. The one observer that writes, `measured_grams`,
//! records only the filament a finished print measured.
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
use pp_gateway::printer_host::{
    BAMBU_FEATURE_DISABLED, BAMBU_FEATURE_INVALID, BAMBU_PIN_UNREADABLE, BambuPin, HostReads,
    bambu_watch,
};
use pp_gateway::{
    HTTPS_UNAVAILABLE, HostKind, HostReadError, MoonrakerHost, MoonrakerHosts, MoonrakerStatus,
    PrinterHostState, PrinterHostStatus, PrinterObservations, REDIRECT_UNAVAILABLE,
};
use pp_storage::{
    SettingsClient, WriterOwner, printer_hosts,
    read_model::{
        ReadClient,
        farm::{
            Farm, FarmClient, HostState, IntegrationKind, Observation, PrinterIntegration,
            iso_time, read_settings,
        },
    },
    send_queue::writes::SendQueueClient,
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
                Ok(Ok(Some(hosts))) => Ok(watched_hosts(hosts)),
                Ok(Ok(None)) => Err(HostReadError::Invalid),
                Ok(Err(_)) | Err(_) => Err(HostReadError::Unavailable),
            }
        })
    }
}

/// The Moonraker snapshot, then the PrusaLink and Bambu hosts storage
/// selected for the watchers (`printer_hosts::WatchedHosts` applies the cap
/// and id rules). A Bambu host carries its stored pin, which the watchers
/// need before they connect.
fn watched_hosts(hosts: printer_hosts::WatchedHosts) -> Vec<MoonrakerHost> {
    let mut watched: Vec<MoonrakerHost> = hosts
        .moonraker
        .into_iter()
        .map(|host| MoonrakerHost {
            id: host.id,
            base_url: host.base_url,
            api_key: host.api_key,
            enabled: host.enabled,
            kind: HostKind::Moonraker,
        })
        .collect();
    for host in hosts.prusalink {
        watched.push(MoonrakerHost {
            id: host.id,
            base_url: host.base_url,
            api_key: None,
            enabled: host.enabled,
            kind: HostKind::PrusaLink {
                username: host.username,
                password: host.password,
            },
        });
    }
    // Node's tenant-global admission: with the feature off, or its stored
    // toggles invalid, no Bambu host connects.
    let feature = match hosts.bambu_feature {
        Some(true) => None,
        Some(false) => Some(BAMBU_FEATURE_DISABLED),
        None => Some(BAMBU_FEATURE_INVALID),
    };
    for row in hosts.bambu {
        let (pin, refused) = match &row.pin {
            Some(Ok(pin)) => {
                let pin = BambuPin {
                    host: pin.host.clone(),
                    port: pin.port,
                    sha256: pin.sha256,
                };
                (Some(pin), feature)
            }
            Some(Err(_)) => (None, feature.or(Some(BAMBU_PIN_UNREADABLE))),
            None => (None, feature),
        };
        watched.push(MoonrakerHost {
            id: row.id.clone(),
            base_url: row.address().unwrap_or_default().to_owned(),
            api_key: None,
            enabled: row.enabled,
            kind: bambu_watch(&row.config, pin.as_ref(), refused),
        });
    }
    watched
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

/// Farm reads answer from the watchers' observation port. Moonraker,
/// PrusaLink and Bambu have Rust watchers; HTTPS and redirecting Moonraker
/// hosts cannot be monitored here.
pub(crate) struct WatchedFarm(pub(crate) MoonrakerStatus);

impl FarmObservations for WatchedFarm {
    fn observe(&self, integration: &PrinterIntegration) -> Observation {
        if !watched(&integration.kind) {
            return Observation::NotWatched;
        }
        observed(&self.0, &integration.id)
    }
}

/// What the watchers last observed for integration `id`.
pub(crate) fn observed(status: &MoonrakerStatus, id: &str) -> Observation {
    let Some(observations) = status.observations() else {
        return Observation::Unavailable;
    };
    observe(&observations, id, observations.latest(id))
}

/// Whether the status watchers poll this kind of integration.
fn watched(kind: &IntegrationKind) -> bool {
    matches!(
        kind,
        IntegrationKind::Moonraker | IntegrationKind::Prusalink | IntegrationKind::Bambu
    )
}

/// One watched integration's observation, from `status` (the latest map
/// entry for farm reads, the stream's own ordered copy for `/ws/printers`).
fn observe(
    observations: &PrinterObservations,
    id: &str,
    status: Option<PrinterHostStatus>,
) -> Observation {
    let Some(status) = status else {
        return Observation::Unavailable;
    };
    // Hosts the watchers hold but never connect to: HTTPS and redirecting
    // hosts, and Bambu while the tenant's feature is off or invalid.
    if [
        HTTPS_UNAVAILABLE,
        REDIRECT_UNAVAILABLE,
        BAMBU_FEATURE_DISABLED,
        BAMBU_FEATURE_INVALID,
    ]
    .contains(&status.message.as_deref().unwrap_or(""))
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
            if !watched(&integration.kind) {
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
    grams: pp_storage::print_grams::PrintGramsClient,
    exports: std::path::PathBuf,
    wake: Arc<tokio::sync::Notify>,
}

impl StatusRoutes {
    /// `wake` is the send worker's: each dispatch wakes it.
    pub fn new(owner: &WriterOwner, origin: &str, wake: Arc<tokio::sync::Notify>) -> Result<Self> {
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
            grams: owner.print_grams(tenant),
            exports: owner
                .lease()
                .context("Storage lease unavailable")?
                .data_dir()
                .join("exports"),
            wake,
        })
    }

    pub fn mount(self, status: MoonrakerStatus) -> Router {
        let stream = PrinterStream::new(HEARTBEAT);
        let producer = stream.clone();
        let settings = self.settings;
        let (measuring, reads, grams) = (settings.clone(), self.reads.clone(), self.grams);
        status.spawn_observer(move |observations, cancelled| {
            crate::measured_grams::observe(measuring, reads, grams, observations, cancelled)
        });
        let hosts = StoredPrinterHosts::new(settings.clone(), self.reads);
        let snapshots = settings.print_file_snapshots();
        let targets = QueueTargets {
            settings: settings.clone(),
            exports: self.exports,
            status: status.clone(),
            wake: self.wake,
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
            snapshots,
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

#[cfg(test)]
mod tests {
    use super::*;
    use pp_storage::printer_hosts::{MoonrakerHost as Stored, StoredIntegration, WatchedHosts};

    fn moonraker(id: &str) -> Stored {
        Stored {
            id: id.into(),
            base_url: "http://m.test".into(),
            api_key: Some("key".into()),
            enabled: true,
        }
    }

    fn prusalink(id: &str) -> StoredIntegration {
        StoredIntegration {
            id: id.into(),
            kind: "prusalink".into(),
            base_url: "http://p.test".into(),
            api_key: None,
            username: "maker".into(),
            password: Some("secret".into()),
            storage: None,
            enabled: true,
        }
    }

    #[test]
    fn ticket_t_25_bambu_hosts_carry_the_pin_for_their_address_only() {
        use pp_gateway::printer_host::{BAMBU_ADDRESS_CHANGED, BAMBU_NOT_PINNED};
        use pp_storage::{printer_hosts::BambuRow, tls_pins::Pin};
        let row = |id: &str, config: serde_json::Value, pinned: Option<(&str, u16)>| BambuRow {
            id: id.into(),
            config,
            enabled: true,
            pin: pinned.map(|(host, port)| {
                Ok(Pin {
                    host: host.into(),
                    port,
                    sha256: [7; 32],
                    pinned_at: String::new(),
                })
            }),
        };
        let config = serde_json::json!({"host": "192.168.1.20", "serial": "FIXTURE01", "access_code": "code"});
        let hosts = watched_hosts(WatchedHosts {
            moonraker: Vec::new(),
            prusalink: Vec::new(),
            bambu: vec![
                row("pinned", config.clone(), Some(("192.168.1.20", 8883))),
                row("moved", config.clone(), Some(("192.168.1.20", 1883))),
                row("readdressed", config.clone(), Some(("192.168.1.21", 8883))),
                row("new", config.clone(), None),
                row(
                    "bad",
                    serde_json::json!({"host": "192.168.1.22"}),
                    Some(("192.168.1.22", 8883)),
                ),
            ],
            bambu_feature: Some(true),
        });
        let pins: Vec<_> = hosts
            .iter()
            .map(|host| match &host.kind {
                HostKind::Bambu { config, pin } => (
                    host.base_url.as_str(),
                    config.as_ref().err().cloned(),
                    pin.clone(),
                ),
                _ => panic!("{} is not a Bambu host", host.id),
            })
            .collect();
        assert_eq!(
            pins,
            [
                ("192.168.1.20", None, Ok([7; 32])),
                ("192.168.1.20", None, Err(BAMBU_ADDRESS_CHANGED.to_owned())),
                ("192.168.1.20", None, Err(BAMBU_ADDRESS_CHANGED.to_owned())),
                ("192.168.1.20", None, Err(BAMBU_NOT_PINNED.to_owned())),
                (
                    "192.168.1.22",
                    Some("access_code is required".to_owned()),
                    Err("access_code is required".to_owned())
                ),
            ]
        );

        // The feature off or its toggles invalid refuses every Bambu host,
        // pinned or not; an unreadable pin refuses only its own host.
        let refused = |feature, pin| {
            let mut pinned = row("x1c", config.clone(), Some(("192.168.1.20", 8883)));
            if let Some(pin) = pin {
                pinned.pin = Some(pin);
            }
            let hosts = watched_hosts(WatchedHosts {
                moonraker: Vec::new(),
                prusalink: Vec::new(),
                bambu: vec![pinned],
                bambu_feature: feature,
            });
            match &hosts[0].kind {
                HostKind::Bambu { pin, .. } => pin.clone().err(),
                _ => panic!("not a Bambu host"),
            }
        };
        assert_eq!(refused(Some(true), None), None);
        assert_eq!(
            refused(Some(false), None).as_deref(),
            Some(BAMBU_FEATURE_DISABLED)
        );
        assert_eq!(refused(None, None).as_deref(), Some(BAMBU_FEATURE_INVALID));
        assert_eq!(
            refused(Some(true), Some(Err("bad".into()))).as_deref(),
            Some(BAMBU_PIN_UNREADABLE)
        );
        assert_eq!(
            refused(Some(false), Some(Err("bad".into()))).as_deref(),
            Some(BAMBU_FEATURE_DISABLED)
        );
    }

    #[test]
    fn ticket_t_22_prusalink_hosts_follow_moonraker_with_their_credentials() {
        let hosts = watched_hosts(WatchedHosts {
            moonraker: vec![moonraker("a")],
            prusalink: vec![prusalink("mk4")],
            bambu: Vec::new(),
            bambu_feature: None,
        });
        let ids: Vec<_> = hosts.iter().map(|host| host.id.as_str()).collect();
        assert_eq!(ids, ["a", "mk4"]);
        assert!(hosts[0].kind == HostKind::Moonraker);
        assert!(
            hosts[1].kind
                == HostKind::PrusaLink {
                    username: "maker".into(),
                    password: Some("secret".into()),
                }
        );
        assert_eq!(hosts[1].api_key, None);
    }
}
