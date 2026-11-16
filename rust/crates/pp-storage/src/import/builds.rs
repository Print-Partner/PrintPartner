//! Converts Builds, Plans, feature toggles and API keys. A value today's
//! reader already discards is dropped and named. Anything that reader would
//! keep, and the typed shape cannot store, refuses the import. The shapes
//! marked checked are read and left as stored. The report names keys and
//! counts. It does not print a stored value, a path or a secret.

use super::{
    ImportError,
    convert::{Counts as Notes, SettingsReport},
    io_error,
};
use crate::manifest_options::KitManifest;
use pp_domain::features::{Feature, Toggles};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{fs, io::ErrorKind, path::Path};

const TOGGLES: &str = "feature_toggles";
const PLAN_TOGGLES: &str = "feature_toggles:plan:";
const SETUP: &str = "production_setup:";
const ROLES: &str = "role_filaments_";
const VARIANTS: &str = "plan_variant_selection_";
const KITS: &str = "kit_manifest_";
const KEYS: &str = "api_keys_v1";
const NAMING: &str = "stl_naming_defaults";
const BRIEF: &str = "build_planning.v1.";
const CREATE: &str = "build_planning.create.";
const SNAPSHOT: &str = ".printpartner-source-snapshot.json";
const MAX_MANIFEST: u64 = 8 * 1024 * 1024;
const ROLE_FIELDS: [&str; 3] = [
    "filament_color_id",
    "filament_custom_hex",
    "spoolman_spool_id",
];
const KIT_FIELDS: [&str; 10] = [
    "name",
    "layers",
    "base_source_id",
    "addon_source_ids",
    "selections",
    "include",
    "exclude",
    "replacements",
    "choice_tree",
    "category_links",
];

/// One role's filament choice. A spool id is text, including one Node stored
/// as a JSON number.
#[derive(Serialize)]
struct RoleFilament {
    filament_color_id: Option<String>,
    filament_custom_hex: Option<String>,
    spoolman_spool_id: Option<String>,
}

/// `plan_variant_selection_{build}`: a record of strings. Every value in the
/// map is a JSON string.
#[derive(Serialize)]
#[serde(transparent)]
struct VariantSelection(Map<String, Value>);

/// `plan_snapshots.payload_json`. Known members stay; every other key stays
/// in `rest`. Absent members stay absent.
#[derive(Serialize, Deserialize)]
struct SnapshotPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layers: Option<Vec<Map<String, Value>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kit_manifest: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipe_summary: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    captured_at: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accepted_plan_revision_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accepted_plan_version: Option<Value>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// Rewrites the Build and Plan rows on the upgraded copy. `documents` is the
/// folder `repos/` is read from. Snapshot files are checked and not written.
pub(super) fn convert(
    connection: &Connection,
    documents: &Path,
) -> Result<SettingsReport, ImportError> {
    let mut notes = Notes::new();
    let stored = settings(connection)?;
    layers(connection, &stored, &mut notes)?;
    setups(connection, &stored, &mut notes)?;
    roles(connection, &stored, &mut notes)?;
    variants(connection, &stored, &mut notes)?;
    kits(connection, &stored, &mut notes)?;
    snapshots(connection, &mut notes)?;
    api_keys(connection, &stored, &mut notes)?;
    checked(connection, documents, &stored, &mut notes)?;
    Ok(notes.report())
}

