//! Farm log (GRE-1126): the fleet-wide record of farm actions, newest first,
//! with the undo each one allows.
//!
//! Storage appends an entry to the `printer.farm_log` setting in the same
//! writer transaction as the action: a new Checkoff burst, and a successful
//! burst undo. An idempotent burst replay returns its stored receipt before
//! any write, so it appends nothing. Entries name their burst by undo token and
//! keep only counts and ids; the receipt in `printer.checkoff_bursts` stays
//! the one copy of what undo restores.
//!
//! Sends are read from the existing `printer.send_queue` records, not copied.
//! A send is a printer command (dispatch or cancel), so it never offers undo,
//! and neither does an undo entry: there is no redo.

use super::{Body, Outcome, Response, burst};
use crate::read_model::farm::{Farm, SendState};
use anyhow::Result;
use rusqlite::Transaction;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) const KEY: &str = "printer.farm_log";
/// The newest entries kept; older ones are dropped as new ones append.
pub const MAX_ENTRIES: usize = 10_000;
pub const DEFAULT_PAGE: u16 = 50;
pub const MAX_PAGE: u16 = 200;
const BURST: &str = "checkoff_burst";
const UNDO: &str = "checkoff_burst_undo";
const SEND: &str = "send";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum FarmLogKind {
    CheckoffBurst,
    CheckoffBurstUndo,
    Send,
}

/// Why an entry offers no undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum FarmUndoReason {
    AlreadyUndone,
    /// The burst's receipt is older than the receipts storage keeps.
    ReceiptExpired,
    /// Something the burst wrote, or something that depends on it, changed.
    StateChanged,
    /// A Build the burst touched is archived.
    Archived,
    /// Dispatch and cancel are printer commands.
    PrinterCommand,
    /// An undo cannot itself be undone or redone.
    UndoEntry,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FarmLogUndo {
    pub undoable: bool,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<FarmUndoReason>")
    )]
    pub reason: Option<FarmUndoReason>,
    /// The refusal text `POST /api/v2/farm-log/{id}/undo` would answer.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub detail: Option<String>,
}

impl FarmLogUndo {
    fn allowed() -> Self {
        Self {
            undoable: true,
            reason: None,
            detail: None,
        }
    }

    fn refused(reason: FarmUndoReason, detail: &str) -> Self {
        Self {
            undoable: false,
            reason: Some(reason),
            detail: Some(detail.to_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FarmLogEntry {
    /// `{kind}:{seq}` for logged actions, `send:{queue item id}` for sends.
    pub id: String,
    pub kind: FarmLogKind,
    pub at: String,
    pub summary: String,
    pub build_ids: Vec<i64>,
    pub printer_ids: Vec<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<u32>")
    )]
    pub units_confirmed: Option<u32>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<u32>")
    )]
    pub units_rejected: Option<u32>,
    /// For an undo, the burst entry it reversed, when still kept.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub undoes: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<SendState>")
    )]
    pub send_state: Option<SendState>,
    pub undo: FarmLogUndo,
}

/// `GET /api/v2/farm-log` body.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FarmLogPage {
    pub entries: Vec<FarmLogEntry>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub next_cursor: Option<String>,
}

/// A validated page request.
pub(super) struct Query {
    after: Option<(i128, String)>,
    limit: usize,
}

impl Query {
    /// `limit` is 1 to 200 (default 50); `cursor` is a page's `next_cursor`.
    pub(super) fn parse(
        cursor: Option<&str>,
        limit: Option<&str>,
    ) -> std::result::Result<Self, Response> {
        let limit = match limit {
            None => DEFAULT_PAGE,
            Some(raw) => raw
                .parse::<u16>()
                .ok()
                .filter(|limit| (1..=MAX_PAGE).contains(limit))
                .ok_or_else(|| Response::failure(400, "limit must be 1 to 200", None))?,
        };
        let after = cursor
            .map(|raw| decode(raw).ok_or_else(|| Response::failure(400, "cursor is invalid", None)))
            .transpose()?;
        Ok(Self {
            after,
            limit: usize::from(limit),
        })
    }
}

fn encode(key: &(i128, String)) -> String {
    hex::encode(format!("v1\n{}\n{}", key.0, key.1))
}

fn decode(raw: &str) -> Option<(i128, String)> {
    let text = String::from_utf8(hex::decode(raw).ok()?).ok()?;
    let mut parts = text.splitn(3, '\n');
    (parts.next()? == "v1").then_some(())?;
    let time = parts.next()?.parse().ok()?;
    Some((time, parts.next()?.to_owned()))
}

/// Nanoseconds since the epoch, for ordering; an unparsable time sorts oldest.
fn sort_time(at: &str) -> i128 {
    time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
        .map_or(i128::MIN, time::OffsetDateTime::unix_timestamp_nanos)
}

