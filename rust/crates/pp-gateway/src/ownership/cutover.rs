//! Cutover commits after drain and graceful Node exit, while the storage lease is held.
//! An operation can switch only when every participant is fenced from Node: the
//! gateway fences HTTP routes, and Node reads the committed record at startup and
//! skips or refuses each participant listed in `node-fenced-participants.json`.

use super::{
    Owner,
    manifest::{Disposition, OwnershipIndex},
    open_lease_file, read_bounded_file,
};
use pp_compat::CompatExit;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

pub const CUTOVER_FILE: &str = "operation-cutover.json";
pub const EVIDENCE_FILE: &str = "operation-cutover-evidence.jsonl";
const CUTOVER_RECORD_LIMIT: u64 = 256 * 1024;
const RECORD_SCHEMA: &str = "pp-operation-cutover-v1";
const PARITY_SCHEMA: &str = "pp-operation-parity-v1";
const TEMPORARY_PREFIX: &str = ".operation-cutover.";
const TEMPORARY_SUFFIX: &str = ".tmp";
const NODE_FENCED_SCHEMA: &str = "pp-node-fenced-participants-v1";
const BUNDLED_NODE_FENCED: &str =
    include_str!("../../../../../web/apps/server/src/data/node-fenced-participants.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeFenced {
    schema: String,
    participants: Vec<String>,
}

static NODE_FENCED: std::sync::LazyLock<Result<BTreeSet<String>, String>> =
    std::sync::LazyLock::new(|| {
        let fenced: NodeFenced = serde_json::from_str(BUNDLED_NODE_FENCED)
            .map_err(|error| format!("bundled Node fence list does not parse: {error}"))?;
        if fenced.schema != NODE_FENCED_SCHEMA {
            return Err(format!(
                "bundled Node fence list has schema {}, expected {NODE_FENCED_SCHEMA}",
                fenced.schema
            ));
        }
        Ok(fenced.participants.into_iter().collect())
    });

/// Checks the bundled Node fence list before routes are served, so a broken
/// list stops startup instead of refusing every cutover.
pub fn admit_bundled_node_fences() -> Result<(), String> {
    NODE_FENCED.as_ref().map(|_| ()).map_err(Clone::clone)
}

