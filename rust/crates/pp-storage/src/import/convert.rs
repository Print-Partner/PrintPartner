//! Converts the stored `printer.fleet` and `integrations` documents into the
//! typed shapes Settings reads. Checkoff links, unmatched prints and the send
//! queue are converted beside them in [`super::checkoff`]. Source metadata,
//! import rules, categories, activity and custom filaments are converted in
//! [`super::sources`]. A key the typed shape has no place for is dropped and
//! named, never valued. A value the typed shape cannot hold refuses the
//! import; the dry run counts it and still prints the rest.

use super::ImportError;
use crate::{
    auth::Secret,
    printer_setup::{
        Unread,
        integrations::{self, Entry, HostConfig, Integration, ServiceKind},
        printers::{self, LoadedFilament, Printer},
    },
};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{collections::BTreeMap, fmt, path::Path};

const FLEET: &str = "printer.fleet";
const INTEGRATIONS: &str = "integrations";
const SLICERS: [&str; 3] = ["orca", "prusa", "bambu"];

/// One converted field, with the stored and converted text and how many rows
/// changed the same way. The text is an id or a size, never a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldChange {
    pub field: String,
    pub stored: String,
    pub converted: String,
    pub count: u64,
}

/// A named outcome with a count and no stored value: a dropped key, a moved
/// alias, a filled string, or a reason the document cannot be read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Counted {
    pub name: String,
    pub count: u64,
}

/// What the conversion did. The dry run and the saved import report both
/// carry it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SettingsReport {
    pub changes: Vec<FieldChange>,
    /// Dropped keys, moved aliases and filled strings. No stored value.
    pub notes: Vec<Counted>,
    /// Addresses that hold a username or password. They are kept as stored.
    pub credential_addresses: u64,
    /// Why a document is not the typed shape. The import writes nothing when
    /// this is not empty.
    pub faults: Vec<Counted>,
}

impl SettingsReport {
    pub(super) fn write(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\nPrinters, integrations, Checkoff, sends and Sources:")?;
        writeln!(f, "  changes (field, stored, converted, count):")?;
        if self.changes.is_empty() {
            writeln!(f, "    none")?;
        }
        for change in &self.changes {
            writeln!(
                f,
                "    {}: {} -> {} = {}",
                change.field, change.stored, change.converted, change.count
            )?;
        }
        writeln!(f, "  dropped, moved or filled (name and count, no value):")?;
        if self.notes.is_empty() {
            writeln!(f, "    none")?;
        }
        for note in &self.notes {
            writeln!(f, "    {} = {}", note.name, note.count)?;
        }
        writeln!(
            f,
            "  addresses with a username or password, kept as stored: {}",
            self.credential_addresses
        )?;
        if !self.faults.is_empty() {
            writeln!(f, "  not the typed shape:")?;
            for fault in &self.faults {
                writeln!(f, "    {} = {}", fault.name, fault.count)?;
            }
            writeln!(f, "  The import would refuse this installation.")?;
        }
        Ok(())
    }

    pub(super) fn refused(&self) -> bool {
        !self.faults.is_empty()
    }
}

struct Notes {
    changes: BTreeMap<(String, String, String), u64>,
    notes: BTreeMap<String, u64>,
    faults: BTreeMap<String, u64>,
    seen: u64,
    credentials: u64,
}

impl Notes {
    fn change(&mut self, field: &str, stored: &str, converted: &str) {
        let key = (field.to_owned(), stored.to_owned(), converted.to_owned());
        *self.changes.entry(key).or_default() += 1;
    }

    fn note(&mut self, name: &str) {
        *self.notes.entry(name.to_owned()).or_default() += 1;
    }

    fn fault(&mut self, name: &str) {
        self.seen += 1;
        *self.faults.entry(name.to_owned()).or_default() += 1;
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
            credential_addresses: self.credentials,
            faults: list(&self.faults),
        }
    }
}

