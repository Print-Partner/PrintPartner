use crate::{
    Envelope, SettingsClient, WriterFailure, WriterOwner, auth::AuthPolicy, catalog, jobs,
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[cfg(test)]
mod tests;

mod capture_manifest;
mod captured;

pub use capture_manifest::{
    CaptureManifestInventory, inspect_capture_manifest, resolve_capture_manifest,
};
pub use captured::{CaptureId, CapturedPayloadV1};

const COLUMNS: &str = include_str!("remote_sources/columns38.sql");

type Install = fn(&Connection) -> Result<()>;

pub(crate) const SCHEMA_STEPS: [(u64, Install); 2] =
    [(38, install_imports), (39, install_preclaim)];

fn install_imports(connection: &Connection) -> Result<()> {
    for (table, column, definition) in added_columns()? {
        crate::schema::add_column(
            connection,
            table,
            column,
            &format!("ALTER TABLE {table} ADD COLUMN {definition}"),
        )?;
    }
    connection.execute_batch(include_str!("uploads/schema.sql"))?;
    Ok(connection.execute_batch(include_str!("remote_sources/schema.sql"))?)
}

fn install_preclaim(connection: &Connection) -> Result<()> {
    Ok(connection.execute_batch(include_str!("remote_sources/preclaim39.sql"))?)
}

type Object = (String, String, Option<String>);

fn objects(connection: &Connection) -> Result<Vec<Object>> {
    Ok(connection
        .prepare("SELECT type,name,sql FROM sqlite_master ORDER BY name")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

fn expected(version: u64) -> Result<Connection> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(
        "CREATE TABLE projects(id INTEGER PRIMARY KEY);
         CREATE TABLE source_revisions(id INTEGER PRIMARY KEY);
         CREATE TABLE source_docs(id INTEGER PRIMARY KEY);",
    )?;
    for (step, install) in SCHEMA_STEPS {
        if step <= version {
            install(&expected)?;
        }
    }
    Ok(expected)
}

pub(crate) fn owned_objects() -> Result<BTreeSet<String>> {
    let stubs: BTreeSet<String> = objects(&expected(0)?)?
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    Ok(objects(&expected(u64::MAX)?)?
        .into_iter()
        .map(|(_, name, _)| name)
        .filter(|name| !stubs.contains(name))
        .collect())
}

pub(crate) fn added_columns() -> Result<Vec<(&'static str, &'static str, &'static str)>> {
    COLUMNS
        .lines()
        .map(|statement| {
            let words: Vec<&str> = statement.split_whitespace().collect();
            let definition = statement
                .split_once(" ADD COLUMN ")
                .map(|(_, definition)| definition.trim_end_matches(';'));
            match (words.get(2), words.get(5), definition) {
                (Some(table), Some(column), Some(definition)) => Ok((*table, *column, definition)),
                _ => Err(anyhow!("Invalid Source column migration")),
            }
        })
        .collect()
}

pub(crate) fn validate_schema(connection: &Connection, version: u64) -> Result<()> {
    let owned = owned_objects()?;
    let actual: Vec<Object> = objects(connection)?
        .into_iter()
        .filter(|(_, name, _)| owned.contains(name))
        .collect();
    let expected: Vec<Object> = objects(&expected(version)?)?
        .into_iter()
        .filter(|(_, name, _)| owned.contains(name))
        .collect();
    ensure!(actual == expected, "Source import schema mismatch");
    if version < SCHEMA_STEPS[0].0 {
        return Ok(());
    }
    for (table, column, definition) in added_columns()? {
        ensure!(
            column_definition(connection, table, column)? == definition,
            "Source revision column schema mismatch: {table}.{column}"
        );
    }
    for (check, message) in [
        (
            "SELECT EXISTS(SELECT 1 FROM source_import_quota q LEFT JOIN source_import_operations o USING(tenant,operation_key) WHERE o.tenant IS NULL)",
            "Orphan Source quota",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM source_import_operations WHERE json_extract(document,'$.tenant') IS NOT tenant OR json_extract(document,'$.key') IS NOT operation_key OR json_extract(document,'$.actor') IS NOT actor OR json_extract(document,'$.intent_digest') IS NOT intent_digest OR json_extract(document,'$.source_id') IS NOT source_id OR json_extract(document,'$.job_id') IS NOT job_id OR json_extract(document,'$.state') IS NOT state)",
            "Corrupt Source import journal",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM source_import_operations o LEFT JOIN source_import_quota q USING(tenant,operation_key) WHERE q.settled IS NOT json_extract(o.document,'$.cleanup_settled') OR q.reserved_bytes IS NOT CASE WHEN q.settled THEN 0 ELSE json_extract(o.document,'$.reserved_bytes') END OR q.retained_bytes IS NOT CASE WHEN q.settled THEN coalesce(json_extract(o.document,'$.artifact.stored_bytes'),0) ELSE 0 END)",
            "Corrupt Source quota journal",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM source_docs WHERE NOT ((source_revision_id IS NULL AND input_digest IS NULL AND producer_version IS NULL) OR (source_revision_id IS NOT NULL AND input_digest IS NOT NULL AND length(input_digest) = 64 AND producer_version IS NOT NULL)))",
            "Partial Source document provenance",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM source_revisions WHERE NOT ((source_configuration_version IS NULL AND activation_observation_digest IS NULL AND input_digest IS NULL AND producer_version IS NULL) OR (source_configuration_version IS NOT NULL AND activation_observation_digest IS NOT NULL AND length(activation_observation_digest) = 64 AND input_digest IS NOT NULL AND length(input_digest) = 64 AND producer_version IS NOT NULL)))",
            "Partial Source revision provenance",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM source_revision_observations WHERE attempt_generation <= 0 OR cancel_requested NOT IN (0,1) OR receipt_activated NOT IN (0,1) OR (receipt_activated IS NOT NULL AND revision_id IS NULL))",
            "Invalid Source observation context",
        ),
    ] {
        let failed: bool = connection.query_row(check, [], |row| row.get(0))?;
        ensure!(!failed, "{message}");
    }
    if version < SCHEMA_STEPS[1].0 {
        return validate_journal(connection, false);
    }
    for (check, message) in [
        (
            "SELECT EXISTS(SELECT 1 FROM source_preclaim_refusals WHERE cursor<=0 OR phase!='authority_refused' OR admitted_generation!=0 OR cancel_requested NOT IN (0,1))",
            "Invalid Source preclaim refusal",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM source_preclaim_refusals p JOIN source_revision_observations o USING(tenant_id,operation_key,cursor))",
            "Source observation cursor collision",
        ),
    ] {
        let failed: bool = connection.query_row(check, [], |row| row.get(0))?;
        ensure!(!failed, "{message}");
    }
    validate_journal(connection, true)
}

fn column_definition(connection: &Connection, table: &str, column: &str) -> Result<String> {
    let sql: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get(0),
    )?;
    let body = &sql[sql
        .find('(')
        .ok_or_else(|| anyhow!("Missing table definition"))?
        + 1..];
    let mut depth = 0;
    let mut literal = false;
    let mut start = 0;
    for (index, character) in body.char_indices() {
        if character == '\'' {
            literal = !literal;
        }
        if literal {
            continue;
        }
        if character == '(' {
            depth += 1;
        } else if (character == ',' || character == ')') && depth == 0 {
            let definition = body[start..index].trim();
            if definition.split_whitespace().next() == Some(column) {
                return Ok(definition.to_owned());
            }
            start = index + 1;
        } else if character == ')' {
            depth -= 1;
        }
    }
    Err(anyhow!("Missing Source column definition {table}.{column}"))
}

fn validate_journal(connection: &Connection, preclaim: bool) -> Result<()> {
    let documents = connection
        .prepare("SELECT document FROM source_import_operations")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for document in documents {
        let op: Operation = serde_json::from_str(&document)?;
        validate_operation(&op)?;
        let payload = jobs::Payload::SuppliedSourceImport {
            project_id: u64::try_from(op.source_id)?,
            operation_key: op.key.clone(),
            input_version: op.input_version,
        };
        let job = connection
            .query_row(
                "SELECT document FROM durable_jobs WHERE id=?1 UNION ALL SELECT archived_document FROM durable_job_keys WHERE job_id=?1 AND archived_document IS NOT NULL LIMIT 1",
                [&op.job_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|job| jobs::decode(&job))
            .transpose()?;
        ensure!(
            job.as_ref().is_some_and(|job| job.payload_version == 1
                && job.job_id == op.job_id
                && job.tenant == op.tenant
                && job.payload == payload),
            "Import job binding mismatch"
        );
        ensure!(
            !op.cleanup_settled || job.as_ref().is_some_and(|job| job.state.terminal()),
            "Import completion job mismatch"
        );
        if preclaim && let Some(job) = job {
            validate_preclaim_refusal(connection, &op, &job)?;
        }
    }
    Ok(())
}

/// An upstream revision key already names a revision that is not complete.
/// Activation returns this and writes neither that revision nor its attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncompleteRevisionKey;
impl std::fmt::Display for IncompleteRevisionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Incomplete revision key")
    }
}
impl std::error::Error for IncompleteRevisionKey {}

/// Opening storage found an import whose job and cursor show a preclaim refusal
/// without its `source_preclaim_refusals` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingPreclaimRefusal;
impl std::fmt::Display for MissingPreclaimRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Missing Source preclaim refusal")
    }
}
impl std::error::Error for MissingPreclaimRefusal {}

