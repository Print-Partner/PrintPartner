//! Farm reads for Today, Farm and Production (GRE-1125): the configured
//! printers, printer Checkoff results, unmatched prints and the send queue.
//!
//! One deferred read of the caller's tenant settings `printer.fleet`,
//! `integrations`, `printer.checkoff_links`, `printer.unattributed_prints`
//! and the send queue (`crate::send_queue`). It writes nothing: unlike Node's routes it never
//! repairs links or reconciles live sends, and rows Node would repair are
//! skipped. Bodies carry no host addresses, file paths or printer error text.
//!
//! [`Farm::printers`] joins the stored printers with what the status watchers
//! observed. Observation is a read: it never sends, checks off or dispatches.

use super::Credential;
use crate::{
    Envelope, Shared, WriterOwner, auth,
    printer_display_state::{BedClearState, PrinterExecutionState, PrinterStateView},
};
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// A larger setting fails the read instead of answering a partial farm.
const MAX_SETTING_BYTES: i64 = 4 * 1024 * 1024;
/// Node keeps the newest 100 unmatched prints, open ones included. Results
/// and the queue are not capped: Node trims only their finished entries.
const MAX_UNMATCHED: usize = 100;

/// A farm setting is larger than [`MAX_SETTING_BYTES`].
#[derive(Debug)]
pub struct TooLarge;

impl std::error::Error for TooLarge {}

impl std::fmt::Display for TooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Farm settings exceed the read limit")
    }
}

/// What the status watchers know about one integration id.
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// No watcher in this runtime monitors the integration's driver.
    NotWatched,
    /// Watched, but the driver cannot monitor this host here (HTTPS, redirects).
    NotSupported,
    /// Watched with no status: monitoring is stopped or has not answered yet.
    Unavailable,
    Seen {
        state: HostState,
        /// Whole percent, 0 to 100.
        progress_percent: Option<u8>,
        /// The file the printer reports, to match a completion to a result.
        filename: Option<String>,
        /// The last successful response, RFC 3339.
        last_heard_at: Option<String>,
        /// When the host entered `state` as this process saw it, RFC 3339.
        /// It binds a `complete` report to one completion: a confirmation
        /// clears it only if applied after this.
        state_since: Option<String>,
    },
}

/// The watcher's `PrinterHostState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Idle,
    Printing,
    Paused,
    Complete,
    Error,
    Offline,
    Unknown,
}

/// Why a printer's execution state is not a live reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum PrinterReason {
    /// The printer has no printer integration.
    NotSetUp,
    /// The printer names an integration that is gone or is not a printer.
    IntegrationMissing,
    /// The integration is disabled, so Checkoff is manual.
    MonitoringDisabled,
    /// This runtime cannot monitor the integration, so Checkoff is manual.
    MonitoringNotSupported,
    /// Monitoring has no status yet or is stopped.
    AwaitingStatus,
    /// Monitoring polled the printer and it did not answer.
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum IntegrationKind {
    Moonraker,
    Prusalink,
    Bambu,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct PrinterIntegration {
    pub id: String,
    pub kind: IntegrationKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct LoadedFilament {
    pub slot: u8,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub filament_color_id: Option<String>,
    pub label: String,
}

/// One configured printer with its execution and bed-clear facts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FarmPrinter {
    pub id: String,
    pub name: String,
    pub model: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<PrinterIntegration>")
    )]
    pub integration: Option<PrinterIntegration>,
    pub state: PrinterStateView,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<PrinterReason>")
    )]
    pub reason: Option<PrinterReason>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub last_heard_at: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<u8>")
    )]
    pub progress_percent: Option<u8>,
    pub loaded_filaments: Vec<LoadedFilament>,
}

/// `GET /api/v2/printers` body, in stored order.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FarmPrinterList {
    pub printers: Vec<FarmPrinter>,
}

/// Node's link states, with its legacy `applied` read as `verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum CheckoffResultState {
    Watching,
    AwaitingVerify,
    HostFailed,
    Dismissed,
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum HostOutcome {
    Unknown,
    Success,
    Failed,
    Cancelled,
}