fn settings(connection: &Connection) -> Result<Vec<(String, String)>, ImportError> {
    let mut statement =
        connection.prepare("SELECT key, value FROM app_settings WHERE tenant_id = 'default'")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn replace(
    connection: &Connection,
    key: &str,
    raw: &str,
    written: &str,
) -> Result<(), ImportError> {
    if written != raw {
        super::convert::store(connection, key, written)?;
    }
    Ok(())
}

fn json_text(value: &impl Serialize) -> Result<String, ImportError> {
    serde_json::to_string(value).map_err(|error| ImportError::Database(error.to_string()))
}

/// A positive Build id, written the way the key stores it. `01` is not `1`.
fn build_id(suffix: &str) -> Option<i64> {
    let id = suffix.parse::<u64>().ok()?;
    let id = pp_domain::PositiveId::new(id).ok()?;
    if suffix != id.get().to_string() {
        return None;
    }
    i64::try_from(id.get()).ok()
}

fn layers(
    connection: &Connection,
    stored: &[(String, String)],
    notes: &mut Notes,
) -> Result<(), ImportError> {
    for (key, raw) in stored {
        let plan = key.strip_prefix(PLAN_TOGGLES);
        if key != TOGGLES && plan.is_none() {
            continue;
        }
        if let Some(suffix) = plan
            && build_id(suffix).is_none()
        {
            notes.fault("feature_toggles plan key is not a Build id");
            continue;
        }
        if let Some(written) = layer(raw, notes)? {
            replace(connection, key, raw, &written)?;
        }
    }
    Ok(())
}

fn layer(raw: &str, notes: &mut Notes) -> Result<Option<String>, ImportError> {
    let Ok(Value::Object(map)) = serde_json::from_str(raw) else {
        notes.fault("feature_toggles is not an object");
        return Ok(None);
    };
    let mut toggles = Toggles::default();
    for (key, value) in &map {
        match Feature::from_key(key) {
            Some(feature) => match value.as_bool() {
                Some(enabled) => toggles.set(feature, Some(enabled)),
                None => {
                    notes.fault("feature_toggles choice is not a boolean");
                    return Ok(None);
                }
            },
            None => notes.note(&format!("dropped feature_toggles {key}")),
        }
    }
    let written = json_text(&toggles)?;
    if written != raw {
        notes.note("feature_toggles stored as Toggles");
    }
    Ok(Some(written))
}

fn setups(
    connection: &Connection,
    stored: &[(String, String)],
    notes: &mut Notes,
) -> Result<(), ImportError> {
    for (key, raw) in stored {
        let Some(suffix) = key.strip_prefix(SETUP) else {
            continue;
        };
        let Some(id) = build_id(suffix) else {
            notes.fault("production_setup key is not a Build id");
            continue;
        };
        let written = match crate::production::setup::stored_setup(Some(raw), id) {
            Some(value) => {
                let written = json_text(&value)?;
                if written != raw.as_str() {
                    notes.note("production_setup stored as ProductionSetup");
                }
                written
            }
            None => {
                // The reader ignores the document and synthesizes the default.
                notes.note("production_setup ignored");
                json_text(&crate::production::setup::default_setup(id))?
            }
        };
        replace(connection, key, raw, &written)?;
    }
    Ok(())
}

fn roles(
    connection: &Connection,
    stored: &[(String, String)],
    notes: &mut Notes,
) -> Result<(), ImportError> {
    for (key, raw) in stored {
        if key
            .strip_prefix(ROLES)
            .is_none_or(|suffix| build_id(suffix).is_none())
        {
            if key.starts_with(ROLES) {
                notes.fault("role_filaments key is not a Build id");
            }
            continue;
        }
        if let Some(written) = role_text(raw, notes)? {
            replace(connection, key, raw, &written)?;
        }
    }
    Ok(())
}

fn role_text(raw: &str, notes: &mut Notes) -> Result<Option<String>, ImportError> {
    let Ok(Value::Object(roles)) = serde_json::from_str(raw) else {
        notes.note("role_filaments ignored");
        return Ok(Some("{}".to_owned()));
    };
    let mut kept = Map::new();
    for (role, value) in roles {
        let Some(fields) = value.as_object() else {
            notes.note("role_filaments role is not an object");
            continue;
        };
        kept.insert(role, Value::Object(one_role(fields, notes)?));
    }
    let written = json_text(&kept)?;
    if written != raw {
        notes.note("role_filaments stored as RoleFilament");
    }
    Ok(Some(written))
}

fn one_role(
    fields: &Map<String, Value>,
    notes: &mut Notes,
) -> Result<Map<String, Value>, ImportError> {
    let role = RoleFilament {
        filament_color_id: role_text_field(fields, "filament_color_id", notes),
        filament_custom_hex: role_text_field(fields, "filament_custom_hex", notes),
        spoolman_spool_id: spool_id(fields, notes),
    };
    for key in fields.keys() {
        if !ROLE_FIELDS.contains(&key.as_str()) {
            notes.note(&format!("dropped role_filaments {key}"));
        }
    }
    let value =
        serde_json::to_value(role).map_err(|error| ImportError::Database(error.to_string()))?;
    let Value::Object(object) = value else {
        return Err(ImportError::Database(
            "role filament was not an object".to_owned(),
        ));
    };
    Ok(object)
}

fn role_text_field(fields: &Map<String, Value>, key: &str, notes: &mut Notes) -> Option<String> {
    match fields.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            notes.note(&format!("role_filaments {key} is not text"));
            None
        }
    }
}

