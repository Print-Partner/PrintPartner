use crate::{
    catalog::{IssuedTokenState, SourceBinding, SourceWorkProof, State},
    jobs::{
        AttemptLease, CompletedResult, EffectIntent, EffectOperation, EffectReceipt, JobRecord,
        Payload, PersistentState, ResultArtifact, SourceScanResult,
    },
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

const PRODUCER_VERSION: &str = "local-import-scan-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalDocumentKind {
    Readme,
    Markdown,
    Pdf,
}

impl LocalDocumentKind {
    fn stored(&self) -> &'static str {
        match self {
            Self::Readme => "readme",
            Self::Markdown => "md",
            Self::Pdf => "pdf",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalDocumentRecord {
    pub relative_path: String,
    pub kind: LocalDocumentKind,
    pub size_bytes: u64,
    pub content_sha256: String,
}

#[derive(Clone, Debug)]
pub struct LocalScanObservation {
    source_id: i64,
    local_path: Option<String>,
    pub(crate) configuration_version: i64,
    pub(crate) authority_actor: String,
    pub(crate) authority_basis_digest: String,
    pub(crate) activation_observation_digest: String,
    pub(crate) path_observation_digest: String,
}

impl LocalScanObservation {
    pub fn source_id(&self) -> i64 {
        self.source_id
    }

    pub fn local_path(&self) -> Option<&str> {
        self.local_path.as_deref()
    }

    pub fn settlement(self, documents: Vec<LocalDocumentRecord>) -> LocalScanSettlement {
        LocalScanSettlement {
            observation: self,
            documents,
        }
    }
}

pub struct LocalScanSettlement {
    observation: LocalScanObservation,
    documents: Vec<LocalDocumentRecord>,
}

#[derive(Clone, Debug)]
pub struct LocalScanApplied {
    source_id: i64,
    receipt: ResultArtifact,
    inventory_digest: String,
    index_digest: String,
    doc_count: u64,
}

impl LocalScanApplied {
    pub fn local_source_scan_result(&self) -> Result<SourceScanResult> {
        Ok(SourceScanResult {
            project_id: Some(u64::try_from(self.source_id)?),
            stl_count: 0,
            downloaded: 0,
            doc_count: self.doc_count,
            docs_downloaded: 0,
            pdf_extract_job_id: None,
            postprocess_warning: None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalScanFailure {
    AlreadyObserved,
    NotSettled,
}

impl std::fmt::Display for LocalScanFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyObserved => formatter.write_str("Local scan already observed"),
            Self::NotSettled => formatter.write_str("Local scan is not settled"),
        }
    }
}

impl std::error::Error for LocalScanFailure {}

pub struct CompletedLocalScan {
    pub job: JobRecord,
    pub result_digest: String,
}

pub enum LocalCompletionInspection {
    NotCommitted,
    Completed(CompletedLocalScan),
    CommittedReservationConflict(CompletedLocalScan),
    CommittedReservationCorrupt(CompletedLocalScan),
}

pub enum HeldLocalScanInspection {
    NotApplied(JobRecord),
    Applied(JobRecord, LocalScanApplied),
}

pub(crate) struct PendingLocalCompletion {
    pub completion: CompletedLocalScan,
    pub proof: SourceWorkProof,
    pub binding: SourceBinding,
}

pub(crate) struct PendingLocalReconciliation {
    pub job: JobRecord,
    pub proof: SourceWorkProof,
    pub binding: SourceBinding,
}

struct CompletedJournal {
    source_id: i64,
    reservation_incarnation: String,
    reservation_token: u64,
    inventory_digest: String,
    index_digest: String,
    receipt: ResultArtifact,
    result_digest: String,
}

fn digest_bytes(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains(['\\', '\0', ':'])
        && !value.starts_with('/')
        && value.split('/').count() <= 64
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn token_text(token: u64) -> Result<String> {
    ensure!(token != 0, "Invalid Source reservation token");
    Ok(format!("{token:016x}"))
}

fn token_value(value: &str) -> Result<u64> {
    ensure!(
        value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && value != "0000000000000000",
        "Invalid stored Source reservation token"
    );
    Ok(u64::from_str_radix(value, 16)?)
}

fn scan_source(job: &JobRecord) -> Result<i64> {
    let Payload::ImportScan { project_id } = job.payload else {
        return Err(anyhow!("Attempt is not an ImportScan"));
    };
    i64::try_from(project_id).map_err(Into::into)
}

fn expected_binding(job: &JobRecord) -> Result<SourceBinding> {
    Ok(SourceBinding {
        tenant: job.tenant.clone(),
        id: scan_source(job)?,
    })
}

pub(crate) fn require_live_reservation(
    state: &State,
    proof: &SourceWorkProof,
    expected: &SourceBinding,
) -> Result<()> {
    match state.classify_issued(&proof.incarnation, proof.token) {
        IssuedTokenState::Active(actual) => ensure!(actual == expected, "Wrong Source reservation"),
        IssuedTokenState::Retired => return Err(anyhow!("Source reservation already retired")),
        IssuedTokenState::NeverIssued => return Err(anyhow!("Never-issued Source reservation")),
        IssuedTokenState::WrongIncarnation => {
            return Err(anyhow!("Wrong Source writer incarnation"));
        }
    }
    Ok(())
}

pub(crate) fn project_observation(
    tx: &Transaction<'_>,
    tenant: &str,
    source_id: i64,
    actor: &str,
) -> Result<LocalScanObservation> {
    let (source_type, local_path, configuration_version, source_kind, current_revision): (
        String,
        Option<String>,
        i64,
        String,
        Option<i64>,
    ) = tx.query_row(
        "SELECT source_type,local_path,source_configuration_version,source_kind,current_source_revision_id FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, source_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    ensure!(source_type == "local", "ImportScan requires a Local Source");
    ensure!(
        configuration_version > 0,
        "Invalid Source configuration version"
    );
    let authority_basis_digest = digest_bytes(serde_json::to_vec(&(tenant, actor))?);
    let activation_observation_digest = digest_bytes(serde_json::to_vec(&(
        source_id,
        configuration_version,
        source_type,
        source_kind,
        current_revision,
    ))?);
    let path_observation_digest = digest_bytes(serde_json::to_vec(&local_path)?);
    Ok(LocalScanObservation {
        source_id,
        local_path,
        configuration_version,
        authority_actor: actor.to_owned(),
        authority_basis_digest,
        activation_observation_digest,
        path_observation_digest,
    })
}

/// The claim observation for a GitHub Source. It hashes the Source's identity
/// and leaves out the revision, local path and commit that activation writes,
/// so a settlement after publication still matches the claim.
pub(crate) fn github_observation(
    tx: &Transaction<'_>,
    tenant: &str,
    source_id: i64,
    actor: &str,
) -> Result<LocalScanObservation> {
    let (source_kind, source_type, url, branch, tag, local_path, configuration_version): (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
    ) = tx.query_row(
        "SELECT source_kind,source_type,url,branch,tag,local_path,source_configuration_version FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, source_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    ensure!(
        source_kind == "github",
        "GitHub sync requires a GitHub Source"
    );
    ensure!(
        configuration_version > 0,
        "Invalid Source configuration version"
    );
    let authority_basis_digest = digest_bytes(serde_json::to_vec(&(tenant, actor))?);
    let identity = digest_bytes(serde_json::to_vec(&(
        source_id,
        configuration_version,
        source_kind,
        source_type,
        url,
        branch,
        tag,
    ))?);
    Ok(LocalScanObservation {
        source_id,
        local_path,
        configuration_version,
        authority_actor: actor.to_owned(),
        authority_basis_digest,
        activation_observation_digest: identity.clone(),
        path_observation_digest: identity,
    })
}

fn mark_source_synced(
    tx: &Transaction<'_>,
    tenant: &str,
    source_id: i64,
    configuration_version: i64,
    local_path: Option<&str>,
    synced_at: &str,
) -> Result<()> {
    let raw: Option<String> = tx.query_row(
        "SELECT metadata_json FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, source_id],
        |row| row.get(0),
    )?;
    let metadata = raw
        .map(|raw| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw))
        .transpose()?
        .map(|mut metadata| {
            metadata.remove("sync_error");
            metadata.remove("sync_required");
            serde_json::to_string(&metadata)
        })
        .transpose()?;
    ensure!(
        tx.execute(
            "UPDATE projects SET last_synced_at=?3,metadata_json=?4 WHERE tenant_id=?1 AND id=?2 AND source_configuration_version=?5 AND local_path IS ?6",
            params![tenant, source_id, synced_at, metadata, configuration_version, local_path],
        )? == 1,
        "Local Source changed while marking it synced"
    );
    Ok(())
}

pub(crate) fn mark_local_failure(
    tx: &Transaction<'_>,
    tenant: &str,
    source_id: i64,
    detail: &str,
) -> Result<()> {
    let raw: Option<String> = tx.query_row(
        "SELECT metadata_json FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, source_id],
        |row| row.get(0),
    )?;
    ensure!(
        tx.execute(
            "UPDATE projects SET metadata_json=?3 WHERE tenant_id=?1 AND id=?2",
            params![
                tenant,
                source_id,
                node_json::failure_metadata(raw.as_deref(), detail)
            ],
        )? == 1,
        "Local Source disappeared while recording failure"
    );
    Ok(())
}

pub(crate) fn observe(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    proof: &SourceWorkProof,
    policy: crate::auth::AuthPolicy,
) -> Result<LocalScanObservation> {
    let job = crate::jobs::claimed_job(tx, lease)?;
    ensure!(!job.cancel_requested, "ImportScan was cancelled");
    let authority = crate::jobs::require_original_authority(tx, &job, policy)?;
    let binding = expected_binding(&job)?;
    require_live_reservation(state, proof, &binding)?;
    let observation = project_observation(tx, &job.tenant, binding.id, authority.subject())?;
    let already_observed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_scan_executions WHERE (job_id=?1 AND generation=?2) OR (reservation_incarnation=?3 AND reservation_token=?4))",
        params![job.job_id, job.generation, proof.incarnation, token_text(proof.token)?], |row| row.get(0),
    )?;
    ensure!(!already_observed, LocalScanFailure::AlreadyObserved);
    let fence_digest = digest_bytes(lease.fence());
    tx.execute(
        "INSERT INTO source_scan_executions(job_id,generation,tenant,attempt_worker,attempt_fence_digest,source_id,reservation_incarnation,reservation_token,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest,producer_version,phase,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'observed',?15)",
        params![
            job.job_id,
            job.generation,
            job.tenant,
            lease.worker(),
            fence_digest,
            binding.id,
            proof.incarnation,
            token_text(proof.token)?,
            observation.configuration_version,
            observation.authority_actor,
            observation.authority_basis_digest,
            observation.activation_observation_digest,
            observation.path_observation_digest,
            PRODUCER_VERSION,
            time::OffsetDateTime::now_utc().unix_timestamp(),
        ],
    )?;
    Ok(observation)
}

fn validate_documents(documents: &[LocalDocumentRecord]) -> Result<()> {
    ensure!(documents.len() <= 10_000, "Too many Local documents");
    let mut previous: Option<&str> = None;
    for document in documents {
        ensure!(
            valid_path(&document.relative_path),
            "Invalid Local document path"
        );
        ensure!(
            valid_digest(&document.content_sha256),
            "Invalid Local document digest"
        );
        ensure!(
            previous.is_none_or(|path| {
                path.encode_utf16()
                    .cmp(document.relative_path.encode_utf16())
                    == Ordering::Less
            }),
            "Local documents are not canonically ordered"
        );
        previous = Some(&document.relative_path);
    }
    Ok(())
}

fn inventory_digest(documents: &[LocalDocumentRecord]) -> Result<String> {
    let mut hash = Sha256::new();
    for document in documents {
        hash.update(serde_json::to_vec(document)?);
        hash.update(b"\n");
    }
    Ok(hex::encode(hash.finalize()))
}

fn index_digest(documents: &[LocalDocumentRecord]) -> Result<String> {
    let mut hash = Sha256::new();
    for document in documents {
        hash.update(serde_json::to_vec(&(
            &document.relative_path,
            document.kind.stored(),
            document.size_bytes,
            &document.content_sha256,
            if document.kind == LocalDocumentKind::Pdf {
                "pending"
            } else {
                "na"
            },
            Option::<i64>::None,
            Option::<String>::None,
            Option::<String>::None,
        ))?);
        hash.update(b"\n");
    }
    Ok(hex::encode(hash.finalize()))
}

pub(crate) fn apply_local_documents(
    tx: &Transaction<'_>,
    tenant: &str,
    settlement: LocalScanSettlement,
) -> Result<LocalScanApplied> {
    validate_documents(&settlement.documents)?;
    let current = project_observation(
        tx,
        tenant,
        settlement.observation.source_id,
        &settlement.observation.authority_actor,
    )?;
    ensure!(
        current.source_id == settlement.observation.source_id
            && current.local_path == settlement.observation.local_path
            && current.configuration_version == settlement.observation.configuration_version
            && current.authority_actor == settlement.observation.authority_actor
            && current.authority_basis_digest == settlement.observation.authority_basis_digest
            && current.activation_observation_digest
                == settlement.observation.activation_observation_digest
            && current.path_observation_digest == settlement.observation.path_observation_digest,
        "Local Source observation changed"
    );
    let inventory_digest = inventory_digest(&settlement.documents)?;
    let index_digest = index_digest(&settlement.documents)?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    tx.execute(
        "DELETE FROM source_docs WHERE tenant_id=?1 AND project_id=?2",
        params![tenant, current.source_id],
    )?;
    for document in &settlement.documents {
        tx.execute(
            "INSERT INTO source_docs(tenant_id,project_id,path,kind,size_bytes,content_hash,extract_status,extract_error,page_count,updated_at,source_revision_id,input_digest,producer_version)
             VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,NULL,?8,NULL,NULL,NULL)",
            params![
                tenant,
                current.source_id,
                document.relative_path,
                document.kind.stored(),
                i64::try_from(document.size_bytes)?,
                document.content_sha256,
                if document.kind == LocalDocumentKind::Pdf { "pending" } else { "na" },
                now.to_string(),
            ],
        )?;
    }
    mark_source_synced(
        tx,
        tenant,
        current.source_id,
        current.configuration_version,
        current.local_path.as_deref(),
        &now.to_string(),
    )?;
    let target = format!("source:{}", current.source_id);
    let receipt = ResultArtifact {
        receipt_id: format!("local-scan-{}", hex::encode(rand::random::<[u8; 16]>())),
        content_hash: index_digest.clone(),
        target,
    };
    Ok(LocalScanApplied {
        source_id: current.source_id,
        receipt,
        inventory_digest,
        index_digest,
        doc_count: settlement.documents.len() as u64,
    })
}

pub(crate) fn settle(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    proof: &SourceWorkProof,
    policy: crate::auth::AuthPolicy,
    settlement: LocalScanSettlement,
) -> Result<(JobRecord, LocalScanApplied)> {
    let mut job = crate::jobs::claimed_job(tx, lease)?;
    ensure!(!job.cancel_requested, "ImportScan was cancelled");
    let authority = crate::jobs::require_original_authority(tx, &job, policy)?;
    let binding = expected_binding(&job)?;
    require_live_reservation(state, proof, &binding)?;
    ensure!(
        settlement.observation.source_id == binding.id,
        "Local scan settlement Source mismatch"
    );
    ensure!(
        settlement.observation.authority_actor == authority.subject(),
        "Local Source authority changed"
    );
    let basis_hash = settlement.observation.activation_observation_digest.clone();
    let (phase, incarnation, token): (String, String, String) = tx.query_row(
        "SELECT phase,reservation_incarnation,reservation_token FROM source_scan_executions WHERE job_id=?1 AND generation=?2 AND attempt_worker=?3 AND attempt_fence_digest=?4",
        params![job.job_id, job.generation, lease.worker(), digest_bytes(lease.fence())],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    ensure!(phase == "observed", "Local scan is not observed");
    ensure!(
        incarnation == proof.incarnation && token_value(&token)? == proof.token,
        "Local scan observation reservation mismatch"
    );
    let applied = apply_local_documents(tx, &job.tenant, settlement)?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    job.effects.push(EffectReceipt {
        intent: EffectIntent {
            operation: EffectOperation::SourceRefresh,
            basis_hash,
            content_hash: applied.index_digest.clone(),
            target: applied.receipt.target.clone(),
        },
        attempt: job.attempt,
        generation: job.generation,
        confirmed: true,
        no_effect: false,
        receipt: Some(applied.receipt.clone()),
        checklist_completion: None,
    });
    crate::jobs::save(tx, &mut job, "local_scan_settled")?;
    tx.execute(
        "UPDATE source_scan_executions SET phase='local_settled',effect_applied=1,inventory_digest=?3,index_digest=?4,receipt_id=?5,receipt_hash=?6,receipt_target=?7,updated_at=?8 WHERE job_id=?1 AND generation=?2 AND phase='observed'",
        params![job.job_id, job.generation, applied.inventory_digest, applied.index_digest, applied.receipt.receipt_id, applied.receipt.content_hash, applied.receipt.target, now],
    )?;
    Ok((job, applied))
}

pub(crate) fn complete(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    proof: SourceWorkProof,
    policy: crate::auth::AuthPolicy,
    applied: LocalScanApplied,
    result: SourceScanResult,
) -> Result<PendingLocalCompletion> {
    let completed = tx
        .query_row(
            "SELECT source_id,reservation_incarnation,reservation_token,inventory_digest,index_digest,receipt_id,receipt_hash,receipt_target,result_digest FROM source_scan_executions WHERE tenant=?1 AND job_id=?2 AND generation=?3 AND attempt_worker=?4 AND attempt_fence_digest=?5 AND phase='completed'",
            params![lease.tenant(), lease.job_id(), lease.generation(), lease.worker(), digest_bytes(lease.fence())],
            |row| {
                let token: String = row.get(2)?;
                Ok(CompletedJournal {
                    source_id: row.get(0)?,
                    reservation_incarnation: row.get(1)?,
                    reservation_token: token_value(&token).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            error.into(),
                        )
                    })?,
                    inventory_digest: row.get(3)?,
                    index_digest: row.get(4)?,
                    receipt: ResultArtifact {
                        receipt_id: row.get(5)?,
                        content_hash: row.get(6)?,
                        target: row.get(7)?,
                    },
                    result_digest: row.get(8)?,
                })
            },
        )
        .optional()?;
    if let Some(completed) = completed {
        ensure!(
            completed.source_id == applied.source_id
                && completed.reservation_incarnation == proof.incarnation
                && completed.reservation_token == proof.token
                && completed.inventory_digest == applied.inventory_digest
                && completed.index_digest == applied.index_digest
                && completed.receipt == applied.receipt
                && result == applied.local_source_scan_result()?,
            "Local scan terminal request mismatch"
        );
        let result_digest = digest_bytes(serde_json::to_vec(&CompletedResult::SourceScan(
            result.clone(),
        ))?);
        ensure!(
            result_digest == completed.result_digest,
            "Local scan terminal result mismatch"
        );
        let job = crate::jobs::load(tx, lease.job_id())?
            .ok_or_else(|| anyhow!("Completed Local scan Job is missing"))?;
        ensure!(
            job.tenant == lease.tenant()
                && job.generation == lease.generation()
                && job.state == PersistentState::Succeeded
                && job.result.as_ref() == Some(&applied.receipt)
                && job.public_result.as_ref() == Some(&CompletedResult::SourceScan(result)),
            "Completed Local scan Job mismatch"
        );
        let stored_result_digest = digest_bytes(serde_json::to_vec(
            job.public_result
                .as_ref()
                .ok_or_else(|| anyhow!("Completed Local scan result is missing"))?,
        )?);
        ensure!(
            stored_result_digest == completed.result_digest,
            "Completed Local scan result digest mismatch"
        );
        let binding = SourceBinding {
            tenant: job.tenant.clone(),
            id: completed.source_id,
        };
        match state.classify_issued(&proof.incarnation, proof.token) {
            IssuedTokenState::Active(actual) => ensure!(
                actual == &binding,
                "Local scan terminal reservation binding mismatch"
            ),
            IssuedTokenState::Retired => {}
            IssuedTokenState::NeverIssued | IssuedTokenState::WrongIncarnation => {
                return Err(anyhow!("Local scan terminal reservation mismatch"));
            }
        }
        return Ok(PendingLocalCompletion {
            completion: CompletedLocalScan {
                job,
                result_digest: completed.result_digest,
            },
            proof,
            binding,
        });
    }
    let mut job = crate::jobs::claimed_job(tx, lease)?;
    ensure!(!job.cancel_requested, "ImportScan was cancelled");
    let binding = expected_binding(&job)?;
    require_live_reservation(state, &proof, &binding)?;
    let authority = crate::jobs::require_original_authority(tx, &job, policy)?;
    let current = project_observation(tx, &job.tenant, binding.id, authority.subject())?;
    ensure!(applied.source_id == binding.id, "Wrong Local scan Source");
    ensure!(
        result == applied.local_source_scan_result()?,
        "Wrong Local scan result"
    );
    let row: (String, String, String, String, String, String, i64, String, String, String, String) = tx.query_row(
        "SELECT phase,reservation_incarnation,reservation_token,inventory_digest,index_digest,receipt_id,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest FROM source_scan_executions WHERE job_id=?1 AND generation=?2 AND attempt_worker=?3 AND attempt_fence_digest=?4",
        params![job.job_id, job.generation, lease.worker(), digest_bytes(lease.fence())],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?)),
    )?;
    ensure!(
        row.0 == "local_settled"
            && row.1 == proof.incarnation
            && token_value(&row.2)? == proof.token
            && row.3 == applied.inventory_digest
            && row.4 == applied.index_digest
            && row.5 == applied.receipt.receipt_id
            && row.6 == current.configuration_version
            && row.7 == current.authority_actor
            && row.8 == current.authority_basis_digest
            && row.9 == current.activation_observation_digest
            && row.10 == current.path_observation_digest,
        "Local scan settlement mismatch"
    );
    ensure!(
        job.effects.iter().any(|effect| effect.confirmed
            && effect.intent.operation == EffectOperation::SourceRefresh
            && effect.receipt.as_ref() == Some(&applied.receipt)),
        "Local scan receipt is not confirmed"
    );
    let result_digest = digest_bytes(serde_json::to_vec(&CompletedResult::SourceScan(
        result.clone(),
    ))?);
    job.result = Some(applied.receipt);
    job.public_result = Some(CompletedResult::SourceScan(result));
    job.progress = Some(100);
    job.state = PersistentState::Succeeded;
    crate::jobs::save(tx, &mut job, "local_scan_completed")?;
    tx.execute(
        "UPDATE source_scan_executions SET phase='completed',result_digest=?3,updated_at=?4 WHERE job_id=?1 AND generation=?2 AND phase='local_settled'",
        params![job.job_id, job.generation, result_digest, time::OffsetDateTime::now_utc().unix_timestamp()],
    )?;
    Ok(PendingLocalCompletion {
        completion: CompletedLocalScan { job, result_digest },
        proof,
        binding,
    })
}

