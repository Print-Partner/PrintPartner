//! Converts Checkoff links, unmatched prints, plan bindings and the send
//! queue. A converter calls the reader that already accepts the stored shape.
//! An item that reader discards is dropped and named by index. Anything that
//! reader would keep, and this conversion cannot store, refuses the import.
//! The report names keys, indexes and counts, never a stored value.

use super::{
    ImportError,
    convert::{Counted, FieldChange, SettingsReport},
};
use crate::{
    checkoff_progress::links,
    read_model::{farm::iso_time, views::js_trim},
    send_queue::{INTERRUPTED_ERROR, NOT_STARTED_ERROR},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::SystemTime,
};

const LINKS: &str = "printer.checkoff_links";
const PRINTS: &str = "printer.unattributed_prints";
const BINDINGS: &str = "printer.plan_bindings";
const QUEUE: &str = "printer.send_queue";
const LINK_STATES: [&str; 6] = [
    "watching",
    "awaiting_verify",
    "host_failed",
    "dismissed",
    "verified",
    "applied",
];
const QUEUE_STATES: [&str; 5] = ["queued", "sending", "done", "error", "cancelled"];
const LINK_FIELDS: &[&str] = &[
    "id",
    "profile_id",
    "integration_id",
    "printer_id",
    "host_name",
    "filename",
    "remote_path",
    "upload_job_id",
    "plan_revision_id",
    "remote_identity",
    "remote_drift",
    "classification",
    "imported_inventory",
    "units",
    "unlabeled_names",
    "resolved_units",
    "state",
    "host_outcome",
    "saw_active",
    "started",
    "last_progress",
    "created_at",
    "completed_at",
    "applied_at",
    "units_marked",
];
const PRINT_STRINGS: [&str; 6] = [
    "integration_id",
    "printer_id",
    "host_name",
    "completed_at",
    "claimed_at",
    "dismissed_at",
];
const PRINT_FIELDS: [&str; 12] = [
    "id",
    "filename",
    "integration_id",
    "printer_id",
    "host_name",
    "completed_at",
    "gcode_objects",
    "candidates",
    "claimed_at",
    "claimed_profile_id",
    "dismissed",
    "dismissed_at",
];
const QUEUE_FIELDS: &[&str] = &[
    "id",
    "filename",
    "artifact_path",
    "printer_id",
    "profile_id",
    "match",
    "wait_for_idle",
    "start",
    "state",
    "checkoff_units",
    "upload_job_id",
    "error",
    "host_name",
    "created_at",
    "updated_at",
];

struct Notes {
    changes: BTreeMap<(String, String, String), u64>,
    notes: BTreeMap<String, u64>,
    faults: BTreeMap<String, u64>,
}

impl Notes {
    fn change(&mut self, field: &str, stored: &str, converted: &str) {
        *self
            .changes
            .entry((field.into(), stored.into(), converted.into()))
            .or_default() += 1;
    }

    fn note(&mut self, name: &str) {
        *self.notes.entry(name.into()).or_default() += 1;
    }

    fn fault(&mut self, name: &str) {
        *self.faults.entry(name.into()).or_default() += 1;
    }

    fn report(&self) -> SettingsReport {
        let list = |map: &BTreeMap<String, u64>| {
            map.iter()
                .map(|(name, count)| Counted {
                    name: name.clone(),
                    count: *count,
                })
                .collect()
        };
        SettingsReport {
            changes: self
                .changes
                .iter()
                .map(|((field, stored, converted), count)| FieldChange {
                    field: field.clone(),
                    stored: stored.clone(),
                    converted: converted.clone(),
                    count: *count,
                })
                .collect(),
            notes: list(&self.notes),
            credential_addresses: 0,
            faults: list(&self.faults),
        }
    }
}