fn spool_id(fields: &Map<String, Value>, notes: &mut Notes) -> Option<String> {
    match fields.get("spoolman_spool_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Number(number)) => {
            notes.note("role_filaments spool id stored as text");
            Some(number.to_string())
        }
        Some(_) => {
            notes.note("role_filaments spoolman_spool_id is not text");
            None
        }
    }
}

fn variants(
    connection: &Connection,
    stored: &[(String, String)],
    notes: &mut Notes,
) -> Result<(), ImportError> {
    for (key, raw) in stored {
        if key
            .strip_prefix(VARIANTS)
            .is_none_or(|suffix| build_id(suffix).is_none())
        {
            if key.starts_with(VARIANTS) {
                notes.fault("plan_variant_selection key is not a Build id");
            }
            continue;
        }
        if let Some(written) = variant_text(raw, notes)? {
            replace(connection, key, raw, &written)?;
        }
    }
    Ok(())
}

fn variant_text(raw: &str, notes: &mut Notes) -> Result<Option<String>, ImportError> {
    let Ok(Value::Object(choices)) = serde_json::from_str(raw) else {
        // Node's reader returns `{}` for text that is not a JSON object.
        notes.note("plan_variant_selection ignored");
        return Ok(Some("{}".to_owned()));
    };
    let mut kept = Map::new();
    for (key, value) in choices {
        match value {
            Value::String(text) => {
                kept.insert(key, Value::String(text));
            }
            Value::Number(number) => {
                notes.note("plan_variant_selection value stored as text");
                kept.insert(key, Value::String(number.to_string()));
            }
            _ => {
                notes.fault("plan_variant_selection value is not text");
                return Ok(None);
            }
        }
    }
    let written = json_text(&VariantSelection(kept))?;
    if written != raw {
        notes.note("plan_variant_selection stored as VariantSelection");
    }
    Ok(Some(written))
}

fn kits(
    connection: &Connection,
    stored: &[(String, String)],
    notes: &mut Notes,
) -> Result<(), ImportError> {
    for (key, raw) in stored {
        if key
            .strip_prefix(KITS)
            .is_none_or(|suffix| build_id(suffix).is_none())
        {
            if key.starts_with(KITS) {
                notes.fault("kit_manifest key is not a Build id");
            }
            continue;
        }
        replace(connection, key, raw, &kit_text(raw, notes)?)?;
    }
    Ok(())
}

