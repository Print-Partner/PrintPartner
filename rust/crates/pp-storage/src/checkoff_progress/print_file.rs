//! The print-file side of `POST /printer-checkoff/file-assignments` and its
//! `/preview` (`routes/printer-checkoff.ts`), behind `desktop_rebuild`
//! (GRE-1220).
//!
//! [`printer`] is Node's `resolveAssignmentPrinter`: the printer a print is
//! recorded against and what watches it. [`preview`] is
//! `suggestPrintFileAttribution` over the Build's Accepted Plan. Both only
//! read.
//!
//! [`SnapshotStore`] is Node's `services/printer-file-snapshots.ts`: the
//! bytes a client pulled through `GET /printers/{id}/files/content?profile_id=`
//! classified at EOF and held under a token for 15 minutes, so the assignment
//! records what the operator previewed without reading the printer again. The
//! store lives in process memory. A token is bound to the Build, printer,
//! remote path and file name it was captured for, and to a fingerprint of the
//! printer's linked integration ([`file_source`]), so a changed host
//! invalidates it.

use super::{
    Assignment, Body, Outcome, Response,
    attribution::{self, Mapping, Unit},
    links,
};
use crate::read_model::farm::setting_list;
use anyhow::Result;
pub use pp_domain::print_file::Classification;
use pp_domain::print_file::{MAX_CLASSIFIABLE_BYTES, Rejection, classify};
use rusqlite::Transaction;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

const FLEET_KEY: &str = "printer.fleet";
const INTEGRATIONS_KEY: &str = "integrations";
/// `UNMANAGED_PRINTER_ID` and `UNMANAGED_PRINTER_NAME` in `@print-partner/contracts`.
const UNMANAGED_PRINTER_ID: &str = "unmanaged";
const UNMANAGED_PRINTER_NAME: &str = "A printer PrintPartner does not manage";

/// `manualIntegrationId`.
pub fn manual_integration_id(printer_id: &str) -> String {
    format!("manual:{printer_id}")
}

/// The request's `tracking`: `"host"`, `"manual"`, or anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    Host,
    Manual,
    Default,
}

impl Tracking {
    pub fn parse(raw: Option<&Value>) -> Self {
        match raw.and_then(Value::as_str) {
            Some("host") => Self::Host,
            Some("manual") => Self::Manual,
            _ => Self::Default,
        }
    }
}

/// The printer a print-file request is recorded against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePrinter {
    pub printer_id: String,
    pub printer_name: String,
    pub integration_id: String,
    /// The fingerprint a snapshot of this printer's files must carry, or
    /// `None` when the printer has no available linked host.
    pub source: Option<String>,
    /// Whether Node reads print files off `integration_id`: a Moonraker or
    /// PrusaLink host, never a manual one.
    pub host_files: bool,
}

/// The fleet row with `String(id) === printer_id`. A `null` row, or a `null`
/// entry in a row's `loaded_filaments`, makes Node's `loadFleet` throw and
/// read as an empty fleet.
fn fleet_row<'a>(fleet: &'a [Value], printer_id: &str) -> Option<&'a Value> {
    let throws = |row: &Value| {
        row.is_null()
            || row["loaded_filaments"]
                .as_array()
                .is_some_and(|slots| slots.iter().any(Value::is_null))
    };
    if fleet.iter().any(throws) {
        return None;
    }
    fleet
        .iter()
        .find(|row| links::js_string(row.get("id").unwrap_or(&json!("undefined"))) == printer_id)
}

/// `parsePrinterMachine`'s `integration_id`: trimmed, absent when blank.
fn linked_id(row: &Value) -> Option<String> {
    let raw = row.get("integration_id").filter(|value| !value.is_null())?;
    let id = links::js_trim(&links::js_string(raw)).to_owned();
    (!id.is_empty()).then_some(id)
}

/// `getIntegrationConfig` when the integration is not disabled.
fn available<'a>(integrations: &'a [Value], id: &str) -> Option<&'a Value> {
    integrations
        .iter()
        .find(|item| item["id"].as_str() == Some(id))
        .filter(|item| item["config"]["enabled"] != Value::Bool(false))
}

