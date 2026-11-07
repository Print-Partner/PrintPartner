//! Printer send queue storage (GRE-1208, the storage half of GRE-365).
//!
//! Queue items live in the `printer_send_queue` table, installed by schema
//! step 44. Node kept them as one JSON array in each tenant's
//! `printer.send_queue` setting. Rust reads that setting for as long as it
//! exists, so every reader sees exactly what Node stored, and the table holds
//! nothing for that tenant meanwhile.
//!
//! [`recover`] runs when the writer opens, after the ownership marker decision
//! has given the directory to Rust, so no Node process writes the setting
//! again. For each tenant whose setting holds a clean queue it moves every item
//! into the table, in stored order, and deletes the setting in the same
//! transaction. A queue that is not clean (not a JSON array, or an item Node's
//! `parseItem` or Accept's production check would drop or refuse) stays in the
//! setting, and so does a queue the table's constraints refuse: each tenant
//! moves inside its own savepoint. A setting that stays is rewritten only to
//! mark its `sending` items interrupted. A moved, kept or stored `sending`
//! item becomes `error`
//! with [`INTERRUPTED_ERROR`]: the process that started its upload is gone, so
//! the printer outcome is unknown and only the user may send it again.
//!
//! Readers call [`rows`]: `None` means the tenant still has the setting and
//! the reader keeps its Node-compatible setting read; `Some` holds the table's
//! items in Node's item JSON shape.
//!
//! [`writes`] holds the user actions on table items (GRE-365): Send now and
//! Send ready dispatch an item as a durable `printer-upload` job in the same
//! transaction, and Remove cancels it. [`settle`] moves a dispatched item to
//! `done` or `error` in the transaction that changes its job.

use crate::read_model::{farm::iso_time, views::js_trim};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Map, Value, json};
use std::{collections::HashSet, time::SystemTime};

/// The schema step that installs the table.
pub(crate) const SCHEMA_STEP: u64 = 44;
/// Node's setting key for the queue.
pub(crate) const LEGACY_KEY: &str = "printer.send_queue";
/// Node's `INTERRUPTED_PRINTER_SEND_ERROR`, word for word.
pub const INTERRUPTED_ERROR: &str = "Interrupted by process restart. Printer outcome is unknown, so this send was not marked successful. The staged file remains for an explicit retry. This process will not retry a print on its own.";
/// An item whose Rust upload was dispatched but had not started when the
/// process stopped. Nothing was sent.
pub const NOT_STARTED_ERROR: &str = "Interrupted by process restart before the upload began. Nothing was sent to the printer. Send now sends it.";
/// An item whose upload stopped with its printer outcome unknown while this
/// process kept running.
pub const UNKNOWN_OUTCOME_ERROR: &str = "Printer outcome is unknown, so this send was not marked successful. The staged file remains for an explicit retry. This process will not retry a print on its own.";

const STATES: [&str; 5] = ["queued", "sending", "done", "error", "cancelled"];
const COLUMNS: &str = "id,filename,artifact_path,printer_id,profile_id,match,wait_for_idle,start,state,checkoff_units,upload_job_id,error,host_name,created_at,updated_at";

/// The tenant's queue items from the table in stored order, or `None` while
/// the tenant's legacy setting row exists.
pub(crate) fn rows(connection: &Connection, tenant: &str) -> Result<Option<Vec<Value>>> {
    if legacy(connection, tenant)? {
        return Ok(None);
    }
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM printer_send_queue WHERE tenant_id=?1 ORDER BY position"
    ))?;
    let items = statement
        .query_map([tenant], |row| {
            let mut item = Map::new();
            for (index, key) in ["id", "filename", "artifact_path", "printer_id"]
                .into_iter()
                .enumerate()
            {
                item.insert(key.into(), Value::String(row.get(index)?));
            }
            if let Some(profile) = row.get::<_, Option<i64>>(4)? {
                item.insert("profile_id".into(), json!(profile));
            }
            if let Some(matched) = row.get::<_, Option<String>>(5)? {
                item.insert("match".into(), Value::String(matched));
            }
            item.insert("wait_for_idle".into(), json!(row.get::<_, bool>(6)?));
            item.insert("start".into(), json!(row.get::<_, bool>(7)?));
            item.insert("state".into(), Value::String(row.get(8)?));
            if let Some(units) = row.get::<_, Option<String>>(9)? {
                let units = serde_json::from_str(&units).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        error.into(),
                    )
                })?;
                item.insert("checkoff_units".into(), units);
            }
            for (index, key) in [(10, "upload_job_id"), (11, "error"), (12, "host_name")] {
                if let Some(text) = row.get::<_, Option<String>>(index)? {
                    item.insert(key.into(), Value::String(text));
                }
            }
            item.insert("created_at".into(), Value::String(row.get(13)?));
            item.insert("updated_at".into(), Value::String(row.get(14)?));
            Ok(Value::Object(item))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(items))
}