fn kit_text(raw: &str, notes: &mut Notes) -> Result<String, ImportError> {
    let manifest = match KitManifest::from_stored(raw) {
        Ok(manifest) => {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) {
                for key in map.keys() {
                    if !KIT_FIELDS.contains(&key.as_str()) {
                        notes.note(&format!("dropped kit_manifest {key}"));
                    }
                }
            }
            manifest
        }
        Err(_) => {
            // `from_stored_or_empty` reads a kit this rejects as empty.
            notes.note("kit_manifest ignored");
            KitManifest::default()
        }
    };
    let mut output = Vec::new();
    manifest.write_json(&mut output);
    let written =
        String::from_utf8(output).map_err(|error| ImportError::Database(error.to_string()))?;
    if written != raw {
        notes.note("kit_manifest stored as KitManifest");
    }
    Ok(written)
}

fn snapshots(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let mut statement = connection
        .prepare("SELECT id, payload_json FROM plan_snapshots WHERE tenant_id = 'default'")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let rows: Vec<(i64, String)> = rows.collect::<rusqlite::Result<_>>()?;
    drop(statement);
    for (id, raw) in rows {
        let Some(written) = snapshot_text(&raw, notes)? else {
            continue;
        };
        if written != raw {
            connection.execute(
                "UPDATE plan_snapshots SET payload_json = ?1 WHERE id = ?2",
                (written, id),
            )?;
        }
    }
    Ok(())
}

fn snapshot_text(raw: &str, notes: &mut Notes) -> Result<Option<String>, ImportError> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        notes.fault("plan_snapshots payload_json is not JSON");
        return Ok(None);
    };
    if !snapshot_fields(&value, notes) {
        return Ok(None);
    }
    let Ok(payload) = serde_json::from_value::<SnapshotPayload>(value) else {
        notes.fault("plan_snapshots payload_json is not a SnapshotPayload");
        return Ok(None);
    };
    let written = json_text(&payload)?;
    if written != raw {
        notes.note("plan_snapshots payload stored as SnapshotPayload");
    }
    Ok(Some(written))
}

fn snapshot_fields(value: &Value, notes: &mut Notes) -> bool {
    let Some(object) = value.as_object() else {
        notes.fault("plan_snapshots payload_json is not an object");
        return false;
    };
    if let Some(layers) = object.get("layers") {
        let Some(layers) = layers.as_array() else {
            notes.fault("plan_snapshots layers is not an array");
            return false;
        };
        if layers.iter().any(|layer| !layer.is_object()) {
            notes.fault("plan_snapshots layer is not an object");
            return false;
        }
    }
    for key in ["recipe_summary", "captured_at"] {
        if let Some(text) = object.get(key)
            && !text.is_string()
            && !text.is_null()
        {
            notes.fault("plan_snapshots text field is not text");
            return false;
        }
    }
    for key in ["accepted_plan_revision_id", "accepted_plan_version"] {
        if let Some(id) = object.get(key)
            && !revision_id(id)
        {
            notes.fault("plan_snapshots revision id is not an integer");
            return false;
        }
    }
    true
}

fn revision_id(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Number(number) => number.as_i64().is_some(),
        _ => false,
    }
}

fn api_keys(
    connection: &Connection,
    stored: &[(String, String)],
    notes: &mut Notes,
) -> Result<(), ImportError> {
    let Some((_, raw)) = stored.iter().find(|(key, _)| key == KEYS) else {
        return Ok(());
    };
    if raw.is_empty() {
        return Ok(());
    }
    match crate::auth::canonical_collection(raw) {
        Ok((written, rehashed)) => {
            for _ in 0..rehashed {
                notes.note("api_keys_v1 re-hashed");
            }
            if written != *raw {
                notes.note("api_keys_v1 stored as StoredKey");
            }
            replace(connection, KEYS, raw, &written)?;
        }
        Err(_) => notes.fault("api_keys_v1 is not a StoredKey collection"),
    }
    Ok(())
}