/// Rewrites the three settings. `true` means the send-queue setting is deleted
/// after the report has counted it. A fault leaves that setting as stored.
pub(super) fn convert(
    connection: &Connection,
    source: Option<&Path>,
) -> Result<(SettingsReport, bool), ImportError> {
    let mut notes = Notes {
        changes: BTreeMap::new(),
        notes: BTreeMap::new(),
        faults: BTreeMap::new(),
    };
    let now = iso_time(SystemTime::now());
    convert_links(connection, &now, &mut notes)?;
    convert_prints(connection, &mut notes)?;
    convert_bindings(connection, &mut notes)?;
    let drop_queue = convert_queue(connection, source, &now, &mut notes)?;
    Ok((notes.report(), drop_queue && notes.faults.is_empty()))
}

fn setting(connection: &Connection, key: &str) -> Result<Option<String>, ImportError> {
    Ok(connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}

fn write_setting(connection: &Connection, key: &str, value: &Value) -> Result<(), ImportError> {
    let text =
        serde_json::to_string(value).map_err(|error| ImportError::Database(error.to_string()))?;
    connection.execute(
        "UPDATE app_settings SET value = ?1 WHERE tenant_id = 'default' AND key = ?2",
        (text, key),
    )?;
    Ok(())
}

fn array(raw: &str) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(&links::js_json(raw)) {
        Ok(Value::Array(items)) => Some(items),
        _ => None,
    }
}

fn field_text<'a>(row: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    row.get(key).and_then(trimmed)
}

/// A non-empty string, as JavaScript treats it as present. Whitespace stays.
fn nonempty<'a>(row: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn unknown(object: &Map<String, Value>, known: &[&str], label: &str, notes: &mut Notes) {
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            notes.note(&format!("dropped {label} {key}"));
        }
    }
}

fn trimmed(value: &Value) -> Option<&str> {
    value.as_str().map(js_trim).filter(|text| !text.is_empty())
}

fn convert_links(connection: &Connection, now: &str, notes: &mut Notes) -> Result<(), ImportError> {
    let Some(raw) = setting(connection, LINKS)? else {
        return Ok(());
    };
    let Some(items) = array(&raw) else {
        notes.note("printer.checkoff_links is not a list");
        return write_setting(connection, LINKS, &json!([]));
    };
    let before = notes.faults.len();
    let mut links = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(reason) = link_drop(item) {
            notes.note(&format!("printer.checkoff_links index {index}: {reason}"));
            continue;
        }
        let Some(link) = links::parse(item, now) else {
            notes.fault(&format!(
                "printer.checkoff_links index {index} did not pass the reader"
            ));
            continue;
        };
        if let Some(object) = item.as_object() {
            unknown(object, LINK_FIELDS, "printer.checkoff_links", notes);
            if object.get("state").and_then(Value::as_str) == Some("applied") {
                notes.change("printer.checkoff_links state", "applied", "verified");
            }
            for unit in object
                .get("units")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if unit.get("part_id").is_some_and(Value::is_string)
                    && links::js_integer(unit.get("part_id"), 1.0).is_some()
                {
                    notes.note("printer.checkoff_links units.part_id stored as text");
                }
            }
        }
        links.push(links::public(&link));
    }
    if notes.faults.len() == before {
        write_setting(connection, LINKS, &Value::Array(links))?;
    }
    Ok(())
}

/// Why Node's `parseLink` returns null. A link it keeps is not listed here.
fn link_drop(raw: &Value) -> Option<&'static str> {
    let Some(row) = raw.as_object() else {
        return Some("is not an object");
    };
    if !row
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| LINK_STATES.contains(&state))
    {
        return Some("state is not a link state");
    }
    for (key, reason) in [
        ("id", "id is missing"),
        ("integration_id", "integration_id is missing"),
        ("printer_id", "printer_id is missing"),
        ("filename", "filename is missing"),
    ] {
        if field_text(row, key).is_none() {
            return Some(reason);
        }
    }
    if links::js_integer(row.get("profile_id"), 1.0).is_none() {
        return Some("profile_id is not a positive integer");
    }
    None
}

fn convert_prints(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let Some(raw) = setting(connection, PRINTS)? else {
        return Ok(());
    };
    let Some(items) = array(&raw) else {
        notes.note("printer.unattributed_prints is not a list");
        return write_setting(connection, PRINTS, &json!([]));
    };
    let before = notes.faults.len();
    let mut prints = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match one_print(item, index, notes) {
            Some(print) => prints.push(print),
            None if notes.faults.len() != before => return Ok(()),
            None => {}
        }
    }
    if notes.faults.len() == before {
        write_setting(connection, PRINTS, &Value::Array(prints))?;
    }
    Ok(())
}