/// The Builds and printers a burst receipt's response names.
fn touched(response: &Value) -> (Vec<i64>, Vec<String>) {
    let links: Vec<&Value> = response["beds"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|bed| &bed["link"])
        .collect();
    let mut builds: Vec<i64> = links
        .iter()
        .filter_map(|l| l["profile_id"].as_i64())
        .collect();
    let mut printers: Vec<String> = links
        .iter()
        .filter_map(|l| l["printer_id"].as_str().map(str::to_owned))
        .collect();
    builds.sort_unstable();
    builds.dedup();
    printers.sort();
    printers.dedup();
    (builds, printers)
}

fn append(tx: &Transaction<'_>, tenant: &str, mut entry: Map<String, Value>) -> Result<u64> {
    let mut entries = burst::load_list(tx, tenant, KEY)?;
    let seq = entries
        .last()
        .and_then(|last| last["seq"].as_u64())
        .unwrap_or(0)
        + 1;
    entry.insert("seq".into(), seq.into());
    entry.insert("at".into(), super::links::now().into());
    entries.push(Value::Object(entry));
    let start = entries.len().saturating_sub(MAX_ENTRIES);
    burst::save_list(tx, tenant, KEY, &entries[start..])?;
    Ok(seq)
}

/// Logs a new burst from its receipt response, in the burst's transaction.
pub(super) fn append_burst(tx: &Transaction<'_>, tenant: &str, response: &Value) -> Result<()> {
    let (builds, printers) = touched(response);
    let entry = json!({
        "kind": BURST,
        "undo_token": response["undo_token"],
        "build_ids": builds,
        "printer_ids": printers,
        "units_confirmed": response["units_confirmed"],
        "units_rejected": response["units_rejected"],
        "beds": response["beds"].as_array().map_or(0, Vec::len),
    });
    append(tx, tenant, entry.as_object().cloned().unwrap_or_default()).map(|_| ())
}

/// Logs a successful undo of the burst with `token`, in the undo's
/// transaction. Returns the new entry's id.
pub(super) fn append_undo(
    tx: &Transaction<'_>,
    tenant: &str,
    token: &str,
    response: &Value,
) -> Result<String> {
    let undoes = burst::load_list(tx, tenant, KEY)?
        .iter()
        .find(|e| e["kind"] == BURST && e["undo_token"] == token)
        .and_then(|e| e["seq"].as_u64())
        .map(|seq| format!("{BURST}:{seq}"));
    let (builds, printers) = touched(response);
    let entry = json!({
        "kind": UNDO,
        "undo_token": token,
        "undoes": undoes,
        "build_ids": builds,
        "printer_ids": printers,
    });
    let seq = append(tx, tenant, entry.as_object().cloned().unwrap_or_default())?;
    Ok(format!("{UNDO}:{seq}"))
}

