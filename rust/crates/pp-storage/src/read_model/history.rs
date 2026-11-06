//! Build history: one read-only, paged, newest-first list of a Build's
//! accepted revisions, snapshots, checkoff bursts, sends and assistant
//! decisions. Every page is read in one deferred transaction and writes
//! nothing.
//!
//! Sources, all scoped to the caller's tenant and the Build:
//! - `plan_revisions` (accepted revisions, `accepted_at`)
//! - `plan_snapshots` (`created_at`)
//! - `print_jobs` with their `print_job_parts` counts (checkoff bursts, `at`)
//! - the send queue's items for the Build (sends, `created_at`), from
//!   `crate::send_queue`
//! - `plan_decisions` (`created_at`), the rows `GET /plans/:id/decisions` serves
//!
//! Entries sort by (time, type, id), newest first. The cursor names the last
//! entry of a page, so entries recorded later sort ahead of it and never shift
//! the pages after it. Every sort field is immutable once a row exists.

use super::Credential;
use crate::{Envelope, Shared, WriterOwner, auth};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pp_domain::PositiveId;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Rows read per source. A Build past this answers `Failure::TooLarge`
/// instead of a partial history.
const MAX_SOURCE_ROWS: usize = 10_000;
pub const DEFAULT_LIMIT: u16 = 50;
pub const MAX_LIMIT: u16 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    TooLarge,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Build history exceeds the read limit")
    }
}

impl std::error::Error for Failure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    Decision,
    CheckoffBurst,
    Send,
    Snapshot,
    Revision,
}

impl EntryType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::CheckoffBurst => "checkoff_burst",
            Self::Send => "send",
            Self::Snapshot => "snapshot",
            Self::Revision => "revision",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        [
            Self::Decision,
            Self::CheckoffBurst,
            Self::Send,
            Self::Snapshot,
            Self::Revision,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    User,
    Assistant,
}

/// Why an entry cannot be undone from History.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoReason {
    /// The burst was recorded before Checkoff issued undo tokens.
    NoUndoToken,
    /// A printer command cannot be taken back.
    PrinterCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Undo {
    pub undoable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<UndoReason>,
}

impl Undo {
    fn refused(reason: UndoReason) -> Self {
        Self {
            undoable: false,
            reason: Some(reason),
        }
    }
}

/// The record an entry opens: `kind` is the entry type, `id` its row id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Link {
    pub kind: EntryType,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// `{type}:{row id}`, unique within the Build's history.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: EntryType,
    /// The stored timestamp, unchanged.
    pub at: String,
    pub actor: Actor,
    pub summary: String,
    pub link: Link,
    /// Present on checkoff bursts and sends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<Undo>,
    #[serde(skip)]
    sort: SortKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Page {
    pub plan_id: i64,
    pub entries: Vec<Entry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug)]
pub enum Outcome {
    Page(Page),
    MissingBuild,
}

/// Ordering of one entry: time in Unix nanoseconds (`i128::MIN` when the
/// stored value is not RFC 3339), then type, then row id. Integer ids are
/// zero-padded so they compare numerically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    time: i128,
    kind: EntryType,
    id: String,
}

/// The sort key of the last entry a page returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(SortKey);

impl Cursor {
    /// Reads a cursor this module wrote. Anything else is `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 1024 {
            return None;
        }
        let text = String::from_utf8(URL_SAFE_NO_PAD.decode(raw).ok()?).ok()?;
        let mut fields = text.splitn(4, '|');
        if fields.next()? != "v1" {
            return None;
        }
        let time = fields.next()?.parse().ok()?;
        let kind = EntryType::parse(fields.next()?)?;
        let id = fields.next()?.to_owned();
        (!id.is_empty()).then_some(Self(SortKey { time, kind, id }))
    }

    fn encode(key: &SortKey) -> String {
        URL_SAFE_NO_PAD.encode(format!("v1|{}|{}|{}", key.time, key.kind.as_str(), key.id))
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    /// `None` names no Build: the read still checks the credential, then
    /// answers `MissingBuild`.
    pub build: Option<PositiveId>,
    pub cursor: Option<Cursor>,
    pub limit: u16,
}

#[derive(Clone)]
pub struct HistoryClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}