/// A preclaim refusal is the entry's only observation: its admitted entry's
/// first cursor, and the targeted claim refusal that failed its job before any
/// attempt. A settled entry keeps the refusal as its last observation. An entry
/// whose job records that refusal and whose cursor counted it must have the row;
/// a refusal from before the row was written left the cursor at 0.
fn validate_preclaim_refusal(
    connection: &Connection,
    op: &Operation,
    job: &jobs::JobRecord,
) -> Result<()> {
    let refusal = job.authority_refusal.as_ref();
    let Some((cursor, job_id, cancel)) = connection
        .query_row(
            "SELECT cursor,job_id,cancel_requested FROM source_preclaim_refusals WHERE tenant_id=?1 AND operation_key=?2",
            params![op.tenant, op.key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?)),
        )
        .optional()?
    else {
        ensure!(
            !(op.observation_cursor.is_some_and(|cursor| cursor > 0)
                && refusal.is_some_and(|refusal| {
                    refusal.phase == jobs::AuthorityRefusalPhase::TargetedClaim
                        && refusal.generation == 0
                })),
            MissingPreclaimRefusal
        );
        return Ok(());
    };
    let (observations, attempts): (i64, i64) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM source_revision_observations WHERE tenant_id=?1 AND operation_key=?2),(SELECT COUNT(*) FROM source_revision_attempts WHERE tenant_id=?1 AND operation_key=?2)",
        params![op.tenant, op.key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        cursor == 1
            && job_id == op.job_id
            && op.authority_revision.is_some()
            && match (&op.state, op.cleanup_settled) {
                (State::Admitted, false) => true,
                (State::Failed, true) => !cancel,
                (State::Cancelled, true) => cancel,
                _ => false,
            }
            && op.observation_cursor == Some(cursor)
            && observations == 0
            && attempts == 0
            && job.state == jobs::PersistentState::Failed
            && job.attempt == 0
            && job.generation == 1
            && job.fence.is_none()
            && job.worker.is_none()
            && job.cancel_requested == cancel
            && refusal.is_some_and(|refusal| {
                refusal.phase == jobs::AuthorityRefusalPhase::TargetedClaim
                    && refusal.generation == 0
            }),
        "Invalid Source preclaim refusal binding"
    );
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Existing {
        source_id: i64,
    },
    Create {
        metadata: Box<catalog::CreateSource>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Files { paths: Vec<String> },
    Zip { path: String },
    Captured(CapturedInputV2),
}

impl Input {
    /// The path of the one ZIP a captured import holds, which the worker
    /// unpacks before preparing media (GRE-1444).
    pub fn captured_zip_path(&self) -> Option<&str> {
        match self {
            Self::Captured(captured) => captured.payload.zip_path(),
            Self::Files { .. } | Self::Zip { .. } => None,
        }
    }

    pub fn version(&self) -> u32 {
        match self {
            Self::Files { .. } | Self::Zip { .. } => 1,
            Self::Captured(_) => 2,
        }
    }

    fn requested_paths(&self) -> Vec<&str> {
        match self {
            Self::Files { paths } => paths.iter().map(String::as_str).collect(),
            Self::Zip { path } => vec![path],
            Self::Captured(captured) => captured.payload.requested_paths(),
        }
    }
}

#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
struct Sha256Digest(String);