/// Node's `printerFileSnapshotSource` over raw setting rows.
fn source(fleet: &[Value], integrations: &[Value], printer_id: &str) -> Option<String> {
    let integration = available(integrations, &linked_id(fleet_row(fleet, printer_id)?)?)?;
    Some(hex::encode(Sha256::digest(integration.to_string())))
}

/// The snapshot fingerprint of `printer_id`'s linked host, read the way the
/// assignment route reads it. The files/content route passes it to
/// [`SnapshotStore::begin`].
pub fn file_source(
    client: &crate::SettingsClient,
    wait: Duration,
    printer_id: &str,
) -> Result<Option<String>> {
    let reader = client.reader(wait)?;
    let list = |key: &str| -> Result<Vec<Value>> {
        Ok(match reader.snapshot("default", key)? {
            crate::SettingSnapshot::Stored { value } => serde_json::from_str::<Value>(&value)
                .ok()
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default(),
            crate::SettingSnapshot::Missing => Vec::new(),
        })
    };
    Ok(source(
        &list(FLEET_KEY)?,
        &list(INTEGRATIONS_KEY)?,
        printer_id,
    ))
}

fn invalid(detail: &str) -> Response {
    Response::failure(400, detail, None)
}

/// JavaScript's `.length` of a string, in UTF-16 units.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// A trimmed string field cut to `max` UTF-16 units, absent when blank or
/// not a string.
fn source_field(body: &Value, key: &str, max: usize) -> Option<String> {
    let text = links::js_trim(body.get(key)?.as_str()?);
    (!text.is_empty()).then(|| links::js_slice(text, max))
}

/// Which route a request is parsed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRoute {
    Preview,
    Assign,
}

/// What the assignment route parses beyond the shared print-file request.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignInput {
    pub plan_revision_id: i64,
    pub units: Vec<Unit>,
    pub mappings: Option<Vec<Mapping>>,
    pub retain_unmatched: bool,
    pub completed: bool,
}

/// Node's `parsePrintFileRequest` with the printer resolved, plus the
/// preview's suggestion or the assignment's own fields.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRequest {
    pub profile_id: i64,
    pub printer: FilePrinter,
    pub filename: String,
    /// Where the bytes came from: one source or none, never two.
    pub remote_path: Option<String>,
    pub upload_token: Option<String>,
    pub snapshot_token: Option<String>,
    pub object_names: Vec<String>,
    pub suggestion: Option<Value>,
    pub assign: Option<AssignInput>,
}

impl FileRequest {
    /// The scope a snapshot for this request must have been captured for.
    pub fn scope(&self) -> Option<Scope> {
        Some(Scope {
            profile_id: self.profile_id,
            printer_id: self.printer.printer_id.clone(),
            remote_path: self.remote_path.clone()?,
            filename: self.filename.clone(),
        })
    }
}

/// A print-file request for the writer.
#[derive(Debug)]
pub(super) enum Op {
    Assign(Box<Assignment>),
    Request { body: Value, route: FileRoute },
}

pub(super) fn run(tx: &Transaction<'_>, tenant: &str, op: Op, repos: &Path) -> Outcome {
    match op {
        Op::Assign(assignment) => {
            if let Some(expected) = &assignment.snapshot_source {
                let fleet = setting_list(tx, tenant, FLEET_KEY)?;
                let integrations = setting_list(tx, tenant, INTEGRATIONS_KEY)?;
                let current = source(&fleet, &integrations, &assignment.link.printer_id);
                if current.as_ref() != Some(expected) {
                    return Ok(Ok(Response::failure(409, SNAPSHOT_EXPIRED, None)));
                }
            }
            super::assign::assign(tx, tenant, *assignment, repos)
        }
        Op::Request { body, route } => request(tx, tenant, &body, route, repos),
    }
}