/// `Some` is a print Node keeps, in the typed fields. `None` is a print Node
/// discards, already noted, or a fault, which leaves the setting as stored.
fn one_print(item: &Value, index: usize, notes: &mut Notes) -> Option<Value> {
    let Some(row) = item.as_object() else {
        notes.note(&format!(
            "printer.unattributed_prints index {index}: is not an object"
        ));
        return None;
    };
    if !row.get("id").is_some_and(Value::is_string)
        || !row.get("filename").is_some_and(Value::is_string)
    {
        notes.note(&format!(
            "printer.unattributed_prints index {index}: id or filename is not text"
        ));
        return None;
    }
    unknown(row, &PRINT_FIELDS, "printer.unattributed_prints", notes);
    let mut print = Map::new();
    print.insert("id".into(), row["id"].clone());
    print.insert("filename".into(), row["filename"].clone());
    for key in PRINT_STRINGS {
        match optional_string(row, key) {
            Ok(Some(text)) => {
                print.insert(key.into(), Value::String(text));
            }
            Ok(None) => {}
            Err(()) => {
                notes.fault(&format!(
                    "printer.unattributed_prints index {index}: {key} is not text"
                ));
                return None;
            }
        }
    }
    match row.get("dismissed") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(flag)) => {
            print.insert("dismissed".into(), Value::Bool(*flag));
        }
        Some(_) => {
            notes.fault(&format!(
                "printer.unattributed_prints index {index}: dismissed is not true or false"
            ));
            return None;
        }
    }
    match row.get("claimed_profile_id") {
        None | Some(Value::Null) => {}
        Some(value) => match links::js_integer(Some(value), 1.0).filter(|_| value.is_number()) {
            Some(id) => {
                print.insert("claimed_profile_id".into(), json!(id));
            }
            None => {
                notes.fault(&format!(
                    "printer.unattributed_prints index {index}: claimed_profile_id is not a positive integer"
                ));
                return None;
            }
        },
    }
    if let Some(objects) = row.get("gcode_objects")
        && !objects.is_null()
    {
        let Some(names) = string_list(objects) else {
            notes.fault(&format!(
                "printer.unattributed_prints index {index}: gcode_objects is not a list of text"
            ));
            return None;
        };
        print.insert("gcode_objects".into(), Value::Array(names));
    }
    if let Some(candidates) = row.get("candidates")
        && !candidates.is_null()
    {
        let Some(candidates) = candidates_of(candidates, notes) else {
            notes.fault(&format!(
                "printer.unattributed_prints index {index}: candidates is not the typed list"
            ));
            return None;
        };
        print.insert("candidates".into(), Value::Array(candidates));
    }
    Some(Value::Object(print))
}

fn optional_string(row: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match row.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(()),
    }
}

fn string_list(value: &Value) -> Option<Vec<Value>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(|text| Value::String(text.to_owned())))
        .collect()
}

fn candidates_of(value: &Value, notes: &mut Notes) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for item in value.as_array()? {
        let row = item.as_object()?;
        unknown(
            row,
            &["stl_basename", "copy_count", "matching_filenames"],
            "printer.unattributed_prints candidate",
            notes,
        );
        let mut candidate = Map::new();
        if let Some(name) = row.get("stl_basename") {
            candidate.insert(
                "stl_basename".into(),
                Value::String(name.as_str()?.to_owned()),
            );
        }
        if let Some(count) = row.get("copy_count") {
            let number = count.as_f64().filter(|number| number.is_finite())?;
            candidate.insert("copy_count".into(), json!(number));
        }
        if let Some(names) = row.get("matching_filenames") {
            candidate.insert(
                "matching_filenames".into(),
                Value::Array(string_list(names)?),
            );
        }
        out.push(Value::Object(candidate));
    }
    Some(out)
}