/// Rewrites the two settings on the upgraded copy when they can become the
/// typed documents. A document that cannot is left as stored, and the report
/// says why.
pub(super) fn rewrite(
    connection: &Connection,
    source: Option<&Path>,
    documents: &Path,
) -> Result<(SettingsReport, bool, Option<String>), ImportError> {
    let mut notes = Notes {
        changes: BTreeMap::new(),
        notes: BTreeMap::new(),
        faults: BTreeMap::new(),
        seen: 0,
        credentials: 0,
    };
    if let Some(text) = stored(connection, FLEET)?
        && let Some(written) = convert_fleet(&text, &mut notes)
    {
        store(connection, FLEET, &written)?;
    }
    if let Some(text) = stored(connection, INTEGRATIONS)?
        && let Some(written) = convert_integrations(&text, &mut notes)
    {
        store(connection, INTEGRATIONS, &written)?;
    }
    let (checkoff, drop_queue) = super::checkoff::convert(connection, source)?;
    let (sources, filaments) = super::sources::convert(connection, documents)?;
    let mut report = notes.report();
    report.changes.extend(checkoff.changes);
    report.notes.extend(checkoff.notes);
    report.faults.extend(checkoff.faults);
    report.changes.extend(sources.changes);
    report.notes.extend(sources.notes);
    report.faults.extend(sources.faults);
    report.changes.sort_by(|left, right| {
        (&left.field, &left.stored, &left.converted).cmp(&(
            &right.field,
            &right.stored,
            &right.converted,
        ))
    });
    report
        .notes
        .sort_by(|left, right| left.name.cmp(&right.name));
    report
        .faults
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok((report, drop_queue, filaments))
}

fn stored(connection: &Connection, key: &str) -> Result<Option<String>, ImportError> {
    let text = connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(text.filter(|text: &String| !text.is_empty()))
}

fn store(connection: &Connection, key: &str, text: &str) -> Result<(), ImportError> {
    connection.execute(
        "UPDATE app_settings SET value = ?1 WHERE tenant_id = 'default' AND key = ?2",
        (text, key),
    )?;
    Ok(())
}

fn convert_fleet(text: &str, notes: &mut Notes) -> Option<String> {
    let before = notes.seen;
    let rows = match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(rows)) => rows,
        _ => {
            notes.fault("printer.fleet is not a list");
            return None;
        }
    };
    let mut printers = Vec::new();
    let mut ids = BTreeMap::<String, ()>::new();
    for row in &rows {
        let Some(printer) = one_printer(row, notes) else {
            continue;
        };
        if ids.insert(printer.id.clone(), ()).is_some() {
            notes.fault("printer.fleet has a repeated id");
            continue;
        }
        printers.push(printer);
    }
    if notes.seen != before {
        return None;
    }
    let written = serde_json::to_string(&printers).ok()?;
    if printers::parse(&written).is_err() {
        notes.fault("printer.fleet did not pass the typed reader");
        return None;
    }
    Some(written)
}

fn one_printer(row: &Value, notes: &mut Notes) -> Option<Printer> {
    let Some(object) = row.as_object() else {
        notes.fault("printer.fleet entry is not an object");
        return None;
    };
    drop_unknown(object, PRINTER_FIELDS, "printer", "", notes);
    let id = printer_id(object, notes)?;
    let name = text_or_default(object, "name", "Printer", notes)?;
    let model = model_of(object, &name, notes)?;
    let bed_width_mm = millimetres(object, "bed_width_mm", Some(250), false, notes)?;
    let bed_depth_mm = millimetres(object, "bed_depth_mm", Some(210), false, notes)?;
    let bed_height_mm = millimetres(object, "bed_height_mm", None, false, notes);
    let margin_mm = millimetres(object, "margin_mm", Some(4), true, notes)?;
    let max_filament_slots = slots_of(object, notes)?;
    let loaded_filaments = filaments(object, max_filament_slots, notes)?;
    let integration_id = optional_id(object, "integration_id", notes)?;
    let device_id = optional_id(object, "device_id", notes)?;
    let preset_id = optional_id(object, "preset_id", notes)?;
    let preferred_slicer = slicer_of(object, notes)?;
    Some(Printer {
        id,
        name,
        model,
        bed_width_mm,
        bed_depth_mm,
        bed_height_mm,
        margin_mm,
        max_filament_slots,
        loaded_filaments,
        integration_id,
        device_id,
        preset_id,
        preferred_slicer,
        unread: Unread::default(),
    })
}