/// Parse a preview or assignment body in Node's order: the shared request
/// (`parsePrintFileRequest`, which resolves the printer), then the preview's
/// Accepted Plan or the assignment's revision, units and mappings.
fn request(
    tx: &Transaction<'_>,
    tenant: &str,
    body: &Value,
    route: FileRoute,
    repos: &Path,
) -> Outcome {
    let refused = |response| Ok(Ok(response));
    let Some(profile_id) = links::js_integer(body.get("profile_id"), 1.0) else {
        return refused(invalid("profile_id is required"));
    };
    let filename = body["filename"]
        .as_str()
        .map(links::js_trim)
        .unwrap_or_default();
    if filename.is_empty() || js_len(filename) > 500 {
        return refused(invalid("filename is required"));
    }
    let lower = filename.to_lowercase();
    if ![".gcode", ".gco", ".bgcode", ".3mf"]
        .iter()
        .any(|extension| lower.ends_with(extension))
    {
        return refused(invalid(
            "Choose a .gcode, .gco, .bgcode, or .3mf print file",
        ));
    }
    let (remote_path, upload_token, snapshot_token) = match sources(body) {
        Ok(sources) => sources,
        Err(response) => return refused(response),
    };
    let printer_id = body["printer_id"]
        .as_str()
        .map(links::js_trim)
        .unwrap_or_default();
    let printer = match printer(
        tx,
        tenant,
        printer_id,
        Tracking::parse(body.get("tracking")),
        remote_path.is_some(),
    )? {
        Ok(printer) => printer,
        Err(response) => return refused(response),
    };
    let names = body["object_names"].as_array();
    if names.is_some_and(|names| {
        names.len() > 500
            || names.iter().any(|name| {
                name.as_str()
                    .is_none_or(|name| js_len(links::js_trim(name)) > 200)
            })
    }) {
        return refused(invalid(
            "Import supports at most 500 object names of up to 200 characters; no objects were imported",
        ));
    }
    let object_names: Vec<String> = names
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|name| links::js_slice(links::js_trim(name), 200))
        .filter(|name| !name.is_empty())
        .take(500)
        .collect();
    let mut parsed = FileRequest {
        profile_id,
        printer,
        filename: filename.to_owned(),
        remote_path,
        upload_token,
        snapshot_token,
        object_names,
        suggestion: None,
        assign: None,
    };
    match route {
        FileRoute::Preview => {
            match preview(
                tx,
                tenant,
                profile_id,
                &parsed.object_names,
                &parsed.filename,
                repos,
            )? {
                Ok(Ok(suggestion)) => parsed.suggestion = Some(suggestion),
                Ok(Err(response)) => return refused(response),
                Err(refusal) => return Ok(Err(refusal)),
            }
        }
        FileRoute::Assign => match assign_input(body) {
            Ok(input) => parsed.assign = Some(input),
            Err(response) => return refused(response),
        },
    }
    Ok(Ok(Response::ok(Body::FileRequest(Box::new(parsed)))))
}

/// The file's source: `remote_path`, `upload_token`, and a `snapshot_token`
/// that only a `remote_path` may carry.
type Sources = (Option<String>, Option<String>, Option<String>);

fn sources(body: &Value) -> std::result::Result<Sources, Response> {
    let remote_path = source_field(body, "remote_path", 1_000);
    let upload_token = source_field(body, "upload_token", 200);
    if remote_path.is_some() && upload_token.is_some() {
        return Err(invalid("Send remote_path or upload_token, not both"));
    }
    let Some(raw) = body.get("snapshot_token") else {
        return Ok((remote_path, upload_token, None));
    };
    let token = raw.as_str().map(|token| links::js_trim(token).to_owned());
    match token.filter(|token| !token.is_empty()) {
        Some(token) if remote_path.is_some() => Ok((remote_path, upload_token, Some(token))),
        _ => Err(invalid(
            "A snapshot token requires its original printer file path",
        )),
    }
}

/// `parsePrinterCheckoffUnitToken`: `<part_id>:<unit_index>`.
fn unit_token(raw: &str) -> Option<Unit> {
    let mut fields = links::js_trim(raw).split(':');
    let (part, index) = (fields.next()?, fields.next()?);
    if fields.next().is_some() || part.is_empty() {
        return None;
    }
    let number = |text: &str, min| links::js_integer(Some(&Value::String(text.into())), min);
    Some(Unit::at(number(part, 1.0)?, number(index, 0.0)?))
}

/// A JSON number that is an integer of at least `min`, as Node's
/// `typeof value === "number" && Number.isInteger(value)` checks it.
fn whole(value: &Value, min: f64) -> Option<i64> {
    value
        .is_number()
        .then(|| links::js_integer(Some(value), min))
        .flatten()
}