impl Sha256Digest {
    fn new(value: String) -> Result<Self> {
        digest(&value)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Sha256Digest([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmissionLimits {
    pub reserved_bytes: u64,
    pub max_input_bytes: u64,
    pub max_prepared_bytes: u64,
}

fn validate_admission_limits(limits: AdmissionLimits, quota: Option<u64>) -> Result<()> {
    ensure!(
        (1..=256 * 1024 * 1024).contains(&limits.max_input_bytes)
            && (1..=1024 * 1024 * 1024).contains(&limits.max_prepared_bytes),
        "Invalid import limits"
    );
    let minimum = limits
        .max_input_bytes
        .checked_add(3 * limits.max_prepared_bytes)
        .and_then(|minimum| minimum.checked_add(16 * 1024 * 1024))
        .ok_or_else(|| anyhow!("Quota overflow"))?;
    ensure!(
        limits.reserved_bytes >= minimum
            && quota.is_none_or(|quota| limits.reserved_bytes <= quota),
        "Import quota exceeded"
    );
    Ok(())
}

fn ensure_owner_import_quota(
    tx: &Transaction<'_>,
    disk_bytes: u64,
    reserved_bytes: u64,
    quota: u64,
) -> Result<()> {
    let pending: i64 = tx.query_row(
        "SELECT COALESCE(SUM(reserved_bytes),0) FROM source_import_quota WHERE settled=0",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        disk_bytes
            .checked_add(u64::try_from(pending)?)
            .and_then(|total| total.checked_add(reserved_bytes))
            .is_some_and(|total| total <= quota),
        "Owner import quota exceeded"
    );
    Ok(())
}

#[derive(Serialize)]
struct CaptureManifestBindingV1<'a> {
    manifest_version: u32,
    capture_id: &'a CaptureId,
    operation_key: &'a str,
    actor: &'a str,
    target_digest: &'a Sha256Digest,
    input: &'a CapturedPayloadV1,
    admission_limits: AdmissionLimits,
    policy_digest: &'a Sha256Digest,
    requested_files: &'a [File],
    requested_digest: &'a Sha256Digest,
}

fn calculate_capture_binding(
    capture_id: &CaptureId,
    operation_key: &str,
    actor: &str,
    target_digest: &Sha256Digest,
    input: &CapturedPayloadV1,
    admission_limits: AdmissionLimits,
    requested_files: &[File],
) -> Result<(Sha256Digest, Sha256Digest)> {
    let requested_digest = Sha256Digest::new(serialized_digest(&requested_files)?)?;
    let policy_digest = input.policy_digest()?;
    let manifest_digest = Sha256Digest::new(serialized_digest(&(
        "pp-source-capture-binding-v1",
        CaptureManifestBindingV1 {
            manifest_version: 1,
            capture_id,
            operation_key,
            actor,
            target_digest,
            input,
            admission_limits,
            policy_digest: &policy_digest,
            requested_files,
            requested_digest: &requested_digest,
        },
    ))?)?;
    Ok((policy_digest, manifest_digest))
}

fn target_digest(target: &Target) -> Result<Sha256Digest> {
    Sha256Digest::new(serialized_digest(&("pp-source-import-target-v1", target))?)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapturedInputV2 {
    capture_version: u32,
    capture_id: CaptureId,
    payload: CapturedPayloadV1,
    policy_digest: Sha256Digest,
    target_digest: Sha256Digest,
    manifest_digest: Sha256Digest,
}

impl CapturedInputV2 {
    pub(crate) fn capture_id(&self) -> &CaptureId {
        &self.capture_id
    }

    pub fn bind(
        capture_id: CaptureId,
        payload: CapturedPayloadV1,
        actor: &str,
        operation_key: &str,
        target: &Target,
        admission_limits: AdmissionLimits,
        requested_files: &[File],
    ) -> Result<Self> {
        payload.validate()?;
        let target_digest = target_digest(target)?;
        let (policy_digest, manifest_digest) = calculate_capture_binding(
            &capture_id,
            operation_key,
            actor,
            &target_digest,
            &payload,
            admission_limits,
            requested_files,
        )?;
        Ok(Self {
            capture_version: 1,
            capture_id,
            payload,
            policy_digest,
            target_digest,
            manifest_digest,
        })
    }

    pub fn validate(
        &self,
        actor: &str,
        operation_key: &str,
        target: Option<&Target>,
        admission_limits: AdmissionLimits,
        requested_files: &[File],
    ) -> Result<()> {
        ensure!(self.capture_version == 1, "Unsupported capture version");
        self.payload.validate()?;
        let (policy_digest, manifest_digest) = calculate_capture_binding(
            &self.capture_id,
            operation_key,
            actor,
            &self.target_digest,
            &self.payload,
            admission_limits,
            requested_files,
        )?;
        ensure!(
            policy_digest == self.policy_digest,
            "Capture policy digest mismatch"
        );
        if let Some(target) = target {
            ensure!(
                target_digest(target)? == self.target_digest,
                "Capture target digest mismatch"
            );
        }
        ensure!(
            manifest_digest == self.manifest_digest,
            "Capture manifest digest mismatch"
        );
        Ok(())
    }
}

pub fn decode_import_input_version(value: &serde_json::Value) -> Result<u32> {
    if value.get("policy").is_some() {
        let payload: CapturedPayloadV1 = serde_json::from_value(value.clone())
            .map_err(|error| anyhow!("Invalid captured payload: {error}"))?;
        payload.validate()?;
        return Ok(payload.input_version());
    }
    let input: Input = serde_json::from_value(value.clone())
        .map_err(|error| anyhow!("Invalid import input: {error}"))?;
    Ok(input.version())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub key: String,
    pub target: Target,
    pub input: Input,
    pub reserved_bytes: u64,
    pub max_input_bytes: u64,
    pub max_prepared_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Basis {
    pub current_source_revision_id: Option<i64>,
    pub url: String,
    pub branch: String,
    pub tag: Option<String>,
    pub source_kind: String,
    pub source_type: String,
    pub local_path: Option<String>,
    pub last_commit_sha: Option<String>,
    pub legacy_manifest_cutover: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Admitted,
    OwnedInputReady,
    Published,
    Activated,
    Conflict,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnedInput {
    pub locator: String,
    pub digest: String,
    pub files: Vec<File>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub tenant: String,
    pub source_id: i64,
    pub upstream_key: String,
    pub manifest_digest: String,
    pub locator: String,
    pub stored_bytes: u64,
    pub files: Vec<File>,
    pub suggested_rules: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorityRevision {
    pub version: u8,
    pub source_configuration_version: i64,
    pub activation_observation_digest: String,
    pub input_digest: String,
    pub producer_version: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Postprocessing {
    NotActivated,
    DocumentMetadataIndexed,
    DocumentMetadataIndexedPdfPending,
    IndexError,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub tenant: String,
    pub operation_key: String,
    pub job_id: String,
    pub source_id: i64,
    pub revision_id: i64,
    pub artifact: Artifact,
    pub basis: Basis,
    pub activated: bool,
    pub applied_at: String,
    pub postprocessing: Postprocessing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_revision: Option<SourceAuthorityRevision>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub tenant: String,
    pub actor: String,
    pub key: String,
    pub intent_digest: String,
    pub source_id: i64,
    pub job_id: String,
    pub input_version: u32,
    pub input: Input,
    pub requested_files: Vec<File>,
    pub requested_digest: String,
    pub state: State,
    pub basis: Basis,
    pub default_rules: bool,
    pub original_rules: Option<String>,
    pub reserved_bytes: u64,
    pub max_input_bytes: u64,
    pub max_prepared_bytes: u64,
    pub owned: Option<OwnedInput>,
    pub artifact: Option<Artifact>,
    pub receipt: Option<Receipt>,
    pub cleanup_settled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_revision: Option<SourceAuthorityRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_cursor: Option<i64>,
}

/// A step of a claimed import attempt. Every step but `Read` records a Source
/// observation.
#[derive(Clone, Debug)]
pub enum Phase {
    Read,
    Owned(OwnedInput),
    Published(Artifact),
    Activate,
    Cleanup,
    /// Ends the import. The reason, in words, becomes the failed job's
    /// `error`; a cancelled import ignores it.
    Fail(String),
}

impl Phase {
    fn observation(&self) -> Option<&'static str> {
        match self {
            Self::Read => None,
            Self::Owned(_) => Some("owned"),
            Self::Published(_) => Some("published"),
            Self::Activate => Some("activated"),
            Self::Cleanup => Some("cleanup"),
            Self::Fail(_) => Some("failed"),
        }
    }
}

/// The attempt context a Source observation records.
enum Observed {
    /// A targeted claim refused before the job's first attempt.
    Preclaim { job_id: String, cancel: bool },
    /// A live attempt, or a claim refused after an earlier attempt, which
    /// holds no fence. A generic recovered claim uses this shape too.
    Attempt {
        job_id: String,
        generation: i64,
        fence: Option<String>,
        receipt: Option<(i64, bool)>,
        cancel: bool,
    },
}

impl Observed {
    fn fenced(job: &jobs::JobRecord, lease: &jobs::AttemptLease, op: &Operation) -> Self {
        Self::Attempt {
            job_id: job.job_id.clone(),
            generation: lease.generation(),
            fence: Some(lease.fence().to_owned()),
            receipt: op
                .receipt
                .as_ref()
                .map(|receipt| (receipt.revision_id, receipt.activated)),
            cancel: job.cancel_requested,
        }
    }
}

pub struct PreflightedCapture {
    pub(crate) credential: jobs::Credential,
    pub(crate) tenant: String,
    pub(crate) actor: String,
    pub(crate) target: Target,
    pub(crate) replay: Option<Box<Operation>>,
}

pub struct PreparedCapturedAdmission {
    preflight: PreflightedCapture,
    request: Admission,
    expected_files: Vec<File>,
    manifest: Vec<u8>,
}

/// Retained capture claim that still matches an admitted journal row.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedCapturedClaim {
    manifest: Vec<u8>,
    inventory: Vec<File>,
    tenant: String,
    operation_key: String,
    job_id: String,
}

/// How a capture manifest lines up with the upload journal and import jobs.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureJournalCorrelation {
    ExactAdmitted(ResolvedCapturedClaim),
    ExactOwnedOrLater,
    Absent,
    MismatchRepairRequired,
}

impl PreflightedCapture {
    pub fn replay(&self) -> Option<&Operation> {
        self.replay.as_deref()
    }

    pub fn prepare(
        self,
        capture_id: CaptureId,
        operation_key: String,
        payload: CapturedPayloadV1,
        limits: AdmissionLimits,
        expected_files: Vec<File>,
    ) -> Result<PreparedCapturedAdmission> {
        let bound = capture_manifest::bind_capture_manifest(
            capture_id,
            payload,
            &self.actor,
            &operation_key,
            &self.target,
            limits,
            &expected_files,
        )?;
        let (captured, manifest) = bound.into_parts();
        let request = Admission {
            key: operation_key,
            target: self.target.clone(),
            input: Input::Captured(captured),
            reserved_bytes: limits.reserved_bytes,
            max_input_bytes: limits.max_input_bytes,
            max_prepared_bytes: limits.max_prepared_bytes,
        };
        Ok(PreparedCapturedAdmission {
            preflight: self,
            request,
            expected_files,
            manifest,
        })
    }
}

impl PreparedCapturedAdmission {
    pub fn manifest(&self) -> &[u8] {
        &self.manifest
    }
}

pub(crate) enum UploadsOutcome {
    Operation(Box<Operation>),
    Preflighted(PreflightedCapture),
    CaptureCorrelation(CaptureJournalCorrelation),
}

pub(crate) enum AdmissionAuthority {
    Credential(jobs::Credential),
    Preflighted(Box<PreflightedCapture>),
}

pub(crate) enum Command {
    Admit(Admit),
    Get {
        credential: jobs::Credential,
        policy: AuthPolicy,
        storage: Arc<crate::Shared>,
        key: String,
    },
    PreflightCapture {
        credential: jobs::Credential,
        policy: AuthPolicy,
        storage: Arc<crate::Shared>,
        operation_key: String,
        target: Target,
        payload: CapturedPayloadV1,
        limits: AdmissionLimits,
        disk_bytes: u64,
        quota: u64,
    },
    CorrelateCapture {
        manifest: Vec<u8>,
        inventory: Vec<File>,
    },
    Phase {
        lease: jobs::AttemptLease,
        source_work: catalog::SourceWorkProof,
        phase: Phase,
        policy: AuthPolicy,
    },
}

pub(crate) struct Admit {
    authority: AdmissionAuthority,
    policy: AuthPolicy,
    storage: Arc<crate::Shared>,
    request: Admission,
    expected_files: Vec<File>,
    disk_bytes: u64,
    accounting_epoch: u64,
    quota: u64,
}

impl Command {
    pub(crate) fn changes_accounting(&self) -> bool {
        !matches!(
            self,
            Self::Get { .. }
                | Self::PreflightCapture { .. }
                | Self::CorrelateCapture { .. }
                | Self::Phase {
                    phase: Phase::Read,
                    ..
                }
        )
    }
}

#[derive(Clone)]
pub struct ImportClient {
    storage: SettingsClient,
    policy: AuthPolicy,
    quota: u64,
}

impl WriterOwner {
    /// Opens the upload journal with the owner's import quota in bytes. Every
    /// client of one owner must name the same quota.
    pub fn imports(&self, policy: AuthPolicy, quota: u64) -> Result<ImportClient> {
        ensure!(
            quota > 0 && quota <= 64 * 1024 * 1024 * 1024,
            "Invalid desktop quota"
        );
        crate::auth::validate_policy(policy)?;
        let mut configured = self
            .client
            .shared
            .import_quota
            .lock()
            .map_err(|_| anyhow!("Import quota poisoned"))?;
        ensure!(
            configured.is_none_or(|configured| configured == quota),
            "Import quota already configured"
        );
        *configured = Some(quota);
        Ok(ImportClient {
            storage: self.client(),
            policy,
            quota,
        })
    }

    /// The directory published Source revisions and owned import inputs live
    /// under.
    pub fn import_repos_root(&self) -> Result<std::path::PathBuf> {
        Ok(self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow!("Storage stopped"))?
            .data_dir()
            .join("repos"))
    }
}

impl ImportClient {
    /// Read before measuring `disk_bytes`; admission refuses a measurement
    /// taken across another journal write.
    pub fn accounting_epoch(&self) -> u64 {
        self.storage.shared.import_epoch.load(Ordering::Acquire)
    }

    pub fn admit(
        &self,
        credential: jobs::Credential,
        request: Admission,
        expected_files: Vec<File>,
        disk_bytes: u64,
        accounting_epoch: u64,
        cancelled: &AtomicBool,
    ) -> Result<Operation> {
        match submit(
            &self.storage,
            Command::Admit(Admit {
                authority: AdmissionAuthority::Credential(credential),
                policy: self.policy,
                storage: self.storage.shared.clone(),
                request,
                expected_files,
                disk_bytes,
                accounting_epoch,
                quota: self.quota,
            }),
            cancelled,
        )? {
            UploadsOutcome::Operation(operation) => Ok(*operation),
            UploadsOutcome::Preflighted(_) | UploadsOutcome::CaptureCorrelation(_) => {
                Err(anyhow!("Unexpected capture preflight reply"))
            }
        }
    }

    pub fn admit_prepared(
        &self,
        prepared: PreparedCapturedAdmission,
        disk_bytes: u64,
        accounting_epoch: u64,
    ) -> Result<Operation> {
        match submit(
            &self.storage,
            Command::Admit(Admit {
                authority: AdmissionAuthority::Preflighted(Box::new(prepared.preflight)),
                policy: self.policy,
                storage: self.storage.shared.clone(),
                request: prepared.request,
                expected_files: prepared.expected_files,
                disk_bytes,
                accounting_epoch,
                quota: self.quota,
            }),
            &AtomicBool::new(false),
        )? {
            UploadsOutcome::Operation(operation) => Ok(*operation),
            UploadsOutcome::Preflighted(_) | UploadsOutcome::CaptureCorrelation(_) => {
                Err(anyhow!("Unexpected capture preflight reply"))
            }
        }
    }

    /// Read `disk_bytes` the same way as [`Self::admit`]: measure under the
    /// owner's repos root after reading [`Self::accounting_epoch`]. Preflight
    /// does not write, so a stale epoch is not refused here.
    pub fn preflight_capture(
        &self,
        credential: jobs::Credential,
        operation_key: String,
        target: Target,
        payload: CapturedPayloadV1,
        limits: AdmissionLimits,
        disk_bytes: u64,
    ) -> Result<PreflightedCapture> {
        match submit(
            &self.storage,
            Command::PreflightCapture {
                credential,
                policy: self.policy,
                storage: self.storage.shared.clone(),
                operation_key,
                target,
                payload,
                limits,
                disk_bytes,
                quota: self.quota,
            },
            &AtomicBool::new(false),
        )? {
            UploadsOutcome::Preflighted(preflight) => Ok(preflight),
            UploadsOutcome::Operation(_) | UploadsOutcome::CaptureCorrelation(_) => {
                Err(anyhow!("Unexpected import reply"))
            }
        }
    }

    /// Correlate retained capture-manifest bytes and inventory with the journal
    /// without admitting or charging. Distinguishes absent, exact admitted,
    /// owned-or-later, and conflicting rows.
    pub fn correlate_capture_manifest(
        &self,
        manifest: Vec<u8>,
        inventory: Vec<File>,
    ) -> Result<CaptureJournalCorrelation> {
        match submit(
            &self.storage,
            Command::CorrelateCapture {
                manifest,
                inventory,
            },
            &AtomicBool::new(false),
        )? {
            UploadsOutcome::CaptureCorrelation(correlation) => Ok(correlation),
            UploadsOutcome::Operation(_) | UploadsOutcome::Preflighted(_) => {
                Err(anyhow!("Unexpected capture correlation reply"))
            }
        }
    }

    pub fn get(&self, credential: jobs::Credential, key: String) -> Result<Operation> {
        match submit(
            &self.storage,
            Command::Get {
                credential,
                policy: self.policy,
                storage: self.storage.shared.clone(),
                key,
            },
            &AtomicBool::new(false),
        )? {
            UploadsOutcome::Operation(operation) => Ok(*operation),
            UploadsOutcome::Preflighted(_) | UploadsOutcome::CaptureCorrelation(_) => {
                Err(anyhow!("Unexpected capture preflight reply"))
            }
        }
    }
}

fn capture_actor(
    tx: &Transaction<'_>,
    credential: &jobs::Credential,
    policy: AuthPolicy,
    storage: &Arc<crate::Shared>,
) -> Result<(String, String)> {
    match credential {
        jobs::Credential::Session(token) => {
            crate::auth::job_session_actor(tx, token.expose(), policy)
        }
        jobs::Credential::RoutedKey { tenant, key } => {
            crate::auth::authority::queries::job_key_actor_ref(tx, tenant, key)
        }
        jobs::Credential::PhysicalOwner(owner) => {
            ensure!(
                Arc::ptr_eq(storage, owner.storage_identity()),
                "Foreign physical owner"
            );
            Ok(("default".into(), "physical-owner".into()))
        }
    }
}

pub(crate) fn validate_capture_target(
    tx: &Transaction<'_>,
    tenant: &str,
    target: &mut Target,
) -> Result<()> {
    match target {
        Target::Existing { source_id } => {
            catalog::validate_capture_existing(tx, tenant, *source_id)
        }
        Target::Create { metadata } => catalog::validate_capture_create(tx, tenant, metadata),
    }
}

pub(crate) struct PreflightCaptureCheck<'a> {
    pub tenant: &'a str,
    pub actor: &'a str,
    pub operation_key: &'a str,
    pub target: &'a mut Target,
    pub payload: &'a CapturedPayloadV1,
    pub limits: AdmissionLimits,
    pub quota: Option<u64>,
    pub disk_bytes: u64,
}

pub(crate) fn preflight_capture_target(
    tx: &Transaction<'_>,
    check: PreflightCaptureCheck<'_>,
) -> Result<Option<Box<Operation>>> {
    validate_admission_limits(check.limits, check.quota)?;
    ensure!(
        !check.operation_key.is_empty()
            && check.operation_key.len() <= 128
            && !check.operation_key.chars().any(char::is_control),
        "Invalid operation key"
    );
    if let Some(prior) = load(tx, check.tenant, check.operation_key)? {
        ensure!(
            prior.actor == check.actor,
            "Import belongs to another actor"
        );
        let Input::Captured(captured) = &prior.input else {
            anyhow::bail!("Import idempotency conflict");
        };
        ensure!(
            &captured.payload == check.payload,
            "Import idempotency conflict"
        );
        captured.validate(
            check.actor,
            check.operation_key,
            Some(check.target),
            check.limits,
            &prior.requested_files,
        )?;
        Ok(Some(Box::new(prior)))
    } else {
        validate_capture_target(tx, check.tenant, check.target)?;
        if let Some(quota) = check.quota {
            ensure_owner_import_quota(tx, check.disk_bytes, check.limits.reserved_bytes, quota)?;
        }
        Ok(None)
    }
}

pub(crate) fn submit(
    storage: &SettingsClient,
    command: Command,
    cancelled: &AtomicBool,
) -> Result<UploadsOutcome> {
    // Admission failures are typed, so callers sort them with
    // `WriterFailure::from` rather than by text.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut queue = storage
        .shared
        .queue
        .lock()
        .map_err(|_| WriterFailure::Stopped)?;
    loop {
        if queue.closed {
            return Err(WriterFailure::Stopped.into());
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(WriterFailure::Cancelled.into());
        }
        if queue.pending.len() < storage.shared.capacity {
            let (reply, receiver) = mpsc::channel();
            queue
                .pending
                .push_back(Envelope::Uploads { command, reply });
            storage.shared.changed.notify_all();
            drop(queue);
            return receiver
                .recv()
                .map_err(|_| anyhow!("Import commit outcome unknown"))?;
        }
        if Instant::now() >= deadline {
            return Err(WriterFailure::QueueFull.into());
        }
        queue = storage
            .shared
            .changed
            .wait_timeout(queue, Duration::from_millis(5))
            .map_err(|_| WriterFailure::Stopped)?
            .0;
    }
}

/// Runs `command` on the writer thread: advances the import accounting epoch
/// when it changed what admission counts, and publishes the import job
/// changes it committed to `/ws/jobs` like a job command's.
pub(crate) fn run(
    connection: &mut Connection,
    catalog_state: &catalog::State,
    command: Command,
    admission: crate::writer_admission::Deadline,
    shared: &crate::Shared,
) -> Result<UploadsOutcome> {
    let changes_accounting = command.changes_accounting();
    let changes_before = connection.total_changes();
    jobs::begin_job_collection();
    let result = execute(connection, catalog_state, command, admission);
    jobs::end_job_collection(
        &shared.job_subscriptions,
        result.is_ok(),
        result.as_ref().err(),
    );
    if result.is_ok() && changes_accounting && connection.total_changes() != changes_before {
        shared.import_epoch.fetch_add(1, Ordering::AcqRel);
    }
    result
}

fn execute(
    connection: &mut Connection,
    catalog_state: &catalog::State,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<UploadsOutcome> {
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    let outcome = match command {
        Command::Get {
            credential,
            policy,
            storage,
            key,
        } => {
            let (tenant, actor) = jobs::actor(&tx, credential, policy, &storage)?;
            let op = load(&tx, &tenant, &key)?.ok_or_else(|| anyhow!("Import not found"))?;
            ensure!(op.actor == actor, "Import belongs to another actor");
            UploadsOutcome::Operation(Box::new(op))
        }
        Command::Admit(command) => {
            UploadsOutcome::Operation(Box::new(admit(&tx, catalog_state, command)?))
        }
        Command::PreflightCapture {
            credential,
            policy,
            storage,
            operation_key,
            mut target,
            payload,
            limits,
            disk_bytes,
            quota,
        } => {
            let (tenant, actor) = capture_actor(&tx, &credential, policy, &storage)?;
            let replay = preflight_capture_target(
                &tx,
                PreflightCaptureCheck {
                    tenant: &tenant,
                    actor: &actor,
                    operation_key: &operation_key,
                    target: &mut target,
                    payload: &payload,
                    limits,
                    quota: Some(quota),
                    disk_bytes,
                },
            )?;
            UploadsOutcome::Preflighted(PreflightedCapture {
                credential,
                tenant,
                actor,
                target,
                replay,
            })
        }
        Command::CorrelateCapture {
            manifest,
            inventory,
        } => UploadsOutcome::CaptureCorrelation(correlate_capture(&tx, manifest, inventory)?),
        Command::Phase {
            lease,
            source_work,
            phase,
            policy,
        } => {
            let mut job = match jobs::claimed_job(&tx, &lease) {
                Ok(job) => job,
                Err(error) if matches!(phase, Phase::Cleanup) => {
                    let Some(job) = jobs::load(&tx, lease.job_id())? else {
                        return Err(error);
                    };
                    ensure!(
                        job.tenant == lease.tenant()
                            && job.generation == lease.generation()
                            && job.state.terminal()
                            && job.fence.is_none()
                            && job.worker.is_none(),
                        "Stale attempt fence"
                    );
                    job
                }
                Err(error) => return Err(error),
            };
            let mut op = bound(&tx, &job)?.ok_or_else(|| anyhow!("Missing import operation"))?;
            let cleanup_retry = job.state.terminal();
            if cleanup_retry {
                ensure!(
                    op.cleanup_settled
                        && repeats_observation(
                            &tx,
                            &op,
                            "cleanup",
                            Observed::fenced(&job, &lease, &op),
                        )?,
                    "Stale attempt fence"
                );
            }
            crate::source_scan::require_live_reservation(
                catalog_state,
                &source_work,
                &catalog::SourceBinding {
                    tenant: job.tenant.clone(),
                    id: op.source_id,
                },
            )?;
            if let Err(error) = jobs::revalidate_source_authority(&tx, &job, policy) {
                let observed = Observed::fenced(&job, &lease, &op);
                if !cleanup_retry && jobs::commit_source_authority_refusal(&tx, &mut job, &error)? {
                    record_observation(&tx, &mut op, "authority_refused", observed)?;
                    store(&tx, &op)?;
                    tx.commit()?;
                    jobs::job_collection_committed();
                }
                return Err(error);
            }
            UploadsOutcome::Operation(Box::new(if cleanup_retry {
                op
            } else {
                advance(&tx, catalog_state, &lease, job, op, phase)?
            }))
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// Loads the journal entry of a supplied import job and checks its binding.
fn bound(tx: &Transaction<'_>, job: &jobs::JobRecord) -> Result<Option<Operation>> {
    let jobs::Payload::SuppliedSourceImport {
        project_id,
        operation_key,
        input_version,
    } = &job.payload
    else {
        return Err(anyhow!("Not a supplied Source import"));
    };
    let Some(op) = load(tx, &job.tenant, operation_key)? else {
        return Ok(None);
    };
    ensure!(
        op.job_id == job.job_id
            && op.source_id == i64::try_from(*project_id)?
            && op.input_version == *input_version,
        "Import job binding mismatch"
    );
    Ok(Some(op))
}

fn advance(
    tx: &Transaction<'_>,
    catalog_state: &catalog::State,
    lease: &jobs::AttemptLease,
    mut job: jobs::JobRecord,
    mut op: Operation,
    phase: Phase,
) -> Result<Operation> {
    ensure!(
        !job.cancel_requested || matches!(phase, Phase::Read | Phase::Fail(_) | Phase::Cleanup),
        "Import cancelled"
    );
    let observation = phase.observation();
    match phase {
        Phase::Read => {}
        Phase::Owned(owned) => {
            ensure!(op.state == State::Admitted, "Input already captured");
            op.owned = Some(owned);
            op.state = State::OwnedInputReady;
        }
        Phase::Published(artifact) => {
            ensure!(op.state == State::OwnedInputReady, "Input not ready");
            op.artifact = Some(artifact);
            op.state = State::Published;
        }
        Phase::Activate => {
            ensure!(op.state == State::Published, "Artifact not published");
            activate(tx, catalog_state, &mut op, lease)?;
        }
        Phase::Fail(reason) => {
            ensure!(op.receipt.is_none(), "Import already completed");
            if matches!(op.state, State::Failed | State::Cancelled)
                && repeats_observation(tx, &op, "failed", Observed::fenced(&job, lease, &op))?
            {
                return Ok(op);
            }
            op.state = if job.cancel_requested {
                State::Cancelled
            } else {
                // Kept for cleanup, which ends the job; a version bump here
                // would end this attempt's lease.
                job.recovery = Some(jobs::bounded_reason(&reason));
                jobs::keep_recovery(tx, &job)?;
                State::Failed
            };
        }
        Phase::Cleanup => {
            job.state = match op.state {
                State::Activated => jobs::PersistentState::Succeeded,
                State::Cancelled => jobs::PersistentState::Cancelled,
                State::Conflict | State::Failed => jobs::PersistentState::Failed,
                State::Admitted | State::OwnedInputReady | State::Published => {
                    return Err(anyhow!("Import is not settled"));
                }
            };
            settle(tx, &mut op)?;
            jobs::save(tx, &mut job, "source_import_settled")?;
        }
    }
    if let Some(name) = observation {
        let observed = Observed::fenced(&job, lease, &op);
        record_observation(tx, &mut op, name, observed)?;
    }
    store(tx, &op)?;
    Ok(op)
}

/// Releases the import's reservation and keeps the bytes of its published
/// artifact.
fn settle(tx: &Transaction<'_>, op: &mut Operation) -> Result<()> {
    op.cleanup_settled = true;
    let retained = op
        .artifact
        .as_ref()
        .map_or(0, |artifact| artifact.stored_bytes);
    tx.execute(
        "UPDATE source_import_quota SET reserved_bytes=0,retained_bytes=?3,settled=1 WHERE tenant=?1 AND operation_key=?2",
        params![op.tenant, op.key, i64::try_from(retained)?],
    )?;
    Ok(())
}

/// Settles the journal of a supplied import whose job ended before its
/// `Cleanup` phase, as a refused authority, a missing Source or the attempt
/// limit leaves it. An unfinished import becomes failed, or cancelled if its
/// owner asked, and releases its reservation. Nothing else runs: the job, the
/// Source and its observations stay as they are. Returns false for a job that
/// is not terminal, holds a fence, or whose import is already settled.
pub(crate) fn settle_terminal(tx: &Transaction<'_>, job: &jobs::JobRecord) -> Result<bool> {
    if job.kind != jobs::JobKind::SuppliedSourceImport
        || !job.state.terminal()
        || job.fence.is_some()
        || job.worker.is_some()
    {
        return Ok(false);
    }
    let Some(mut op) = bound(tx, job)? else {
        return Ok(false);
    };
    if op.cleanup_settled {
        return Ok(false);
    }
    if matches!(
        op.state,
        State::Admitted | State::OwnedInputReady | State::Published
    ) {
        op.state = if job.cancel_requested {
            State::Cancelled
        } else {
            State::Failed
        };
    }
    settle(tx, &mut op)?;
    store(tx, &op)?;
    Ok(true)
}

/// Settles every unsettled import whose job, live or archived, has ended. The
/// storage owner runs this at startup, after every attempt is recovered. A
/// queued import whose Source is gone, which deletion allowed before it waited
/// for imports, fails as its claim would and then settles.
pub(crate) fn settle_ended(tx: &Transaction<'_>) -> Result<()> {
    let rows = tx
        .prepare("SELECT coalesce((SELECT document FROM durable_jobs WHERE id=o.job_id),(SELECT archived_document FROM durable_job_keys WHERE job_id=o.job_id)),EXISTS(SELECT 1 FROM projects WHERE tenant_id=o.tenant AND id=o.source_id) FROM source_import_operations o JOIN source_import_quota q USING(tenant,operation_key) WHERE q.settled=0")?
        .query_map([], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (document, source) in rows {
        let Some(document) = document else {
            continue;
        };
        let mut job = jobs::decode(&document)?;
        if !source && job.state == jobs::PersistentState::Queued {
            jobs::fail_source_unavailable(tx, &mut job)?;
        }
        ensure!(
            settle_terminal(tx, &job)? || source,
            "Unsettled import Source missing"
        );
    }
    Ok(())
}

/// True while an import of the Source holds an unsettled reservation.
pub(crate) fn source_reserved(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_import_operations o JOIN source_import_quota q USING(tenant,operation_key) WHERE o.tenant=?1 AND o.source_id=?2 AND q.settled=0)",
        params![tenant, id],
        |row| row.get(0),
    )?)
}

fn repeats_observation(
    tx: &Transaction<'_>,
    op: &Operation,
    phase: &str,
    observed: Observed,
) -> Result<bool> {
    let Observed::Attempt {
        job_id,
        generation,
        fence: Some(fence),
        receipt,
        cancel,
    } = observed
    else {
        return Ok(false);
    };
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_revision_observations WHERE tenant_id=?1 AND operation_key=?2 AND cursor=?3 AND phase=?4 AND job_id=?5 AND attempt_generation=?6 AND attempt_fence=?7 AND revision_id IS ?8 AND receipt_activated IS ?9 AND cancel_requested=?10)",
        params![
            op.tenant,
            op.key,
            op.observation_cursor,
            phase,
            job_id,
            generation,
            fence,
            receipt.map(|(revision, _)| revision),
            receipt.map(|(_, activated)| activated),
            cancel,
        ],
        |row| row.get(0),
    )?)
}

fn record_observation(
    tx: &Transaction<'_>,
    op: &mut Operation,
    phase: &str,
    observed: Observed,
) -> Result<()> {
    if op.authority_revision.is_none() {
        return Ok(());
    }
    let cursor = op
        .observation_cursor
        .and_then(|cursor| cursor.checked_add(1))
        .ok_or_else(|| anyhow!("Invalid Source observation cursor"))?;
    let now = crate::auth::catalog_timestamp();
    match observed {
        Observed::Preclaim { job_id, cancel } => tx.execute(
            "INSERT INTO source_preclaim_refusals(tenant_id,operation_key,cursor,phase,job_id,admitted_generation,cancel_requested,created_at) VALUES(?1,?2,?3,?4,?5,0,?6,?7)",
            params![op.tenant, op.key, cursor, phase, job_id, cancel, now],
        )?,
        Observed::Attempt {
            job_id,
            generation,
            fence,
            receipt,
            cancel,
        } => tx.execute(
            "INSERT INTO source_revision_observations(tenant_id,operation_key,cursor,phase,job_id,attempt_generation,attempt_fence,revision_id,receipt_activated,cancel_requested,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                op.tenant,
                op.key,
                cursor,
                phase,
                job_id,
                generation,
                fence,
                receipt.map(|(revision, _)| revision),
                receipt.map(|(_, activated)| activated),
                cancel,
                now
            ],
        )?,
    };
    op.observation_cursor = Some(cursor);
    Ok(())
}

/// Records a targeted claim's authority refusal in the import's journal, before
/// the job records its own refusal. A legacy entry has no authority revision and
/// returns before any observation is written. Revisioned entries keep the fence
/// and observation the claim already recorded.
pub(crate) fn record_targeted_authority_refusal(
    tx: &Transaction<'_>,
    job: &jobs::JobRecord,
) -> Result<()> {
    let Some(op) = bound(tx, job)? else {
        return Ok(());
    };
    if op.authority_revision.is_none() {
        return Ok(());
    }
    let mut op = op;
    let observed = if job.generation == 0 {
        let attempted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM source_revision_attempts WHERE tenant_id=?1 AND operation_key=?2)",
            params![op.tenant, op.key],
            |row| row.get(0),
        )?;
        ensure!(
            job.state == jobs::PersistentState::Queued
                && job.attempt == 0
                && job.fence.is_none()
                && job.worker.is_none()
                && job.effects.is_empty()
                && job.result.is_none()
                && op.state == State::Admitted
                && op.observation_cursor == Some(0)
                && !attempted,
            "Invalid preclaim Source refusal context"
        );
        Observed::Preclaim {
            job_id: job.job_id.clone(),
            cancel: job.cancel_requested,
        }
    } else {
        Observed::Attempt {
            job_id: job.job_id.clone(),
            generation: job.generation,
            fence: None,
            receipt: None,
            cancel: job.cancel_requested,
        }
    };
    record_observation(tx, &mut op, "authority_refused", observed)?;
    store(tx, &op)
}