pub(crate) fn inspect(
    tx: &Transaction<'_>,
    state: &State,
    client_worker: &str,
    lease: &AttemptLease,
) -> Result<LocalCompletionInspection> {
    ensure!(lease.worker() == client_worker, "Foreign worker lease");
    let row: Option<(String, String, String, Option<String>)> = tx
        .query_row(
            "SELECT phase,reservation_incarnation,reservation_token,result_digest FROM source_scan_executions WHERE tenant=?1 AND job_id=?2 AND generation=?3 AND attempt_worker=?4 AND attempt_fence_digest=?5",
            params![lease.tenant(), lease.job_id(), lease.generation(), client_worker, digest_bytes(lease.fence())],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((phase, incarnation, token, result_digest)) = row else {
        return Ok(LocalCompletionInspection::NotCommitted);
    };
    if phase != "completed" {
        return Ok(LocalCompletionInspection::NotCommitted);
    }
    let job = crate::jobs::load(tx, lease.job_id())?
        .ok_or_else(|| anyhow!("Completed Local scan Job is missing"))?;
    ensure!(
        job.tenant == lease.tenant()
            && job.generation == lease.generation()
            && job.state == PersistentState::Succeeded
            && matches!(job.public_result, Some(CompletedResult::SourceScan(_))),
        "Completed Local scan Job mismatch"
    );
    let stored_result_digest = digest_bytes(serde_json::to_vec(
        job.public_result
            .as_ref()
            .ok_or_else(|| anyhow!("Completed Local scan result is missing"))?,
    )?);
    let result_digest =
        result_digest.ok_or_else(|| anyhow!("Completed Local scan result digest is missing"))?;
    ensure!(
        stored_result_digest == result_digest,
        "Completed Local scan result digest mismatch"
    );
    let completion = CompletedLocalScan { job, result_digest };
    let token = token_value(&token)?;
    Ok(match state.classify_issued(&incarnation, token) {
        IssuedTokenState::WrongIncarnation | IssuedTokenState::Retired => {
            LocalCompletionInspection::Completed(completion)
        }
        IssuedTokenState::Active(binding) => {
            let source_id: i64 = tx.query_row(
                "SELECT source_id FROM source_scan_executions WHERE job_id=?1 AND generation=?2",
                params![lease.job_id(), lease.generation()],
                |row| row.get(0),
            )?;
            if binding.tenant == lease.tenant() && binding.id == source_id {
                LocalCompletionInspection::Completed(completion)
            } else {
                LocalCompletionInspection::CommittedReservationConflict(completion)
            }
        }
        IssuedTokenState::NeverIssued => {
            LocalCompletionInspection::CommittedReservationCorrupt(completion)
        }
    })
}

pub(crate) fn inspect_held(
    tx: &Transaction<'_>,
    state: &State,
    client_worker: &str,
    lease: &AttemptLease,
    proof: &SourceWorkProof,
) -> Result<HeldLocalScanInspection> {
    ensure!(lease.worker() == client_worker, "Foreign worker lease");
    let job = crate::jobs::load(tx, lease.job_id())?
        .ok_or_else(|| anyhow!("Held Local scan Job is missing"))?;
    ensure!(
        job.tenant == lease.tenant()
            && job.generation == lease.generation()
            && job.worker.as_deref() == Some(client_worker)
            && job.fence.as_deref() == Some(lease.fence()),
        "Held Local scan attempt mismatch"
    );
    let binding = expected_binding(&job)?;
    require_live_reservation(state, proof, &binding)?;
    let row: (String, String, String, Option<String>, Option<String>, Option<String>) = tx.query_row(
        "SELECT phase,reservation_incarnation,reservation_token,inventory_digest,index_digest,receipt_id FROM source_scan_executions WHERE tenant=?1 AND job_id=?2 AND generation=?3 AND attempt_worker=?4 AND attempt_fence_digest=?5",
        params![lease.tenant(), lease.job_id(), lease.generation(), client_worker, digest_bytes(lease.fence())],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    ensure!(
        row.1 == proof.incarnation && token_value(&row.2)? == proof.token,
        "Held Local scan reservation mismatch"
    );
    if row.0 == "observed" {
        return Ok(HeldLocalScanInspection::NotApplied(job));
    }
    ensure!(row.0 == "local_settled", "Held Local scan is not adoptable");
    let receipt =
        job.effects
            .iter()
            .find(|effect| {
                effect.confirmed
                    && effect.intent.operation == EffectOperation::SourceRefresh
                    && effect.receipt.as_ref().is_some_and(|receipt| {
                        Some(receipt.receipt_id.as_str()) == row.5.as_deref()
                    })
            })
            .and_then(|effect| effect.receipt.clone())
            .ok_or_else(|| anyhow!("Held Local scan receipt is not confirmed"))?;
    let doc_count = u64::try_from(tx.query_row(
        "SELECT count(*) FROM source_docs WHERE tenant_id=?1 AND project_id=?2",
        params![job.tenant, binding.id],
        |row| row.get::<_, i64>(0),
    )?)?;
    Ok(HeldLocalScanInspection::Applied(
        job,
        LocalScanApplied {
            source_id: binding.id,
            receipt,
            inventory_digest: row
                .3
                .ok_or_else(|| anyhow!("Held Local scan inventory digest is missing"))?,
            index_digest: row
                .4
                .ok_or_else(|| anyhow!("Held Local scan index digest is missing"))?,
            doc_count,
        },
    ))
}

pub(crate) fn reconcile(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    proof: SourceWorkProof,
    applied: &LocalScanApplied,
    reason: &str,
) -> Result<PendingLocalReconciliation> {
    ensure!(
        !reason.is_empty() && reason.len() <= 1024,
        "Invalid reconciliation reason"
    );
    let mut job = crate::jobs::load(tx, lease.job_id())?
        .ok_or_else(|| anyhow!("Local scan reconciliation Job is missing"))?;
    let original_attempt = job.generation == lease.generation()
        && job.worker.as_deref() == Some(lease.worker())
        && job.fence.as_deref() == Some(lease.fence());
    // A Cancel or recovery bumps the generation; a recorded reconciliation
    // keeps it. Either way the attempt no longer holds the Job.
    let released_attempt = (job.generation == lease.generation()
        || job.generation == lease.generation() + 1)
        && job.state == PersistentState::ReconciliationRequired
        && job.worker.is_none()
        && job.fence.is_none();
    ensure!(
        job.tenant == lease.tenant() && (original_attempt || released_attempt),
        "Local scan reconciliation attempt mismatch"
    );
    let binding = expected_binding(&job)?;
    let row: (String, String, String, Option<String>, Option<String>, Option<String>) = tx.query_row(
        "SELECT phase,reservation_incarnation,reservation_token,inventory_digest,receipt_id,reconciliation_reason FROM source_scan_executions WHERE tenant=?1 AND job_id=?2 AND generation=?3 AND attempt_worker=?4 AND attempt_fence_digest=?5",
        params![lease.tenant(), lease.job_id(), lease.generation(), lease.worker(), digest_bytes(lease.fence())],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    ensure!(row.0 != "observed", LocalScanFailure::NotSettled);
    ensure!(
        (row.0 == "local_settled" || row.0 == "reconciliation_required")
            && row.1 == proof.incarnation
            && token_value(&row.2)? == proof.token
            && row.3.as_deref() == Some(applied.inventory_digest.as_str())
            && row.4.as_deref() == Some(applied.receipt.receipt_id.as_str()),
        "Local scan reconciliation settlement mismatch"
    );
    ensure!(
        job.effects.iter().any(|effect| effect.confirmed
            && effect.intent.operation == EffectOperation::SourceRefresh
            && effect.receipt.as_ref() == Some(&applied.receipt)),
        "Local scan reconciliation receipt is not confirmed"
    );
    // A retry after a lost reply returns the recorded Job; its reservation
    // is already retired.
    if row.0 == "reconciliation_required" {
        ensure!(
            released_attempt && row.5.as_deref() == Some(reason),
            "Local scan reconciliation reason mismatch"
        );
        return Ok(PendingLocalReconciliation {
            job,
            proof,
            binding,
        });
    }
    require_live_reservation(state, &proof, &binding)?;
    // Keep the text a Cancel or recovery left for the user.
    if original_attempt || job.recovery.is_none() {
        job.recovery = Some("Local scan completion requires reconciliation".into());
    }
    job.state = PersistentState::ReconciliationRequired;
    job.fence = None;
    job.worker = None;
    job.lease_until = None;
    crate::jobs::save(tx, &mut job, "local_scan_reconciliation_required")?;
    ensure!(
        tx.execute(
            "UPDATE source_scan_executions SET phase='reconciliation_required',primary_failure=?3,reconciliation_reason=?3,updated_at=?4 WHERE job_id=?1 AND generation=?2 AND phase='local_settled'",
            params![job.job_id, lease.generation(), reason, time::OffsetDateTime::now_utc().unix_timestamp()],
        )? == 1,
        "Local scan reconciliation journal mismatch"
    );
    Ok(PendingLocalReconciliation {
        job,
        proof,
        binding,
    })
}

pub(crate) fn validate_schema(connection: &Connection, version: u64) -> Result<()> {
    let objects = |connection: &Connection| -> Result<Vec<(String, Option<String>)>> {
        Ok(connection
            .prepare("SELECT name,sql FROM sqlite_master WHERE name LIKE 'source_scan_execution%' OR name LIKE 'sqlite_autoindex_source_scan_execution%' ORDER BY name")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    };
    let actual = objects(connection)?;
    if version < 40 {
        ensure!(actual.is_empty(), "Unexpected Local scan schema");
        return Ok(());
    }
    let expected = Connection::open_in_memory()?;
    expected
        .execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE durable_jobs(id TEXT PRIMARY KEY);")?;
    expected.execute_batch(include_str!("source_scan/schema.sql"))?;
    ensure!(actual == objects(&expected)?, "Local scan schema mismatch");
    Ok(())
}

pub(crate) mod node_json;
#[cfg(test)]
mod tests;
