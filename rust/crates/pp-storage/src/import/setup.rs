//! Step 3 of the import: the stored `printer.fleet` and `integrations`
//! settings become the documents [`crate::printer_setup`] reads.
//!
//! Node read both settings leniently on every use. Rust's readers type every
//! field they name, so the import writes each document once in Rust's shape.
//! Nothing stored is dropped and no value is guessed:
//!
//! - A field Rust names converts the way Node read it: an id that is a
//!   number becomes text, a size written as text becomes a number, a field
//!   Node defaulted gets that default, and of two spellings of an integration
//!   field the one Node read counts. Bed width, depth and height round down
//!   to whole millimetres, the margin rounds up, and each changed size is
//!   listed.
//! - A key Rust does not name is kept with its value in the row's, or the
//!   config's, [`Unread`] map. The report counts such keys by name.
//! - A value Rust's shape cannot hold is counted in [`SetupReport::refused`]
//!   and the import refuses the installation. A dry run lists the counts.
//! - A kind Rust does not create is kept byte for byte, and an address that
//!   holds a username or password stays as stored.
//!
//! [`write`] then reads both settings back with Rust's own readers and checks
//! that every stored entry and every unread key arrived.

use super::ImportError;
use crate::{
    auth::Secret,
    printer_setup::{
        LoadedFilament, Printer, ServiceKind, SetupDocument, Unread,
        integrations::{self, HostConfig, Integration},
        printers::{self, MAX_FILAMENT_SLOTS},
    },
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, value::RawValue};
use std::{collections::BTreeMap, fmt};

pub(super) const FLEET: &str = "printer.fleet";
pub(super) const INTEGRATIONS: &str = "integrations";

const PRINTER: &str = "printer";
/// An integration whose `type` cannot be read.
const INTEGRATION: &str = "integration";
const NOT_A_LIST: &str = "(the setting is not a list)";
const NOT_AN_OBJECT: &str = "(an entry is not an object)";
const REPEATED_ID: &str = "(an id is stored twice)";

/// The printer fields Rust stores, which are the ones Node read.
const PRINTER_FIELDS: [&str; 13] = [
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
const FILAMENT_FIELDS: [&str; 3] = ["slot", "filament_color_id", "label"];
const INTEGRATION_FIELDS: [&str; 6] = ["id", "type", "name", "config", "created_at", "updated_at"];
const CONFIG_FIELDS: [&str; 6] = [
    "base_url", "enabled", "username", "api_key", "password", "storage",
];
/// Node's second spelling of a config field, read when the first holds
/// nothing.
const SECOND_SPELLINGS: [(&str, &str); 4] = [
    ("base_url", "baseUrl"),
    ("api_key", "apiKey"),
    ("username", "user"),
    ("storage", "storage_path"),
];

/// One stored length the import changed to whole millimetres.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SizeChange {
    /// The printer's place in the stored list, from 1.
    pub printer: usize,
    pub field: &'static str,
    /// The stored number.
    pub stored: String,
    pub converted: u32,
}

/// How many printers, or integrations of one kind, hold something, by its
/// name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Counted {
    /// `printer`, the integration's stored `type`, or `integration` when
    /// that cannot be read.
    pub kind: &'static str,
    /// A field or key, with `config.` before one of an integration's config.
    /// A key that is not a plain identifier is shown as `?`.
    pub name: String,
    pub count: u64,
}

/// An integration whose stored address holds a username or password. The
/// import keeps the address as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CredentialAddress {
    /// The integration's place in the stored list, from 1.
    pub entry: usize,
    pub kind: ServiceKind,
}

/// What the conversion of the two settings found. It holds field and key
/// names, places in a list, counts and changed sizes, and no address,
/// credential, name or other stored value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SetupReport {
    /// Printers and integrations in the converted documents.
    pub printers: u64,
    pub integrations: u64,
    pub sizes: Vec<SizeChange>,
    /// Stored keys Rust's shape does not name. Each is kept with its value in
    /// the converted row and never read.
    pub kept_keys: Vec<Counted>,
    /// Stored values Rust's shape cannot hold. The import refuses while any
    /// is listed.
    pub refused: Vec<Counted>,
    pub credential_addresses: Vec<CredentialAddress>,
}