/// Records a generic claim's authority refusal for a recovered supplied import,
/// before the job records its own. The observation keeps the recovered
/// generation and no fence, as a targeted claim does after an earlier attempt.
/// Generation 0 is still admitted-only and is left to the job journal. A later
/// claim finds the job terminal and writes no second observation.
pub(crate) fn record_recovered_authority_refusal(
    tx: &Transaction<'_>,
    job: &jobs::JobRecord,
) -> Result<()> {
    if job.generation == 0 {
        return Ok(());
    }
    let Some(mut op) = bound(tx, job)? else {
        return Ok(());
    };
    record_observation(
        tx,
        &mut op,
        "authority_refused",
        Observed::Attempt {
            job_id: job.job_id.clone(),
            generation: job.generation,
            fence: None,
            receipt: None,
            cancel: job.cancel_requested,
        },
    )?;
    store(tx, &op)
}

/// True for a supplied import whose input only its admitting owner holds, so
/// only that owner's targeted claim may run it.
pub(crate) fn awaits_owner_input(tx: &Transaction<'_>, job: &jobs::JobRecord) -> Result<bool> {
    Ok(job.kind == jobs::JobKind::SuppliedSourceImport
        && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM source_import_operations WHERE tenant=?1 AND job_id=?2 AND state='admitted')",
            params![job.tenant, job.job_id],
            |row| row.get(0),
        )?)
}

