//! Kit bundle format (`print-partner-kit`, `.print-partner-kit.zip`).
//!
//! Ported from `web/apps/server/src/services/export-kit.ts` (bundle data,
//! `kit.json` text and the upload parser) and `kit-bundle-share.ts` (source
//! references, in [`source_refs`]). `fixtures/parity/kit-bundle` holds the
//! Node golden that `tests/kit_bundle_golden.rs` replays.
//!
//! Not here yet: the accepted-progress bundle mode, which reads the accepted
//! operational export (GRE-422), and publishing the zip under the exports
//! directory.

mod js;
pub mod source_refs;

use std::fmt;
use std::io::{Cursor, Read, Write};

use serde_json::{Map, Value, json};

pub use js::stringify;

pub const KIT_FORMAT: &str = "print-partner-kit";
pub const KIT_VERSION: u32 = 3;
pub const KIT_JSON_NAME: &str = "kit.json";
pub const MAX_KIT_JSON_BYTES: u64 = 16 * 1024 * 1024;
pub const KIT_JSON_TOO_LARGE_DETAIL: &str =
    "Kit bundle data exceeds the 16 MiB expanded JSON limit";

/// Why an uploaded kit bundle was refused. `Display` is Node's error message
/// (the `detail` the import route returns), except for `Json` and `Zip`,
/// whose text came from V8 and adm-zip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KitBundleError {
    /// 413 in the import route; every other refusal is 400.
    TooLarge,
    NotKit,
    UnsupportedVersion,
    MissingKitJson,
    /// `kit.json` was the JSON literal `null`.
    NullDocument,
    Json(String),
    Zip(String),
}