/// Replace each table item's `checkoff_units` with the remapped units Accept
/// computed from [`rows`]. Only for a tenant without the legacy setting.
pub(crate) fn write_units(tx: &Transaction<'_>, tenant: &str, items: &[Value]) -> Result<()> {
    for item in items {
        let units = item
            .get("checkoff_units")
            .filter(|units| !units.is_null())
            .map(serde_json::to_string)
            .transpose()?;
        tx.execute(
            "UPDATE printer_send_queue SET checkoff_units=?1 WHERE tenant_id=?2 AND id=?3",
            params![units, tenant, item["id"].as_str()],
        )?;
    }
    Ok(())
}

/// Move each tenant's clean legacy queue into the table, then mark every
/// `sending` item interrupted. One immediate transaction.
pub(crate) fn recover(connection: &mut Connection) -> Result<()> {
    let mut tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = iso_time(SystemTime::now());
    let tenants = tx
        .prepare("SELECT tenant_id FROM app_settings WHERE key=?1 ORDER BY tenant_id")?
        .query_map([LEGACY_KEY], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for tenant in tenants {
        // A queue the table refuses (an id over 128 characters, say) stays in
        // the setting: dropping the savepoint rolls back that tenant alone.
        let savepoint = tx.savepoint()?;
        match adopt(&savepoint, &tenant, &now) {
            Ok(()) => savepoint.commit()?,
            Err(error) if refused(&error) => drop(savepoint),
            Err(error) => return Err(error),
        }
        interrupt_kept(&tx, &tenant, &now)?;
    }
    let dispatched = tx
        .prepare("SELECT tenant_id,upload_job_id FROM printer_send_queue WHERE state='sending' AND upload_job_id IS NOT NULL")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute(
        "UPDATE printer_send_queue SET state='error',error=?1,updated_at=?2 WHERE state='sending'",
        params![INTERRUPTED_ERROR, now],
    )?;
    // Job recovery has already queued again every upload interrupted before
    // its effect was admitted. Nothing reached the printer, so the item says
    // so and stays sendable, and the upload never starts on its own.
    #[cfg(feature = "desktop_rebuild")]
    for (tenant, job) in dispatched {
        if crate::jobs::release_printer_upload(
            &tx,
            &tenant,
            "open",
            &job,
            NOT_STARTED_ERROR,
            "send_queue_interrupted",
        )? {
            tx.execute(
                "UPDATE printer_send_queue SET error=?1 WHERE tenant_id=?2 AND upload_job_id=?3",
                params![NOT_STARTED_ERROR, tenant, job],
            )?;
        }
    }
    #[cfg(not(feature = "desktop_rebuild"))]
    drop(dispatched);
    tx.commit()?;
    Ok(())
}

/// Settle the item that `job` was dispatched for, in the transaction that
/// saves the job: a finished upload is `done`, and a failed, cancelled or
/// uncertain one is `error`. A finished state also applies to an `error`
/// item, so an upload that left reconciliation by any path, including the
/// generic job reconciliation, never leaves its item looking unsent.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn settle(tx: &Connection, job: &crate::jobs::JobRecord) -> Result<()> {
    use crate::jobs::PersistentState;
    let from = if job.state.terminal() {
        "'sending','error'"
    } else {
        "'sending'"
    };
    let error = match job.state {
        PersistentState::Succeeded | PersistentState::UploadedOnly => None,
        PersistentState::Failed => Some(job.recovery.as_deref().unwrap_or("Upload failed")),
        PersistentState::Cancelled => Some("Send cancelled"),
        PersistentState::ReconciliationRequired => Some(UNKNOWN_OUTCOME_ERROR),
        _ => return Ok(()),
    };
    tx.execute(
        &format!("UPDATE printer_send_queue SET state=?1,error=?2,updated_at=?3 WHERE tenant_id=?4 AND upload_job_id=?5 AND state IN ({from})"),
        params![
            if error.is_some() { "error" } else { "done" },
            error.map(|error| utf16_prefix(error, 500)),
            iso_time(SystemTime::now()),
            job.tenant,
            job.job_id,
        ],
    )?;
    Ok(())
}