/// Registers the published artifact as a complete Source revision and points the
/// Source at it if the Source still matches the admission basis. Otherwise the
/// revision stays inactive and the import ends in conflict. A complete revision
/// with the same manifest and locator is reused and keeps its first provenance.
/// Each import records its own config, observation, input, and producer binding
/// on `source_revision_attempts`. An existing revision that is not complete
/// returns [`IncompleteRevisionKey`] and leaves that revision and its attempt.
fn activate(
    tx: &Transaction<'_>,
    catalog_state: &catalog::State,
    op: &mut Operation,
    lease: &jobs::AttemptLease,
) -> Result<()> {
    let revision = op
        .authority_revision
        .clone()
        .ok_or_else(|| anyhow!("Legacy Source operation requires repair"))?;
    let current: i64 = tx.query_row(
        "SELECT source_configuration_version FROM projects WHERE tenant_id=?1 AND id=?2",
        params![op.tenant, op.source_id],
        |row| row.get(0),
    )?;
    ensure!(
        current == revision.source_configuration_version,
        "Source configuration changed after admission"
    );
    let artifact = op
        .artifact
        .clone()
        .ok_or_else(|| anyhow!("Missing artifact"))?;
    let now = crate::auth::catalog_timestamp();
    let existing = tx
        .query_row(
            "SELECT id,completeness,manifest_digest,snapshot_locator,synced_at FROM source_revisions WHERE tenant_id=?1 AND project_id=?2 AND upstream_revision_key=?3",
            params![op.tenant, op.source_id, artifact.upstream_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let (id, synced_at) = match existing {
        Some((id, completeness, manifest_digest, locator, synced_at)) => {
            if completeness != "complete" {
                return Err(IncompleteRevisionKey.into());
            }
            ensure!(
                manifest_digest == artifact.manifest_digest && locator == artifact.locator,
                "Accepted revision identity mismatch"
            );
            (id, synced_at)
        }
        None => {
            tx.execute(
                "INSERT INTO source_revisions(tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at,completeness,source_configuration_version,activation_observation_digest,input_digest,producer_version) VALUES(?1,?2,?3,?4,?5,?6,'complete',?7,?8,?9,?10)",
                params![
                    op.tenant,
                    op.source_id,
                    artifact.upstream_key,
                    artifact.manifest_digest,
                    artifact.locator,
                    now,
                    revision.source_configuration_version,
                    revision.activation_observation_digest,
                    revision.input_digest,
                    revision.producer_version
                ],
            )?;
            (tx.last_insert_rowid(), now.clone())
        }
    };
    tx.execute(
        "INSERT INTO source_revision_attempts(tenant_id,operation_key,job_id,source_id,revision_id,source_configuration_version,activation_observation_digest,input_digest,producer_version,attempt_generation,attempt_fence,activated,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,?12)",
        params![
            op.tenant,
            op.key,
            op.job_id,
            op.source_id,
            id,
            revision.source_configuration_version,
            revision.activation_observation_digest,
            revision.input_digest,
            revision.producer_version,
            lease.generation(),
            lease.fence(),
            now
        ],
    )?;
    tx.execute(
        "INSERT INTO source_revision_artifacts(revision_id,artifact_kind,input_digest,producer_version,artifact_digest) VALUES(?1,'snapshot_manifest',?2,?3,?4) ON CONFLICT DO NOTHING",
        params![id, revision.input_digest, revision.producer_version, artifact.manifest_digest],
    )?;
    let basis = &op.basis;
    let local_path = pp_os::display_path(
        &catalog_state
            .directory
            .join("repos")
            .join(&artifact.locator),
    )
    .to_string_lossy()
    .into_owned();
    let activated = tx.execute(
        "UPDATE projects SET current_source_revision_id=?3,local_path=?4,last_commit_sha=?5,last_synced_at=?6 WHERE tenant_id=?1 AND id=?2 AND current_source_revision_id IS ?7 AND url=?8 AND branch=?9 AND tag IS ?10 AND source_kind=?11 AND source_type=?12 AND local_path IS ?13 AND last_commit_sha IS ?14 AND legacy_manifest_cutover=?15",
        params![
            op.tenant,
            op.source_id,
            id,
            local_path,
            artifact.upstream_key,
            synced_at,
            basis.current_source_revision_id,
            basis.url,
            basis.branch,
            basis.tag,
            basis.source_kind,
            basis.source_type,
            basis.local_path,
            basis.last_commit_sha,
            basis.legacy_manifest_cutover
        ],
    )? == 1;
    let postprocessing = if activated {
        tx.execute(
            "UPDATE source_revision_attempts SET activated=1 WHERE tenant_id=?1 AND operation_key=?2",
            params![op.tenant, op.key],
        )?;
        apply_activation(tx, catalog_state, op, &artifact, &revision, id, &now)?
    } else {
        Postprocessing::NotActivated
    };
    op.state = if activated {
        State::Activated
    } else {
        State::Conflict
    };
    op.receipt = Some(Receipt {
        tenant: op.tenant.clone(),
        operation_key: op.key.clone(),
        job_id: op.job_id.clone(),
        source_id: op.source_id,
        revision_id: id,
        artifact,
        basis: op.basis.clone(),
        activated,
        applied_at: now,
        postprocessing,
        authority_revision: Some(revision),
    });
    Ok(())
}

/// Marks the activated Source up to date, applies the suggested import rules
/// if the Source still has its default rules, and replaces its document index.
/// A failed index keeps the activation and reports `IndexError`.
fn apply_activation(
    tx: &Transaction<'_>,
    catalog_state: &catalog::State,
    op: &Operation,
    artifact: &Artifact,
    revision: &SourceAuthorityRevision,
    revision_id: i64,
    now: &str,
) -> Result<Postprocessing> {
    let (metadata, rules): (Option<String>, Option<String>) = tx.query_row(
        "SELECT metadata_json,imported_paths FROM projects WHERE tenant_id=?1 AND id=?2",
        params![op.tenant, op.source_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut metadata = metadata
        .and_then(|metadata| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&metadata).ok()
        })
        .unwrap_or_default();
    metadata.insert("remote_update_status".into(), "up_to_date".into());
    metadata.insert("remote_checked_at".into(), now.into());
    metadata.remove("sync_error");
    metadata.remove("sync_required");
    tx.execute(
        "UPDATE projects SET metadata_json=?3 WHERE tenant_id=?1 AND id=?2",
        params![op.tenant, op.source_id, serde_json::to_string(&metadata)?],
    )?;
    if op.default_rules && rules == op.original_rules && !artifact.suggested_rules.is_empty() {
        catalog::run(
            tx,
            catalog_state,
            &op.tenant,
            catalog::Request::SaveImportRules {
                id: op.source_id,
                rules: artifact.suggested_rules.clone(),
            },
        )?;
    }
    tx.execute_batch("SAVEPOINT import_docs")?;
    let indexed = (|| -> Result<()> {
        tx.execute(
            "DELETE FROM source_docs WHERE tenant_id=?1 AND project_id=?2",
            params![op.tenant, op.source_id],
        )?;
        let collator = icu_collator::Collator::try_new(Default::default(), Default::default())
            .map_err(|error| anyhow!("{error}"))?;
        let mut ordered: Vec<&File> = artifact.files.iter().collect();
        ordered.sort_by(|a, b| {
            (a.kind != "readme")
                .cmp(&(b.kind != "readme"))
                .then_with(|| collator.compare(&a.path, &b.path))
        });
        for file in ordered {
            if ["readme", "md", "pdf"].contains(&file.kind.as_str()) {
                tx.execute(
                    "INSERT INTO source_docs(tenant_id,project_id,path,kind,size_bytes,content_hash,extract_status,source_revision_id,input_digest,producer_version,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        op.tenant,
                        op.source_id,
                        file.path,
                        file.kind,
                        i64::try_from(file.size)?,
                        &file.sha256[..24],
                        if file.kind == "pdf" { "pending" } else { "na" },
                        revision_id,
                        revision.input_digest,
                        revision.producer_version,
                        now
                    ],
                )?;
            }
        }
        Ok(())
    })();
    if indexed.is_err() {
        tx.execute_batch("ROLLBACK TO import_docs; RELEASE import_docs")?;
        return Ok(Postprocessing::IndexError);
    }
    tx.execute_batch("RELEASE import_docs")?;
    Ok(if artifact.files.iter().any(|file| file.kind == "pdf") {
        Postprocessing::DocumentMetadataIndexedPdfPending
    } else {
        Postprocessing::DocumentMetadataIndexed
    })
}

fn admit(
    tx: &Transaction<'_>,
    catalog_state: &catalog::State,
    command: Admit,
) -> Result<Operation> {
    let Admit {
        authority,
        policy,
        storage,
        mut request,
        mut expected_files,
        disk_bytes,
        accounting_epoch,
        quota,
    } = command;
    ensure!(
        storage.import_epoch.load(Ordering::Acquire) == accounting_epoch,
        "Import accounting changed; retry admission"
    );
    let (tenant, actor, durable_authority, prepared_capture) = match authority {
        AdmissionAuthority::Credential(credential) => {
            let (tenant, actor, durable_authority) =
                crate::auth::authority::admit(tx, credential, policy, &storage)?;
            (tenant, actor, durable_authority, false)
        }
        AdmissionAuthority::Preflighted(preflight) => {
            let (tenant, actor, durable_authority) =
                crate::auth::authority::admit(tx, preflight.credential, policy, &storage)?;
            ensure!(
                tenant == preflight.tenant && actor == preflight.actor,
                "Capture authority changed"
            );
            (tenant, actor, durable_authority, true)
        }
    };
    ensure!(
        !request.key.is_empty()
            && request.key.len() <= 128
            && !request.key.chars().any(char::is_control),
        "Invalid operation key"
    );
    let canonical = |path: &str| {
        path.replace('\\', "/")
            .trim_start_matches('/')
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>()
            .join("/")
    };
    ensure!(
        !expected_files.is_empty() && expected_files.len() <= 10000,
        "Invalid files"
    );
    let mut folded = HashSet::new();
    for file in &mut expected_files {
        let normalized = canonical(&file.path);
        path(&normalized)?;
        digest(&file.sha256)?;
        ensure!(
            folded.insert(normalized.to_lowercase()),
            "Duplicate uploaded file path: {}",
            file.path
        );
        file.path = normalized;
    }
    match &mut request.input {
        Input::Files { paths } => paths.iter_mut().for_each(|path| *path = canonical(path)),
        Input::Zip { path } => *path = canonical(path),
        Input::Captured(_) if !prepared_capture => {
            return Err(anyhow!("Captured imports require prepared admission"));
        }
        Input::Captured(_) => {}
    }
    if let Input::Captured(captured) = &request.input {
        ensure!(
            request.input.requested_paths()
                == expected_files
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect::<Vec<_>>()
                && expected_files.iter().all(|file| file.kind == "input"),
            "Capture inventory mismatch"
        );
        captured.validate(
            &actor,
            &request.key,
            Some(&request.target),
            AdmissionLimits {
                reserved_bytes: request.reserved_bytes,
                max_input_bytes: request.max_input_bytes,
                max_prepared_bytes: request.max_prepared_bytes,
            },
            &expected_files,
        )?;
    }
    files(&expected_files, request.max_input_bytes)?;
    let requested_digest = serialized_digest(&expected_files)?;
    let intent_digest = serialized_digest(&(&request, &expected_files))?;
    if let Some(prior) = load(tx, &tenant, &request.key)? {
        ensure!(
            prior.actor == actor && prior.intent_digest == intent_digest,
            "Import idempotency conflict"
        );
        return Ok(prior);
    }
    match &mut request.target {
        Target::Existing { source_id } => {
            catalog::validate_capture_existing(tx, &tenant, *source_id)?
        }
        Target::Create { metadata } => catalog::validate_capture_create(tx, &tenant, metadata)?,
    }
    validate_limits(&request, quota)?;
    ensure_owner_import_quota(tx, disk_bytes, request.reserved_bytes, quota)?;
    let input_version = request.input.version();
    let source_id = match request.target {
        Target::Existing { source_id } => source_id,
        Target::Create { metadata } => match catalog::run(
            tx,
            catalog_state,
            &tenant,
            catalog::Request::CreateCatalogSource { source: *metadata },
        )? {
            catalog::Outcome::Source(Some(source)) => source.id,
            _ => return Err(anyhow!("Supplied Source was not created")),
        },
    };
    let basis = basis(tx, &tenant, source_id)?;
    let (source_configuration_version, original_rules): (i64, Option<String>) = tx.query_row(
        "SELECT source_configuration_version,imported_paths FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, source_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        source_configuration_version > 0,
        "Invalid Source configuration version"
    );
    let authority_revision = SourceAuthorityRevision {
        version: 1,
        source_configuration_version,
        activation_observation_digest: serialized_digest(&basis)?,
        input_digest: requested_digest.clone(),
        producer_version: PRODUCER.into(),
    };
    let jobs::Outcome::Job(job, _) = jobs::user(
        tx,
        &tenant,
        &actor,
        durable_authority,
        jobs::UserOperation::Enqueue {
            key: format!(
                "source-import:{}",
                hex::encode(Sha256::digest(request.key.as_bytes()))
            ),
            payload_version: 1,
            payload: jobs::Payload::SuppliedSourceImport {
                project_id: source_id.try_into()?,
                operation_key: request.key.clone(),
                input_version,
            },
        },
    )?
    else {
        return Err(anyhow!("Supplied import was not enqueued"));
    };
    let op = Operation {
        tenant,
        actor,
        key: request.key,
        intent_digest,
        source_id,
        job_id: job.job_id,
        input_version,
        input: request.input,
        requested_files: expected_files,
        requested_digest,
        state: State::Admitted,
        basis,
        default_rules: original_rules.is_none(),
        original_rules,
        reserved_bytes: request.reserved_bytes,
        max_input_bytes: request.max_input_bytes,
        max_prepared_bytes: request.max_prepared_bytes,
        owned: None,
        artifact: None,
        receipt: None,
        cleanup_settled: false,
        authority_revision: Some(authority_revision),
        observation_cursor: Some(0),
    };
    validate_operation(&op)?;
    tx.execute(
        "INSERT INTO source_import_operations(tenant,operation_key,actor,intent_digest,source_id,job_id,state,document_version,document) VALUES(?1,?2,?3,?4,?5,?6,'admitted',1,?7)",
        params![op.tenant, op.key, op.actor, op.intent_digest, op.source_id, op.job_id, serde_json::to_string(&op)?],
    )?;
    tx.execute(
        "INSERT INTO source_import_quota VALUES(?1,?2,?3,0,0)",
        params![op.tenant, op.key, i64::try_from(op.reserved_bytes)?],
    )?;
    Ok(op)
}

const PRODUCER: &str = "supplied-source-import-v1";

fn validate_limits(request: &Admission, quota: u64) -> Result<()> {
    ensure!(
        (1..=256 * 1024 * 1024).contains(&request.max_input_bytes)
            && (1..=1024 * 1024 * 1024).contains(&request.max_prepared_bytes),
        "Invalid import limits"
    );
    let minimum = request
        .max_input_bytes
        .checked_add(3 * request.max_prepared_bytes)
        .and_then(|minimum| minimum.checked_add(16 * 1024 * 1024))
        .ok_or_else(|| anyhow!("Quota overflow"))?;
    ensure!(
        (minimum..=quota).contains(&request.reserved_bytes),
        "Import quota exceeded"
    );
    Ok(())
}

fn serialized_digest(value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

fn store(tx: &Transaction<'_>, op: &Operation) -> Result<()> {
    validate_operation(op)?;
    tx.execute(
        "UPDATE source_import_operations SET state=json_extract(?3,'$.state'),document=?3 WHERE tenant=?1 AND operation_key=?2",
        params![op.tenant, op.key, serde_json::to_string(op)?],
    )?;
    Ok(())
}

fn load(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Option<Operation>> {
    tx.query_row(
        "SELECT document FROM source_import_operations WHERE tenant=?1 AND operation_key=?2",
        params![tenant, key],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|document| {
        let op: Operation = serde_json::from_str(&document)?;
        validate_operation(&op)?;
        Ok(op)
    })
    .transpose()
}

pub(crate) fn correlate_capture(
    tx: &Transaction<'_>,
    manifest: Vec<u8>,
    inventory: Vec<File>,
) -> Result<CaptureJournalCorrelation> {
    let identity = capture_manifest::capture_manifest_identity(&manifest)?;
    let documents = tx
        .prepare("SELECT document FROM source_import_operations ORDER BY rowid")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for document in documents {
        let operation: Operation = serde_json::from_str(&document)?;
        validate_operation(&operation)?;
        let Input::Captured(captured) = &operation.input else {
            continue;
        };
        if captured.capture_id() != &identity.capture_id {
            continue;
        }
        if operation.key != identity.operation_key || operation.actor != identity.actor {
            return Ok(CaptureJournalCorrelation::MismatchRepairRequired);
        }
        if capture_manifest::capture_claim_matches(&manifest, &operation, &inventory).is_err() {
            return Ok(CaptureJournalCorrelation::MismatchRepairRequired);
        }
        if operation.state == State::Admitted {
            return Ok(CaptureJournalCorrelation::ExactAdmitted(
                ResolvedCapturedClaim {
                    manifest,
                    inventory,
                    tenant: operation.tenant,
                    operation_key: operation.key,
                    job_id: operation.job_id,
                },
            ));
        }
        return Ok(CaptureJournalCorrelation::ExactOwnedOrLater);
    }
    // Absent invites a fresh admission, so the retained inventory must still
    // match the verified manifest.
    if identity.requested_files != inventory {
        return Ok(CaptureJournalCorrelation::MismatchRepairRequired);
    }

    let jobs = tx
        .prepare("SELECT document FROM durable_jobs ORDER BY rowid")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for document in jobs {
        let job = jobs::decode(&document)?;
        if matches!(
            &job.payload,
            jobs::Payload::SuppliedSourceImport {
                operation_key,
                input_version: 2,
                ..
            } if operation_key == &identity.operation_key
        ) {
            return Ok(CaptureJournalCorrelation::MismatchRepairRequired);
        }
    }
    Ok(CaptureJournalCorrelation::Absent)
}

pub(crate) fn validate_resolved_claim(
    tx: &Transaction<'_>,
    claim: &ResolvedCapturedClaim,
) -> Result<String> {
    (|| {
        let operation =
            load(tx, &claim.tenant, &claim.operation_key)?.ok_or_else(|| anyhow!("missing"))?;
        ensure!(
            operation.state == State::Admitted && operation.job_id == claim.job_id,
            "mismatch"
        );
        capture_manifest::capture_claim_matches(&claim.manifest, &operation, &claim.inventory)?;
        let job = jobs::load_for_capture_claim(tx, &claim.job_id)?;
        ensure!(
            job.tenant == operation.tenant
                && job.job_id == operation.job_id
                && matches!(
                    &job.payload,
                    jobs::Payload::SuppliedSourceImport {
                        project_id,
                        operation_key,
                        input_version: 2,
                    } if u64::try_from(operation.source_id).is_ok_and(|id| id == *project_id)
                        && operation_key == &operation.key
                ),
            "mismatch"
        );
        ensure!(
            catalog::get(tx, &operation.tenant, operation.source_id)?.is_some(),
            "mismatch"
        );
        Ok(claim.job_id.clone())
    })()
    .map_err(|_| anyhow!("Captured import requires repair"))
}

fn basis(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<Basis> {
    Ok(tx.query_row(
        "SELECT current_source_revision_id,url,branch,tag,source_kind,source_type,local_path,last_commit_sha,legacy_manifest_cutover FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, id],
        |row| {
            Ok(Basis {
                current_source_revision_id: row.get(0)?,
                url: row.get(1)?,
                branch: row.get(2)?,
                tag: row.get(3)?,
                source_kind: row.get(4)?,
                source_type: row.get(5)?,
                local_path: row.get(6)?,
                last_commit_sha: row.get(7)?,
                legacy_manifest_cutover: row.get(8)?,
            })
        },
    )?)
}

fn digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Invalid digest"
    );
    Ok(())
}

fn path(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 4096
            && !value.contains(['\\', ':', '\0'])
            && !value.starts_with('/')
            && value
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "Invalid relative path"
    );
    Ok(())
}

fn files(files: &[File], max: u64) -> Result<()> {
    ensure!(!files.is_empty() && files.len() <= 10000, "Invalid files");
    let mut names = HashSet::new();
    let mut total = 0u64;
    for file in files {
        path(&file.path)?;
        digest(&file.sha256)?;
        ensure!(names.insert(&file.path), "Duplicate file");
        total = total
            .checked_add(file.size)
            .ok_or_else(|| anyhow!("Input size overflow"))?;
    }
    ensure!(total <= max, "Input limit");
    Ok(())
}

fn validate_operation(op: &Operation) -> Result<()> {
    ensure!(
        op.source_id > 0 && !op.key.is_empty() && op.key.len() <= 128 && !op.actor.is_empty(),
        "Invalid import identity"
    );
    digest(&op.job_id)?;
    digest(&op.intent_digest)?;
    digest(&op.requested_digest)?;
    if let Some(revision) = &op.authority_revision {
        ensure!(
            revision.version == 1
                && revision.source_configuration_version > 0
                && revision.producer_version == PRODUCER
                && revision.input_digest == op.requested_digest
                && revision.activation_observation_digest == serialized_digest(&op.basis)?,
            "Invalid Source authority revision"
        );
        ensure!(
            op.observation_cursor.is_some_and(|cursor| cursor >= 0),
            "Invalid U10 cursor"
        );
    } else {
        ensure!(
            op.observation_cursor.is_none(),
            "Legacy Source cursor without authority revision"
        );
    }
    files(&op.requested_files, op.max_input_bytes)?;
    ensure!(
        serialized_digest(&op.requested_files)? == op.requested_digest,
        "Input digest mismatch"
    );
    match &op.input {
        Input::Files { .. } | Input::Zip { .. } => {
            ensure!(op.input_version == 1, "Invalid import input version");
        }
        Input::Captured(captured) => {
            ensure!(op.input_version == 2, "Invalid import input version");
            captured.validate(
                &op.actor,
                &op.key,
                None,
                AdmissionLimits {
                    reserved_bytes: op.reserved_bytes,
                    max_input_bytes: op.max_input_bytes,
                    max_prepared_bytes: op.max_prepared_bytes,
                },
                &op.requested_files,
            )?;
        }
    }
    ensure!(
        op.input.requested_paths()
            == op
                .requested_files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
            && op.requested_files.iter().all(|file| file.kind == "input"),
        "Input binding mismatch"
    );
    if let Some(owned) = &op.owned {
        ensure!(
            owned.locator == format!(".pp-imports/{}", op.job_id)
                && owned.digest == op.requested_digest
                && owned.files == op.requested_files,
            "Owned input binding mismatch"
        );
    }
    if let Some(artifact) = &op.artifact {
        digest(&artifact.upstream_key)?;
        digest(&artifact.manifest_digest)?;
        files(&artifact.files, op.max_prepared_bytes)?;
        ensure!(
            artifact.tenant == op.tenant
                && artifact.source_id == op.source_id
                && artifact.locator
                    == format!("{}/revisions/{}", op.source_id, artifact.upstream_key)
                && artifact.stored_bytes <= op.reserved_bytes
                && artifact.files.iter().all(|file| {
                    ["stl", "artifact", "readme", "md", "pdf"].contains(&file.kind.as_str())
                }),
            "Artifact binding mismatch"
        );
    }
    if let Some(receipt) = &op.receipt {
        ensure!(
            receipt.tenant == op.tenant
                && receipt.source_id == op.source_id
                && receipt.operation_key == op.key
                && receipt.job_id == op.job_id
                && receipt.revision_id > 0
                && receipt.basis == op.basis
                && Some(&receipt.artifact) == op.artifact.as_ref()
                && receipt.activated == (op.state == State::Activated),
            "Receipt binding mismatch"
        );
        ensure!(
            receipt.authority_revision == op.authority_revision,
            "Receipt authority revision mismatch"
        );
        let pdf = receipt.artifact.files.iter().any(|file| file.kind == "pdf");
        ensure!(
            match receipt.postprocessing {
                Postprocessing::NotActivated => !receipt.activated && op.state == State::Conflict,
                Postprocessing::DocumentMetadataIndexed => receipt.activated && !pdf,
                Postprocessing::DocumentMetadataIndexedPdfPending => receipt.activated && pdf,
                Postprocessing::IndexError => receipt.activated,
            },
            "Receipt postprocessing mismatch"
        );
    }
    let (owned, artifact, receipt) = (
        op.owned.is_some(),
        op.artifact.is_some(),
        op.receipt.is_some(),
    );
    ensure!(
        match op.state {
            State::Admitted => !owned && !artifact && !receipt,
            State::OwnedInputReady => owned && !artifact && !receipt,
            State::Published => owned && artifact && !receipt,
            State::Activated | State::Conflict => owned && artifact && receipt,
            State::Failed | State::Cancelled => !receipt,
        },
        "Invalid import state"
    );
    ensure!(
        !op.cleanup_settled
            || matches!(
                op.state,
                State::Activated | State::Conflict | State::Failed | State::Cancelled
            ),
        "Unsettled import cleanup"
    );
    Ok(())
}