const PRINTER_FIELDS: &[&str] = &[
    "id",
    "name",
    "model",
    "bed_width_mm",
    "bed_depth_mm",
    "bed_height_mm",
    "margin_mm",
    "max_filament_slots",
    "loaded_filaments",
    "integration_id",
    "device_id",
    "preset_id",
    "preferred_slicer",
];

fn printer_id(object: &Map<String, Value>, notes: &mut Notes) -> Option<String> {
    let Some(value) = object.get("id") else {
        notes.fault("printer.fleet id is missing");
        return None;
    };
    let text = match value {
        Value::String(text) if !text.is_empty() => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::String(_) => {
            notes.fault("printer.fleet id is empty");
            return None;
        }
        _ => {
            notes.fault("printer.fleet id is not text or a number");
            return None;
        }
    };
    if value.as_str() != Some(text.as_str()) {
        notes.change("printer.fleet id", &value.to_string(), &quote(&text));
    }
    Some(text)
}

fn text_or_default(
    object: &Map<String, Value>,
    field: &str,
    default: &str,
    notes: &mut Notes,
) -> Option<String> {
    match object.get(field) {
        Some(Value::String(text)) => Some(text.clone()),
        None | Some(Value::Null) => {
            notes.change(&format!("printer.fleet {field}"), "absent", &quote(default));
            Some(default.to_owned())
        }
        Some(_) => {
            notes.fault(&format!("printer.fleet {field} is not text"));
            None
        }
    }
}

fn model_of(object: &Map<String, Value>, name: &str, notes: &mut Notes) -> Option<String> {
    match object.get("model") {
        Some(Value::String(text)) => Some(text.clone()),
        None | Some(Value::Null) if object.get("name").and_then(Value::as_str).is_some() => {
            notes.note("filled printer model from its name");
            Some(name.to_owned())
        }
        None | Some(Value::Null) => {
            notes.change("printer.fleet model", "absent", &quote(name));
            Some(name.to_owned())
        }
        Some(_) => {
            notes.fault("printer.fleet model is not text");
            None
        }
    }
}

/// Whole millimetres. Width, depth and height round down; the margin rounds
/// up. `default` fills a missing size. A missing height stays missing.
fn millimetres(
    object: &Map<String, Value>,
    field: &str,
    default: Option<u32>,
    up: bool,
    notes: &mut Notes,
) -> Option<u32> {
    let Some(value) = object.get(field).filter(|value| !value.is_null()) else {
        if let Some(fill) = default {
            notes.change(
                &format!("printer.fleet {field}"),
                "absent",
                &fill.to_string(),
            );
        }
        return default;
    };
    let Some(mm) = whole_millimetres(value, up) else {
        notes.fault(&format!("printer.fleet {field} is not a millimetre size"));
        return None;
    };
    if value.as_u64() != Some(u64::from(mm)) {
        notes.change(
            &format!("printer.fleet {field}"),
            &value.to_string(),
            &mm.to_string(),
        );
    }
    Some(mm)
}

fn whole_millimetres(value: &Value, up: bool) -> Option<u32> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let rounded = if up { number.ceil() } else { number.floor() };
    if rounded > f64::from(u32::MAX) {
        return None;
    }
    Some(rounded as u32)
}