/// True when `job` is the upload a send queue item dispatched. Its send
/// needs the Farm feature, as Node's queue requires.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn owns(tx: &Connection, job: &crate::jobs::JobRecord) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM printer_send_queue WHERE tenant_id=?1 AND upload_job_id=?2)",
        params![job.tenant, job.job_id],
        |row| row.get(0),
    )?)
}

/// At startup, the item of an upload that job recovery left awaiting
/// reconciliation says the restart interrupted it, as Node's restart does. A
/// lease that expired while the process kept running keeps the
/// unknown-outcome text [`settle`] gave it.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn interrupted(tx: &Connection, job: &crate::jobs::JobRecord) -> Result<()> {
    if job.state == crate::jobs::PersistentState::ReconciliationRequired {
        tx.execute(
            "UPDATE printer_send_queue SET error=?1 WHERE tenant_id=?2 AND upload_job_id=?3 AND state='error'",
            params![INTERRUPTED_ERROR, job.tenant, job.job_id],
        )?;
    }
    Ok(())
}

/// In a setting that stayed, mark each `sending` item interrupted, as Node's
/// own restart reconciliation does, and change nothing else. A value that is
/// not a JSON array, or holds no `sending` item, is left byte for byte.
fn interrupt_kept(tx: &Connection, tenant: &str, now: &str) -> Result<()> {
    let raw: Option<Option<String>> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, LEGACY_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let Some(Ok(Value::Array(mut items))) = raw.flatten().map(|raw| serde_json::from_str(&raw))
    else {
        return Ok(());
    };
    let mut changed = false;
    for item in items.iter_mut().filter(|item| item["state"] == "sending") {
        item["state"] = json!("error");
        item["error"] = json!(INTERRUPTED_ERROR);
        item["updated_at"] = json!(now);
        changed = true;
    }
    if changed {
        tx.execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id=?2 AND key=?3",
            params![Value::Array(items).to_string(), tenant, LEGACY_KEY],
        )?;
    }
    Ok(())
}

fn refused(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(failure, _))
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn legacy(connection: &Connection, tenant: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, LEGACY_KEY],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn adopt(tx: &Connection, tenant: &str, now: &str) -> Result<()> {
    let raw: Option<String> = tx.query_row(
        "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
        params![tenant, LEGACY_KEY],
        |row| row.get(0),
    )?;
    let occupied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM printer_send_queue WHERE tenant_id=?1)",
        [tenant],
        |row| row.get(0),
    )?;
    let Some(items) = raw.as_deref().and_then(|raw| clean(raw, now)) else {
        return Ok(());
    };
    if occupied {
        return Ok(());
    }
    for (position, item) in items.iter().enumerate() {
        // Node's restart reconciliation, applied as the item moves, so two
        // stale `sending` items on one printer never meet the unique index.
        let (state, error, updated_at) = if item.state == "sending" {
            ("error", Some(INTERRUPTED_ERROR), now)
        } else {
            (
                item.state.as_str(),
                item.error.as_deref(),
                item.updated_at.as_str(),
            )
        };
        tx.execute(
            &format!(
                "INSERT INTO printer_send_queue(tenant_id,position,{COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)"
            ),
            params![
                tenant,
                position as i64,
                item.id,
                item.filename,
                item.artifact_path,
                item.printer_id,
                item.profile_id,
                item.matched,
                item.wait_for_idle,
                item.start,
                state,
                item.units,
                item.upload_job_id,
                error,
                item.host_name,
                item.created_at,
                updated_at,
            ],
        )?;
    }
    tx.execute(
        "DELETE FROM app_settings WHERE tenant_id=?1 AND key=?2",
        params![tenant, LEGACY_KEY],
    )?;
    Ok(())
}