fn convert_bindings(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let Some(raw) = setting(connection, BINDINGS)? else {
        return Ok(());
    };
    if js_trim(&raw).is_empty() {
        return write_setting(connection, BINDINGS, &json!([]));
    }
    let Some(items) = array(&raw) else {
        notes.fault("printer.plan_bindings is not a list");
        return Ok(());
    };
    let mut bindings = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(row) = item.as_object() else {
            notes.fault(&format!(
                "printer.plan_bindings index {index}: is not an object"
            ));
            return Ok(());
        };
        unknown(
            row,
            &["integration_id", "profile_id", "updated_at"],
            "printer.plan_bindings",
            notes,
        );
        let integration = row
            .get("integration_id")
            .and_then(Value::as_str)
            .filter(|text| !js_trim(text).is_empty());
        let updated = row.get("updated_at").and_then(Value::as_str);
        let profile = match row.get("profile_id") {
            Some(Value::Null) => Some(Value::Null),
            Some(value) if value.is_number() => {
                links::js_integer(Some(value), 1.0).map(|id| json!(id))
            }
            _ => None,
        };
        let (Some(integration), Some(updated), Some(profile)) = (integration, updated, profile)
        else {
            notes.fault(&format!(
                "printer.plan_bindings index {index}: is not a binding"
            ));
            return Ok(());
        };
        bindings.push(json!({
            "integration_id": integration,
            "profile_id": profile,
            "updated_at": updated,
        }));
    }
    write_setting(connection, BINDINGS, &Value::Array(bindings))
}

struct QueueItem {
    id: String,
    filename: String,
    artifact_path: String,
    printer_id: String,
    profile_id: Option<i64>,
    matched: Option<String>,
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

fn convert_queue(
    connection: &Connection,
    source: Option<&Path>,
    now: &str,
    notes: &mut Notes,
) -> Result<bool, ImportError> {
    let Some(raw) = setting(connection, QUEUE)? else {
        return Ok(false);
    };
    let occupied: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM printer_send_queue WHERE tenant_id = 'default')",
        [],
        |row| row.get(0),
    )?;
    if occupied {
        notes.fault("printer.send_queue table already holds rows");
        return Ok(false);
    }
    let Some(items) = array(&raw) else {
        notes.note("printer.send_queue is not a list");
        return Ok(true);
    };
    let before = notes.faults.len();
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let Some(row) = one_queue_item(item, index, source, now, notes) else {
            if notes.faults.len() != before {
                return Ok(false);
            }
            continue;
        };
        if !ids.insert(row.id.clone()) {
            notes.fault(&format!("printer.send_queue index {index}: repeated id"));
            return Ok(false);
        }
        rows.push(row);
    }
    if notes.faults.len() != before {
        return Ok(false);
    }
    connection.execute_batch("SAVEPOINT send_queue")?;
    let inserted = insert_queue(connection, &rows);
    if inserted.is_err() {
        connection.execute_batch("ROLLBACK TO send_queue; RELEASE send_queue")?;
        notes.fault("printer.send_queue did not pass the table");
        return Ok(false);
    }
    connection.execute_batch("RELEASE send_queue")?;
    Ok(true)
}

fn insert_queue(connection: &Connection, rows: &[QueueItem]) -> Result<(), rusqlite::Error> {
    for (position, item) in rows.iter().enumerate() {
        connection.execute(
            "INSERT INTO printer_send_queue(
                tenant_id, position, id, filename, artifact_path, printer_id, profile_id,
                match, wait_for_idle, start, state, checkoff_units, upload_job_id, error,
                host_name, created_at, updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                "default",
                position as i64,
                item.id,
                item.filename,
                item.artifact_path,
                item.printer_id,
                item.profile_id,
                item.matched,
                item.wait_for_idle,
                item.start,
                item.state,
                item.units,
                item.upload_job_id,
                item.error,
                item.host_name,
                item.created_at,
                item.updated_at,
            ],
        )?;
    }
    Ok(())
}