fn lines(f: &mut fmt::Formatter<'_>, counted: &[Counted]) -> fmt::Result {
    for Counted { kind, name, count } in counted {
        writeln!(f, "  {kind} {name} = {count}")?;
    }
    Ok(())
}

impl fmt::Display for SetupReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Bed sizes rounded to whole millimetres: {}",
            self.sizes.len()
        )?;
        for size in &self.sizes {
            writeln!(
                f,
                "  printer {} {}: {} -> {}",
                size.printer, size.field, size.stored, size.converted
            )?;
        }
        writeln!(
            f,
            "Printer and integration keys kept unread (kind, key, rows that hold it): {}",
            self.kept_keys.len()
        )?;
        lines(f, &self.kept_keys)?;
        if !self.refused.is_empty() {
            writeln!(
                f,
                "Printer and integration values Rust's documents cannot hold (kind, field, rows): {}. The import refuses this installation.",
                self.refused.len()
            )?;
            lines(f, &self.refused)?;
        }
        writeln!(
            f,
            "Integration addresses that hold a username or password, kept as stored: {}",
            self.credential_addresses.len()
        )
    }
}

/// The two documents as the import stores them. `None` where the source
/// stores no such setting; nothing is written for it. No `Debug`: the
/// integrations text holds credentials.
pub(super) struct Converted {
    pub(super) fleet: Option<String>,
    pub(super) integrations: Option<String>,
    pub(super) report: SetupReport,
}

/// A key as the report shows it.
fn shown(prefix: &str, key: &str) -> String {
    let plain = |character: char| character.is_ascii_alphanumeric() || character == '_';
    let plain = !key.is_empty() && key.len() <= 40 && key.chars().all(plain);
    format!("{prefix}{}", if plain { key } else { "?" })
}

