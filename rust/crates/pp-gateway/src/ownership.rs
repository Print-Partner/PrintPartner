//! Source Sync operation ownership.
//!
//! One admitted record names every participant that can enqueue or perform
//! `source.sync`. Node remains the only supported owner. A Rust assignment is
//! rejected instead of being routed back to Node. Worker and startup
//! participants for the same operation must agree, and a shared table or file
//! cannot have two owners.
//!
//! Desktop startup persists one receipt in the lease data directory before it
//! binds or starts the compatibility process. `Gateway::start` checks the same
//! bundled manifest again in memory. The Node process admits that file before
//! HTTP handlers, MCP `start_sync`, job execution, and watcher timers.

pub mod cutover;
pub mod live_formats;
pub mod manifest;
pub mod resources;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const SOURCE_SYNC_OPERATION: &str = "source.sync";
pub const SOURCE_SYNC_LIVE_FORMAT: &str = "source-revision-workspace-v1";
pub const SOURCE_SYNC_PARTICIPANTS: &[&str] = &[
    "http.POST /jobs/sync",
    "http.POST /api/v1/jobs/sync",
    "http.POST /jobs/import-scan",
    "http.POST /api/v1/jobs/import-scan",
    "http.POST /jobs/extract-source-docs",
    "http.POST /api/v1/jobs/extract-source-docs",
    "http.POST /jobs/check-source-updates",
    "http.POST /api/v1/jobs/check-source-updates",
    "http.POST /sources",
    "http.POST /api/v1/sources",
    "http.PUT /sources/:id/repo-manifest",
    "http.PUT /api/v1/sources/:id/repo-manifest",
    "http.POST /assistant/actions/apply",
    "http.POST /api/v1/assistant/actions/apply",
    "http.POST /sources/:id/upload-zip",
    "http.POST /api/v1/sources/:id/upload-zip",
    "http.POST /sources/:id/upload-files",
    "http.POST /api/v1/sources/:id/upload-files",
    "http.GET /sources/:id/docs",
    "http.GET /api/v1/sources/:id/docs",
    "http.GET /sources/:id/docs/*",
    "http.GET /api/v1/sources/:id/docs/*",
    "http.GET /sources/:id/cover",
    "http.GET /api/v1/sources/:id/cover",
    "assistant.propose_import_source_files",
    "mcp.start_sync",
    "job.sync",
    "job.import-scan",
    "job.extract-source-docs",
    "job.check-source-updates",
    "startup.startSourceWatcher",
    "worker.startSourceWatcher",
    "migration.migrateLegacySourceManifestOverridesForTenant",
];

const RECEIPT_FILE: &str = "source-sync-admission.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    Node,
    Rust,
}