/// A verify decision on one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum UnitResult {
    Confirmed,
    Rejected,
}

/// One Progress unit a print holds: what confirming its bed would mark.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct CheckoffUnit {
    pub part_id: i64,
    pub unit_index: u32,
    /// The Part's file name, or null when the Part is gone or the read did not
    /// name it (the `/ws/printers` producer never does).
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub part_filename: Option<String>,
}

/// A unit a verify already decided.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ResolvedUnit {
    pub part_id: i64,
    pub unit_index: u32,
    pub result: UnitResult,
}

/// One print a send linked to a Build, as the printer reported it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct CheckoffResult {
    pub id: String,
    pub profile_id: i64,
    pub printer_id: String,
    pub integration_id: String,
    pub filename: String,
    pub state: CheckoffResultState,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<HostOutcome>")
    )]
    pub host_outcome: Option<HostOutcome>,
    pub unit_count: u32,
    pub resolved_unit_count: u32,
    /// The stored units with a valid Part ID and unit index, in stored order.
    pub units: Vec<CheckoffUnit>,
    /// The stored decisions with a valid Part ID, unit index and result.
    pub resolved_units: Vec<ResolvedUnit>,
    pub created_at: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub completed_at: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub applied_at: Option<String>,
}

/// `GET /api/v2/printer-checkoff` body, in stored order.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct CheckoffResultList {
    pub results: Vec<CheckoffResult>,
}

/// A finished print no send link claims.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct UnmatchedPrint {
    pub id: String,
    pub printer_id: String,
    pub integration_id: String,
    pub filename: String,
    pub completed_at: String,
    pub object_count: u32,
    pub candidate_count: u32,
}

/// `GET /api/v2/printer-checkoff/unattributed` body: open prints that no
/// watching, awaiting or verified link matches, in stored order.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct UnmatchedPrintList {
    pub prints: Vec<UnmatchedPrint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum SendState {
    Queued,
    Sending,
    Done,
    Error,
    Cancelled,
}

/// One send queue item. The artifact path and error text stay server-side.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct QueuedSend {
    pub id: String,
    pub filename: String,
    pub printer_id: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub profile_id: Option<i64>,
    pub state: SendState,
    pub start: bool,
    pub wait_for_idle: bool,
    pub unit_count: u32,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub created_at: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub updated_at: Option<String>,
}

/// `GET /api/v2/printer-send-queue` body, in stored order. A `sending` item
/// stays `sending` here until the send's owner reconciles it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SendQueue {
    pub items: Vec<QueuedSend>,
}

/// GRE-1002's printer vocabulary, as `/ws/printers` sends it. A printer no
/// watcher reads (not set up, manual, awaiting status) is `unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    Idle,
    Printing,
    Paused,
    Complete,
    Error,
    Offline,
    Unknown,
}

/// A loaded filament slot on the stream. Unknown fields are absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamFilament {
    pub slot: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One printer on `GET /ws/printers` (GRE-1014). Unknown fields are absent,
/// never guessed, and no field carries a host address.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamPrinter {
    pub id: String,
    pub name: String,
    pub state: StreamState,
    /// The farm reads' execution, bed-clear and display facts.
    pub display: PrinterStateView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<PrinterReason>,
    /// The watching Checkoff result for the file the printer is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<u8>,
    /// Absent while the bed state is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bed_needs_clearing: Option<bool>,
    pub filaments: Vec<StreamFilament>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heard_at: Option<String>,
}

