use super::JobKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompletedResult {
    SourceSync(SourceSyncResult),
    SourceScan(SourceScanResult),
    SourceDocuments(SourceDocumentsResult),
    SourceUpdates(SourceUpdatesResult),
    StlPack(StlPackResult),
    ChecklistHtml(ChecklistHtmlResult),
    KitBundle(KitBundleResult),
    AcceptedPlates(AcceptedPlatesResult),
    Direct3mf(Direct3mfResult),
    PrinterUpload(PrinterUploadResult),
}

impl CompletedResult {
    pub fn kind(&self) -> JobKind {
        match self {
            Self::SourceSync(_) => JobKind::Sync,
            Self::SourceScan(_) => JobKind::ImportScan,
            Self::SourceDocuments(_) => JobKind::ExtractSourceDocs,
            Self::SourceUpdates(_) => JobKind::CheckSourceUpdates,
            Self::StlPack(_) => JobKind::ExportStlPack,
            Self::ChecklistHtml(_) => JobKind::ExportChecklistHtml,
            Self::KitBundle(_) => JobKind::ExportKitBundle,
            Self::AcceptedPlates(_) => JobKind::ExportAcceptedPlate3mf,
            Self::Direct3mf(_) => JobKind::ExportDirect3mf,
            Self::PrinterUpload(_) => JobKind::PrinterUpload,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceSyncResult {
    pub synced: u64,
    pub failed: u64,
    pub results: Vec<SourceScanResult>,
    pub failures: Vec<SourceSyncFailure>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceSyncFailure {
    pub project_id: Option<u64>,
    pub name: Option<String>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceScanResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<u64>,
    pub stl_count: u64,
    pub downloaded: u64,
    pub doc_count: u64,
    pub docs_downloaded: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_extract_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postprocess_warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceDocumentsResult {
    pub project_id: u64,
    pub extracted: u64,
    pub errors: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceUpdatesResult {
    pub checked: Vec<CheckedSourceUpdate>,
    pub skipped: Vec<SkippedSourceUpdate>,
    pub updates_available: u64,
    pub checked_count: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckedSourceUpdate {
    pub source_id: u64,
    pub name: String,
    pub update_status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkippedSourceUpdate {
    pub source_id: u64,
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StlPackResult {
    pub root_path: String,
    pub download_url: Option<String>,
    pub file_counts: BTreeMap<String, u64>,
    pub zip_counts: BTreeMap<String, u64>,
    pub warnings: Vec<String>,
    pub missing_only: bool,
    pub file_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChecklistHtmlResult {
    pub path: String,
    pub download_url: Option<String>,
    pub part_count: u64,
    pub thumb_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KitBundleResult {
    pub path: String,
    pub download_url: Option<String>,
    pub profile_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportBasis {
    pub profile_id: u64,
    pub plan_version: u64,
    pub plan_revision_id: u64,
    pub plan_revision_digest: String,
    pub required_unit_mapping_digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptedPlatesResult {
    pub format: String,
    pub profile_id: u64,
    pub basis: ExportBasis,
    pub plate_revision_id: u64,
    pub plate_revision_number: u64,
    pub layout_digest: String,
    pub download_url: String,
    pub manifest_download_url: String,
    pub bundle_download_url: String,
    pub plates: Vec<AcceptedPlateResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptedPlateResult {
    pub plate_id: String,
    pub ordinal: u64,
    pub filename: String,
    pub download_url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Direct3mfResult {
    pub format: String,
    pub profile_id: u64,
    pub basis: ExportBasis,
    pub download_url: String,
    pub filename: String,
    pub tokens: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrinterUploadResult {
    pub printer_id: String,
    pub printer_name: String,
    pub integration_id: String,
    pub integration_type: String,
    pub host_name: String,
    pub filename: String,
    pub remote_path: String,
    pub started: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkoff_link_id: Option<String>,
    pub checkoff_units: u64,
    /// The slicer's gram estimate read from the sent file, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filament_estimate_g: Option<f64>,
}