fn slots_of(object: &Map<String, Value>, notes: &mut Notes) -> Option<u8> {
    let Some(value) = object
        .get("max_filament_slots")
        .filter(|value| !value.is_null())
    else {
        notes.change("printer.fleet max_filament_slots", "absent", "1");
        return Some(1);
    };
    let number = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    let Some(number) = number.filter(|number| number.is_finite() && number.fract() == 0.0) else {
        notes.fault("printer.fleet max_filament_slots is not a whole number");
        return None;
    };
    let slots = number.clamp(1.0, 4.0) as u8;
    if value.as_u64() != Some(u64::from(slots)) {
        notes.change(
            "printer.fleet max_filament_slots",
            &value.to_string(),
            &slots.to_string(),
        );
    }
    Some(slots)
}

fn filaments(
    object: &Map<String, Value>,
    slots: u8,
    notes: &mut Notes,
) -> Option<Vec<LoadedFilament>> {
    let stored = object
        .get("loaded_filaments")
        .filter(|value| !value.is_null());
    let rows = match stored {
        None => Vec::new(),
        Some(Value::Array(rows)) => rows.clone(),
        Some(_) => {
            notes.fault("printer.fleet loaded_filaments is not a list");
            return None;
        }
    };
    for row in &rows {
        if let Some(row) = row.as_object() {
            drop_unknown(
                row,
                &["slot", "filament_color_id", "label"],
                "printer",
                "loaded_filaments.",
                notes,
            );
        }
    }
    let mut by_slot = vec![None; usize::from(slots)];
    for row in &rows {
        let one = one_filament(row, slots, &mut by_slot, notes)?;
        if one {
            notes.note("dropped printer loaded_filaments outside its slots");
        }
    }
    let filled: Vec<LoadedFilament> = by_slot
        .into_iter()
        .enumerate()
        .map(|(index, one)| {
            one.unwrap_or(LoadedFilament {
                slot: (index + 1) as u8,
                filament_color_id: None,
                label: String::new(),
            })
        })
        .collect();
    if stored.is_none() {
        notes.change(
            "printer.fleet loaded_filaments",
            "absent",
            &format!("{slots} slots"),
        );
    } else if serde_json::to_value(&filled).ok().as_ref() != stored {
        notes.note("rewrote printer loaded_filaments");
    }
    Some(filled)
}

/// `true` when the row's slot is not one this printer has, so it is dropped.
fn one_filament(
    row: &Value,
    slots: u8,
    by_slot: &mut [Option<LoadedFilament>],
    notes: &mut Notes,
) -> Option<bool> {
    let Some(object) = row.as_object() else {
        notes.fault("printer.fleet loaded_filaments entry is not an object");
        return None;
    };
    let slot = match object.get("slot") {
        None | Some(Value::Null) => 1,
        Some(value) => match whole_slot(value) {
            Some(slot) => slot,
            None => {
                notes.fault("printer.fleet loaded_filaments.slot is not a whole number");
                return None;
            }
        },
    };
    if slot == 0 || slot > slots {
        return Some(true);
    }
    let filament_color_id = match object.get("filament_color_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            notes.fault("printer.fleet filament_color_id is not text");
            return None;
        }
    };
    let label = match object.get("label") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            notes.fault("printer.fleet label is not text");
            return None;
        }
    };
    let index = usize::from(slot - 1);
    if by_slot[index].is_some() {
        notes.fault("printer.fleet loaded_filaments repeats a slot");
        return None;
    }
    by_slot[index] = Some(LoadedFilament {
        slot,
        filament_color_id,
        label,
    });
    Some(false)
}

fn whole_slot(value: &Value) -> Option<u8> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    (number.is_finite() && number.fract() == 0.0 && (0.0..=255.0).contains(&number))
        .then_some(number as u8)
}

fn optional_id(
    object: &Map<String, Value>,
    field: &str,
    notes: &mut Notes,
) -> Option<Option<String>> {
    let Some(value) = object.get(field) else {
        return Some(None);
    };
    let text = match value {
        Value::Null => return Some(None),
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => {
            notes.fault(&format!("printer.fleet {field} is not text"));
            return None;
        }
    };
    if text.is_empty() {
        notes.note(&format!("cleared printer {field}"));
        return Some(None);
    }
    if value.as_str() != Some(text.as_str()) {
        notes.change(
            &format!("printer.fleet {field}"),
            &value.to_string(),
            &quote(&text),
        );
    }
    Some(Some(text))
}