impl Owner {
    fn parse(value: &str) -> Result<Self, AdmissionError> {
        match value {
            "node" | "compat" => Ok(Self::Node),
            "rust" => Ok(Self::Rust),
            other => Err(AdmissionError::UnknownOwner {
                value: other.to_string(),
            }),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Rust => "rust",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Malformed(String),
    UnknownOwner {
        value: String,
    },
    UnknownOperation {
        value: String,
    },
    UnknownParticipantKind {
        value: String,
    },
    ParticipantKindMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    ResourceSetMismatch {
        id: String,
    },
    UnresolvedResources {
        operation: String,
    },
    UnknownLiveFormat {
        value: String,
    },
    UnsupportedRustOwner {
        participant: String,
    },
    ConflictingParticipant {
        operation: String,
        left: String,
        right: String,
    },
    ConflictingResource {
        resource: String,
    },
    IncompatibleRollback {
        operation: String,
    },
    IncompatibleLiveFormat {
        operation: String,
        format: String,
        node_reads: u32,
        rust_writes: Option<u32>,
    },
    DuplicateParticipant {
        id: String,
    },
    UnknownParticipant {
        id: String,
    },
    ParticipantSetMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    UnrepresentedParticipant {
        kind: String,
    },
    RestartManifestChanged,
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedResources { operation } => {
                write!(formatter, "unresolved resources for {operation}")
            }
            Self::Malformed(detail) => write!(formatter, "malformed ownership manifest: {detail}"),
            Self::UnknownOwner { value } => {
                write!(formatter, "unknown owner {value}")
            }
            Self::UnknownOperation { value } => {
                write!(formatter, "unknown operation {value}")
            }
            Self::UnknownParticipantKind { value } => {
                write!(formatter, "unknown participant kind {value}")
            }
            Self::ParticipantKindMismatch {
                id,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "participant kind mismatch for {id}: expected {expected}, got {actual}"
                )
            }
            Self::UnknownLiveFormat { value } => {
                write!(formatter, "unknown live format rollback {value}")
            }
            Self::UnsupportedRustOwner { participant } => {
                write!(
                    formatter,
                    "unsupported rust owner for {participant}; Node remains the Source Sync owner"
                )
            }
            Self::ConflictingParticipant {
                operation,
                left,
                right,
            } => write!(
                formatter,
                "conflicting ownership for {operation}: {left} vs {right}"
            ),
            Self::ResourceSetMismatch { id } => {
                write!(formatter, "participant resource set mismatch for {id}")
            }
            Self::ConflictingResource { resource } => {
                write!(formatter, "conflicting resource authority for {resource}")
            }
            Self::IncompatibleRollback { operation } => {
                write!(
                    formatter,
                    "incompatible live-format rollback for {operation}"
                )
            }
            Self::IncompatibleLiveFormat {
                operation,
                format,
                node_reads,
                rust_writes,
            } => match rust_writes {
                Some(written) => write!(
                    formatter,
                    "rollback of {operation} refused: Rust writes {format} {written}, Node reads up to {node_reads}"
                ),
                None => write!(
                    formatter,
                    "rollback of {operation} refused: Rust writer for {format} is undeclared"
                ),
            },
            Self::DuplicateParticipant { id } => {
                write!(formatter, "duplicate participant {id}")
            }
            Self::UnknownParticipant { id } => {
                write!(formatter, "unknown participant {id} has no owner")
            }
            Self::ParticipantSetMismatch {
                missing,
                unexpected,
            } => write!(
                formatter,
                "participant set mismatch missing={missing:?} unexpected={unexpected:?}"
            ),
            Self::UnrepresentedParticipant { kind } => {
                write!(formatter, "unrepresented participant {kind}")
            }
            Self::RestartManifestChanged => {
                write!(
                    formatter,
                    "restart manifest does not match the admitted record"
                )
            }
        }
    }
}