/// Counts by kind and name, for the kept keys and for the refusals.
#[derive(Default)]
struct Notes {
    kept: BTreeMap<(&'static str, String), u64>,
    refused: BTreeMap<(&'static str, String), u64>,
}

impl Notes {
    fn refuse(&mut self, kind: &'static str, name: &str) {
        *self.refused.entry((kind, name.to_owned())).or_default() += 1;
    }

    fn counted(counts: BTreeMap<(&'static str, String), u64>) -> Vec<Counted> {
        let counted = |((kind, name), count)| Counted { kind, name, count };
        counts.into_iter().map(counted).collect()
    }
}

/// The fields of one stored object as its row's shape reads them. A value
/// the shape cannot hold is noted in `wrong` by its field, and the row is
/// then not converted.
struct Fields<'a> {
    object: &'a Map<String, Value>,
    /// `config.` for an integration's config.
    prefix: &'static str,
    wrong: Vec<String>,
}

impl<'a> Fields<'a> {
    fn of(object: &'a Map<String, Value>, prefix: &'static str) -> Self {
        let wrong = Vec::new();
        Self {
            object,
            prefix,
            wrong,
        }
    }

    fn refuse(&mut self, field: &str) {
        let name = shown(self.prefix, field);
        if !self.wrong.contains(&name) {
            self.wrong.push(name);
        }
    }

    /// The stored value. Absent and null are both nothing, as for Node.
    fn held(&self, field: &str) -> Option<&'a Value> {
        self.object.get(field).filter(|value| !value.is_null())
    }

    /// Text, or `None` for nothing.
    fn text(&mut self, field: &str) -> Option<String> {
        match self.held(field)? {
            Value::String(text) => Some(text.clone()),
            _ => {
                self.refuse(field);
                None
            }
        }
    }

    /// Text the shape cannot do without.
    fn needed(&mut self, field: &str) -> String {
        if self.held(field).is_none() {
            self.refuse(field);
        }
        self.text(field).unwrap_or_default()
    }

    /// A number, or text that holds one as Node's `Number` read it.
    fn number(&mut self, field: &str) -> Option<f64> {
        let number = match self.held(field)? {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        };
        let number = number.filter(|number| number.is_finite());
        if number.is_none() {
            self.refuse(field);
        }
        number
    }

    /// The keys `known` does not name, with their values.
    fn unread(&self, kind: &'static str, known: &[&str], notes: &mut Notes) -> Unread {
        let mut keys = Map::new();
        let unnamed = |(key, _): &(&String, &Value)| !known.contains(&key.as_str());
        for (key, value) in self.object.iter().filter(unnamed) {
            *(notes.kept)
                .entry((kind, shown(self.prefix, key)))
                .or_default() += 1;
            keys.insert(key.clone(), value.clone());
        }
        Unread::holding(keys)
    }
}

/// One stored length in whole millimetres: `up` rounds up, otherwise down.
/// Nothing stored becomes `default`. Each length that changes is listed.
fn length(
    fields: &mut Fields<'_>,
    (field, up, default): (&'static str, bool, Option<u32>),
    printer: usize,
    sizes: &mut Vec<SizeChange>,
) -> Option<u32> {
    let Some(stored) = fields.number(field) else {
        return default;
    };
    let whole = if up { stored.ceil() } else { stored.floor() };
    if !(0.0..=f64::from(u32::MAX)).contains(&whole) {
        fields.refuse(field);
        return default;
    }
    // A whole number in `u32`'s range, so the cast is exact.
    let converted = whole as u32;
    if Some(f64::from(converted)) != Some(stored) {
        sizes.push(SizeChange {
            printer,
            field,
            stored: stored.to_string(),
            converted,
        });
    }
    Some(converted)
}

/// One row per slot from 1 to `slots`. Node filled a slot without a row with
/// an empty one. A row Node would have discarded, for a slot outside that
/// range or stored twice, is refused, and so is a key the row's shape does
/// not name: the stored row has no place to keep one.
fn loaded_filaments(fields: &mut Fields<'_>, slots: u8) -> Vec<LoadedFilament> {
    let field = "loaded_filaments";
    let none = Vec::new();
    let stored = match fields.held(field) {
        None => &none,
        Some(Value::Array(stored)) => stored,
        Some(_) => {
            fields.refuse(field);
            return Vec::new();
        }
    };
    let mut by_slot = BTreeMap::new();
    for filament in stored {
        let Some(filament) = filament.as_object() else {
            fields.refuse(field);
            continue;
        };
        let mut row = Fields::of(filament, "loaded_filaments.");
        let unnamed = |key: &&String| !FILAMENT_FIELDS.contains(&key.as_str());
        for key in filament.keys().filter(unnamed) {
            row.refuse(key);
        }
        // Node read a missing slot as 1.
        let stored = match row.held("slot") {
            None => Some(1.0),
            Some(_) => row.number("slot"),
        };
        let slot = (1..=slots).find(|slot| stored == Some(f64::from(*slot)));
        let filament = LoadedFilament {
            slot: slot.unwrap_or_default(),
            filament_color_id: row.text("filament_color_id"),
            label: row.text("label").unwrap_or_default(),
        };
        if slot.is_none() || by_slot.insert(filament.slot, filament).is_some() {
            row.refuse("slot");
        }
        // A printer counts once for a field, however many of its rows.
        row.wrong.retain(|name| !fields.wrong.contains(name));
        fields.wrong.append(&mut row.wrong);
    }
    let empty = |slot| LoadedFilament {
        slot,
        filament_color_id: None,
        label: String::new(),
    };
    (1..=slots)
        .map(|slot| by_slot.remove(&slot).unwrap_or_else(|| empty(slot)))
        .collect()
}

/// One stored printer in Rust's shape, with Node's defaults.
fn printer(
    fields: &mut Fields<'_>,
    entry: usize,
    sizes: &mut Vec<SizeChange>,
    notes: &mut Notes,
) -> Printer {
    let id = match fields.held("id") {
        Some(Value::String(id)) if !id.is_empty() => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => {
            fields.refuse("id");
            String::new()
        }
    };
    let name = fields.text("name");
    let model = fields.text("model").or_else(|| name.clone());
    // Node held the slots to 1 to 4 on every read.
    let slots = fields.number("max_filament_slots").unwrap_or(1.0);
    if slots.fract() != 0.0 {
        fields.refuse("max_filament_slots");
    }
    let slots = slots.clamp(1.0, f64::from(MAX_FILAMENT_SLOTS)) as u8;
    // Node read a blank link as none.
    let link = |fields: &mut Fields<'_>, field| {
        let stored = fields.text(field)?;
        Some(stored.trim().to_owned()).filter(|stored| !stored.is_empty())
    };
    // Node read a slicer it did not know as none, which would drop it.
    let slicer = link(fields, "preferred_slicer").map(|slicer| slicer.to_lowercase());
    if (slicer.as_deref()).is_some_and(|slicer| !["orca", "prusa", "bambu"].contains(&slicer)) {
        fields.refuse("preferred_slicer");
    }
    let mut size = |fields: &mut Fields<'_>, field| length(fields, field, entry, sizes);
    Printer {
        id,
        model: model.unwrap_or_else(|| "Printer".into()),
        name: name.unwrap_or_else(|| "Printer".into()),
        bed_width_mm: size(fields, ("bed_width_mm", false, Some(250))).unwrap_or_default(),
        bed_depth_mm: size(fields, ("bed_depth_mm", false, Some(210))).unwrap_or_default(),
        bed_height_mm: size(fields, ("bed_height_mm", false, None)),
        margin_mm: size(fields, ("margin_mm", true, Some(4))).unwrap_or_default(),
        max_filament_slots: slots,
        loaded_filaments: loaded_filaments(fields, slots),
        integration_id: link(fields, "integration_id"),
        device_id: link(fields, "device_id"),
        preset_id: link(fields, "preset_id"),
        preferred_slicer: slicer,
        unread: fields.unread(PRINTER, &PRINTER_FIELDS, notes),
    }
}

/// The elements of a stored list, each as its text. Text that is no JSON
/// list is refused.
fn elements(stored: &str, kind: &'static str, notes: &mut Notes) -> Vec<Box<RawValue>> {
    serde_json::from_str(stored).unwrap_or_else(|_| {
        notes.refuse(kind, NOT_A_LIST);
        Vec::new()
    })
}

/// The stored fleet in Rust's shape, in stored order.
fn fleet(stored: &str, report: &mut SetupReport, notes: &mut Notes) -> Vec<Printer> {
    let mut printers: Vec<Printer> = Vec::new();
    for (index, raw) in elements(stored, PRINTER, notes).iter().enumerate() {
        let row = serde_json::from_str::<Value>(raw.get()).ok();
        let Some(row) = row.as_ref().and_then(Value::as_object) else {
            notes.refuse(PRINTER, NOT_AN_OBJECT);
            continue;
        };
        let mut fields = Fields::of(row, "");
        let printer = printer(&mut fields, index + 1, &mut report.sizes, notes);
        if fields.wrong.is_empty() && printers.iter().any(|known| known.id == printer.id) {
            fields.wrong.push(REPEATED_ID.to_owned());
        }
        for field in &fields.wrong {
            notes.refuse(PRINTER, field);
        }
        if fields.wrong.is_empty() {
            printers.push(printer);
        }
    }
    printers
}

/// Whether `address` is a URL with a username or password in it.
fn holds_credentials(address: &str) -> bool {
    url::Url::parse(address).is_ok_and(|url| !url.username().is_empty() || url.password().is_some())
}

/// One row of the converted integrations document.
#[derive(Serialize)]
#[serde(untagged)]
enum Row {
    Host(Box<Integration>),
    /// A kind Rust cannot create: its stored text.
    Kept(Box<RawValue>),
}

fn kind_name(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Moonraker => "moonraker",
        ServiceKind::Prusalink => "prusalink",
        ServiceKind::Spoolman => "spoolman",
        ServiceKind::Bambu => "bambu",
        ServiceKind::HomeAssistant => "home_assistant",
        ServiceKind::AiAssistant => "ai_assistant",
        ServiceKind::SlicerSidecar => "slicer_sidecar",
    }
}