fn one_queue_item(
    item: &Value,
    index: usize,
    source: Option<&Path>,
    now: &str,
    notes: &mut Notes,
) -> Option<QueueItem> {
    let Some(row) = item.as_object() else {
        notes.note(&format!(
            "printer.send_queue index {index}: is not an object"
        ));
        return None;
    };
    let state = row.get("state").and_then(Value::as_str);
    if !state.is_some_and(|state| QUEUE_STATES.contains(&state)) {
        notes.note(&format!(
            "printer.send_queue index {index}: state is not a queue state"
        ));
        return None;
    }
    let id = field_text(row, "id");
    let filename = field_text(row, "filename");
    let artifact = field_text(row, "artifact_path");
    let printer = field_text(row, "printer_id");
    let (Some(id), Some(filename), Some(artifact), Some(printer)) =
        (id, filename, artifact, printer)
    else {
        notes.note(&format!(
            "printer.send_queue index {index}: id, filename, artifact_path or printer_id is missing"
        ));
        return None;
    };
    if id.chars().count() > 128 {
        notes.fault(&format!(
            "printer.send_queue index {index}: id is longer than 128 characters"
        ));
        return None;
    }
    unknown(row, QUEUE_FIELDS, "printer.send_queue", notes);
    // The row keeps the stored path. The import rewrites it once the
    // destination folder exists; the report only counts the decision.
    let artifact_path = queue_path(artifact, source, index, notes)?;
    let (state, error) = queue_error(state.unwrap(), row, notes);
    Some(QueueItem {
        id: id.to_owned(),
        filename: filename.to_owned(),
        artifact_path,
        printer_id: printer.to_owned(),
        profile_id: queue_profile(row.get("profile_id"), notes),
        matched: queue_match(row, notes),
        wait_for_idle: row.get("wait_for_idle") != Some(&Value::Bool(false)),
        start: truthy(row.get("start").unwrap_or(&Value::Null)),
        state: state.to_owned(),
        units: queue_units(row.get("checkoff_units"), notes),
        upload_job_id: field_text(row, "upload_job_id").map(str::to_owned),
        error,
        host_name: field_text(row, "host_name").map(str::to_owned),
        created_at: nonempty(row, "created_at").unwrap_or(now).to_owned(),
        updated_at: nonempty(row, "updated_at").unwrap_or(now).to_owned(),
    })
}

fn queue_path(
    artifact: &str,
    source: Option<&Path>,
    index: usize,
    notes: &mut Notes,
) -> Option<String> {
    if placed(artifact, source).is_some() {
        notes.note("rebased printer.send_queue artifact_path");
    } else if escapes(artifact, source) {
        notes.fault(&format!(
            "printer.send_queue index {index}: artifact_path leaves the installation"
        ));
        return None;
    } else {
        notes.note("kept printer.send_queue artifact_path");
    }
    Some(artifact.to_owned())
}

fn queue_error(
    state: &str,
    row: &Map<String, Value>,
    notes: &mut Notes,
) -> (String, Option<String>) {
    match state {
        "queued" => {
            notes.change("printer.send_queue state", "queued", "error");
            ("error".into(), Some(NOT_STARTED_ERROR.to_owned()))
        }
        "sending" => {
            notes.change("printer.send_queue state", "sending", "error");
            ("error".into(), Some(INTERRUPTED_ERROR.to_owned()))
        }
        other => (
            other.to_owned(),
            field_text(row, "error").map(|text| text.chars().take(500).collect()),
        ),
    }
}

fn queue_profile(value: Option<&Value>, notes: &mut Notes) -> Option<i64> {
    let value = value.filter(|value| !value.is_null())?;
    let id = links::js_integer(Some(value), 1.0);
    if value.is_string() && id.is_some() {
        notes.note("printer.send_queue profile_id stored as text");
    }
    if id.is_none() {
        notes.note("dropped printer.send_queue profile_id");
    }
    id
}

fn queue_match(row: &Map<String, Value>, notes: &mut Notes) -> Option<String> {
    match row.get("match").and_then(Value::as_str) {
        Some("compatible") => Some("compatible".into()),
        Some("pinned") => Some("pinned".into()),
        Some(_) => {
            notes.note("dropped printer.send_queue match");
            None
        }
        None => None,
    }
}