/// The assignment route's checks after the shared request.
fn assign_input(body: &Value) -> std::result::Result<AssignInput, Response> {
    let plan_revision_id = links::js_integer(body.get("plan_revision_id"), 1.0)
        .ok_or_else(|| invalid("plan_revision_id is required"))?;
    let tokens = body["unit_tokens"].as_array().ok_or_else(|| {
        invalid("unit_tokens is required; confirm the Required units this file produces")
    })?;
    if tokens.len() > 500 {
        return Err(invalid("At most 500 units can be assigned at once"));
    }
    let units = tokens
        .iter()
        .map(|token| token.as_str().and_then(unit_token))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| invalid("unit_tokens holds an unreadable unit"))?;
    let mut mappings = match body.get("object_mappings") {
        None => None,
        Some(Value::Array(entries)) if entries.len() <= 500 => Some(
            entries
                .iter()
                .map(|entry| {
                    let object_index = whole(entry.get("object_index")?, 0.0)?;
                    let part_id = whole(entry.get("part_id")?, 1.0)?;
                    let unit_index = whole(entry.get("unit_index")?, 0.0)?;
                    Some(Mapping {
                        object_index: usize::try_from(object_index).ok()?,
                        unit: Unit::at(part_id, unit_index),
                    })
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| invalid("object_mappings contains an invalid choice"))?,
        ),
        Some(_) => {
            return Err(invalid(
                "object_mappings must be a list of at most 500 choices",
            ));
        }
    };
    let retain_unmatched = body["retain_unmatched"] == Value::Bool(true);
    if retain_unmatched {
        mappings.get_or_insert_with(Vec::new);
    }
    Ok(AssignInput {
        plan_revision_id,
        units,
        mappings,
        retain_unmatched,
        completed: body["completed"] == Value::Bool(true),
    })
}

/// `resolveAssignmentPrinter`. No printer means one PrintPartner does not
/// manage, which nothing watches and which holds no stored file.
fn printer(
    tx: &Transaction<'_>,
    tenant: &str,
    printer_id: &str,
    tracking: Tracking,
    from_storage: bool,
) -> Result<std::result::Result<FilePrinter, Response>> {
    if printer_id.is_empty() {
        if tracking == Tracking::Host {
            return Ok(Err(invalid(
                "PrintPartner cannot watch a printer it does not manage. Name the printer that made this print, or record the print as already made.",
            )));
        }
        if from_storage {
            return Ok(Err(invalid(
                "A file held on a printer needs the printer it sits on. Name that printer, or upload the file instead.",
            )));
        }
        return Ok(Ok(FilePrinter {
            printer_id: UNMANAGED_PRINTER_ID.into(),
            printer_name: UNMANAGED_PRINTER_NAME.into(),
            integration_id: manual_integration_id(UNMANAGED_PRINTER_ID),
            source: None,
            host_files: false,
        }));
    }
    let fleet = setting_list(tx, tenant, FLEET_KEY)?;
    let integrations = setting_list(tx, tenant, INTEGRATIONS_KEY)?;
    let Some(row) = fleet_row(&fleet, printer_id) else {
        return Ok(Err(Response::failure(404, "Printer not found", None)));
    };
    let printer_name = links::js_string(
        row.get("name")
            .filter(|name| !name.is_null())
            .unwrap_or(&json!("Printer")),
    );
    let (integration_id, host_files) = if tracking == Tracking::Manual {
        (manual_integration_id(printer_id), false)
    } else {
        let linked = linked_id(row)
            .and_then(|id| Some((available(&integrations, &id)?["type"].as_str(), id)));
        match linked {
            Some((kind, id)) => (id, matches!(kind, Some("moonraker" | "prusalink"))),
            None => {
                return Ok(Err(Response::failure(
                    409,
                    "Use manual tracking because this printer has no available host",
                    None,
                )));
            }
        }
    };
    Ok(Ok(FilePrinter {
        printer_id: printer_id.to_owned(),
        printer_name,
        integration_id,
        source: source(&fleet, &integrations, printer_id),
        host_files,
    }))
}