/// Whether a Rust-owned `participant` cannot also run in Node. The gateway
/// fences HTTP routes; Node fences the listed participants at startup. A
/// broken bundled list fences nothing, so cutover stays refused.
pub fn node_fences(participant: &str) -> bool {
    participant.starts_with("http.")
        || NODE_FENCED
            .as_ref()
            .is_ok_and(|fenced| fenced.contains(participant))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CutoverRecord {
    schema: String,
    manifest_sha256: String,
    operations: BTreeMap<String, RustOwned>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustOwned {
    parity_sha256: String,
    fixtures: u32,
    participants: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParityReceipt {
    schema: String,
    operation: String,
    manifest_sha256: String,
    fixtures: u32,
    passed: u32,
    failed: u32,
}

impl ParityReceipt {
    fn parse(text: &str) -> Result<Self, CutoverRefusal> {
        let receipt: ParityReceipt =
            serde_json::from_str(text).map_err(|error| CutoverRefusal::ParityMalformed {
                detail: error.to_string(),
            })?;
        if receipt.schema != PARITY_SCHEMA {
            return Err(CutoverRefusal::ParityMalformed {
                detail: format!("unsupported schema {}", receipt.schema),
            });
        }
        if receipt.fixtures == 0 {
            return Err(CutoverRefusal::ParityNoFixtures);
        }
        if receipt.passed != receipt.fixtures || receipt.failed != 0 {
            return Err(CutoverRefusal::ParityFailures {
                fixtures: receipt.fixtures,
                passed: receipt.passed,
                failed: receipt.failed,
            });
        }
        Ok(receipt)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationOwners {
    record: Option<CutoverRecord>,
}

impl OperationOwners {
    pub fn node_only() -> Self {
        Self::default()
    }

    pub fn owner_of(&self, participant: &str) -> Owner {
        if self.rust_participants().any(|id| id == participant) {
            Owner::Rust
        } else {
            Owner::Node
        }
    }

    pub fn rust_operations(&self) -> impl Iterator<Item = &str> {
        self.record
            .iter()
            .flat_map(|record| record.operations.keys().map(String::as_str))
    }

    pub fn rust_participants(&self) -> impl Iterator<Item = &str> {
        self.record
            .iter()
            .flat_map(|record| record.operations.values())
            .flat_map(|owned| owned.participants.iter().map(String::as_str))
    }
}

#[derive(Debug)]
pub struct CutoverPlan {
    operation: String,
    next: CutoverRecord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainEvidence {
    pub tracked_idle: bool,
    pub gateway_idle: bool,
    pub compat: Option<CompatExit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CutoverReceipt {
    Committed {
        operation: String,
        participants: usize,
        manifest_sha256: String,
        parity_sha256: String,
        fixtures: u32,
        directory_synced: bool,
    },
    DrainTimedOut {
        operation: String,
        tracked_idle: bool,
        gateway_idle: bool,
    },
    CompatNotReaped {
        operation: String,
    },
    CompatInterrupted {
        operation: String,
    },
    CompatForceKilled {
        operation: String,
    },
    CommitFailed {
        operation: String,
        detail: String,
    },
    /// Authority may have changed. Recover the record before deciding ownership.
    CommitUnproved {
        operation: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CutoverRefusal {
    FlagOff,
    OwnershipManifestMismatch {
        recorded: String,
        current: String,
    },
    UnknownOperation,
    NotSwitchable {
        participant: String,
    },
    NodeCannotFence {
        participant: String,
    },
    AlreadyRustOwned,
    ParityMalformed {
        detail: String,
    },
    ParityNoFixtures,
    ParityFailures {
        fixtures: u32,
        passed: u32,
        failed: u32,
    },
    ParityOperationMismatch {
        expected: String,
        actual: String,
    },
    ParityManifestMismatch {
        expected: String,
        actual: String,
    },
    ShutdownInProgress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CutoverError {
    Malformed {
        detail: String,
    },
    RecordWithoutFlag,
    RestartManifestChanged {
        recorded: String,
        bundled: String,
    },
    UnknownOperation {
        operation: String,
    },
    NotSwitchable {
        participant: String,
    },
    NodeCannotFence {
        participant: String,
    },
    NoParityFixtures {
        operation: String,
    },
    ParticipantSetMismatch {
        operation: String,
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BootReceipt {
    pub rust_operations: Vec<String>,
    pub abandoned_temporaries: u32,
    pub evidence_saved: bool,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Evidence<'a> {
    Refused {
        operation: &'a str,
        #[serde(flatten)]
        refusal: &'a CutoverRefusal,
    },
    Cutover(&'a CutoverReceipt),
    Boot(&'a BootReceipt),
    BootFailed(&'a CutoverError),
}

enum Blocked<'a> {
    Unknown,
    NotSwitchable(&'a str),
    NodeCannotFence(&'a str),
}

fn switchable<'a>(
    index: &'a OwnershipIndex,
    operation: &str,
) -> Result<BTreeSet<&'a str>, Blocked<'a>> {
    let participants = index.participants_of(operation);
    if participants.is_empty() {
        return Err(Blocked::Unknown);
    }
    if let Some(id) = participants.iter().find(|id| {
        !index
            .lookup(id)
            .is_ok_and(|claim| claim.disposition == Disposition::Owned(Owner::Node))
    }) {
        return Err(Blocked::NotSwitchable(id));
    }
    if let Some(id) = participants.iter().find(|id| !node_fences(id)) {
        return Err(Blocked::NodeCannotFence(id));
    }
    Ok(participants)
}

pub fn plan(
    index: Option<&OwnershipIndex>,
    owners: &OperationOwners,
    operation: &str,
    parity: &str,
) -> Result<CutoverPlan, CutoverRefusal> {
    let index = index.ok_or(CutoverRefusal::FlagOff)?;
    if let Some(record) = &owners.record
        && record.manifest_sha256 != index.digest()
    {
        return Err(CutoverRefusal::OwnershipManifestMismatch {
            recorded: record.manifest_sha256.clone(),
            current: index.digest().to_owned(),
        });
    }
    let participants = switchable(index, operation).map_err(|blocked| match blocked {
        Blocked::Unknown => CutoverRefusal::UnknownOperation,
        Blocked::NotSwitchable(id) => CutoverRefusal::NotSwitchable {
            participant: id.to_owned(),
        },
        Blocked::NodeCannotFence(id) => CutoverRefusal::NodeCannotFence {
            participant: id.to_owned(),
        },
    })?;
    if owners.rust_operations().any(|owned| owned == operation) {
        return Err(CutoverRefusal::AlreadyRustOwned);
    }
    let receipt = ParityReceipt::parse(parity)?;
    if receipt.operation != operation {
        return Err(CutoverRefusal::ParityOperationMismatch {
            expected: operation.to_owned(),
            actual: receipt.operation,
        });
    }
    if receipt.manifest_sha256 != index.digest() {
        return Err(CutoverRefusal::ParityManifestMismatch {
            expected: index.digest().to_owned(),
            actual: receipt.manifest_sha256,
        });
    }
    let mut next = owners.record.clone().unwrap_or_else(|| CutoverRecord {
        schema: RECORD_SCHEMA.to_owned(),
        manifest_sha256: index.digest().to_owned(),
        operations: BTreeMap::new(),
    });
    next.operations.insert(
        operation.to_owned(),
        RustOwned {
            parity_sha256: hex::encode(Sha256::digest(parity.as_bytes())),
            fixtures: receipt.fixtures,
            participants: participants.into_iter().map(str::to_owned).collect(),
        },
    );
    Ok(CutoverPlan {
        operation: operation.to_owned(),
        next,
    })
}

impl CutoverPlan {
    pub fn commit(self, data_dir: &Path, drained: DrainEvidence) -> CutoverReceipt {
        self.commit_with_directory_sync(data_dir, drained, pp_os::sync_directory)
    }

    fn commit_with_directory_sync(
        self,
        data_dir: &Path,
        drained: DrainEvidence,
        sync_directory: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> CutoverReceipt {
        let operation = self.operation;
        let DrainEvidence {
            tracked_idle,
            gateway_idle,
            compat,
        } = drained;
        if !tracked_idle || !gateway_idle {
            return CutoverReceipt::DrainTimedOut {
                operation,
                tracked_idle,
                gateway_idle,
            };
        }
        match compat {
            None => return CutoverReceipt::CompatNotReaped { operation },
            Some(CompatExit::Forced) => return CutoverReceipt::CompatForceKilled { operation },
            Some(CompatExit::Interrupted) => {
                return CutoverReceipt::CompatInterrupted { operation };
            }
            Some(CompatExit::Graceful) => {}
        }
        if let Err(error) = write_record(data_dir, &self.next) {
            return CutoverReceipt::CommitFailed {
                operation,
                detail: error.to_string(),
            };
        }
        if sync_directory(data_dir).is_err() {
            return CutoverReceipt::CommitUnproved { operation };
        }
        let owned = &self.next.operations[&operation];
        CutoverReceipt::Committed {
            participants: owned.participants.len(),
            manifest_sha256: self.next.manifest_sha256.clone(),
            parity_sha256: owned.parity_sha256.clone(),
            fixtures: owned.fixtures,
            directory_synced: true,
            operation,
        }
    }
}

fn write_record(data_dir: &Path, record: &CutoverRecord) -> std::io::Result<()> {
    let encoded = serde_json::to_vec(record)?;
    if encoded.len() as u64 > CUTOVER_RECORD_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "cutover record exceeds read limit",
        ));
    }
    let temporary = data_dir.join(format!(
        "{TEMPORARY_PREFIX}{}{TEMPORARY_SUFFIX}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    let written = (|| {
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        pp_os::publish_rename(&temporary, &data_dir.join(CUTOVER_FILE))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

pub fn recover(
    data_dir: &Path,
    index: Option<&OwnershipIndex>,
) -> Result<(OperationOwners, BootReceipt), CutoverError> {
    let found =
        remove_temporaries(data_dir).and_then(|abandoned| Ok((abandoned, read_record(data_dir)?)));
    if matches!(found, Ok((0, None))) {
        return Ok((
            OperationOwners::node_only(),
            BootReceipt {
                rust_operations: Vec::new(),
                abandoned_temporaries: 0,
                evidence_saved: true,
            },
        ));
    }
    let recovered = found.and_then(|(abandoned_temporaries, text)| {
        let owners = match text {
            Some(text) => admit_record(&text, index)?,
            None => OperationOwners::node_only(),
        };
        let boot = BootReceipt {
            rust_operations: owners.rust_operations().map(str::to_owned).collect(),
            abandoned_temporaries,
            evidence_saved: true,
        };
        Ok((owners, boot))
    });
    match recovered {
        Ok((owners, mut boot)) => {
            boot.evidence_saved = append_evidence(data_dir, &Evidence::Boot(&boot)).is_ok();
            Ok((owners, boot))
        }
        Err(error) => {
            let _ = append_evidence(data_dir, &Evidence::BootFailed(&error));
            Err(error)
        }
    }
}

fn remove_temporaries(data_dir: &Path) -> Result<u32, CutoverError> {
    let failed = |error: std::io::Error| CutoverError::Malformed {
        detail: format!("cutover temporaries {error}"),
    };
    let mut removed = 0;
    for entry in fs::read_dir(data_dir).map_err(failed)? {
        let name = entry.map_err(failed)?.file_name();
        if name.to_str().is_some_and(|name| {
            name.starts_with(TEMPORARY_PREFIX) && name.ends_with(TEMPORARY_SUFFIX)
        }) {
            fs::remove_file(data_dir.join(name)).map_err(failed)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn read_record(data_dir: &Path) -> Result<Option<String>, CutoverError> {
    match open_lease_file(&data_dir.join(CUTOVER_FILE)) {
        Ok(Some(file)) => read_bounded_file(file, CUTOVER_RECORD_LIMIT)
            .map(Some)
            .map_err(|error| CutoverError::Malformed {
                detail: format!("cutover record {error}"),
            }),
        Ok(None) => Ok(None),
        Err(error) => Err(CutoverError::Malformed {
            detail: format!("cutover record is not a regular file or cannot be opened: {error}"),
        }),
    }
}

fn admit_record(
    text: &str,
    index: Option<&OwnershipIndex>,
) -> Result<OperationOwners, CutoverError> {
    let index = index.ok_or(CutoverError::RecordWithoutFlag)?;
    let record: CutoverRecord =
        serde_json::from_str(text).map_err(|error| CutoverError::Malformed {
            detail: format!("cutover record {error}"),
        })?;
    if record.schema != RECORD_SCHEMA {
        return Err(CutoverError::Malformed {
            detail: format!("unsupported cutover schema {}", record.schema),
        });
    }
    if record.manifest_sha256 != index.digest() {
        return Err(CutoverError::RestartManifestChanged {
            recorded: record.manifest_sha256,
            bundled: index.digest().to_owned(),
        });
    }
    for (operation, owned) in &record.operations {
        let expected = switchable(index, operation).map_err(|blocked| match blocked {
            Blocked::Unknown => CutoverError::UnknownOperation {
                operation: operation.clone(),
            },
            Blocked::NotSwitchable(id) => CutoverError::NotSwitchable {
                participant: id.to_owned(),
            },
            Blocked::NodeCannotFence(id) => CutoverError::NodeCannotFence {
                participant: id.to_owned(),
            },
        })?;
        if owned.fixtures == 0 {
            return Err(CutoverError::NoParityFixtures {
                operation: operation.clone(),
            });
        }
        let actual: BTreeSet<&str> = owned.participants.iter().map(String::as_str).collect();
        if actual != expected {
            return Err(CutoverError::ParticipantSetMismatch {
                operation: operation.clone(),
                missing: expected
                    .difference(&actual)
                    .map(|id| (*id).to_owned())
                    .collect(),
                unexpected: actual
                    .difference(&expected)
                    .map(|id| (*id).to_owned())
                    .collect(),
            });
        }
    }
    Ok(OperationOwners {
        record: Some(record),
    })
}

pub fn append_evidence(data_dir: &Path, evidence: &Evidence<'_>) -> std::io::Result<()> {
    append_evidence_with_directory_sync(data_dir, evidence, pp_os::sync_directory)
}

fn append_evidence_with_directory_sync(
    data_dir: &Path,
    evidence: &Evidence<'_>,
    sync_directory: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(evidence)?;
    line.push(b'\n');
    create_evidence_log(data_dir)?;
    let mut options = fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let mut file = options.open(data_dir.join(EVIDENCE_FILE))?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "cutover evidence log is not a regular file",
        ));
    }
    file.write_all(&line)?;
    file.sync_all()?;
    sync_directory(data_dir)
}

/// Unix creates a missing evidence log in the append itself; the directory
/// sync after it makes the new entry durable.
#[cfg(unix)]
fn create_evidence_log(_data_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Windows cannot flush a directory, so a missing evidence log is created
/// empty and moved into place write-through before the append. Its temporary
/// name stays outside the cutover record's temporaries, which boot counts.
#[cfg(windows)]
fn create_evidence_log(data_dir: &Path) -> std::io::Result<()> {
    let path = data_dir.join(EVIDENCE_FILE);
    if fs::symlink_metadata(&path).is_ok() {
        return Ok(());
    }
    let temporary = data_dir.join(format!(
        ".{EVIDENCE_FILE}.{}.tmp",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    fs::File::create_new(&temporary)?.sync_all()?;
    let published = match pp_os::publish_new(&temporary, &path) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    };
    let _ = fs::remove_file(&temporary);
    published
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use serde_json::json;

    #[test]
    fn ticket_t_3_drain_node_fence_list_names_bundled_operations() {
        let index = super::super::manifest::admit_bundled_manifest().unwrap();
        let fenced = NODE_FENCED.as_ref().unwrap();
        assert_eq!(admit_bundled_node_fences(), Ok(()));
        // Node fences whole operations: every non-HTTP participant of each
        // listed operation, and nothing outside them.
        let mut expected = BTreeSet::new();
        for operation in [
            "profile.sync",
            "source.sync",
            "export.accepted-plate-3mf",
            "export.checklist-html",
            "export.direct-3mf",
            "export.kit-bundle",
            "export.stl-pack",
            "printer.send-queue",
        ] {
            expected.extend(
                index
                    .participants_of(operation)
                    .into_iter()
                    .filter(|id| !id.starts_with("http."))
                    .map(str::to_owned),
            );
        }
        assert_eq!(fenced, &expected);
        assert_eq!(fenced.len(), 20);
        for participant in fenced {
            assert!(node_fences(participant));
        }
        assert!(node_fences("http.GET /ws/profile-sync"));
        for unfenced in [
            "migration.migratePrintOutcomesBlob",
            "assistant.propose_add_source",
            "startup.seedStockSlicerInstancesIfEmpty",
        ] {
            assert!(!node_fences(unfenced), "{unfenced}");
        }
    }

    #[test]
    fn ticket_t_3_drain_evidence_directory_sync_failure_preserves_authority() {
        let dir = Scratch::new("pp-evidence-sync-");
        let index = super::super::manifest::admit_bundled_manifest().unwrap();
        let operation = "http:POST /plans current";
        let parity = json!({
            "schema": "pp-operation-parity-v1", "operation": operation,
            "manifest_sha256": index.digest(), "fixtures": 1, "passed": 1, "failed": 0,
        });
        let receipt = plan(
            Some(&index),
            &OperationOwners::node_only(),
            operation,
            &parity.to_string(),
        )
        .unwrap()
        .commit(
            &dir,
            DrainEvidence {
                tracked_idle: true,
                gateway_idle: true,
                compat: Some(CompatExit::Graceful),
            },
        );
        assert!(matches!(receipt, CutoverReceipt::Committed { .. }));
        let authority = fs::read(dir.join(CUTOVER_FILE)).unwrap();
        let mut expected_log = Vec::new();
        for _ in 0..2 {
            expected_log.extend(serde_json::to_vec(&Evidence::Cutover(&receipt)).unwrap());
            expected_log.push(b'\n');
            let error =
                append_evidence_with_directory_sync(&dir, &Evidence::Cutover(&receipt), |path| {
                    assert_eq!(path, &*dir);
                    assert_eq!(fs::read(path.join(EVIDENCE_FILE)).unwrap(), expected_log);
                    Err(std::io::Error::other("injected directory sync failure"))
                })
                .unwrap_err();
            assert_eq!(error.to_string(), "injected directory sync failure");
            assert_eq!(fs::read(dir.join(CUTOVER_FILE)).unwrap(), authority);
        }
        let (owners, boot) = recover(&dir, Some(&index)).unwrap();
        assert!(boot.evidence_saved);
        assert_eq!(owners.owner_of("http.POST /plans"), Owner::Rust);
        assert_eq!(owners.owner_of("http.POST /api/v1/plans"), Owner::Node);
        assert_eq!(fs::read(dir.join(CUTOVER_FILE)).unwrap(), authority);
    }

    #[test]
    fn ticket_t_3_drain_cutover_directory_fsync_failure_is_unproved_after_rename() {
        let dir = Scratch::new("pp-cutover-sync-");
        let index = super::super::manifest::admit_bundled_manifest().unwrap();
        let operation = "http:POST /plans current";
        let parity = json!({
            "schema": "pp-operation-parity-v1", "operation": operation,
            "manifest_sha256": index.digest(), "fixtures": 1, "passed": 1, "failed": 0,
        });
        let plan = plan(
            Some(&index),
            &OperationOwners::node_only(),
            operation,
            &parity.to_string(),
        )
        .unwrap();
        let expected = serde_json::to_vec(&plan.next).unwrap();
        let receipt = plan.commit_with_directory_sync(
            &dir,
            DrainEvidence {
                tracked_idle: true,
                gateway_idle: true,
                compat: Some(CompatExit::Graceful),
            },
            |path| {
                assert_eq!(fs::read(path.join(CUTOVER_FILE)).unwrap(), expected);
                Err(std::io::Error::other("injected directory sync failure"))
            },
        );
        assert_eq!(fs::read(dir.join(CUTOVER_FILE)).unwrap(), expected);
        let (owners, _) = recover(&dir, Some(&index)).unwrap();
        assert_eq!(fs::read(dir.join(CUTOVER_FILE)).unwrap(), expected);
        assert_eq!(owners.owner_of("http.POST /plans"), Owner::Rust);
        assert_eq!(owners.owner_of("http.POST /api/v1/plans"), Owner::Node);
        assert_eq!(
            serde_json::to_value(receipt).unwrap(),
            json!({
                "outcome": "commit_unproved", "operation": "http:POST /plans current",
            })
        );
    }
}
