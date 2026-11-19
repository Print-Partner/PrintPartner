//! Imported slicer profiles: the printer, process and filament settings a
//! slicer applies, as defined by the **Slicer profile** entry in the glossary
//! (`GLOSSARY.md`). A slicer profile is not a Plan; "profile" in this module
//! never means a Build's requirements.
use serde::Serialize;
use serde_json::value::RawValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Printer,
    Process,
    Filament,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SlicerKind {
    Orca,
    Prusa,
    Bambu,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsText {
    utf16: Vec<u16>,
}

impl JsText {
    pub fn from_utf16(utf16: Vec<u16>) -> Self {
        Self { utf16 }
    }

    pub fn from_string(value: impl AsRef<str>) -> Self {
        Self {
            utf16: value.as_ref().encode_utf16().collect(),
        }
    }

    pub fn as_utf16(&self) -> &[u16] {
        &self.utf16
    }

    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.utf16)
    }

    pub fn to_node_json_string(&self) -> String {
        let mut output = String::from("\"");
        for decoded in char::decode_utf16(self.utf16.iter().copied()) {
            match decoded {
                Ok('\u{0008}') => output.push_str("\\b"),
                Ok('\u{0009}') => output.push_str("\\t"),
                Ok('\u{000a}') => output.push_str("\\n"),
                Ok('\u{000c}') => output.push_str("\\f"),
                Ok('\u{000d}') => output.push_str("\\r"),
                Ok('"') => output.push_str("\\\""),
                Ok('\\') => output.push_str("\\\\"),
                Ok(control @ '\u{0000}'..='\u{001f}') => {
                    output.push_str(&format!("\\u{:04x}", u32::from(control)));
                }
                Ok(scalar) => output.push(scalar),
                Err(error) => {
                    output.push_str(&format!("\\u{:04x}", error.unpaired_surrogate()));
                }
            }
        }
        output.push('"');
        output
    }

    pub fn is_empty(&self) -> bool {
        self.utf16.is_empty()
    }
}

impl Serialize for JsText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        RawValue::from_string(self.to_node_json_string())
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JsInteger(f64);

impl JsInteger {
    pub fn new(value: f64) -> Result<Self, ImportShapeError> {
        if !value.is_finite() || value.fract() != 0.0 {
            return Err(ImportShapeError::NonIntegralNumber);
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

impl Serialize for JsInteger {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportProvenance {
    pub name: JsText,
    pub slicer_version: Option<JsText>,
    pub resolved_flat_config: JsText,
    pub source_path: JsText,
}

#[derive(Clone, Debug, Serialize)]
pub struct PrinterImportInput {
    pub provenance: ImportProvenance,
    pub slicer_format: SlicerKind,
    pub nozzle_diameter_mm: Option<JsText>,
    pub extruder_count: Option<JsInteger>,
    pub raw_json: Option<JsText>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessImportInput {
    pub provenance: ImportProvenance,
    pub slicer_format: SlicerKind,
    pub compatible_printers: Option<JsText>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FilamentImportInput {
    pub provenance: ImportProvenance,
    pub material_type: JsText,
    pub nozzle_temp_c: Option<JsInteger>,
    pub bed_temp_c: Option<JsInteger>,
    pub fan_pct: Option<JsInteger>,
    pub extrusion_multiplier: Option<JsText>,
    pub pressure_advance: Option<JsText>,
    pub retraction: Option<JsText>,
    pub raw: RawFilamentSource,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "format", content = "value", rename_all = "lowercase")]
pub enum RawFilamentSource {
    Json(JsText),
    Ini(JsText),
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ProfileImportInner {
    Printer(PrinterImportInput),
    Process(ProcessImportInput),
    Filament(FilamentImportInput),
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileImport(ProfileImportInner);

impl ProfileImport {
    pub fn printer(input: PrinterImportInput) -> Result<Self, ImportShapeError> {
        validate_provenance(&input.provenance)?;
        Ok(Self(ProfileImportInner::Printer(input)))
    }

    pub fn process(input: ProcessImportInput) -> Result<Self, ImportShapeError> {
        validate_provenance(&input.provenance)?;
        Ok(Self(ProfileImportInner::Process(input)))
    }

    pub fn filament(input: FilamentImportInput) -> Result<Self, ImportShapeError> {
        validate_provenance(&input.provenance)?;
        Ok(Self(ProfileImportInner::Filament(input)))
    }

    pub fn kind(&self) -> ProfileKind {
        match self.0 {
            ProfileImportInner::Printer(_) => ProfileKind::Printer,
            ProfileImportInner::Process(_) => ProfileKind::Process,
            ProfileImportInner::Filament(_) => ProfileKind::Filament,
        }
    }
}

fn validate_provenance(provenance: &ImportProvenance) -> Result<(), ImportShapeError> {
    if provenance.name.is_empty() {
        return Err(ImportShapeError::EmptyName);
    }
    if provenance.source_path.is_empty() {
        return Err(ImportShapeError::EmptySourcePath);
    }
    let resolved = String::from_utf16(provenance.resolved_flat_config.as_utf16())
        .map_err(|_| ImportShapeError::InvalidResolvedJson)?;
    RawValue::from_string(resolved).map_err(|_| ImportShapeError::InvalidResolvedJson)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportShapeError {
    EmptyName,
    EmptySourcePath,
    InvalidResolvedJson,
    NonIntegralNumber,
}