/// The config of a stored Moonraker, PrusaLink or Spoolman row in Rust's
/// shape. Where a field holds nothing and Node's second spelling of it holds
/// a value, that value is the field's, as Node read it. A second spelling
/// Node did not read is one more unread key.
fn host_config(fields: &mut Fields<'_>, kind: ServiceKind, notes: &mut Notes) -> HostConfig {
    let mut read = Vec::from(CONFIG_FIELDS);
    let mut text = |fields: &mut Fields<'_>, field: &'static str| {
        let second = SECOND_SPELLINGS.iter().find(|(first, _)| *first == field);
        match second {
            Some((_, second)) if fields.held(field).is_none() => {
                read.push(*second);
                fields.text(second)
            }
            _ => fields.text(field),
        }
    };
    let base_url = text(fields, "base_url");
    if base_url.is_none() {
        fields.refuse("base_url");
    }
    let enabled = match fields.held("enabled") {
        None => true,
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => {
            fields.refuse("enabled");
            true
        }
    };
    let secret = |text: Option<String>| text.map(Secret::new);
    HostConfig {
        base_url: base_url.unwrap_or_default(),
        enabled,
        username: text(fields, "username"),
        api_key: secret(text(fields, "api_key")),
        password: secret(text(fields, "password")),
        storage: text(fields, "storage"),
        unread: fields.unread(kind_name(kind), &read, notes),
    }
}