fn slicer_of(object: &Map<String, Value>, notes: &mut Notes) -> Option<Option<String>> {
    let Some(value) = object.get("preferred_slicer") else {
        return Some(None);
    };
    let Value::String(text) = value else {
        if value.is_null() {
            return Some(None);
        }
        notes.fault("printer.fleet preferred_slicer is not text");
        return None;
    };
    let raw = text.trim().to_ascii_lowercase();
    if SLICERS.contains(&raw.as_str()) {
        if text != raw.as_str() {
            notes.change("printer.fleet preferred_slicer", &quote(text), &quote(&raw));
        }
        return Some(Some(raw));
    }
    notes.note("cleared printer preferred_slicer");
    Some(None)
}

fn convert_integrations(text: &str, notes: &mut Notes) -> Option<String> {
    let before = notes.seen;
    let rows = match serde_json::from_str::<Vec<Box<serde_json::value::RawValue>>>(text) {
        Ok(rows) => rows,
        Err(_) => {
            notes.fault("integrations is not a list");
            return None;
        }
    };
    let mut entries = Vec::new();
    let mut ids = BTreeMap::<String, ()>::new();
    for raw in rows {
        let Some(entry) = one_integration(raw, notes) else {
            continue;
        };
        if ids.insert(entry.id().to_owned(), ()).is_some() {
            notes.fault("integrations has a repeated id");
            continue;
        }
        entries.push(entry);
    }
    if notes.seen != before {
        return None;
    }
    let written = serde_json::to_string(&entries).ok()?;
    if integrations::parse(&written).is_err() {
        notes.fault("integrations did not pass the typed reader");
        return None;
    }
    Some(written)
}

fn one_integration(raw: Box<serde_json::value::RawValue>, notes: &mut Notes) -> Option<Entry> {
    let Ok(value) = serde_json::from_str::<Value>(raw.get()) else {
        notes.fault("integrations entry is not json");
        return None;
    };
    let Some(object) = value.as_object() else {
        notes.fault("integrations entry is not an object");
        return None;
    };
    let Some(kind) = object
        .get("type")
        .and_then(|kind| serde_json::from_value::<ServiceKind>(kind.clone()).ok())
    else {
        notes.fault("integrations type is not one Rust keeps");
        return None;
    };
    if !kind.creatable() {
        return kept(raw, object, notes);
    }
    host(object, kind, notes).map(Entry::Host)
}

fn kept(
    raw: Box<serde_json::value::RawValue>,
    object: &Map<String, Value>,
    notes: &mut Notes,
) -> Option<Entry> {
    count_addresses(object.get("config"), notes);
    match Entry::parse(raw) {
        Ok(entry) => Some(entry),
        Err(_) => {
            notes.fault("integrations kept row is not the typed shape");
            None
        }
    }
}

fn host(object: &Map<String, Value>, kind: ServiceKind, notes: &mut Notes) -> Option<Integration> {
    let label = kind_name(kind);
    drop_unknown(object, HOST_FIELDS, &label, "", notes);
    let id = required_text(object, "id", &format!("{label} id"), notes)?;
    if id.is_empty() {
        notes.fault(&format!("{label} id is empty"));
        return None;
    }
    let name = required_text(object, "name", &format!("{label} name"), notes)?;
    let config = object.get("config").and_then(Value::as_object);
    let Some(config) = config else {
        notes.fault(&format!("{label} config is not an object"));
        return None;
    };
    let config = host_config(config, &label, notes)?;
    let created_at = required_text(object, "created_at", &format!("{label} created_at"), notes)?;
    let updated_at = required_text(object, "updated_at", &format!("{label} updated_at"), notes)?;
    Some(Integration {
        id,
        kind,
        name,
        config,
        created_at,
        updated_at,
        unread: Unread::default(),
    })
}

const HOST_FIELDS: &[&str] = &["id", "type", "name", "config", "created_at", "updated_at"];