/// `suggestPrintFileAttribution` against the Build's Accepted Plan, or
/// `materializeProblem`'s answer for why there is none.
fn preview(
    tx: &Transaction<'_>,
    tenant: &str,
    profile_id: i64,
    object_names: &[String],
    filename: &str,
    repos: &Path,
) -> Result<std::result::Result<std::result::Result<Value, Response>, super::Refusal>> {
    let snapshot = match attribution::accepted(tx, tenant, profile_id, repos)? {
        Ok(snapshot) => snapshot,
        Err(Ok(response)) => return Ok(Ok(Err(response))),
        Err(Err(refusal)) => return Ok(Err(refusal)),
    };
    let suggestion = attribution::resolve(&snapshot, object_names, Some(filename), true);
    let suggested = attribution::confirm(&snapshot, &suggestion.units)
        .unwrap_or_else(|| suggestion.units.clone());
    let basis = if suggestion.units.is_empty() {
        "none"
    } else if suggestion.fallback == "used" {
        "filename"
    } else {
        "object_names"
    };
    let notices: Vec<String> = suggestion
        .outcomes
        .iter()
        .filter_map(|outcome| match outcome.kind {
            "already_completed" => Some(format!("{} is already checked off.", outcome.raw_name)),
            "duplicate_observation" => Some(format!(
                "{} repeats the same identified unit; it is not another copy.",
                outcome.raw_name
            )),
            _ => None,
        })
        .collect();
    let objects: Vec<Value> = suggestion
        .outcomes
        .iter()
        .filter(|outcome| matches!(outcome.kind, "unmatched" | "ambiguous_filename"))
        .map(|outcome| json!({ "object_index": outcome.input_index, "name": outcome.raw_name }))
        .collect();
    let parts: Vec<Value> = snapshot
        .parts
        .iter()
        .filter(|part| part.included && part.units.iter().any(|unit| unit.required))
        .map(|part| {
            let units: Vec<Value> = part
                .units
                .iter()
                .filter(|unit| unit.required && !unit.completed)
                .map(|unit| {
                    json!({
                        "part_id": part.projection_part_id,
                        "unit_index": unit.unit_index,
                        "object_name": unit.object_name,
                    })
                })
                .collect();
            json!({
                "part_id": part.projection_part_id,
                "filename": part.filename,
                "relative_path": if part.relative_path.is_empty() { &part.filename } else { &part.relative_path },
                "source_label": part.source_layer,
                "units": units,
            })
        })
        .collect();
    Ok(Ok(Ok(json!({
        "suggested_units": suggested,
        "suggestion_basis": basis,
        "unlabeled_names": suggestion.unmatched_object_names,
        "plan_revision_id": snapshot.revision_id,
        "match_review": { "notices": notices, "objects": objects, "parts": parts },
    }))))
}

/// What a snapshot is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub profile_id: i64,
    pub printer_id: String,
    pub remote_path: String,
    pub filename: String,
}

/// What PrintPartner learned from a print file's bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspected {
    pub classification: Classification,
    /// The provider identity plus `size_bytes` and `sha256` of the bytes.
    pub identity: Value,
}

struct Saved {
    scope: Scope,
    source: String,
    expires: Instant,
    inspected: Inspected,
}

#[derive(Default)]
struct Held {
    /// One capture at a time, as Node allows.
    active: bool,
    ready: HashMap<String, Saved>,
    /// Snapshots an assignment holds; they still count toward the limit.
    reserved: usize,
}

/// Node keeps a token for 15 minutes and at most 128 at once.
const TTL: Duration = Duration::from_secs(15 * 60);
const MAX_READY: usize = 128;

/// Print-file snapshots captured by the files/content route and redeemed by
/// the assignment route. One store per process, shared by both.
#[derive(Default)]
pub struct SnapshotStore(Mutex<Held>);

/// Node's answer when a `snapshot_token` does not redeem.
pub const SNAPSHOT_EXPIRED: &str =
    "This file preview expired or its source changed. Cancel and reopen the file.";

/// Node's capture refusal.
pub const CAPTURE_UNAVAILABLE: &str =
    "File inspection unavailable. Finish the current file, then reopen this file.";