impl std::error::Error for AdmissionError {}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdmissionReceipt {
    pub operation: String,
    pub owner: String,
    pub worker_pid: u32,
    pub admission_count: u32,
    pub participant_count: u32,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedOperation {
    pub operation: String,
    pub owner: Owner,
    pub participant_ids: BTreeSet<String>,
    pub manifest_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    operation: String,
    owner: String,
    live_format: LiveFormat,
    participants: Vec<Participant>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveFormat {
    id: String,
    rollback: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Participant {
    id: String,
    kind: String,
    owner: String,
    resources: Vec<String>,
}

#[derive(Clone, Copy)]
struct ParsedParticipant<'a> {
    id: &'a str,
    kind: &'a str,
    owner: Owner,
    resources: &'a [String],
}

const BUNDLED_SOURCE_SYNC_MANIFEST: &str =
    include_str!("../../../../web/apps/server/src/data/source-sync-ownership.json");

pub fn admit_bundled_source_sync() -> Result<AdmittedOperation, AdmissionError> {
    admit_observed(BUNDLED_SOURCE_SYNC_MANIFEST, SOURCE_SYNC_PARTICIPANTS)
}

/// Admit the bundled manifest and persist one receipt in `data_dir`.
///
/// `data_dir` is the desktop storage lease directory, the same directory the
/// Node child receives. A later start reuses the receipt. This does not create
/// a second data owner.
pub fn boot_bundled_source_sync(data_dir: &Path) -> Result<AdmissionReceipt, AdmissionError> {
    boot_source_sync(data_dir, BUNDLED_SOURCE_SYNC_MANIFEST)
}

pub fn admit_manifest(manifest: &str) -> Result<AdmittedOperation, AdmissionError> {
    admit_observed(manifest, &[])
}

pub fn admit_observed(
    manifest: &str,
    observed: &[&str],
) -> Result<AdmittedOperation, AdmissionError> {
    let parsed: Manifest = serde_json::from_str(manifest)
        .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    if parsed.operation != SOURCE_SYNC_OPERATION {
        return Err(AdmissionError::UnknownOperation {
            value: parsed.operation,
        });
    }
    if parsed.live_format.id != SOURCE_SYNC_LIVE_FORMAT {
        return Err(AdmissionError::UnknownLiveFormat {
            value: parsed.live_format.id,
        });
    }
    match parsed.live_format.rollback.as_str() {
        "compatible" => {}
        "incompatible" => {
            return Err(AdmissionError::IncompatibleRollback {
                operation: parsed.operation,
            });
        }
        other => {
            return Err(AdmissionError::UnknownLiveFormat {
                value: other.to_string(),
            });
        }
    }
    let operation_owner = Owner::parse(&parsed.owner)?;
    let mut seen = BTreeSet::new();
    let mut parsed_participants = Vec::new();
    for participant in &parsed.participants {
        if !seen.insert(participant.id.clone()) {
            return Err(AdmissionError::DuplicateParticipant {
                id: participant.id.clone(),
            });
        }
        validate_kind(&participant.kind)?;
        if SOURCE_SYNC_PARTICIPANTS.contains(&participant.id.as_str()) {
            let expected = match participant.id.split('.').next() {
                Some("http")
                    if participant
                        .id
                        .split_once(' ')
                        .is_some_and(|(_, path)| path.starts_with("/api/v1/")) =>
                {
                    "http_alias"
                }
                Some("http") => "http",
                Some("job") => "job",
                Some("mcp") => "mcp_action",
                Some("assistant") => "assistant_action",
                Some("startup") => "startup_watcher",
                Some("worker") => "worker",
                Some("migration") => "migration",
                _ => {
                    return Err(AdmissionError::Malformed(format!(
                        "unclassified participant {}",
                        participant.id
                    )));
                }
            };
            if participant.kind != expected {
                return Err(AdmissionError::ParticipantKindMismatch {
                    id: participant.id.clone(),
                    expected: expected.to_string(),
                    actual: participant.kind.clone(),
                });
            }
        }
        validate_resources(&participant.resources)?;
        if SOURCE_SYNC_PARTICIPANTS.contains(&participant.id.as_str()) {
            let expected: BTreeSet<&str> = expected_resources(&participant.id)
                .iter()
                .copied()
                .collect();
            let actual: BTreeSet<&str> = participant.resources.iter().map(String::as_str).collect();
            if actual != expected {
                return Err(AdmissionError::ResourceSetMismatch {
                    id: participant.id.clone(),
                });
            }
        }
        parsed_participants.push(ParsedParticipant {
            id: &participant.id,
            kind: &participant.kind,
            owner: Owner::parse(&participant.owner)?,
            resources: &participant.resources,
        });
    }
    enforce_agreement(operation_owner, &parsed_participants)?;
    if !observed.is_empty() {
        let declared: BTreeSet<&str> = parsed_participants.iter().map(|item| item.id).collect();
        let expected: BTreeSet<&str> = observed.iter().copied().collect();
        let missing = expected
            .difference(&declared)
            .map(|id| (*id).to_string())
            .collect::<Vec<_>>();
        let unexpected = declared
            .difference(&expected)
            .map(|id| (*id).to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() || !unexpected.is_empty() {
            return Err(AdmissionError::ParticipantSetMismatch {
                missing,
                unexpected,
            });
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(manifest.as_bytes());
    Ok(AdmittedOperation {
        operation: parsed.operation,
        owner: operation_owner,
        participant_ids: seen,
        manifest_sha256: hex::encode(hasher.finalize()),
    })
}

fn validate_kind(kind: &str) -> Result<(), AdmissionError> {
    match kind {
        "http" | "http_alias" | "mcp_action" | "job" | "startup_watcher" | "worker"
        | "migration" | "assistant_action" => Ok(()),
        other => Err(AdmissionError::UnknownParticipantKind {
            value: other.to_string(),
        }),
    }
}

fn expected_resources(id: &str) -> &'static [&'static str] {
    if id.ends_with("/upload-zip") || id.ends_with("/upload-files") {
        return &[
            "table:projects",
            "table:source_revisions",
            "table:source_docs",
            "file:source-workspace",
            "file:source-covers",
            "table:build_profiles",
        ];
    }
    if id.ends_with("/cover") {
        return &["file:source-covers"];
    }
    if id.ends_with("/docs") {
        return &["table:source_docs"];
    }
    if id.ends_with("/docs/*") {
        return &["table:source_docs", "file:source-workspace"];
    }
    if id == "assistant.propose_import_source_files" {
        return &[
            "table:projects",
            "table:source_revisions",
            "table:source_docs",
            "file:source-workspace",
            "table:build_profiles",
        ];
    }
    if id == "worker.startSourceWatcher" {
        return &[
            "table:projects",
            "table:source_revisions",
            "table:source_docs",
            "file:source-workspace",
            "file:source-covers",
            "table:app_settings",
            "table:app_events",
        ];
    }
    if id.starts_with("migration.") || id.ends_with("/repo-manifest") {
        return &[
            "table:projects",
            "table:source_revisions",
            "file:source-workspace",
        ];
    }
    if id.ends_with("check-source-updates") {
        return &["table:projects", "table:app_settings", "table:app_events"];
    }
    if id.ends_with("extract-source-docs") {
        return &["table:source_docs", "file:source-workspace"];
    }
    if matches!(id, "http.POST /sources" | "http.POST /api/v1/sources") {
        return &["table:projects", "table:source_revisions"];
    }
    &[
        "table:projects",
        "table:source_revisions",
        "table:source_docs",
        "file:source-workspace",
        "file:source-covers",
    ]
}

fn validate_resources(resources: &[String]) -> Result<(), AdmissionError> {
    if resources.is_empty() {
        return Err(AdmissionError::Malformed(
            "participant resources must name a table or file".to_string(),
        ));
    }
    for resource in resources {
        let name = resource
            .strip_prefix("table:")
            .or_else(|| resource.strip_prefix("file:"));
        if name.is_none_or(str::is_empty) {
            return Err(AdmissionError::Malformed(format!(
                "resource {resource} is not table: or file:"
            )));
        }
    }
    Ok(())
}

fn enforce_agreement(
    operation_owner: Owner,
    participants: &[ParsedParticipant<'_>],
) -> Result<(), AdmissionError> {
    let worker = participants.iter().find(|item| item.kind == "worker");
    let startup = participants
        .iter()
        .find(|item| item.kind == "startup_watcher");
    if let (Some(worker), Some(startup)) = (worker, startup)
        && worker.owner != startup.owner
    {
        return Err(AdmissionError::ConflictingParticipant {
            operation: SOURCE_SYNC_OPERATION.to_string(),
            left: format!("{}:{}", worker.id, worker.owner.as_str()),
            right: format!("{}:{}", startup.id, startup.owner.as_str()),
        });
    }
    let mut resources: BTreeMap<&str, Owner> = BTreeMap::new();
    for participant in participants {
        for resource in participant.resources {
            if let Some(existing) = resources.insert(resource.as_str(), participant.owner)
                && existing != participant.owner
            {
                return Err(AdmissionError::ConflictingResource {
                    resource: resource.clone(),
                });
            }
        }
        if participant.owner != operation_owner
            && (participant.owner == Owner::Rust || operation_owner == Owner::Rust)
        {
            return Err(AdmissionError::ConflictingParticipant {
                operation: SOURCE_SYNC_OPERATION.to_string(),
                left: format!("operation:{}", operation_owner.as_str()),
                right: format!("{}:{}", participant.id, participant.owner.as_str()),
            });
        }
    }
    if operation_owner == Owner::Rust || participants.iter().any(|item| item.owner == Owner::Rust) {
        let participant = participants
            .iter()
            .find(|item| item.owner == Owner::Rust)
            .map(|item| item.id.to_string())
            .unwrap_or_else(|| SOURCE_SYNC_OPERATION.to_string());
        return Err(AdmissionError::UnsupportedRustOwner { participant });
    }
    Ok(())
}

/// Inventory sections the route checker used to ignore. A conflict, an unknown
/// owner, or a missing worker/startup/MCP role fails closed.
pub fn admit_planted_sections(document: &serde_json::Value) -> Result<(), AdmissionError> {
    let present = ["workers", "startup", "mcp", "operations"]
        .iter()
        .any(|key| document.get(key).is_some());
    if !present {
        return Ok(());
    }
    let mut participants: Vec<(String, String, Owner)> = Vec::new();
    for (kind, key) in [
        ("worker", "workers"),
        ("startup_watcher", "startup"),
        ("mcp_action", "mcp"),
    ] {
        let Some(entries) = document.get(key) else {
            continue;
        };
        let entries = entries
            .as_array()
            .ok_or_else(|| AdmissionError::Malformed(format!("{key} must be an array")))?;
        for entry in entries {
            let operation = entry
                .get("operation")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let raw_owner = entry
                .get("owner")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            participants.push((
                operation.to_string(),
                kind.to_string(),
                Owner::parse(raw_owner)?,
            ));
        }
    }
    if let Some(operations) = document.get("operations") {
        let operations = operations
            .as_object()
            .ok_or_else(|| AdmissionError::Malformed("operations must be an object".to_string()))?;
        for (operation, body) in operations {
            let entries = body
                .get("participants")
                .and_then(|value| value.as_array())
                .ok_or_else(|| {
                    AdmissionError::Malformed("operations participants missing".to_string())
                })?;
            for entry in entries {
                let kind = entry
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                validate_kind(kind)?;
                let raw_owner = entry
                    .get("owner")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                participants.push((
                    operation.clone(),
                    kind.to_string(),
                    Owner::parse(raw_owner)?,
                ));
            }
        }
    }
    if participants.is_empty() {
        return Err(AdmissionError::Malformed(
            "ownership sections declare no participants".to_string(),
        ));
    }
    let mut by_operation: BTreeMap<String, Vec<(String, Owner)>> = BTreeMap::new();
    for (operation, kind, owner) in participants {
        if operation != SOURCE_SYNC_OPERATION {
            return Err(AdmissionError::UnknownOperation { value: operation });
        }
        by_operation
            .entry(operation)
            .or_default()
            .push((kind, owner));
    }
    for (operation, group) in by_operation {
        let owners: BTreeSet<Owner> = group.iter().map(|(_, owner)| *owner).collect();
        let kinds: BTreeSet<&str> = group.iter().map(|(kind, _)| kind.as_str()).collect();
        if owners.len() > 1 {
            return Err(AdmissionError::ConflictingParticipant {
                operation,
                left: format!("{:?}", group[0]),
                right: format!("{:?}", group[1]),
            });
        }
        if owners.contains(&Owner::Rust) {
            return Err(AdmissionError::UnsupportedRustOwner {
                participant: operation,
            });
        }
        for required in ["worker", "startup_watcher", "mcp_action"] {
            if !kinds.contains(required) {
                return Err(AdmissionError::UnrepresentedParticipant {
                    kind: required.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Persists one receipt in a directory the caller already owns.
///
/// Production desktop startup calls [`boot_bundled_source_sync`] with the
/// storage-lease data directory before bind and before the compatibility
/// process. Node startup calls its own writer for that same directory.
pub fn boot_source_sync(
    data_dir: &Path,
    manifest: &str,
) -> Result<AdmissionReceipt, AdmissionError> {
    let admitted = admit_observed(manifest, SOURCE_SYNC_PARTICIPANTS)?;
    write_or_reuse_receipt(data_dir, &admitted)
}

pub fn fingerprint_tree(root: &Path) -> Result<String, AdmissionError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for (relative, absolute) in files {
        hasher.update(relative.as_bytes());
        hasher.update(fs::read(&absolute).map_err(|error| {
            AdmissionError::Malformed(format!("read {}: {error}", absolute.display()))
        })?);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), AdmissionError> {
    if !current.exists() {
        return Ok(());
    }
    if current.is_file() {
        let relative = current
            .strip_prefix(root)
            .unwrap_or(current)
            .to_string_lossy()
            .replace('\\', "/");
        files.push((relative, current.to_path_buf()));
        return Ok(());
    }
    let entries = fs::read_dir(current).map_err(|error| {
        AdmissionError::Malformed(format!("read dir {}: {error}", current.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        if file_type.is_symlink() {
            return Err(AdmissionError::Malformed(format!(
                "symlink is not a source tree member: {}",
                entry.path().display()
            )));
        }
        collect_files(root, &entry.path(), files)?;
    }
    Ok(())
}

fn receipt_is_single_node_admission(
    receipt: &AdmissionReceipt,
    admitted: &AdmittedOperation,
) -> bool {
    let Ok(participant_count) = u32::try_from(admitted.participant_ids.len()) else {
        return false;
    };
    receipt.operation == admitted.operation
        && receipt.owner == admitted.owner.as_str()
        && receipt.admission_count == 1
        && receipt.participant_count == participant_count
        && receipt.worker_pid > 0
        && u64::from(receipt.worker_pid) <= i32::MAX as u64
        && receipt.manifest_sha256.len() == 64
        && receipt
            .manifest_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_receipt(
    text: &str,
    admitted: &AdmittedOperation,
) -> Result<AdmissionReceipt, AdmissionError> {
    let parsed: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| AdmissionError::Malformed(format!("admission receipt {error}")))?;
    let Some(object) = parsed.as_object() else {
        return Err(AdmissionError::Malformed(
            "admission receipt is not a single Node admission".into(),
        ));
    };
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "operation"
                | "owner"
                | "worker_pid"
                | "admission_count"
                | "participant_count"
                | "manifest_sha256"
        ) {
            return Err(AdmissionError::Malformed(
                "admission receipt is not a single Node admission".into(),
            ));
        }
    }
    let receipt: AdmissionReceipt = serde_json::from_value(parsed).map_err(|_| {
        AdmissionError::Malformed("admission receipt is not a single Node admission".into())
    })?;
    if !receipt_is_single_node_admission(&receipt, admitted) {
        return Err(AdmissionError::Malformed(
            "admission receipt is not a single Node admission".into(),
        ));
    }
    Ok(receipt)
}

const LEASE_FILE_LIMIT: u64 = 64 * 1024;

fn open_lease_file(path: &Path) -> std::io::Result<Option<fs::File>> {
    match open_read_nofollow(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Opens `path` to read without following a final link or blocking on a
/// FIFO planted at its name.
#[cfg(unix)]
fn open_read_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Opens `path` to read, opening a reparse point planted at its name as
/// itself instead of following it; the caller refuses anything but a regular
/// file.
#[cfg(windows)]
fn open_read_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn read_bounded_file(file: fs::File, limit: u64) -> std::io::Result<String> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "is not a regular file",
        ));
    }
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("exceeds {limit} bytes"),
        ));
    }
    Ok(text)
}

fn read_existing_receipt(
    receipt_path: &Path,
    admitted: &AdmittedOperation,
) -> Result<Option<AdmissionReceipt>, AdmissionError> {
    match open_lease_file(receipt_path) {
        Ok(Some(file)) => read_opened_receipt(file, admitted).map(Some),
        Ok(None) => Ok(None),
        Err(error) => Err(AdmissionError::Malformed(format!(
            "admission receipt is not a regular file or cannot be opened: {error}"
        ))),
    }
}

fn read_opened_receipt(
    file: fs::File,
    admitted: &AdmittedOperation,
) -> Result<AdmissionReceipt, AdmissionError> {
    let existing = read_bounded_file(file, LEASE_FILE_LIMIT)
        .map_err(|error| AdmissionError::Malformed(format!("admission receipt {error}")))?;
    let receipt = parse_receipt(&existing, admitted)?;
    if receipt.manifest_sha256 != admitted.manifest_sha256 {
        return Err(AdmissionError::RestartManifestChanged);
    }
    Ok(receipt)
}

fn publish_receipt(
    temporary: &Path,
    receipt_path: &Path,
    receipt: AdmissionReceipt,
    admitted: &AdmittedOperation,
) -> Result<AdmissionReceipt, AdmissionError> {
    // A link on Unix, a write-through move on Windows; neither replaces a
    // concurrent winner.
    match pp_os::publish_new(temporary, receipt_path) {
        Ok(()) => Ok(receipt),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_existing_receipt(receipt_path, admitted)?.ok_or_else(|| {
                AdmissionError::Malformed("concurrent admission receipt disappeared".into())
            })
        }
        Err(error) => Err(AdmissionError::Malformed(error.to_string())),
    }
}

fn write_or_reuse_receipt(
    data_dir: &Path,
    admitted: &AdmittedOperation,
) -> Result<AdmissionReceipt, AdmissionError> {
    let receipt_path = data_dir.join(RECEIPT_FILE);
    if let Some(receipt) = read_existing_receipt(&receipt_path, admitted)? {
        return Ok(receipt);
    }
    let participant_count = u32::try_from(admitted.participant_ids.len())
        .map_err(|_| AdmissionError::Malformed("participant count overflow".into()))?;
    let receipt = AdmissionReceipt {
        operation: admitted.operation.clone(),
        owner: admitted.owner.as_str().to_string(),
        worker_pid: std::process::id(),
        admission_count: 1,
        participant_count,
        manifest_sha256: admitted.manifest_sha256.clone(),
    };
    let encoded = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    let temporary = data_dir.join(format!(
        ".{RECEIPT_FILE}.{}.tmp",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut file = pp_os::create_new_private_file(&temporary)
        .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    let result = (|| {
        file.write_all(&encoded)
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        file.sync_all()
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        drop(file);
        let receipt = publish_receipt(&temporary, &receipt_path, receipt, admitted)?;
        match fs::remove_file(&temporary) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(AdmissionError::Malformed(error.to_string()));
            }
            _ => {}
        }
        pp_os::sync_directory(data_dir)
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        Ok(receipt)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod receipt_publication_tests {
    use super::*;
    use crate::scratch::Scratch;

    #[test]
    fn reads_the_validated_descriptor_after_path_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = Scratch::new("pp-receipt-read-");
        let winner = boot_source_sync(&dir, BUNDLED_SOURCE_SYNC_MANIFEST)?;
        let admitted = admit_observed(BUNDLED_SOURCE_SYNC_MANIFEST, SOURCE_SYNC_PARTICIPANTS)?;
        let path = dir.join(RECEIPT_FILE);
        let file = fs::File::open(&path)?;
        fs::rename(&path, dir.join("original.json"))?;
        fs::write(&path, "{replacement")?;
        assert_eq!(read_opened_receipt(file, &admitted)?, winner);
        assert_eq!(fs::read_to_string(&path)?, "{replacement");
        Ok(())
    }

    #[test]
    fn publication_consumes_its_temporary() -> Result<(), Box<dyn std::error::Error>> {
        let dir = Scratch::new("pp-receipt-publish-");
        boot_source_sync(&dir, BUNDLED_SOURCE_SYNC_MANIFEST)?;
        assert!(dir.join(RECEIPT_FILE).is_file());
        for entry in fs::read_dir(dir.join("."))? {
            assert!(!entry?.file_name().to_string_lossy().ends_with(".tmp"));
        }
        Ok(())
    }

    #[test]
    fn preserves_and_validates_a_concurrent_winner() -> Result<(), Box<dyn std::error::Error>> {
        for scenario in ["same", "different", "malformed", "nonregular"] {
            let dir = Scratch::new(&format!("pp-receipt-race-{scenario}-"));
            let winner = boot_source_sync(&dir, BUNDLED_SOURCE_SYNC_MANIFEST)?;
            let manifest = if scenario == "different" {
                format!("{BUNDLED_SOURCE_SYNC_MANIFEST}\n")
            } else {
                BUNDLED_SOURCE_SYNC_MANIFEST.to_string()
            };
            let admitted = admit_observed(&manifest, SOURCE_SYNC_PARTICIPANTS)?;
            let mut loser = winner.clone();
            loser.worker_pid = 123;
            loser.manifest_sha256 = admitted.manifest_sha256.clone();
            let temporary = dir.join("loser.tmp");
            fs::write(&temporary, serde_json::to_vec(&loser)?)?;
            let receipt_path = dir.join(RECEIPT_FILE);
            if scenario == "malformed" {
                fs::write(&receipt_path, "{broken")?;
            } else if scenario == "nonregular" {
                fs::remove_file(&receipt_path)?;
                fs::create_dir(&receipt_path)?;
            }
            let before = if scenario == "nonregular" {
                None
            } else {
                Some(fs::read(&receipt_path)?)
            };
            let result = publish_receipt(&temporary, &receipt_path, loser, &admitted);
            match scenario {
                "same" => assert_eq!(result?, winner),
                "different" => assert!(matches!(
                    result,
                    Err(AdmissionError::RestartManifestChanged)
                )),
                _ => assert!(matches!(result, Err(AdmissionError::Malformed(_)))),
            }
            if let Some(bytes) = before {
                assert_eq!(fs::read(&receipt_path)?, bytes);
            } else {
                assert!(fs::symlink_metadata(&receipt_path)?.is_dir());
            }
        }
        Ok(())
    }
}