fn host_config(object: &Map<String, Value>, kind: &str, notes: &mut Notes) -> Option<HostConfig> {
    drop_unknown(object, CONFIG_FIELDS, kind, "config.", notes);
    let base_url = aliased(object, "base_url", "baseUrl", kind, notes)?;
    let Some(base_url) = base_url else {
        notes.fault(&format!("{kind} config.base_url is missing"));
        return None;
    };
    if has_userinfo(&base_url) {
        notes.credentials += 1;
    }
    Some(HostConfig {
        base_url,
        enabled: enabled(object, kind, notes),
        username: aliased(object, "username", "user", kind, notes)?,
        api_key: secret(aliased(object, "api_key", "apiKey", kind, notes)?),
        password: secret(plain(object, "password", kind, notes)?),
        storage: aliased(object, "storage", "storage_path", kind, notes)?,
        unread: Unread::default(),
    })
}

const CONFIG_FIELDS: &[&str] = &[
    "base_url",
    "baseUrl",
    "enabled",
    "username",
    "user",
    "api_key",
    "apiKey",
    "password",
    "storage",
    "storage_path",
];

fn enabled(object: &Map<String, Value>, kind: &str, notes: &mut Notes) -> bool {
    match object.get("enabled") {
        Some(Value::Bool(value)) => *value,
        None | Some(Value::Null) => {
            notes.change(&format!("{kind} config.enabled"), "absent", "true");
            true
        }
        Some(_) => {
            notes.change(&format!("{kind} config.enabled"), "not a boolean", "true");
            true
        }
    }
}

/// The canonical field, or the alias when the canonical one is absent. A
/// wrong type refuses. The alias is named and never valued.
fn aliased(
    object: &Map<String, Value>,
    canonical: &str,
    alias: &str,
    kind: &str,
    notes: &mut Notes,
) -> Option<Option<String>> {
    let primary = object.get(canonical).filter(|value| !value.is_null());
    let secondary = object.get(alias).filter(|value| !value.is_null());
    if secondary.is_some() && primary.is_some() {
        notes.note(&format!("dropped {kind} config.{alias}"));
    }
    let (value, moved) = match (primary, secondary) {
        (Some(value), _) => (value, false),
        (None, Some(value)) => (value, true),
        (None, None) => return Some(None),
    };
    let Value::String(text) = value else {
        notes.fault(&format!("{kind} config.{canonical} is not text"));
        return None;
    };
    if moved {
        notes.note(&format!("moved {kind} config.{alias} to {canonical}"));
    }
    Some(Some(text.clone()))
}

fn plain(
    object: &Map<String, Value>,
    field: &str,
    kind: &str,
    notes: &mut Notes,
) -> Option<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => {
            notes.fault(&format!("{kind} config.{field} is not text"));
            None
        }
    }
}

fn required_text(
    object: &Map<String, Value>,
    field: &str,
    label: &str,
    notes: &mut Notes,
) -> Option<String> {
    match object.get(field) {
        Some(Value::String(text)) => Some(text.clone()),
        _ => {
            notes.fault(&format!("{label} is missing"));
            None
        }
    }
}

fn secret(value: Option<String>) -> Option<Secret> {
    value.map(Secret::new)
}

fn count_addresses(config: Option<&Value>, notes: &mut Notes) {
    let Some(object) = config.and_then(Value::as_object) else {
        return;
    };
    for field in ["base_url", "baseUrl", "host"] {
        if let Some(Value::String(text)) = object.get(field)
            && has_userinfo(text)
        {
            notes.credentials += 1;
        }
    }
}

fn has_userinfo(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|url| !url.username().is_empty() || url.password().is_some())
}

fn drop_unknown(
    object: &Map<String, Value>,
    known: &[&str],
    kind: &str,
    prefix: &str,
    notes: &mut Notes,
) {
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            notes.note(&format!("dropped {kind} {prefix}{key}"));
        }
    }
}

fn kind_name(kind: ServiceKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "integration".to_owned())
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}