fn count(value: &Value) -> Option<u32> {
    value.as_u64().map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn logged(entry: &Value) -> Option<FarmLogEntry> {
    let seq = entry["seq"].as_u64()?;
    let kind = match entry["kind"].as_str()? {
        BURST => FarmLogKind::CheckoffBurst,
        UNDO => FarmLogKind::CheckoffBurstUndo,
        _ => return None,
    };
    let units_confirmed = count(&entry["units_confirmed"]);
    let units_rejected = count(&entry["units_rejected"]);
    let summary = match kind {
        FarmLogKind::CheckoffBurst => format!(
            "Checked off {} units and rejected {} on {} beds",
            units_confirmed.unwrap_or(0),
            units_rejected.unwrap_or(0),
            entry["beds"].as_u64().unwrap_or(0)
        ),
        _ => "Undid a Checkoff burst".to_owned(),
    };
    Some(FarmLogEntry {
        id: format!("{}:{seq}", entry["kind"].as_str()?),
        kind,
        at: entry["at"].as_str()?.to_owned(),
        summary,
        build_ids: entry["build_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect(),
        printer_ids: entry["printer_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_owned))
            .collect(),
        units_confirmed,
        units_rejected,
        undoes: entry["undoes"].as_str().map(str::to_owned),
        send_state: None,
        undo: FarmLogUndo::refused(FarmUndoReason::UndoEntry, "An undo cannot be undone"),
    })
}

fn sends(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<FarmLogEntry>> {
    let queue = burst::load_list(tx, tenant, "printer.send_queue")?;
    Ok(Farm::parse(vec![], vec![], vec![], vec![], queue)
        .send_queue()
        .items
        .into_iter()
        .filter_map(|item| {
            Some(FarmLogEntry {
                id: format!("{SEND}:{}", item.id),
                kind: FarmLogKind::Send,
                at: item.created_at?,
                summary: format!("Sent {} to a printer", item.filename),
                build_ids: item.profile_id.into_iter().collect(),
                printer_ids: vec![item.printer_id],
                units_confirmed: None,
                units_rejected: None,
                undoes: None,
                send_state: Some(item.state),
                undo: FarmLogUndo::refused(
                    FarmUndoReason::PrinterCommand,
                    "A printer command cannot be undone",
                ),
            })
        })
        .collect())
}

/// One page, newest first. Only the page's bursts are checked for undo.
pub(super) fn page(tx: &Transaction<'_>, tenant: &str, query: &Query, repos: &Path) -> Outcome {
    let stored = burst::load_list(tx, tenant, KEY)?;
    let mut entries: Vec<(i128, FarmLogEntry)> = stored
        .iter()
        .filter_map(logged)
        .chain(sends(tx, tenant)?)
        .map(|entry| (sort_time(&entry.at), entry))
        .collect();
    entries.sort_by(|a, b| (b.0, &b.1.id).cmp(&(a.0, &a.1.id)));
    let after = entries.into_iter().filter(|(time, entry)| {
        query
            .after
            .as_ref()
            .is_none_or(|(at, id)| (*time, &entry.id) < (*at, id))
    });
    let mut page: Vec<(i128, FarmLogEntry)> = after.take(query.limit + 1).collect();
    let next_cursor = (page.len() > query.limit).then(|| {
        page.truncate(query.limit);
        page.last()
            .map(|(time, entry)| encode(&(*time, entry.id.clone())))
    });
    let receipts = burst::load_receipts(tx, tenant)?;
    let mut out = Vec::with_capacity(page.len());
    for (_, mut entry) in page {
        if entry.kind == FarmLogKind::CheckoffBurst {
            let token = stored
                .iter()
                .find(|e| e["kind"] == BURST && entry.id == format!("{BURST}:{}", e["seq"]))
                .and_then(|e| e["undo_token"].as_str())
                .unwrap_or_default();
            entry.undo = match burst::undo_blocked(tx, tenant, &receipts, token, repos)? {
                None => FarmLogUndo::allowed(),
                Some(blocked) => blocked.into(),
            };
        }
        out.push(entry);
    }
    Ok(Ok(Response::ok(Body::FarmLog(Box::new(FarmLogPage {
        entries: out,
        next_cursor: next_cursor.flatten(),
    })))))
}

impl From<burst::Blocked> for FarmLogUndo {
    fn from(blocked: burst::Blocked) -> Self {
        use burst::Blocked as B;
        match blocked {
            B::NotFound => Self::refused(
                FarmUndoReason::ReceiptExpired,
                "This burst's receipt is no longer kept",
            ),
            B::Undone => Self::refused(
                FarmUndoReason::AlreadyUndone,
                "This Checkoff burst was already undone",
            ),
            B::Archived => Self::refused(
                FarmUndoReason::Archived,
                "Archived Plan Progress cannot be changed",
            ),
            B::Changed(reason) => Self::refused(FarmUndoReason::StateChanged, &reason),
        }
    }
}

/// `POST /api/v2/farm-log/{id}/undo`: undoes a logged burst through the
/// burst undo and answers the appended undo entry.
pub(super) fn undo_entry(tx: &Transaction<'_>, tenant: &str, id: &str, repos: &Path) -> Outcome {
    let refuse = |status, detail| Ok(Ok(Response::failure(status, detail, None)));
    let missing = || {
        Ok(Ok(Response::failure(
            404,
            "Farm log entry not found",
            Some("farm_log_entry_not_found"),
        )))
    };
    if id.starts_with(&format!("{SEND}:")) {
        if sends(tx, tenant)?.iter().any(|send| send.id == id) {
            return refuse(409, "A printer command cannot be undone");
        }
        return missing();
    }
    let stored = burst::load_list(tx, tenant, KEY)?;
    let Some(entry) = stored.iter().find(|e| {
        e["kind"]
            .as_str()
            .is_some_and(|kind| id == format!("{kind}:{}", e["seq"]))
    }) else {
        return missing();
    };
    if entry["kind"] == UNDO {
        return refuse(409, "An undo cannot be undone");
    }
    let Some(token) = entry["undo_token"]
        .as_str()
        .filter(|_| entry["kind"] == BURST)
    else {
        return missing();
    };
    let token = token.to_owned();
    match burst::undo_batch(tx, tenant, &token, repos)? {
        Ok(Response { status: 200, .. }) => {}
        other => return Ok(other),
    }
    let entry = burst::load_list(tx, tenant, KEY)?
        .iter()
        .rev()
        .find(|e| e["kind"] == UNDO && e["undo_token"] == token.as_str())
        .and_then(logged);
    match entry {
        Some(entry) => Ok(Ok(Response::ok(Body::FarmLogEntry(Box::new(entry))))),
        None => Ok(Err(super::Refusal::Internal)),
    }
}

#[cfg(test)]
mod tests;