fn queue_units(value: Option<&Value>, notes: &mut Notes) -> Option<String> {
    let value = value.filter(|value| !value.is_null())?;
    let Some(items) = value.as_array() else {
        notes.note("dropped printer.send_queue checkoff_units");
        return None;
    };
    let mut units = Vec::new();
    for item in items {
        let Some(part) = links::js_integer(item.get("part_id"), 1.0) else {
            notes.note("dropped printer.send_queue checkoff_unit");
            continue;
        };
        let Some(index) = links::js_integer(item.get("unit_index"), 0.0) else {
            notes.note("dropped printer.send_queue checkoff_unit");
            continue;
        };
        let mut unit = json!({ "part_id": part, "unit_index": index });
        if let Some(name) = item.get("object_name").and_then(trimmed) {
            unit["object_name"] = Value::String(links::js_slice(name, 200));
        }
        units.push(unit);
    }
    if units.is_empty() {
        None
    } else {
        serde_json::to_string(&units).ok()
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

/// The path below `source` when `path` lies under it and every part is a
/// plain name. `None` is outside, or a path that climbs out.
fn placed(path: &str, source: Option<&Path>) -> Option<Vec<String>> {
    let rest = below(path, source?)?;
    if rest.is_empty() || rest.iter().any(|part| !plain(part)) {
        return None;
    }
    Some(rest)
}

fn escapes(path: &str, source: Option<&Path>) -> bool {
    source
        .and_then(|source| below(path, source))
        .is_some_and(|rest| !rest.is_empty() && rest.iter().any(|part| !plain(part)))
}

fn below(path: &str, source: &Path) -> Option<Vec<String>> {
    let absolute = std::path::absolute(source).ok()?;
    let source = absolute.to_string_lossy();
    let source = source.trim_end_matches(['/', '\\']);
    let root: Vec<&str> = source.split(['/', '\\']).collect();
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts.len() < root.len() || parts[..root.len()] != root[..] {
        return None;
    }
    Some(
        parts[root.len()..]
            .iter()
            .map(|part| (*part).to_owned())
            .collect(),
    )
}

fn plain(part: &str) -> bool {
    let bytes = part.as_bytes();
    // `C:` and `C:name` are drive-relative. `Path::push` on Windows replaces
    // the destination with that drive.
    let drive = bytes.get(1) == Some(&b':') && bytes.first().is_some_and(u8::is_ascii_alphabetic);
    !part.is_empty() && part != "." && part != ".." && !drive
}

/// Deletes the legacy queue setting once the report has counted it.
pub(super) fn drop_send_queue(connection: &Connection) -> Result<u64, ImportError> {
    let deleted = connection.execute(
        "DELETE FROM app_settings WHERE tenant_id = 'default' AND key = ?1",
        [QUEUE],
    )?;
    Ok(deleted as u64)
}

/// Rewrites queue paths that lie under the Node folder onto `destination`.
/// The rows were inserted with the stored path so the report could count them
/// first. An archive has no folder, so its paths stay as stored.
pub(super) fn place_artifacts(
    connection: &Connection,
    source: Option<&Path>,
    destination: &Path,
) -> Result<(), ImportError> {
    let Some(source) = source else {
        return Ok(());
    };
    let mut statement = connection.prepare(
        "SELECT id, artifact_path FROM printer_send_queue WHERE tenant_id = 'default' ORDER BY position",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (id, path) in rows {
        let Some(rest) = placed(&path, Some(source)) else {
            if escapes(&path, Some(source)) {
                return Err(ImportError::Settings(vec![Counted {
                    name: "printer.send_queue artifact_path leaves the installation".into(),
                    count: 1,
                }]));
            }
            continue;
        };
        let mut target = destination.to_path_buf();
        for part in &rest {
            target.push(part);
        }
        let target = pp_os::display_path(&target);
        connection.execute(
            "UPDATE printer_send_queue SET artifact_path = ?1 WHERE tenant_id = 'default' AND id = ?2",
            (target.to_string_lossy(), id),
        )?;
    }
    Ok(())
}