impl fmt::Display for KitBundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str(KIT_JSON_TOO_LARGE_DETAIL),
            Self::NotKit => f.write_str("Not a Print Partner kit file"),
            Self::UnsupportedVersion => f.write_str("Unsupported kit version"),
            Self::MissingKitJson => write!(f, "Missing {KIT_JSON_NAME} in kit archive"),
            Self::NullDocument => f.write_str("Cannot read properties of null (reading 'format')"),
            Self::Json(message) | Self::Zip(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for KitBundleError {}

/// `parseKitBundleBuffer`: read `kit.json` or a kit zip uploaded from the
/// browser. A name ending in `.zip` or containing `.print-partner-kit`, or
/// bytes starting with `PK`, are read as a zip holding `kit.json`.
pub fn parse_kit_bundle(
    bytes: &[u8],
    filename: Option<&str>,
) -> Result<Map<String, Value>, KitBundleError> {
    let name = filename.unwrap_or("").to_lowercase();
    let looks_zip =
        name.ends_with(".zip") || name.contains(".print-partner-kit") || bytes.starts_with(b"PK");
    if !looks_zip {
        return parse_kit_json(bytes);
    }
    let zip_error = |error: zip::result::ZipError| KitBundleError::Zip(error.to_string());
    let io_error = |error: std::io::Error| KitBundleError::Zip(error.to_string());
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(zip_error)?;
    let index = archive
        .index_for_name(KIT_JSON_NAME)
        .ok_or(KitBundleError::MissingKitJson)?;
    let (declared, method, crc) = {
        let entry = archive.by_index(index).map_err(zip_error)?;
        (entry.size(), entry.compression(), entry.crc32())
    };
    // The declared size is checked before inflating, and the bytes read are
    // checked again.
    if declared > MAX_KIT_JSON_BYTES {
        return Err(KitBundleError::TooLarge);
    }
    let mut json = Vec::new();
    if method == zip::CompressionMethod::Stored {
        // adm-zip returns all of a stored entry's bytes, whatever size its
        // headers claim, once their CRC checks out. They are no longer than
        // the upload.
        archive
            .by_index_raw(index)
            .map_err(zip_error)?
            .read_to_end(&mut json)
            .map_err(io_error)?;
        if crc32fast::hash(&json) != crc {
            return Err(KitBundleError::Zip("CRC32 checksum failed".to_owned()));
        }
    } else {
        // Inflating past the declared size is an error, as in adm-zip.
        archive
            .by_index(index)
            .map_err(zip_error)?
            .take(MAX_KIT_JSON_BYTES + 1)
            .read_to_end(&mut json)
            .map_err(io_error)?;
    }
    if json.len() as u64 > MAX_KIT_JSON_BYTES {
        return Err(KitBundleError::TooLarge);
    }
    parse_kit_json(&json)
}

fn parse_kit_json(bytes: &[u8]) -> Result<Map<String, Value>, KitBundleError> {
    if bytes.len() as u64 > MAX_KIT_JSON_BYTES {
        return Err(KitBundleError::TooLarge);
    }
    // Buffer#toString("utf8") replaces invalid sequences with U+FFFD.
    let text = String::from_utf8_lossy(bytes);
    let data: Value = match serde_json::from_str(&text) {
        Ok(data) => data,
        // JSON.parse reads a literal past f64 range as ±Infinity, which
        // JSON.stringify writes as null; serde_json refuses it.
        Err(error) => js::overflowing_numbers_to_null(&text)
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or_else(|| KitBundleError::Json(error.to_string()))?,
    };
    let data = match data {
        Value::Null => return Err(KitBundleError::NullDocument),
        Value::Object(map) if map.get("format").and_then(Value::as_str) == Some(KIT_FORMAT) => map,
        _ => return Err(KitBundleError::NotKit),
    };
    let version = js::to_number(data.get("version").unwrap_or(&Value::Null));
    if ![1.0, 2.0, 3.0].contains(&version) {
        return Err(KitBundleError::UnsupportedVersion);
    }
    Ok(data)
}

/// One working part in an editable kit bundle (`KitBundlePartWithoutProgress`).
#[derive(Debug, Clone, PartialEq)]
pub struct KitBundlePart {
    pub match_key: String,
    pub relative_path: String,
    pub filename: String,
    pub source_layer: String,
    pub status: String,
    pub role: String,
    pub filament_color_id: Option<String>,
    pub filament_custom_hex: Option<String>,
    /// Quantities are integers: Node reads them from integer columns.
    pub quantity_inferred: i64,
    pub quantity_override: Option<i64>,
    pub quantity_effective: i64,
    pub included: bool,
    pub notes: String,
    pub geometry_same: Option<bool>,
    pub requirement: Option<String>,
    pub option_group_id: Option<String>,
    pub manifest_source: Option<String>,
}

impl KitBundlePart {
    fn to_json(&self) -> Value {
        json!({
            "match_key": self.match_key,
            "relative_path": self.relative_path,
            "filename": self.filename,
            "source_layer": self.source_layer,
            "status": self.status,
            "role": self.role,
            "filament_color_id": self.filament_color_id,
            "filament_custom_hex": self.filament_custom_hex,
            "quantity_auto": self.quantity_inferred,
            "quantity_override": self.quantity_override,
            "quantity_effective": self.quantity_effective,
            "included": self.included,
            "notes": self.notes,
            "geometry_same": self.geometry_same,
            "requirement": self.requirement,
            "option_group_id": self.option_group_id,
            "manifest_source": self.manifest_source,
        })
    }
}

/// The editable recipe a kit bundle shares (`EditableKitRecipe`). `layers`,
/// `sources` and `kit_manifest` are already in their bundle shape.
#[derive(Debug, Clone, PartialEq)]
pub struct EditableKitRecipe {
    pub profile_name: String,
    pub order_number: Option<String>,
    pub layers: Vec<Value>,
    pub sources: Vec<Value>,
    pub kit_manifest: Value,
    pub working_parts: Vec<KitBundlePart>,
}

/// `buildKitBundleData` in editable mode.
pub fn build_editable_kit_bundle(recipe: &EditableKitRecipe, exported_at: &str) -> Value {
    json!({
        "format": KIT_FORMAT,
        "version": KIT_VERSION,
        "exported_at": exported_at,
        "profile": { "name": recipe.profile_name, "order_number": recipe.order_number },
        "layers": recipe.layers,
        "parts": recipe.working_parts.iter().map(KitBundlePart::to_json).collect::<Vec<_>>(),
        "kit_manifest": recipe.kit_manifest,
        "sources": recipe.sources,
    })
}

/// `kit.json` exactly as Node writes it: `JSON.stringify(data, null, 2)`.
pub fn kit_json_text(data: &Value) -> String {
    stringify(data, true)
}

/// A kit zip holding only `kit.json`, deflated. Node writes the same entry
/// with fflate, so the archive bytes differ but `kit.json` does not.
pub fn kit_bundle_zip(data: &Value) -> zip::result::ZipResult<Vec<u8>> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    writer.start_file(KIT_JSON_NAME, options)?;
    writer.write_all(kit_json_text(data).as_bytes())?;
    Ok(writer.finish()?.into_inner())
}
