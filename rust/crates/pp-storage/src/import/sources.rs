//! Converts Source metadata, import rules, categories, activity payloads and
//! the custom filament file. A value Node's reader already discards is dropped
//! and named. Anything that reader would keep, and the typed shape cannot
//! store, refuses the import. The report names keys, indexes and counts. It
//! does not print a stored value.

use super::{ImportError, convert::SettingsReport, io_error};
use crate::read_model::views::js_trim;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, path::Path};

const CATEGORIES: &str = "source_categories";
const FILE: &str = "custom_filaments.json";
const METADATA_KEYS: [&str; 7] = [
    "category",
    "sync_required",
    "sync_error",
    "remote_update_status",
    "remote_checked_at",
    "naming",
    "catalog_metadata_version",
];
const EVENT_KEYS: [&str; 3] = ["source_id", "source_name", "error"];
const FILAMENT_KEYS: [&str; 5] = ["id", "display_name", "product_line", "hex", "combo_label"];

struct Notes {
    notes: BTreeMap<String, u64>,
    faults: BTreeMap<String, u64>,
}

impl Notes {
    fn note(&mut self, name: &str) {
        *self.notes.entry(name.to_owned()).or_default() += 1;
    }

    fn fault(&mut self, name: &str) {
        *self.faults.entry(name.to_owned()).or_default() += 1;
    }

    fn failed(&self) -> bool {
        !self.faults.is_empty()
    }

    fn report(&self) -> SettingsReport {
        let list = |map: &BTreeMap<String, u64>| {
            map.iter()
                .map(|(name, count)| super::convert::Counted {
                    name: name.clone(),
                    count: *count,
                })
                .collect()
        };
        SettingsReport {
            changes: Vec::new(),
            notes: list(&self.notes),
            credential_addresses: 0,
            faults: list(&self.faults),
        }
    }
}

/// Rewrites the Source rows on the upgraded copy. `documents` is the folder
/// the filament file is read from. The converted file, when there is one, is
/// returned and not written: the inventory counts the copied bytes first.
pub(super) fn convert(
    connection: &Connection,
    documents: &Path,
) -> Result<(SettingsReport, Option<String>), ImportError> {
    let mut notes = Notes {
        notes: BTreeMap::new(),
        faults: BTreeMap::new(),
    };
    projects(connection, &mut notes)?;
    categories(connection, &mut notes)?;
    events(connection, &mut notes)?;
    let body = (read_store(documents)?)
        .and_then(|text| store(&text, &mut notes))
        .filter(|_| !notes.failed());
    Ok((notes.report(), body))
}

fn projects(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let mut statement = connection.prepare(
        "SELECT id, metadata_json, imported_paths FROM projects WHERE tenant_id = 'default'",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let rows: Vec<_> = rows.collect::<rusqlite::Result<_>>()?;
    for (id, metadata, paths) in rows {
        if let Some(raw) = metadata.filter(|text| !text.is_empty()) {
            connection.execute(
                "UPDATE projects SET metadata_json = ?1 WHERE id = ?2",
                (metadata_text(&raw, notes), id),
            )?;
        }
        if let Some(raw) = paths.filter(|text| !text.is_empty()) {
            connection.execute(
                "UPDATE projects SET imported_paths = ?1 WHERE id = ?2",
                (paths_text(&raw, notes), id),
            )?;
        }
    }
    Ok(())
}

fn metadata_text(raw: &str, notes: &mut Notes) -> String {
    let Ok(Value::Object(map)) = serde_json::from_str(raw) else {
        notes.note("projects.metadata_json is not an object");
        return serde_json::to_string(&json!({"raw": raw})).expect("object");
    };
    let mut out = Map::new();
    for (key, value) in map {
        let value = if key == "category" {
            category_value(value, notes)
        } else {
            if !METADATA_KEYS.contains(&key.as_str()) {
                notes.note(&format!("kept projects.metadata_json {key}"));
            }
            value
        };
        out.insert(key, value);
    }
    serde_json::to_string(&out).expect("object")
}

fn category_value(value: Value, notes: &mut Notes) -> Value {
    let Some(text) = value.as_str() else {
        notes.note("projects.metadata_json category is not text");
        return value;
    };
    let trimmed = js_trim(text);
    if trimmed != text {
        notes.note("projects.metadata_json category trimmed");
    }
    Value::String(trimmed.to_owned())
}

fn paths_text(raw: &str, notes: &mut Notes) -> String {
    let trimmed = js_trim(raw);
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) else {
        notes.note("projects.imported_paths is not a list");
        return "[]".to_owned();
    };
    let mut rules = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(text) = item.as_str() else {
            notes.note(&format!(
                "projects.imported_paths index {index}: is not text"
            ));
            continue;
        };
        if js_trim(text).is_empty() {
            notes.note(&format!("projects.imported_paths index {index}: is blank"));
            continue;
        }
        let rule = normalize_rule(text);
        if rule != text {
            notes.note("projects.imported_paths normalized");
        }
        rules.push(rule);
    }
    serde_json::to_string(&rules).expect("list")
}