impl SnapshotStore {
    fn held(&self) -> MutexGuard<'_, Held> {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        held.ready.retain(|_, saved| saved.expires > now);
        held
    }

    /// Start capturing the bytes a client pulls for `scope`. `source` is
    /// [`file_source`] read before the printer was asked for the file, and
    /// `current` is the same read now; a capture needs both, equal. Refused
    /// while another capture runs or 128 tokens are held.
    pub fn begin(
        self: &Arc<Self>,
        scope: Scope,
        identity: Value,
        source: Option<String>,
        current: Option<&str>,
    ) -> std::result::Result<Capture, &'static str> {
        let mut held = self.held();
        let source = source.filter(|source| Some(source.as_str()) == current);
        let (Some(source), false, true) = (
            source,
            held.active,
            held.ready.len() + held.reserved < MAX_READY,
        ) else {
            return Err(CAPTURE_UNAVAILABLE);
        };
        held.active = true;
        Ok(Capture {
            store: Arc::clone(self),
            token: links::uuid(),
            scope,
            identity,
            source,
            bytes: Vec::new(),
            done: false,
        })
    }

    /// The inspection captured under `token` for exactly `scope`, while the
    /// printer's host still has fingerprint `current`.
    pub fn read(&self, token: &str, scope: &Scope, current: Option<&str>) -> Option<Inspected> {
        let held = self.held();
        let saved = held.ready.get(token)?;
        (Some(saved.source.as_str()) == current && saved.scope == *scope)
            .then(|| saved.inspected.clone())
    }

    /// Take the token for one assignment, as [`Self::read`] matches it. No
    /// other request can read or take it until the reservation is spent or
    /// dropped; dropping it puts the snapshot back.
    pub fn reserve(
        self: &Arc<Self>,
        token: &str,
        scope: &Scope,
        current: Option<&str>,
    ) -> Option<Reservation> {
        let mut held = self.held();
        let saved = held.ready.get(token)?;
        if Some(saved.source.as_str()) != current || saved.scope != *scope {
            return None;
        }
        held.reserved += 1;
        Some(Reservation {
            store: Arc::clone(self),
            token: token.to_owned(),
            saved: held.ready.remove(token),
        })
    }
}

/// A snapshot one assignment holds while it records the print.
pub struct Reservation {
    store: Arc<SnapshotStore>,
    token: String,
    saved: Option<Saved>,
}

impl Reservation {
    pub fn inspected(&self) -> Option<&Inspected> {
        self.saved.as_ref().map(|saved| &saved.inspected)
    }

    /// The link now records what the bytes said, so the snapshot is gone.
    pub fn spend(mut self) {
        self.saved = None;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut held = self.store.held();
        held.reserved -= 1;
        if let Some(saved) = self.saved.take() {
            held.ready.insert(std::mem::take(&mut self.token), saved);
        }
    }
}

/// One capture in progress. Its token goes to the client before the bytes;
/// it is redeemable only after [`Capture::finish`]. Dropping it unfinished
/// releases the store.
pub struct Capture {
    store: Arc<SnapshotStore>,
    token: String,
    scope: Scope,
    identity: Value,
    source: String,
    bytes: Vec<u8>,
    done: bool,
}

impl Capture {
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Keep a chunk the client received. Past Node's inspection limit the
    /// capture fails.
    pub fn push(&mut self, chunk: &[u8]) -> std::result::Result<(), &'static str> {
        if self.bytes.len() + chunk.len() > MAX_CLASSIFIABLE_BYTES {
            return Err("Printer file exceeds inspection size limit");
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    /// Classify the whole file at EOF and make the token redeemable.
    pub fn finish(mut self) -> std::result::Result<(), &'static str> {
        let classified =
            classify(&self.bytes).map_err(|_: Rejection| "Printer file could not be inspected")?;
        let mut identity = match std::mem::take(&mut self.identity) {
            Value::Object(identity) => identity,
            _ => serde_json::Map::new(),
        };
        identity.insert("size_bytes".into(), json!(classified.size_bytes));
        identity.insert("sha256".into(), json!(classified.sha256));
        let saved = Saved {
            scope: self.scope.clone(),
            source: std::mem::take(&mut self.source),
            expires: Instant::now() + TTL,
            inspected: Inspected {
                classification: classified.classification,
                identity: Value::Object(identity),
            },
        };
        let mut held = self.store.held();
        held.ready.insert(self.token.clone(), saved);
        held.active = false;
        self.done = true;
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if !self.done {
            self.store.held().active = false;
        }
    }
}