/// A stored Moonraker, PrusaLink or Spoolman row in Rust's shape. `None`
/// when a value does not fit, with each such field noted.
fn host(row: &Map<String, Value>, kind: ServiceKind, notes: &mut Notes) -> Option<Integration> {
    let mut fields = Fields::of(row, "");
    let config = match fields.held("config") {
        Some(Value::Object(config)) => {
            let mut config = Fields::of(config, "config.");
            let converted = host_config(&mut config, kind, notes);
            fields.wrong.append(&mut config.wrong);
            Some(converted)
        }
        _ => {
            fields.refuse("config");
            None
        }
    };
    let needed = ["id", "name", "created_at", "updated_at"].map(|field| fields.needed(field));
    let [id, name, created_at, updated_at] = needed;
    let unread = fields.unread(kind_name(kind), &INTEGRATION_FIELDS, notes);
    for field in &fields.wrong {
        notes.refuse(kind_name(kind), field);
    }
    let config = config.filter(|_| fields.wrong.is_empty())?;
    Some(Integration {
        id,
        kind,
        name,
        config,
        created_at,
        updated_at,
        unread,
    })
}

/// The stored integrations in Rust's shape, in stored order.
fn hosts(stored: &str, report: &mut SetupReport, notes: &mut Notes) -> Vec<Row> {
    let (mut rows, mut ids) = (Vec::new(), Vec::<String>::new());
    let stored = elements(stored, INTEGRATION, notes);
    for (index, raw) in stored.into_iter().enumerate() {
        let entry = index + 1;
        let value = serde_json::from_str::<Value>(raw.get()).ok();
        let Some(row) = value.as_ref().and_then(Value::as_object) else {
            notes.refuse(INTEGRATION, NOT_AN_OBJECT);
            continue;
        };
        let Some(Ok(kind)) = row.get("type").map(ServiceKind::deserialize) else {
            notes.refuse(INTEGRATION, "type");
            continue;
        };
        // Node listed an integration only when its id is text.
        let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
        let fault = match id {
            "" => Some("id"),
            id if ids.iter().any(|known| known == id) => Some(REPEATED_ID),
            _ => None,
        };
        if let Some(fault) = fault {
            notes.refuse(kind_name(kind), fault);
            continue;
        }
        ids.push(id.to_owned());
        // The address of a converted row is its `base_url`. A kept row's
        // config is not Rust's, so every text in it is looked at.
        let (row, held) = if kind.creatable() {
            let Some(host) = host(row, kind, notes) else {
                continue;
            };
            let held = holds_credentials(&host.config.base_url);
            (Row::Host(Box::new(host)), held)
        } else {
            let config = row.get("config").and_then(Value::as_object);
            let texts = config.into_iter().flat_map(Map::values);
            let held = texts.filter_map(Value::as_str).any(holds_credentials);
            (Row::Kept(raw), held)
        };
        if held {
            let address = CredentialAddress { entry, kind };
            report.credential_addresses.push(address);
        }
        rows.push(row);
    }
    rows
}

/// A converted document as stored text.
fn written(setting: &'static str, document: &impl Serialize) -> Result<String, ImportError> {
    serde_json::to_string(document).map_err(|_| ImportError::Setup {
        setting,
        fault: None,
    })
}

/// Converts the stored text of both settings. Absent or empty text is no
/// setting, as for Node and for Rust's readers.
pub(super) fn convert(
    fleet_text: Option<&str>,
    integrations_text: Option<&str>,
) -> Result<Converted, ImportError> {
    let (mut report, mut notes) = (SetupReport::default(), Notes::default());
    let stored = |text: Option<&str>| text.filter(|text| !text.is_empty()).map(str::to_owned);
    let fleet_text = match stored(fleet_text) {
        None => None,
        Some(text) => {
            let printers = fleet(&text, &mut report, &mut notes);
            report.printers = printers.len() as u64;
            Some(written(FLEET, &printers)?)
        }
    };
    let integrations_text = match stored(integrations_text) {
        None => None,
        Some(text) => {
            let rows = hosts(&text, &mut report, &mut notes);
            report.integrations = rows.len() as u64;
            Some(written(INTEGRATIONS, &rows)?)
        }
    };
    report.kept_keys = Notes::counted(notes.kept);
    report.refused = Notes::counted(notes.refused);
    Ok(Converted {
        fleet: fleet_text,
        integrations: integrations_text,
        report,
    })
}