struct Item {
    id: String,
    filename: String,
    artifact_path: String,
    printer_id: String,
    profile_id: Option<i64>,
    matched: Option<&'static str>,
    wait_for_idle: bool,
    start: bool,
    state: String,
    units: Option<String>,
    upload_job_id: Option<String>,
    error: Option<String>,
    host_name: Option<String>,
    created_at: String,
    updated_at: String,
}

/// Every item of a legacy queue as Node's `parseItem` reads it, or `None`
/// when moving it would drop or change anything Node or Accept reads: a
/// value that is not an array, an item `parseItem` drops, a duplicate id, a
/// `profile_id` that is not a positive integer, or `checkoff_units` that are
/// not an array of whole coordinates. Missing times read as `now`, as Node
/// stamps them on load.
fn clean(raw: &str, now: &str) -> Option<Vec<Item>> {
    let Ok(Value::Array(values)) = serde_json::from_str::<Value>(raw) else {
        return None;
    };
    let mut ids = HashSet::new();
    let mut items = Vec::with_capacity(values.len());
    for value in &values {
        let item = item(value, now)?;
        if !ids.insert(item.id.clone()) {
            return None;
        }
        items.push(item);
    }
    Some(items)
}

fn item(value: &Value, now: &str) -> Option<Item> {
    let row = value.as_object()?;
    let field = |key: &str| row.get(key).unwrap_or(&Value::Null);
    let text = |key: &str| {
        field(key)
            .as_str()
            .map(js_trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let state = field("state")
        .as_str()
        .filter(|state| STATES.contains(state))?;
    let profile_id = match field("profile_id") {
        Value::Null => None,
        profile => Some(profile.as_i64().filter(|profile| *profile > 0)?),
    };
    let units = match field("checkoff_units") {
        Value::Null => None,
        Value::Array(units) => Some(
            units
                .iter()
                .map(unit)
                .collect::<Option<Vec<_>>>()
                .map(|units| Value::Array(units).to_string())?,
        ),
        _ => return None,
    };
    let time = |key: &str| match field(key).as_str() {
        Some(at) if !at.is_empty() => at.to_owned(),
        _ => now.to_owned(),
    };
    Some(Item {
        id: text("id")?,
        filename: text("filename")?,
        artifact_path: text("artifact_path")?,
        printer_id: text("printer_id")?,
        profile_id,
        matched: match field("match").as_str() {
            Some("compatible") => Some("compatible"),
            Some("pinned") => Some("pinned"),
            _ => None,
        },
        wait_for_idle: field("wait_for_idle") != &Value::Bool(false),
        start: truthy(field("start")),
        state: state.to_owned(),
        units,
        upload_job_id: text("upload_job_id"),
        error: text("error").map(|error| utf16_prefix(&error, 500)),
        host_name: text("host_name"),
        created_at: time("created_at"),
        updated_at: time("updated_at"),
    })
}

/// Node's `normalizeUnit` for a coordinate Accept also accepts.
fn unit(value: &Value) -> Option<Value> {
    let part = value.get("part_id")?.as_i64().filter(|part| *part > 0)?;
    let index = value
        .get("unit_index")?
        .as_i64()
        .filter(|index| *index >= 0)?;
    let mut unit = json!({ "part_id": part, "unit_index": index });
    if let Some(name) = value
        .get("object_name")
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|name| !name.is_empty())
    {
        unit["object_name"] = Value::String(utf16_prefix(name, 200));
    }
    Some(unit)
}

/// JavaScript truthiness.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

/// `text.slice(0, units)` in UTF-16 code units, ending on a whole character.
fn utf16_prefix(text: &str, units: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|character| {
            used += character.len_utf16();
            used <= units
        })
        .collect()
}

#[cfg(feature = "desktop_rebuild")]
pub mod writes;

#[cfg(test)]
mod tests;