/// Readers that already accept the stored bytes. A failure refuses the
/// import. Nothing here writes.
fn checked(
    connection: &Connection,
    documents: &Path,
    stored: &[(String, String)],
    notes: &mut Notes,
) -> Result<(), ImportError> {
    let naming = stored
        .iter()
        .find(|(key, _)| key == NAMING)
        .map(|(_, raw)| raw.clone());
    let _profile = crate::catalog::naming::parse_global(naming);
    for (key, raw) in stored {
        if let Some(suffix) = key.strip_prefix(BRIEF) {
            match build_id(suffix) {
                Some(id) => {
                    if crate::read_model::planning::BuildPlanningBrief::parse(raw, id).is_err() {
                        notes.fault("build_planning.v1 did not pass the typed reader");
                    }
                }
                None => notes.fault("build_planning.v1 key is not a Build id"),
            }
        }
        if key.starts_with(CREATE) {
            let ok = raw
                .parse::<u64>()
                .ok()
                .and_then(|id| pp_domain::PositiveId::new(id).ok())
                .is_some();
            if !ok {
                notes.fault("build_planning.create is not a Build id");
            }
        }
    }
    decisions(connection, notes)?;
    reconciliations(connection, notes)?;
    snapshot_files(documents, notes)?;
    Ok(())
}

fn decisions(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let mut statement = connection.prepare(
        "SELECT params_json, result_json FROM plan_decisions WHERE tenant_id = 'default'",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (params, result) = row?;
        for raw in std::iter::once(params).chain(result) {
            if serde_json::from_str::<Value>(&raw).is_err() {
                notes.fault("plan_decisions JSON is not JSON");
            }
        }
    }
    Ok(())
}

fn reconciliations(connection: &Connection, notes: &mut Notes) -> Result<(), ImportError> {
    let mut statement = connection.prepare(
        "SELECT selection_basis_json, result_json FROM plan_draft_required_unit_reconciliations WHERE tenant_id = 'default'",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let basis: String = row.get(0)?;
        let result: String = row.get(1)?;
        if crate::required_units::storage::accept_reconciliation(&basis, &result).is_err() {
            notes.fault("required-unit reconciliation did not pass the typed reader");
        }
    }
    Ok(())
}

fn snapshot_files(documents: &Path, notes: &mut Notes) -> Result<(), ImportError> {
    let mut pending = vec![documents.join("repos")];
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(&directory, error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| io_error(&directory, error))?;
            let path = entry.path();
            let metadata = path
                .symlink_metadata()
                .map_err(|error| io_error(&path, error))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file() || entry.file_name() != SNAPSHOT {
                continue;
            }
            if metadata.len() > MAX_MANIFEST {
                notes.fault("source snapshot manifest is larger than the reader allows");
                continue;
            }
            let bytes = fs::read(&path).map_err(|error| io_error(&path, error))?;
            let Ok(text) = String::from_utf8(bytes) else {
                notes.fault("source snapshot manifest is not JSON");
                continue;
            };
            manifest(&text, notes);
        }
    }
    Ok(())
}

/// `{}` is the stub a synthetic installation writes. Any other object has to
/// be a version 1 manifest with the fields Node's parser requires before it
/// reads each file. The file is not rewritten.
fn manifest(text: &str, notes: &mut Notes) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        notes.fault("source snapshot manifest is not JSON");
        return;
    };
    let Some(object) = value.as_object() else {
        notes.fault("source snapshot manifest is not an object");
        return;
    };
    if object.is_empty() {
        return;
    }
    if object.get("version").and_then(Value::as_i64) != Some(1) {
        notes.fault("source snapshot manifest version is not 1");
        return;
    }
    let digest = object
        .get("manifestDigest")
        .and_then(Value::as_str)
        .is_some_and(is_sha256);
    let revision = object
        .get("upstreamRevisionKey")
        .is_some_and(Value::is_string);
    let files = object.get("files").is_some_and(Value::is_array);
    let omitted = object
        .get("selection")
        .and_then(Value::as_object)
        .is_some_and(|selection| selection.get("omittedFiles").is_some_and(Value::is_array));
    if !(digest && revision && files && omitted) {
        notes.fault("source snapshot manifest is not the typed shape");
    }
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