impl StreamPrinter {
    /// Equal apart from `last_heard_at`, which moves on every poll and is
    /// not a transition.
    pub fn same_state(&self, other: &Self) -> bool {
        let heard = |printer: &Self| Self {
            last_heard_at: None,
            ..printer.clone()
        };
        heard(self) == heard(other)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct StoredPrinter {
    id: String,
    name: String,
    model: String,
    integration_id: Option<String>,
    loaded_filaments: Vec<LoadedFilament>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct StoredIntegration {
    kind: Option<IntegrationKind>,
    enabled: bool,
    /// False for a Moonraker host the watchers never poll: no usable address,
    /// or past their host cap.
    monitorable: bool,
}

/// The tenant's farm settings, parsed once per request.
#[derive(Debug, Clone, PartialEq)]
pub struct Farm {
    printers: Vec<StoredPrinter>,
    integrations: HashMap<String, StoredIntegration>,
    results: Vec<CheckoffResult>,
    unmatched: Vec<OpenPrint>,
    queue: Vec<QueuedSend>,
    /// Read only for match suggestions.
    library: Option<MatchLibrary>,
}

/// An open unmatched print with its raw object names.
#[derive(Debug, Clone, PartialEq)]
struct OpenPrint {
    print: UnmatchedPrint,
    objects: Vec<String>,
}

#[derive(Clone)]
pub struct FarmClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}

pub(crate) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    cancelled: Arc<AtomicBool>,
    match_library: bool,
}

impl WriterOwner {
    pub fn farm_reads(&self, policy: auth::AuthPolicy) -> Result<FarmClient> {
        auth::validate_policy(policy)?;
        ensure!(self.lease.is_some(), "Storage owner is closed");
        Ok(FarmClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}

impl FarmClient {
    pub fn read(
        &self,
        credential: Credential,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Farm> {
        self.submit(credential, cancelled, wait, false)
    }

    /// [`Self::read`] plus the open Builds' parts, Required units, current
    /// Plates and checked-off file names, for [`Farm::suggestions`].
    pub fn read_for_suggestions(
        &self,
        credential: Credential,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Farm> {
        self.submit(credential, cancelled, wait, true)
    }

    fn submit(
        &self,
        credential: Credential,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
        match_library: bool,
    ) -> Result<Farm> {
        let flag = cancelled.clone();
        super::submit_read(&self.shared, &flag, wait, |reply| Envelope::Farm {
            command: Command {
                credential,
                policy: self.policy,
                cancelled,
                match_library,
            },
            reply,
        })
    }
}

pub(crate) fn execute(connection: &mut Connection, command: Command) -> Result<Farm> {
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        "Cancelled before admission"
    );
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let tenant = super::read_tenant(&tx, &command.credential, command.policy)?;
    let list = |key: &str| setting_list(&tx, &tenant, key);
    let mut farm = Farm::parse(
        list("printer.fleet")?,
        list("integrations")?,
        list("printer.checkoff_links")?,
        list("printer.unattributed_prints")?,
        queue_list(&tx, &tenant)?,
    );
    if command.match_library {
        farm.library = Some(MatchLibrary::read(&tx, &tenant, &farm)?);
    }
    farm.name_parts(&tx, &tenant)?;
    tx.commit()?;
    Ok(farm)
}

/// The `default` tenant's farm settings without a caller credential, for the
/// in-process `/ws/printers` producer. Each setting is one reader snapshot.
pub fn read_settings(client: &crate::SettingsClient, wait: Duration) -> Result<Farm> {
    let reader = client.reader(wait)?;
    let list = |key: &str| -> Result<Vec<Value>> {
        match reader.snapshot("default", key)? {
            crate::SettingSnapshot::Missing => Ok(Vec::new()),
            crate::SettingSnapshot::Stored { value } => {
                ensure!(value.len() as i64 <= MAX_SETTING_BYTES, TooLarge);
                Ok(json_list(Some(value)))
            }
        }
    };
    let connection = reader.connection.as_ref().expect("checked out connection");
    let queue = match crate::send_queue::rows(connection, "default")? {
        Some(items) => items,
        None => list(crate::send_queue::LEGACY_KEY)?,
    };
    Ok(Farm::parse(
        list("printer.fleet")?,
        list("integrations")?,
        list("printer.checkoff_links")?,
        list("printer.unattributed_prints")?,
        queue,
    ))
}

/// A JSON array, or empty when missing or unparseable, as Node reads it.
fn json_list(raw: Option<String>) -> Vec<Value> {
    raw.and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| match value {
            Value::Array(items) => Some(items),
            _ => None,
        })
        .unwrap_or_default()
}

/// A stored JSON array, or empty when missing or unparseable, as Node reads it.
/// The send queue: the table's items, or the legacy setting while it exists.
pub(crate) fn queue_list(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<Value>> {
    match crate::send_queue::rows(tx, tenant)? {
        Some(items) => Ok(items),
        None => setting_list(tx, tenant, crate::send_queue::LEGACY_KEY),
    }
}

pub(crate) fn setting_list(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Vec<Value>> {
    let row: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT length(value), CASE WHEN length(value) <= ?3 THEN value END
             FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key, MAX_SETTING_BYTES],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((length, value)) = row else {
        return Ok(Vec::new());
    };
    if length > MAX_SETTING_BYTES {
        bail!(TooLarge);
    }
    Ok(json_list(value))
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A positive safe integer from a JSON number or numeric string, as Node's
/// `Number(...)` plus `Number.isInteger` accepts it.
fn positive_id(value: &Value) -> Option<i64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (number.fract() == 0.0 && number > 0.0 && number <= 9_007_199_254_740_991.0)
        .then_some(number as i64)
}

fn count(value: &Value) -> u32 {
    value
        .as_array()
        .map_or(0, |items| u32::try_from(items.len()).unwrap_or(u32::MAX))
}

/// Nanoseconds since the epoch for an RFC 3339 time, for ordering only.
fn instant(at: Option<&str>) -> Option<i128> {
    time::OffsetDateTime::parse(at?, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(time::OffsetDateTime::unix_timestamp_nanos)
}

/// `at` as Node's `Date.prototype.toISOString` writes it.
pub fn iso_time(at: std::time::SystemTime) -> String {
    let at = time::OffsetDateTime::from(at);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

/// Node's `normalizePrinterFilename`: trimmed basename, lower case.
fn normalized_filename(name: &str) -> String {
    let name = name.trim().replace('\\', "/");
    name.rsplit('/').next().unwrap_or_default().to_lowercase()
}

impl Farm {
    pub(crate) fn parse(
        fleet: Vec<Value>,
        integrations: Vec<Value>,
        links: Vec<Value>,
        unmatched: Vec<Value>,
        queue: Vec<Value>,
    ) -> Self {
        let results: Vec<_> = links.iter().filter_map(result).collect();
        let linked: Vec<_> = results
            .iter()
            .filter(|link| {
                matches!(
                    link.state,
                    CheckoffResultState::Watching
                        | CheckoffResultState::AwaitingVerify
                        | CheckoffResultState::Verified
                )
            })
            .map(|link| {
                (
                    link.integration_id.as_str(),
                    normalized_filename(&link.filename),
                )
            })
            .collect();
        let mut open: Vec<_> = unmatched
            .iter()
            .filter_map(unmatched_print)
            .filter(|OpenPrint { print, .. }| {
                let name = normalized_filename(&print.filename);
                !linked.iter().any(|(integration, file)| {
                    *integration == print.integration_id && *file == name
                })
            })
            .collect();
        open.drain(..open.len().saturating_sub(MAX_UNMATCHED));
        let polled: Option<Vec<String>> =
            crate::printer_hosts::project(Some(&Value::Array(integrations.clone()).to_string()))
                .map(|hosts| {
                    hosts
                        .into_iter()
                        .filter(|host| !host.base_url.trim().is_empty())
                        .map(|host| host.id)
                        .collect()
                });
        Self {
            printers: fleet.iter().filter_map(printer).collect(),
            integrations: integrations
                .iter()
                .filter_map(|item| {
                    let id = item["id"].as_str().filter(|id| !id.is_empty())?;
                    let kind = match item["type"].as_str() {
                        Some("moonraker") => Some(IntegrationKind::Moonraker),
                        Some("prusalink") => Some(IntegrationKind::Prusalink),
                        Some("bambu") => Some(IntegrationKind::Bambu),
                        _ => None,
                    };
                    let enabled = item["config"]["enabled"] != Value::Bool(false);
                    let monitorable = kind != Some(IntegrationKind::Moonraker)
                        || polled
                            .as_ref()
                            .is_none_or(|ids| ids.iter().any(|known| known == id));
                    Some((
                        id.to_owned(),
                        StoredIntegration {
                            kind,
                            enabled,
                            monitorable,
                        },
                    ))
                })
                .collect(),
            results,
            unmatched: open,
            queue: queue.iter().filter_map(queued_send).collect(),
            library: None,
        }
    }

    /// Fills each result unit's Part file name from the tenant's `parts` rows,
    /// in the read's own transaction. A Part that is gone, or has an empty file
    /// name, stays null.
    fn name_parts(&mut self, tx: &Transaction<'_>, tenant: &str) -> Result<()> {
        let mut names: HashMap<i64, Option<String>> = HashMap::new();
        let mut query = tx.prepare("SELECT filename FROM parts WHERE tenant_id=?1 AND id=?2")?;
        for unit in self
            .results
            .iter_mut()
            .flat_map(|result| result.units.iter_mut())
        {
            unit.part_filename = match names.entry(unit.part_id) {
                std::collections::hash_map::Entry::Occupied(known) => known.get().clone(),
                std::collections::hash_map::Entry::Vacant(slot) => slot
                    .insert(
                        query
                            .query_row(params![tenant, unit.part_id], |row| row.get::<_, String>(0))
                            .optional()?
                            .filter(|name| !name.is_empty()),
                    )
                    .clone(),
            };
        }
        Ok(())
    }

    pub fn checkoff(&self) -> CheckoffResultList {
        CheckoffResultList {
            results: self.results.clone(),
        }
    }

    pub fn unmatched(&self) -> UnmatchedPrintList {
        UnmatchedPrintList {
            prints: self
                .unmatched
                .iter()
                .map(|open| open.print.clone())
                .collect(),
        }
    }

    /// Match suggestions for the open unmatched prints; `None` unless the
    /// farm was read with [`FarmClient::read_for_suggestions`].
    pub fn suggestions(&self) -> Option<MatchSuggestionList> {
        self.library.as_ref().map(|library| self.suggest(library))
    }

    pub fn send_queue(&self) -> SendQueue {
        SendQueue {
            items: self.queue.clone(),
        }
    }

    /// Each configured printer, with `observe` answering for its integration.
    /// `observe` is called only for an enabled printer integration.
    pub fn printers(
        &self,
        observe: impl Fn(&PrinterIntegration) -> Observation,
    ) -> FarmPrinterList {
        FarmPrinterList {
            printers: self
                .project(observe)
                .into_iter()
                .map(|(printer, _)| printer)
                .collect(),
        }
    }

    /// Each configured printer as `/ws/printers` sends it.
    pub fn stream_printers(
        &self,
        observe: impl Fn(&PrinterIntegration) -> Observation,
    ) -> Vec<StreamPrinter> {
        self.project(observe)
            .into_iter()
            .map(|(printer, host)| {
                let (state, reported) = match host {
                    Some((state, reported)) => (state, reported),
                    None => (HostState::Unknown, None),
                };
                let running = matches!(state, HostState::Printing | HostState::Paused);
                let result_id = self
                    .results
                    .iter()
                    .find(|result| {
                        running
                            && result.printer_id == printer.id
                            && result.state == CheckoffResultState::Watching
                            && reported.as_deref().map(normalized_filename)
                                == Some(normalized_filename(&result.filename))
                    })
                    .map(|result| result.id.clone());
                StreamPrinter {
                    state: match state {
                        HostState::Idle => StreamState::Idle,
                        HostState::Printing => StreamState::Printing,
                        HostState::Paused => StreamState::Paused,
                        HostState::Complete => StreamState::Complete,
                        HostState::Error => StreamState::Error,
                        HostState::Offline => StreamState::Offline,
                        HostState::Unknown => StreamState::Unknown,
                    },
                    display: printer.state,
                    reason: printer.reason,
                    result_id,
                    progress_percent: printer.progress_percent,
                    bed_needs_clearing: match printer.state.bed_clear() {
                        BedClearState::Clear => Some(false),
                        BedClearState::NeedsClearing => Some(true),
                        BedClearState::Unknown => None,
                    },
                    filaments: printer
                        .loaded_filaments
                        .into_iter()
                        .map(|slot| StreamFilament {
                            slot: slot.slot,
                            color_id: slot.filament_color_id,
                            label: Some(slot.label).filter(|label| !label.is_empty()),
                        })
                        .collect(),
                    last_heard_at: printer.last_heard_at,
                    id: printer.id,
                    name: printer.name,
                }
            })
            .collect()
    }

    /// Each printer with the host state and reported file it was observed in.
    fn project(&self, observe: impl Fn(&PrinterIntegration) -> Observation) -> Vec<Projected> {
        self.printers
            .iter()
            .map(|printer| {
                let integration = printer.integration_id.as_ref().and_then(|id| {
                    let kind = self.integrations.get(id)?.kind?;
                    Some(PrinterIntegration {
                        id: id.clone(),
                        kind,
                    })
                });
                let observation = integration
                    .as_ref()
                    .map(|known| (known, &self.integrations[&known.id]))
                    .filter(|(_, stored)| stored.enabled)
                    .map(|(known, stored)| {
                        if stored.monitorable {
                            observe(known)
                        } else {
                            Observation::NotSupported
                        }
                    });
                let (execution, reason) = execution(printer, &integration, observation.as_ref());
                let (host, progress_percent, last_heard_at) = match &observation {
                    Some(Observation::Seen {
                        state,
                        progress_percent,
                        filename,
                        last_heard_at,
                        state_since,
                    }) => (
                        Some(Host {
                            state: *state,
                            filename: filename.as_deref(),
                            since: instant(state_since.as_deref()),
                        }),
                        *progress_percent,
                        last_heard_at.clone(),
                    ),
                    _ => (None, None, None),
                };
                let active = matches!(
                    execution,
                    PrinterExecutionState::Printing | PrinterExecutionState::Paused
                );
                let farm_printer = FarmPrinter {
                    id: printer.id.clone(),
                    name: printer.name.clone(),
                    model: printer.model.clone(),
                    state: PrinterStateView::new(
                        execution,
                        self.bed_clear(&printer.id, execution, host),
                    ),
                    integration,
                    reason,
                    last_heard_at,
                    progress_percent: progress_percent.filter(|_| active),
                    loaded_filaments: printer.loaded_filaments.clone(),
                };
                let host = host.map(|host| (host.state, host.filename.map(str::to_owned)));
                (farm_printer, host)
            })
            .collect()
    }

    /// Only a verified result (an operator confirmation) makes a bed clear,
    /// and only when no later finished print is waiting on it. A printer that
    /// reports `complete` is clear only when its latest verified result is for
    /// the file it reports and was applied after the host entered `complete`:
    /// a job run outside Print Partner has no result, and a direct reprint of
    /// the verified file completes after that confirmation.
    fn bed_clear(
        &self,
        printer: &str,
        execution: PrinterExecutionState,
        host: Option<Host<'_>>,
    ) -> BedClearState {
        let state = host.as_ref().map(|host| host.state);
        if matches!(
            execution,
            PrinterExecutionState::Printing | PrinterExecutionState::Paused
        ) || state == Some(HostState::Error)
        {
            return BedClearState::NeedsClearing;
        }
        let mut finished = None;
        let mut cleared = None;
        let mut confirmed = None;
        for result in self.results.iter().filter(|r| r.printer_id == printer) {
            match result.state {
                CheckoffResultState::Verified => {
                    finished = finished.max(instant(result.completed_at.as_deref()));
                    let applied = instant(result.applied_at.as_deref());
                    if applied.is_some() && applied >= cleared {
                        cleared = applied;
                        confirmed = Some(normalized_filename(&result.filename));
                    }
                }
                CheckoffResultState::AwaitingVerify | CheckoffResultState::HostFailed => {
                    finished = finished.max(
                        instant(result.completed_at.as_deref())
                            .or_else(|| instant(Some(&result.created_at)))
                            .or(Some(i128::MAX)),
                    );
                }
                CheckoffResultState::Watching | CheckoffResultState::Dismissed => {}
            }
        }
        for print in self
            .unmatched
            .iter()
            .map(|open| &open.print)
            .filter(|p| p.printer_id == printer)
        {
            finished = finished.max(instant(Some(&print.completed_at)).or(Some(i128::MAX)));
        }
        if let Some(host) = host.filter(|host| host.state == HostState::Complete)
            && (confirmed.is_none_or(|file| host.filename.map(normalized_filename) != Some(file))
                || host.since.is_none_or(|since| cleared < Some(since)))
        {
            return BedClearState::NeedsClearing;
        }
        match (finished, cleared) {
            (Some(finished), Some(cleared)) if cleared >= finished => BedClearState::Clear,
            (Some(_), _) => BedClearState::NeedsClearing,
            (None, Some(_)) => BedClearState::Clear,
            (None, None) => BedClearState::Unknown,
        }
    }
}

/// A farm printer with the host state and reported file it was observed in.
type Projected = (FarmPrinter, Option<(HostState, Option<String>)>);

/// One printer's observed host state, for [`Farm::bed_clear`].
#[derive(Clone, Copy)]
struct Host<'a> {
    state: HostState,
    filename: Option<&'a str>,
    since: Option<i128>,
}

fn execution(
    printer: &StoredPrinter,
    integration: &Option<PrinterIntegration>,
    observation: Option<&Observation>,
) -> (PrinterExecutionState, Option<PrinterReason>) {
    use PrinterExecutionState as E;
    let Some(observation) = observation else {
        return match (&printer.integration_id, integration) {
            (None, _) => (E::NotSetUp, Some(PrinterReason::NotSetUp)),
            (Some(_), None) => (E::NotSetUp, Some(PrinterReason::IntegrationMissing)),
            (Some(_), Some(_)) => (E::Manual, Some(PrinterReason::MonitoringDisabled)),
        };
    };
    match observation {
        Observation::NotWatched | Observation::NotSupported => {
            (E::Manual, Some(PrinterReason::MonitoringNotSupported))
        }
        Observation::Unavailable => (E::Unknown, Some(PrinterReason::AwaitingStatus)),
        Observation::Seen { state, .. } => match state {
            HostState::Printing => (E::Printing, None),
            HostState::Paused => (E::Paused, None),
            HostState::Idle | HostState::Complete => (E::Idle, None),
            HostState::Error => (E::Failed, None),
            HostState::Offline => (E::Unreachable, Some(PrinterReason::Unreachable)),
            HostState::Unknown => (E::Unknown, None),
        },
    }
}

/// Node's `parsePrinterMachine` and `ensureSlots`.
fn printer(value: &Value) -> Option<StoredPrinter> {
    let id = match &value["id"] {
        Value::String(id) if !id.is_empty() => id.clone(),
        Value::Number(id) => id.to_string(),
        _ => return None,
    };
    let name = value["name"].as_str().unwrap_or("Printer").to_owned();
    let model = value["model"].as_str().unwrap_or(&name).to_owned();
    let slots = value["max_filament_slots"]
        .as_f64()
        .map_or(1, |slots| slots.clamp(1.0, 4.0) as u8);
    let stored = value["loaded_filaments"].as_array();
    let loaded_filaments = (1..=slots)
        .map(|slot| {
            let row = stored.and_then(|rows| {
                rows.iter()
                    .rev()
                    .find(|row| row["slot"].as_f64() == Some(f64::from(slot)))
            });
            LoadedFilament {
                slot,
                filament_color_id: row
                    .and_then(|row| row["filament_color_id"].as_str())
                    .map(str::to_owned),
                label: row
                    .and_then(|row| row["label"].as_str())
                    .unwrap_or_default()
                    .to_owned(),
            }
        })
        .collect();
    Some(StoredPrinter {
        id,
        name,
        model,
        integration_id: text(value, "integration_id"),
        loaded_filaments,
    })
}

fn result(value: &Value) -> Option<CheckoffResult> {
    let state = match value["state"].as_str()? {
        "watching" => CheckoffResultState::Watching,
        "awaiting_verify" => CheckoffResultState::AwaitingVerify,
        "host_failed" => CheckoffResultState::HostFailed,
        "dismissed" => CheckoffResultState::Dismissed,
        "verified" | "applied" => CheckoffResultState::Verified,
        _ => return None,
    };
    let host_outcome = match (value["host_outcome"].as_str(), state) {
        (Some("unknown"), _) => Some(HostOutcome::Unknown),
        (Some("success"), _) => Some(HostOutcome::Success),
        (Some("failed"), _) => Some(HostOutcome::Failed),
        (Some("cancelled"), _) => Some(HostOutcome::Cancelled),
        (_, CheckoffResultState::Verified | CheckoffResultState::AwaitingVerify) => {
            Some(HostOutcome::Success)
        }
        (_, CheckoffResultState::HostFailed) => Some(HostOutcome::Failed),
        _ => None,
    };
    Some(CheckoffResult {
        id: text(value, "id")?,
        profile_id: positive_id(&value["profile_id"])?,
        printer_id: text(value, "printer_id")?,
        integration_id: text(value, "integration_id")?,
        filename: text(value, "filename")?,
        state,
        host_outcome,
        unit_count: count(&value["units"]),
        resolved_unit_count: count(&value["resolved_units"]),
        units: entries(value, "units")
            .filter_map(|unit| {
                let (part_id, unit_index) = unit_key(unit)?;
                Some(CheckoffUnit {
                    part_id,
                    unit_index,
                    part_filename: None,
                })
            })
            .collect(),
        resolved_units: entries(value, "resolved_units")
            .filter_map(|unit| {
                let (part_id, unit_index) = unit_key(unit)?;
                let result = match unit["result"].as_str()? {
                    "confirmed" => UnitResult::Confirmed,
                    "rejected" => UnitResult::Rejected,
                    _ => return None,
                };
                Some(ResolvedUnit {
                    part_id,
                    unit_index,
                    result,
                })
            })
            .collect(),
        created_at: text(value, "created_at")?,
        completed_at: text(value, "completed_at").or_else(|| text(value, "applied_at")),
        applied_at: text(value, "applied_at"),
    })
}

fn entries<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

/// A unit's Part ID and index, when both are whole numbers in range.
fn unit_key(unit: &Value) -> Option<(i64, u32)> {
    let index = u32::try_from(unit["unit_index"].as_u64()?).ok()?;
    Some((positive_id(&unit["part_id"])?, index))
}

/// Open entries only: not claimed and not dismissed.
fn unmatched_print(value: &Value) -> Option<OpenPrint> {
    if text(value, "claimed_at").is_some() || value["dismissed"] == Value::Bool(true) {
        return None;
    }
    let print = UnmatchedPrint {
        id: text(value, "id")?,
        printer_id: text(value, "printer_id")?,
        integration_id: text(value, "integration_id")?,
        filename: text(value, "filename")?,
        completed_at: text(value, "completed_at")?,
        object_count: count(&value["gcode_objects"]),
        candidate_count: count(&value["candidates"]),
    };
    let objects = value["gcode_objects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Some(OpenPrint { print, objects })
}

/// Node's `parseItem`, without the artifact path and error text.
fn queued_send(value: &Value) -> Option<QueuedSend> {
    let state = match value["state"].as_str()? {
        "queued" => SendState::Queued,
        "sending" => SendState::Sending,
        "done" => SendState::Done,
        "error" => SendState::Error,
        "cancelled" => SendState::Cancelled,
        _ => return None,
    };
    text(value, "artifact_path")?;
    Some(QueuedSend {
        id: text(value, "id")?,
        filename: text(value, "filename")?,
        printer_id: text(value, "printer_id")?,
        profile_id: positive_id(&value["profile_id"]),
        state,
        start: value["start"].as_bool().unwrap_or(false),
        wait_for_idle: value["wait_for_idle"] != Value::Bool(false),
        unit_count: count(&value["checkoff_units"]),
        created_at: text(value, "created_at"),
        updated_at: text(value, "updated_at"),
    })
}

mod suggestions;
pub use suggestions::{
    FileNameEvidence, MatchBasis, MatchConfidence, MatchEvidence, MatchLibrary, MatchSuggestion,
    MatchSuggestionList, ObjectListEvidence, SuggestedBuild, SuggestedPlate, TIME_WINDOW_HOURS,
    TimeWindowEvidence,
};

#[cfg(test)]
mod tests;