/// The stored text of one setting.
fn setting(connection: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    let sql = "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = ?1";
    connection
        .query_row(sql, [key], |row| row.get(0))
        .optional()
}

/// Reads both settings of the staged database and converts them. Nothing is
/// written.
pub(super) fn plan(connection: &Connection) -> Result<Converted, ImportError> {
    let fleet = setting(connection, FLEET)?;
    let integrations = setting(connection, INTEGRATIONS)?;
    convert(fleet.as_deref(), integrations.as_deref())
}

/// Whether the stored entry `found` holds what `source` held: the same id,
/// and every key Rust's shape does not name with its value. `named` lists
/// the keys Rust names in a row and in its `config`; a kind Rust does not
/// create names none, so its whole entry must be the same.
fn arrived(source: &Value, found: &Value, named: [&[&str]; 2]) -> bool {
    let id = |entry: &Value| match &entry["id"] {
        Value::Number(id) => Some(id.to_string()),
        id => id.as_str().map(str::to_owned),
    };
    let kept = |source: &Value, found: &Value, named: &[&str]| {
        let unnamed = |(key, _): &(&String, &Value)| !named.contains(&key.as_str());
        let mut keys = source.as_object().into_iter().flatten().filter(unnamed);
        keys.all(|(key, value)| found.get(key) == Some(value))
    };
    // A second spelling may have been read into its field.
    let second = SECOND_SPELLINGS.map(|(_, second)| second);
    let config: Vec<&str> = named[1].iter().chain(&second).copied().collect();
    id(source).is_some()
        && id(source) == id(found)
        && kept(source, found, named[0])
        && kept(&source["config"], &found["config"], &config)
}

/// Writes the converted documents into the staged database and checks them
/// without the code that converted them. Each setting is read back. Rust's
/// own reader must take it. It must hold the source's entries, one for one
/// and in the source's order, and each must still hold every key Rust does
/// not name, with its value.
pub(super) fn write(connection: &Connection, converted: &Converted) -> Result<(), ImportError> {
    let documents = [
        (FLEET, &converted.fleet, SetupDocument::Printers),
        (
            INTEGRATIONS,
            &converted.integrations,
            SetupDocument::Integrations,
        ),
    ];
    for (key, text, document) in documents {
        let Some(text) = text else { continue };
        let fault = |fault| ImportError::Setup {
            setting: key,
            fault,
        };
        let source = setting(connection, key)?.unwrap_or_default();
        let sql = "UPDATE app_settings SET value = ?1 WHERE tenant_id = 'default' AND key = ?2";
        connection.execute(sql, [text.as_str(), key])?;
        let found = setting(connection, key)?.unwrap_or_default();
        let read = match document {
            SetupDocument::Printers => printers::parse(&found).map(drop),
            SetupDocument::Integrations => integrations::parse(&found).map(drop),
        };
        read.map_err(|invalid| fault(Some(invalid.reason)))?;
        let entries = |text: &str| serde_json::from_str::<Vec<Value>>(text).unwrap_or_default();
        let (source, entries) = (entries(&source), entries(&found));
        let named = |entry: &Value| -> [&[&str]; 2] {
            let kind = ServiceKind::deserialize(&entry["type"]);
            match document {
                SetupDocument::Printers => [&PRINTER_FIELDS, &[]],
                SetupDocument::Integrations if kind.is_ok_and(ServiceKind::creatable) => {
                    [&INTEGRATION_FIELDS, &CONFIG_FIELDS]
                }
                SetupDocument::Integrations => [&[], &[]],
            }
        };
        let mut pairs = source.iter().zip(&entries);
        let same = found == *text
            && source.len() == entries.len()
            && pairs.all(|(source, found)| arrived(source, found, named(source)));
        if !same {
            return Err(fault(None));
        }
    }
    Ok(())
}