/// The normalization the import-rule readers share: JavaScript trim, slash
/// separators, no leading slash, and a trailing slash unless the rule is a
/// file that ends in `.stl`.
fn normalize_rule(rule: &str) -> String {
    let replaced = rule.replace('\\', "/");
    let rule = js_trim(&replaced).trim_start_matches('/');
    if rule.is_empty() || rule.ends_with('/') || rule.to_lowercase().ends_with(".stl") {
        rule.to_owned()
    } else {
        format!("{rule}/")
    }
}

fn categories(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let text: Option<String> = connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = ?1",
            [CATEGORIES],
            |row| row.get(0),
        )
        .optional()?;
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    let stored = category_text(&text, notes);
    connection.execute(
        "UPDATE app_settings SET value = ?1 WHERE tenant_id = 'default' AND key = ?2",
        (stored, CATEGORIES),
    )?;
    Ok(())
}

fn category_text(raw: &str, notes: &mut Notes) -> String {
    let Ok(Value::Array(items)) = serde_json::from_str(raw) else {
        notes.note("source_categories is not a list");
        return "[]".to_owned();
    };
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(text) = item.as_str() else {
            notes.note(&format!("source_categories index {index}: is not text"));
            continue;
        };
        let path = category_path(text);
        let segments: Vec<&str> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        if path.is_empty() || segments.len() > 9 {
            notes.note(&format!(
                "source_categories index {index}: is not a category path"
            ));
            continue;
        }
        if path != text {
            notes.note("source_categories normalized");
        }
        for len in 1..=segments.len() {
            let prefix = segments[..len].join("/");
            let seen = out
                .iter()
                .any(|stored: &String| stored.to_lowercase() == prefix.to_lowercase());
            if seen {
                continue;
            }
            if len < segments.len() {
                notes.note("source_categories expanded");
            }
            out.push(prefix);
        }
    }
    serde_json::to_string(&out).expect("list")
}

fn category_path(raw: &str) -> String {
    raw.split('/')
        .map(js_trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn events(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let mut statement = connection.prepare(
        "SELECT id, payload_json FROM app_events WHERE tenant_id = 'default' AND payload_json IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let rows: Vec<_> = rows.collect::<rusqlite::Result<_>>()?;
    for (id, raw) in rows {
        if raw.is_empty() {
            continue;
        }
        connection.execute(
            "UPDATE app_events SET payload_json = ?1 WHERE id = ?2",
            (event_text(id, &raw, notes), id),
        )?;
    }
    Ok(())
}

fn event_text(id: i64, raw: &str, notes: &mut Notes) -> String {
    let Ok(Value::Object(map)) = serde_json::from_str(raw) else {
        notes.note(&format!(
            "app_events id {id}: payload_json is not an object"
        ));
        return "{}".to_owned();
    };
    let mut out = Map::new();
    for (key, value) in map {
        if key == "source_id" && !value.is_number() {
            notes.note(&format!("app_events id {id}: source_id is not a number"));
        } else if (key == "source_name" || key == "error") && !value.is_string() {
            notes.note(&format!("app_events id {id}: {key} is not text"));
        } else if !EVENT_KEYS.contains(&key.as_str()) {
            notes.note(&format!("kept app_events {key}"));
        }
        out.insert(key, value);
    }
    serde_json::to_string(&out).expect("object")
}

fn read_store(documents: &Path) -> Result<Option<String>, ImportError> {
    let path = documents.join(FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(&path, error)),
    }
}

fn store(text: &str, notes: &mut Notes) -> Option<String> {
    if serde_json::from_str::<Value>(text).is_ok_and(|value| value.is_null()) {
        notes.fault("custom_filaments.json is null");
        return None;
    }
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) else {
        notes.note("custom_filaments.json is not a store");
        return Some(r#"{"filaments":[]}"#.to_owned());
    };
    let mut kept = Map::new();
    for (key, value) in &map {
        if key != "filaments" {
            notes.note(&format!("dropped custom_filaments.json {key}"));
            continue;
        }
        kept.insert(key.clone(), value.clone());
    }
    let Some(entries) = kept.get("filaments").and_then(Value::as_array) else {
        if map.contains_key("filaments") {
            notes.fault("custom_filaments.json filaments is not a list");
            return None;
        }
        notes.note("custom_filaments.json is not a store");
        return Some(r#"{"filaments":[]}"#.to_owned());
    };
    let mut filaments = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(fields) = entry.as_object() else {
            notes.fault(&format!(
                "custom_filaments.json index {index}: is not an object"
            ));
            return None;
        };
        let entry = one_filament(fields, index, notes)?;
        filaments.push(entry);
    }
    Some(serde_json::to_string(&json!({"filaments": filaments})).expect("store"))
}

fn one_filament(
    fields: &Map<String, Value>,
    index: usize,
    notes: &mut Notes,
) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    for (key, value) in fields {
        if !FILAMENT_KEYS.contains(&key.as_str()) {
            notes.note(&format!("kept custom_filaments.json {key}"));
            out.insert(key.clone(), value.clone());
            continue;
        }
        let Some(text) = value.as_str() else {
            notes.fault(&format!(
                "custom_filaments.json index {index}: {key} is not text"
            ));
            return None;
        };
        let trimmed = js_trim(text);
        if trimmed != text {
            notes.note("custom_filaments.json trimmed");
        }
        out.insert(key.clone(), Value::String(trimmed.to_owned()));
    }
    Some(out)
}