pub(crate) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    request: Request,
    cancelled: Arc<AtomicBool>,
}

impl WriterOwner {
    pub fn build_history(&self, policy: auth::AuthPolicy) -> Result<HistoryClient> {
        auth::validate_policy(policy)?;
        ensure!(self.lease.is_some(), "Storage owner is closed");
        Ok(HistoryClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}

impl HistoryClient {
    pub fn read(
        &self,
        credential: Credential,
        request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Outcome> {
        ensure!(
            (1..=MAX_LIMIT).contains(&request.limit),
            "Build history pages hold 1 to {MAX_LIMIT} entries"
        );
        let flag = cancelled.clone();
        super::submit_read(&self.shared, &flag, wait, |reply| Envelope::History {
            command: Command {
                credential,
                policy: self.policy,
                request,
                cancelled,
            },
            reply,
        })
    }
}

pub(crate) fn execute(connection: &mut Connection, command: Command) -> Result<Outcome> {
    let cancelled = &command.cancelled;
    let (tx, found) = super::owned_build(
        connection,
        cancelled,
        &command.credential,
        command.policy,
        command.request.build,
    )?;
    let Some((tenant, build)) = found else {
        tx.commit()?;
        return Ok(Outcome::MissingBuild);
    };
    let mut entries = Vec::new();
    for source in [revisions, snapshots, bursts, sends, decisions] {
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Cancelled before admission"
        );
        entries.extend(source(&tx, &tenant, build)?);
    }
    tx.commit()?;
    Ok(Outcome::Page(page(
        build,
        entries,
        command.request.cursor.as_ref(),
        command.request.limit,
    )))
}

fn page(plan_id: i64, mut entries: Vec<Entry>, cursor: Option<&Cursor>, limit: u16) -> Page {
    if let Some(Cursor(after)) = cursor {
        entries.retain(|entry| entry.sort < *after);
    }
    entries.sort_by(|a, b| b.sort.cmp(&a.sort));
    let limit = usize::from(limit);
    let next_cursor = (entries.len() > limit).then(|| Cursor::encode(&entries[limit - 1].sort));
    entries.truncate(limit);
    Page {
        plan_id,
        entries,
        next_cursor,
    }
}

fn sort_time(at: &str) -> i128 {
    time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
        .map(time::OffsetDateTime::unix_timestamp_nanos)
        .unwrap_or(i128::MIN)
}

fn entry(
    kind: EntryType,
    id: String,
    at: String,
    actor: Actor,
    summary: String,
    undo: Option<Undo>,
) -> Entry {
    let sort_id = match id.parse::<i64>() {
        Ok(number) if number >= 0 => format!("{number:020}"),
        _ => id.clone(),
    };
    Entry {
        id: format!("{}:{id}", kind.as_str()),
        kind,
        sort: SortKey {
            time: sort_time(&at),
            kind,
            id: sort_id,
        },
        at,
        actor,
        summary,
        link: Link { kind, id },
        undo,
    }
}

/// Runs `sql` with `(tenant, build)` and maps each row, refusing more than
/// `MAX_SOURCE_ROWS` rows.
fn collect(
    tx: &Transaction<'_>,
    sql: &str,
    tenant: &str,
    build: i64,
    map: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Entry>,
) -> Result<Vec<Entry>> {
    let mut statement = tx.prepare(&format!("{sql} LIMIT {}", MAX_SOURCE_ROWS + 1))?;
    let entries = statement
        .query_map(params![tenant, build], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(entries.len() <= MAX_SOURCE_ROWS, Failure::TooLarge);
    Ok(entries)
}

fn revisions(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Vec<Entry>> {
    collect(
        tx,
        "SELECT id,revision_number,accepted_at FROM plan_revisions WHERE tenant_id=?1 AND profile_id=?2",
        tenant,
        build,
        |row| {
            let number: i64 = row.get(1)?;
            Ok(entry(
                EntryType::Revision,
                row.get::<_, i64>(0)?.to_string(),
                row.get(2)?,
                Actor::User,
                format!("Accepted revision {number}"),
                None,
            ))
        },
    )
}

fn snapshots(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Vec<Entry>> {
    collect(
        tx,
        "SELECT id,name,source,created_at FROM plan_snapshots WHERE tenant_id=?1 AND profile_id=?2",
        tenant,
        build,
        |row| {
            let name: String = row.get(1)?;
            let source: String = row.get(2)?;
            let summary = match (source.as_str(), name.trim()) {
                ("pre_apply", "") => "Snapshot before apply".to_owned(),
                ("pre_apply", name) => format!("Snapshot before apply: {name}"),
                (_, "") => "Snapshot".to_owned(),
                (_, name) => name.to_owned(),
            };
            Ok(entry(
                EntryType::Snapshot,
                row.get::<_, i64>(0)?.to_string(),
                row.get(3)?,
                if source == "assistant" {
                    Actor::Assistant
                } else {
                    Actor::User
                },
                summary,
                None,
            ))
        },
    )
}

/// One `print_jobs` row is one recorded Checkoff burst: the units one
/// outcome call recorded for one printer link. Outcomes with no job (Node's
/// legacy blob migration, or a deleted job) group by time and link into a
/// burst named `outcomes-{first outcome id}`. Outcomes are counted in one
/// pass over the Build's rows, which `idx_print_job_parts_profile` serves.
fn bursts(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Vec<Entry>> {
    collect(
        tx,
        "WITH counts AS MATERIALIZED (\
           SELECT job_id,MIN(id) AS first,at,MIN(filename) AS filename,\
           SUM(result='confirmed') AS confirmed,SUM(result<>'confirmed') AS rejected \
           FROM print_job_parts WHERE tenant_id=?1 AND profile_id=?2 \
           GROUP BY job_id,CASE WHEN job_id IS NULL THEN at END,\
           CASE WHEN job_id IS NULL THEN COALESCE(link_id,'') END) \
         SELECT job.id,job.at,job.filename,COALESCE(counts.confirmed,0),COALESCE(counts.rejected,0) \
         FROM print_jobs job LEFT JOIN counts ON counts.job_id=job.id \
         WHERE job.tenant_id=?1 AND job.profile_id=?2 \
         UNION ALL \
         SELECT 'outcomes-' || first,at,filename,confirmed,rejected FROM counts WHERE job_id IS NULL",
        tenant,
        build,
        |row| {
            let filename: Option<String> = row.get(2)?;
            let confirmed: i64 = row.get(3)?;
            let rejected: i64 = row.get(4)?;
            let mut summary = format!(
                "Checked off {confirmed} {}",
                if confirmed == 1 { "unit" } else { "units" }
            );
            if rejected > 0 {
                summary.push_str(&format!(", rejected {rejected}"));
            }
            if let Some(file) = filename.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
                summary.push_str(&format!(" from {file}"));
            }
            Ok(entry(
                EntryType::CheckoffBurst,
                row.get(0)?,
                row.get(1)?,
                Actor::User,
                summary,
                Some(Undo::refused(UndoReason::NoUndoToken)),
            ))
        },
    )
}

/// Every queue item the Build owns, in any state: an item enters History when
/// it is queued, so a later dispatch never inserts it behind a cursor. Node
/// keeps a bounded tail of finished items, so older sends drop out.
fn sends(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Vec<Entry>> {
    let items = match crate::send_queue::rows(tx, tenant)? {
        Some(items) => items,
        None => {
            let raw: Option<String> = tx
                .query_row(
                    "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
                    params![tenant, crate::send_queue::LEGACY_KEY],
                    |row| row.get(0),
                )
                .optional()?;
            // Node reads a missing, blank or malformed queue as empty.
            let Some(Value::Array(items)) = raw.and_then(|raw| serde_json::from_str(&raw).ok())
            else {
                return Ok(Vec::new());
            };
            items
        }
    };
    let text = |item: &Value, key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let mut entries = Vec::new();
    for item in &items {
        // Node's `parseItem` drops an item with an unknown state or a blank
        // id, filename, artifact path or printer, and reads `profile_id` with
        // `Number()`.
        let state = item.get("state").and_then(Value::as_str);
        if !matches!(
            state,
            Some("queued" | "sending" | "done" | "error" | "cancelled")
        ) || text(item, "artifact_path").is_none()
            || item.get("profile_id").and_then(js_number) != Some(build as f64)
        {
            continue;
        }
        // Node stamps a missing `created_at` with the time of each load, which
        // has no fixed place in History, so those items are skipped.
        let at = item.get("created_at").and_then(Value::as_str);
        let (Some(id), Some(file), Some(printer_id), Some(at)) = (
            text(item, "id"),
            text(item, "filename"),
            text(item, "printer_id"),
            at.filter(|at| !at.is_empty()),
        ) else {
            continue;
        };
        let printer = text(item, "host_name").unwrap_or(printer_id);
        let summary = match state {
            Some("done") => format!("Sent {file} to {printer}"),
            Some("sending") => format!("Sending {file} to {printer}"),
            Some("error") => format!("Send of {file} to {printer} failed"),
            Some("cancelled") => format!("Cancelled send of {file} to {printer}"),
            _ => format!("Queued {file} for {printer}"),
        };
        entries.push(entry(
            EntryType::Send,
            id,
            at.to_owned(),
            Actor::User,
            summary,
            Some(Undo::refused(UndoReason::PrinterCommand)),
        ));
        ensure!(entries.len() <= MAX_SOURCE_ROWS, Failure::TooLarge);
    }
    Ok(entries)
}

/// JavaScript `Number(value)` for the JSON a queue item can hold: numbers,
/// booleans, and decimal, exponent or `0x`/`0o`/`0b` strings.
fn js_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        Value::String(text) => {
            let text = super::views::js_trim(text);
            let radix = |digits: &str, radix: u32| {
                u64::from_str_radix(digits, radix).ok().map(|n| n as f64)
            };
            match text.get(..2) {
                _ if text.is_empty() => Some(0.0),
                Some("0x" | "0X") => radix(&text[2..], 16),
                Some("0o" | "0O") => radix(&text[2..], 8),
                Some("0b" | "0B") => radix(&text[2..], 2),
                // Rust also reads `inf` and `nan`; neither names a Build.
                _ => text.parse().ok(),
            }
        }
        _ => None,
    }
}

fn decisions(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Vec<Entry>> {
    collect(
        tx,
        "SELECT id,created_at,actor,kind,label,summary FROM plan_decisions WHERE tenant_id=?1 AND profile_id=?2",
        tenant,
        build,
        |row| {
            let actor: String = row.get(2)?;
            let kind: String = row.get(3)?;
            let label: String = row.get(4)?;
            let summary: String = row.get(5)?;
            let summary = [summary.trim(), label.trim(), kind.trim()]
                .into_iter()
                .find(|text| !text.is_empty())
                .unwrap_or("Decision")
                .to_owned();
            Ok(entry(
                EntryType::Decision,
                row.get::<_, i64>(0)?.to_string(),
                row.get(1)?,
                if actor == "user" {
                    Actor::User
                } else {
                    Actor::Assistant
                },
                summary,
                None,
            ))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32) -> String {
        format!("2026-11-01T10:{minute:02}:00.000Z")
    }

    fn sample(kind: EntryType, id: &str, minute: u32) -> Entry {
        entry(kind, id.into(), at(minute), Actor::User, id.into(), None)
    }

    #[test]
    fn build_history_page_orders_newest_first_with_stable_ties() {
        let entries = vec![
            sample(EntryType::Decision, "9", 1),
            sample(EntryType::Decision, "10", 1),
            sample(EntryType::Revision, "1", 1),
            sample(EntryType::Snapshot, "2", 3),
            entry(
                EntryType::Send,
                "x".into(),
                "not a time".into(),
                Actor::User,
                String::new(),
                None,
            ),
        ];
        let ids = |page: &Page| {
            page.entries
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>()
        };
        let first = page(1, entries.clone(), None, 3);
        assert_eq!(
            ids(&first),
            ["snapshot:2", "revision:1", "decision:10"],
            "time, then type, then numeric id"
        );
        let cursor = Cursor::parse(first.next_cursor.as_deref().unwrap()).unwrap();
        let second = page(1, entries, Some(&cursor), 3);
        assert_eq!(ids(&second), ["decision:9", "send:x"]);
        assert_eq!(second.next_cursor, None);
    }
}
