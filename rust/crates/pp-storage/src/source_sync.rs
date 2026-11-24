use crate::{
    SettingsClient,
    catalog::{
        IssuedTokenState, SourceBinding, SourceBusy, SourceWorkLease, SourceWorkProof, State,
        activity::{self, SourceOutcome},
    },
    jobs::{
        AttemptLease, CompletedResult, JobKind, JobRecord, LocalCommit, Payload, PersistentState,
        ResultArtifact, SourceScanResult, SourceSyncFailure, SourceSyncResult,
    },
    source_scan::LocalScanObservation,
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

/// One target of a Sync batch, by its ordinal in the sealed basis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncTargetKey {
    pub(crate) job_id: String,
    pub(crate) ordinal: u32,
}

/// A claimed Local target. It holds the target's Source reservation until it
/// is released or dropped.
pub struct SyncTargetLease {
    pub(crate) key: SyncTargetKey,
    pub(crate) source_work: SourceWorkLease,
    pub(crate) observation: LocalScanObservation,
    pub(crate) claim_generation: i64,
}

impl SyncTargetLease {
    pub fn key(&self) -> &SyncTargetKey {
        &self.key
    }

    pub fn observation(&self) -> &LocalScanObservation {
        &self.observation
    }

    pub(crate) fn candidate(&self) -> Result<SyncCandidate> {
        Ok(SyncCandidate {
            key: self.key.clone(),
            claim_generation: self.claim_generation,
            proof: self.source_work.proof()?,
        })
    }
}

/// A claimed GitHub Source and the token used to read it. `Debug` is not
/// derived: the token must not appear in logs.
pub struct GithubSyncTarget {
    lease: SyncTargetLease,
    url: String,
    branch: String,
    tag: Option<String>,
    name: String,
    token: Option<HeldToken>,
}

impl GithubSyncTarget {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The stored GitHub token, or `None` for a public repository.
    pub fn token(&self) -> Option<&str> {
        self.token.as_ref().map(HeldToken::expose)
    }

    pub fn lease(&self) -> &SyncTargetLease {
        &self.lease
    }

    pub fn lease_mut(&mut self) -> &mut SyncTargetLease {
        &mut self.lease
    }
}

/// A GitHub personal access token. Dropping it overwrites the buffer.
struct HeldToken(String);

impl HeldToken {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for HeldToken {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
    }
}

struct GithubHandoff {
    url: String,
    branch: String,
    tag: Option<String>,
    name: String,
    token: Option<HeldToken>,
}

/// What a worker presents for binding: the target it claimed, the claim
/// generation it saw and the reservation it holds.
pub(crate) struct SyncCandidate {
    key: SyncTargetKey,
    claim_generation: i64,
    proof: SourceWorkProof,
}

/// A claimed target whose held reservation, attempt, sealed basis and recorded
/// content still match. Binding checks that content and writes nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundSyncTarget {
    pub(crate) key: SyncTargetKey,
    pub(crate) source_id: i64,
    pub(crate) claim_generation: i64,
}

/// The successful result a worker stores for one claimed target.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncSettlement {
    pub result: SourceScanResult,
    pub receipt: ResultArtifact,
}

/// One target whose successful result is stored and whose reservation the
/// writer has retired.
#[derive(Clone, Debug, PartialEq)]
pub struct SettledSyncTarget {
    pub(crate) key: SyncTargetKey,
    pub(crate) source_id: i64,
    pub(crate) result: SourceScanResult,
    pub(crate) receipt: ResultArtifact,
}

pub(crate) struct SettledSyncRelease {
    pub(crate) settled: SettledSyncTarget,
    pub(crate) proof: SourceWorkProof,
    pub(crate) binding: SourceBinding,
}

/// One claimed target whose local read failure is stored. The reservation is
/// still held, so the failure does not count as completed progress.
#[derive(Clone, Debug, PartialEq)]
pub struct FailedSyncTarget {
    pub(crate) key: SyncTargetKey,
    pub(crate) failure: SourceSyncFailure,
    pub(crate) progress: u8,
    pub(crate) version: i64,
}

/// One retained failure whose reservation is acknowledged. The stored failure
/// is unchanged. An exact replay returns this same value.
#[derive(Clone, Debug, PartialEq)]
pub struct AcknowledgedSyncTarget {
    pub(crate) key: SyncTargetKey,
    pub(crate) failure: SourceSyncFailure,
    pub(crate) progress: u8,
    pub(crate) version: i64,
}

pub(crate) struct AcknowledgedSyncRelease {
    pub(crate) acknowledged: AcknowledgedSyncTarget,
    pub(crate) proof: SourceWorkProof,
    pub(crate) binding: SourceBinding,
}

/// Caller-facing Sync halt after a Source operation error. Maps onto the
/// existing batch halt codes used by Cancel, authority refusal and worker Fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSyncHalt {
    Fatal,
    UncertainTarget,
}

impl SourceSyncHalt {
    pub(crate) fn stored(self) -> &'static str {
        match self {
            Self::Fatal => "fatal",
            Self::UncertainTarget => "uncertain_target",
        }
    }
}

/// Outcome of a Sync-owned finish. Ordinary completion carries the aggregate
/// and the terminal Job. Reconciliation is reserved for later guard slices.
#[derive(Debug)]
pub enum SourceSyncFinishOutcome {
    Completed(Box<CompletedSourceSync>),
    NeedsReconciliation,
}

/// A finished Sync batch: the terminal Job and the validated target aggregate.
#[derive(Debug)]
pub struct CompletedSourceSync {
    pub job: JobRecord,
    pub aggregate: SourceSyncResult,
}

pub(crate) struct SyncCommandResult<T> {
    pub(crate) value: T,
    pub(crate) job: JobRecord,
    pub(crate) commit: LocalCommit,
}

impl<T> SyncCommandResult<T> {
    fn committed(value: T, job: JobRecord) -> Self {
        Self {
            value,
            job,
            commit: LocalCommit::Committed,
        }
    }

    fn read_only(value: T, job: JobRecord) -> Self {
        Self {
            value,
            job,
            commit: LocalCommit::ReadOnly,
        }
    }
}

/// Read-only view of one Sync target for the live attempt. Pending and claimed
/// targets share [`Self::Pending`]. Unacknowledged terminal rows report
/// reconciliation instead of a settled result or failure.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncTargetInspection {
    Pending,
    Succeeded(SourceScanResult),
    Failed(SourceSyncFailure),
    Halted,
    ReconciliationRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncAuthorityRefusal {
    CredentialInvalid,
    PolicyChanged,
    TenantChanged,
    SubjectChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalAuthorityStatus {
    Valid,
    Refused(SyncAuthorityRefusal),
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncRecoveryDisposition {
    Pending,
    Claimed,
    Succeeded,
    Failed,
    NotRun,
    ReconciliationRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncRecoveryState {
    Completed,
    Failed,
    Cancelled,
    ReconciliationRequired,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SyncRecoveryTarget {
    ordinal: u32,
    requested_source_id: u64,
    disposition: SyncRecoveryDisposition,
    failure: Option<String>,
    result: Option<SourceScanResult>,
    acknowledged: bool,
}

impl SyncRecoveryTarget {
    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }
    pub fn requested_source_id(&self) -> u64 {
        self.requested_source_id
    }
    pub fn disposition(&self) -> SyncRecoveryDisposition {
        self.disposition
    }
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
    pub fn result(&self) -> Option<&SourceScanResult> {
        self.result.as_ref()
    }
    pub fn acknowledged(&self) -> bool {
        self.acknowledged
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SyncRecoveryInspection {
    job_id: String,
    version: i64,
    state: SyncRecoveryState,
    targets: Vec<SyncRecoveryTarget>,
    original_authority: OriginalAuthorityStatus,
}

impl SyncRecoveryInspection {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
    pub fn version(&self) -> i64 {
        self.version
    }
    pub fn state(&self) -> SyncRecoveryState {
        self.state
    }
    pub fn targets(&self) -> &[SyncRecoveryTarget] {
        &self.targets
    }
    pub fn original_authority(&self) -> OriginalAuthorityStatus {
        self.original_authority
    }
}

pub(crate) struct AuthorizedSyncRead {
    job: JobRecord,
    original_subject: Option<String>,
    original_authority: OriginalAuthorityStatus,
}

impl AuthorizedSyncRead {
    pub(crate) fn new(
        job: JobRecord,
        tenant: String,
        _actor: String,
        expected_version: i64,
        original_subject: Option<String>,
        original_authority: OriginalAuthorityStatus,
    ) -> Result<Self> {
        ensure!(job.tenant == tenant, "Sync recovery tenant mismatch");
        ensure!(job.kind == JobKind::Sync, "Job is not Sync");
        ensure!(
            job.state_version == expected_version,
            "Stale Sync recovery version"
        );
        ensure!(
            job.state == PersistentState::ReconciliationRequired || job.state.terminal(),
            "Sync recovery is not retained"
        );
        Ok(Self {
            job,
            original_subject,
            original_authority,
        })
    }

    pub(crate) fn job(&self) -> &JobRecord {
        &self.job
    }
    pub(crate) fn original_subject(&self) -> Option<&str> {
        self.original_subject.as_deref()
    }
}

pub(crate) fn original_authority_status(
    error: &anyhow::Error,
) -> Option<(Option<String>, OriginalAuthorityStatus)> {
    use crate::auth::AuthorityFailure;
    Some(match error.downcast_ref::<AuthorityFailure>()? {
        AuthorityFailure::CredentialInvalid => (
            None,
            OriginalAuthorityStatus::Refused(SyncAuthorityRefusal::CredentialInvalid),
        ),
        AuthorityFailure::PolicyChanged => (
            None,
            OriginalAuthorityStatus::Refused(SyncAuthorityRefusal::PolicyChanged),
        ),
        AuthorityFailure::TenantChanged => (
            None,
            OriginalAuthorityStatus::Refused(SyncAuthorityRefusal::TenantChanged),
        ),
        AuthorityFailure::SubjectChanged => (
            None,
            OriginalAuthorityStatus::Refused(SyncAuthorityRefusal::SubjectChanged),
        ),
        AuthorityFailure::Missing => (None, OriginalAuthorityStatus::Unavailable),
        AuthorityFailure::PolicyRequired => return None,
    })
}

struct ProducerCapture {
    _claim_generation: i64,
    _parent_generation: i64,
    _worker: String,
    _fence_digest: String,
    _incarnation: String,
    _token: String,
    _configuration_version: i64,
    authority_actor: String,
    _authority_basis_digest: String,
    _activation_digest: String,
    _path_digest: String,
}

enum ValidatedTargetOutcome {
    Pending,
    Claimed,
    Succeeded {
        result: SourceScanResult,
        _receipt: crate::jobs::ResultArtifact,
    },
    Failed {
        failure: SourceSyncFailure,
        code: String,
    },
    NotRun,
    ReconciliationRequired {
        result: Option<SourceScanResult>,
    },
}

struct ValidatedTarget {
    ordinal: u32,
    source_id: i64,
    acknowledged: bool,
    failure_detail: Option<String>,
    capture: Option<ProducerCapture>,
    outcome: ValidatedTargetOutcome,
}

struct StoredSyncTargetRow {
    requested_source_id: i64,
    source_name: Option<String>,
    state: String,
    claim_generation: i64,
    parent_generation: Option<i64>,
    attempt_worker: Option<String>,
    attempt_fence_digest: Option<String>,
    reservation_incarnation: Option<String>,
    reservation_token: Option<String>,
    configuration_version: Option<i64>,
    authority_actor: Option<String>,
    authority_basis_digest: Option<String>,
    activation_observation_digest: Option<String>,
    path_observation_digest: Option<String>,
    result_json: Option<String>,
    result_digest: Option<String>,
    receipt_id: Option<String>,
    receipt_hash: Option<String>,
    receipt_target: Option<String>,
    failure_code: Option<String>,
    failure_detail: Option<String>,
    reservation_acknowledged: i64,
}

impl StoredSyncTargetRow {
    fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            requested_source_id: row.get(0)?,
            source_name: row.get(1)?,
            state: row.get(2)?,
            claim_generation: row.get(3)?,
            parent_generation: row.get(4)?,
            attempt_worker: row.get(5)?,
            attempt_fence_digest: row.get(6)?,
            reservation_incarnation: row.get(7)?,
            reservation_token: row.get(8)?,
            configuration_version: row.get(9)?,
            authority_actor: row.get(10)?,
            authority_basis_digest: row.get(11)?,
            activation_observation_digest: row.get(12)?,
            path_observation_digest: row.get(13)?,
            result_json: row.get(14)?,
            result_digest: row.get(15)?,
            receipt_id: row.get(16)?,
            receipt_hash: row.get(17)?,
            receipt_target: row.get(18)?,
            failure_code: row.get(19)?,
            failure_detail: row.get(20)?,
            reservation_acknowledged: row.get(21)?,
        })
    }
}

pub enum NextSyncTarget {
    Local(SyncTargetLease),
    /// A claimed GitHub Source. The lease holds its reservation until it is
    /// settled, failed, or dropped. The token is the stored personal access
    /// token, when one is set, and is cleared when this value is dropped.
    Github(GithubSyncTarget),
    /// The target failed before any reservation and needs no settlement.
    OrdinaryFailure(SourceSyncFailure),
    /// A local read failure whose reservation is still held. It is not a
    /// completed failure, and the following target stays pending.
    RetainedFailure(SourceSyncFailure),
    Halted,
    Complete,
}

pub(crate) enum Next {
    Reserve(ReservedTarget),
    Done(NextSyncTarget, i64),
}

/// A target claimed in the transaction. Its token becomes live only after
/// the commit, as a claim-time reservation does.
pub(crate) struct ReservedTarget {
    key: SyncTargetKey,
    observation: LocalScanObservation,
    claim_generation: i64,
    token: u64,
    binding: SourceBinding,
    version: i64,
    github: Option<GithubHandoff>,
}

impl ReservedTarget {
    pub(crate) fn activate(
        self,
        state: &mut State,
        client: SettingsClient,
    ) -> Result<(NextSyncTarget, i64)> {
        state.activate(self.token, self.binding.tenant, self.binding.id);
        let proof = state.proof(self.token)?;
        let lease = SyncTargetLease {
            key: self.key,
            source_work: crate::catalog::lease(client, proof),
            observation: self.observation,
            claim_generation: self.claim_generation,
        };
        let target = match self.github {
            Some(github) => NextSyncTarget::Github(GithubSyncTarget {
                lease,
                url: github.url,
                branch: github.branch,
                tag: github.tag,
                name: github.name,
                token: github.token,
            }),
            None => NextSyncTarget::Local(lease),
        };
        Ok((target, self.version))
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn digest(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

fn token_text(token: u64) -> Result<String> {
    ensure!(token != 0, "Invalid Source reservation token");
    Ok(format!("{token:016x}"))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_receipt_id(value: &str) -> bool {
    value.len() == 43
        && value.starts_with("local-scan-")
        && value[11..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn basis_digest(tenant: &str, selection_kind: &str, source_ids: &[i64]) -> Result<String> {
    Ok(digest(serde_json::to_vec(&(
        1_u8,
        tenant,
        selection_kind,
        source_ids.iter().enumerate().collect::<Vec<_>>(),
    ))?))
}

fn selected(payload: &Payload) -> Result<Option<Vec<i64>>> {
    let Payload::Sync { project_ids } = payload else {
        return Err(anyhow!("Sync payload required"));
    };
    project_ids
        .as_ref()
        .map(|ids| {
            ids.iter()
                .map(|id| i64::try_from(*id).map_err(Into::into))
                .collect()
        })
        .transpose()
}

pub(crate) fn require_sync_job(tx: &Transaction<'_>, lease: &AttemptLease) -> Result<JobRecord> {
    let job = crate::jobs::claimed_job(tx, lease)?;
    ensure!(job.kind == JobKind::Sync, "Attempt is not a Sync Job");
    Ok(job)
}

/// Checks the stored batch against the Job's payload and returns its
/// Sources in ordinal order.
fn validate_basis(tx: &Transaction<'_>, job: &JobRecord) -> Result<Vec<i64>> {
    let (tenant, selection_kind, stored_digest, target_count): (String, String, String, i64) = tx
        .query_row(
            "SELECT tenant,selection_kind,target_basis_digest,target_count FROM source_sync_batches WHERE job_id=?1",
            [&job.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    ensure!(tenant == job.tenant, "Sync batch tenant mismatch");
    let rows = tx
        .prepare(
            "SELECT ordinal,requested_source_id,tenant FROM source_sync_targets WHERE job_id=?1 ORDER BY ordinal",
        )?
        .query_map([&job.job_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        usize::try_from(target_count)? == rows.len(),
        "Sync target count mismatch"
    );
    let mut source_ids = Vec::with_capacity(rows.len());
    for (expected, (ordinal, source_id, row_tenant)) in rows.into_iter().enumerate() {
        ensure!(
            ordinal == i64::try_from(expected)?,
            "Sync target ordinal mismatch"
        );
        ensure!(row_tenant == job.tenant, "Sync target tenant mismatch");
        source_ids.push(source_id);
    }
    match selected(&job.payload)? {
        None => ensure!(selection_kind == "all", "Sync selection mismatch"),
        Some(expected) => {
            ensure!(selection_kind == "explicit", "Sync selection mismatch");
            ensure!(source_ids == expected, "Sync selected Sources mismatch");
        }
    }
    ensure!(
        stored_digest == basis_digest(&job.tenant, &selection_kind, &source_ids)?,
        "Sync target basis mismatch"
    );
    Ok(source_ids)
}

/// Checks one bound candidate against the sealed basis and its recorded content.
fn validate_target(tx: &Transaction<'_>, job: &JobRecord, bound: &BoundSyncTarget) -> Result<()> {
    let source_ids = validate_basis(tx, job)?;
    validate_bound_target_in_basis(tx, job, bound, &source_ids)
}

/// Checks revision identity, Source configuration, observation and result
/// provenance for a candidate already placed in the sealed basis.
fn validate_bound_target_in_basis(
    tx: &Transaction<'_>,
    job: &JobRecord,
    bound: &BoundSyncTarget,
    source_ids: &[i64],
) -> Result<()> {
    ensure!(bound.key.job_id == job.job_id, "Sync target Job mismatch");
    let source_id = *source_ids
        .get(usize::try_from(bound.key.ordinal)?)
        .ok_or_else(|| anyhow!("Sync target is outside the sealed basis"))?;
    ensure!(source_id == bound.source_id, "Sync target Source mismatch");
    let (
        claim_generation,
        configuration_version,
        authority_actor,
        authority_basis_digest,
        activation_observation_digest,
        path_observation_digest,
        result_json,
        result_digest,
        receipt_id,
        receipt_hash,
        receipt_target,
    ) = tx.query_row(
        "SELECT claim_generation,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest,result_json,result_digest,receipt_id,receipt_hash,receipt_target FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2 AND state='claimed'",
        params![bound.key.job_id, bound.key.ordinal],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        },
    )?;
    ensure!(
        claim_generation == bound.claim_generation,
        "Sync target claim generation mismatch"
    );
    let observation = current_observation(tx, &job.tenant, source_id, &authority_actor)?;
    ensure!(
        observation.configuration_version == configuration_version,
        "Sync Source configuration mismatch"
    );
    ensure!(
        observation.authority_actor == authority_actor
            && observation.authority_basis_digest == authority_basis_digest
            && observation.path_observation_digest == path_observation_digest,
        "Sync Source observation mismatch"
    );
    ensure!(
        observation.activation_observation_digest == activation_observation_digest,
        "Sync revision identity mismatch"
    );
    validate_result_provenance(
        source_id,
        result_json,
        result_digest,
        receipt_id,
        receipt_hash,
        receipt_target,
    )
}

fn validate_result_provenance(
    source_id: i64,
    result_json: Option<String>,
    result_digest: Option<String>,
    receipt_id: Option<String>,
    receipt_hash: Option<String>,
    receipt_target: Option<String>,
) -> Result<()> {
    if result_json.is_none()
        && result_digest.is_none()
        && receipt_id.is_none()
        && receipt_hash.is_none()
        && receipt_target.is_none()
    {
        return Ok(());
    }
    let (
        Some(result_json),
        Some(result_digest),
        Some(receipt_id),
        Some(receipt_hash),
        Some(receipt_target),
    ) = (
        result_json,
        result_digest,
        receipt_id,
        receipt_hash,
        receipt_target,
    )
    else {
        return Err(anyhow!("Sync result capture incomplete"));
    };
    ensure!(valid_digest(&result_digest), "Invalid Sync result digest");
    ensure!(valid_digest(&receipt_hash), "Invalid Sync receipt digest");
    ensure!(valid_receipt_id(&receipt_id), "Invalid Local receipt ID");
    let result: SourceScanResult = serde_json::from_str(&result_json)?;
    let canonical = serde_json::to_string(&result)?;
    ensure!(result_json == canonical, "Sync result is not canonical");
    ensure!(
        digest(canonical.as_bytes()) == result_digest,
        "Sync result digest mismatch"
    );
    ensure!(
        result.project_id == u64::try_from(source_id).ok(),
        "Sync result Source mismatch"
    );
    ensure!(
        receipt_target == format!("source:{source_id}"),
        "Sync receipt Source mismatch"
    );
    Ok(())
}

/// Validates one retained target row against the sealed basis: producer capture,
/// observation digests, result/receipt provenance, and per-state rules.
fn validate_target_in_basis(
    tx: &Transaction<'_>,
    job: &JobRecord,
    key: &SyncTargetKey,
    source_ids: &[i64],
) -> Result<ValidatedTarget> {
    ensure!(job.kind == JobKind::Sync, "Job is not Sync");
    ensure!(key.job_id == job.job_id, "Sync target Job mismatch");
    let source_id = *source_ids
        .get(usize::try_from(key.ordinal)?)
        .ok_or_else(|| anyhow!("Sync target is outside the sealed basis"))?;
    let StoredSyncTargetRow {
        requested_source_id: stored_source_id,
        source_name,
        state,
        claim_generation,
        parent_generation,
        attempt_worker: worker,
        attempt_fence_digest: fence_digest,
        reservation_incarnation: incarnation,
        reservation_token: token,
        configuration_version,
        authority_actor,
        authority_basis_digest,
        activation_observation_digest: activation_digest,
        path_observation_digest: path_digest,
        result_json,
        result_digest,
        receipt_id,
        receipt_hash,
        receipt_target,
        failure_code,
        failure_detail,
        reservation_acknowledged: acknowledged,
    } = tx.query_row(
        "SELECT requested_source_id,source_name,state,claim_generation,parent_generation,attempt_worker,attempt_fence_digest,reservation_incarnation,reservation_token,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest,result_json,result_digest,receipt_id,receipt_hash,receipt_target,failure_code,failure_detail,reservation_acknowledged FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2 AND tenant=?3",
        params![key.job_id, key.ordinal, job.tenant],
        StoredSyncTargetRow::decode,
    )?;
    ensure!(stored_source_id == source_id, "Sync target Source mismatch");
    ensure!(
        matches!(acknowledged, 0 | 1),
        "Invalid Sync acknowledgement state"
    );
    ensure!(claim_generation >= 0, "Invalid Sync claim generation");
    let capture = match (
        parent_generation,
        worker,
        fence_digest,
        incarnation,
        token,
        configuration_version,
        authority_actor,
        authority_basis_digest,
        activation_digest,
        path_digest,
    ) {
        (None, None, None, None, None, None, None, None, None, None) => None,
        (
            Some(parent_generation),
            Some(worker),
            Some(fence_digest),
            Some(incarnation),
            Some(token),
            Some(configuration_version),
            Some(authority_actor),
            Some(authority_basis_digest),
            Some(activation_digest),
            Some(path_digest),
        ) => {
            ensure!(
                claim_generation > 0 && parent_generation > 0,
                "Invalid Sync producer generation"
            );
            ensure!(
                !worker.is_empty() && !authority_actor.is_empty(),
                "Invalid Sync producer identity"
            );
            ensure!(
                valid_digest(&fence_digest),
                "Invalid Sync attempt fence digest"
            );
            ensure!(
                valid_digest(&incarnation),
                "Invalid Sync reservation incarnation"
            );
            ensure!(
                valid_digest(&authority_basis_digest),
                "Invalid Sync authority digest"
            );
            ensure!(
                valid_digest(&activation_digest),
                "Invalid Sync activation digest"
            );
            ensure!(valid_digest(&path_digest), "Invalid Sync path digest");
            ensure!(
                configuration_version > 0,
                "Invalid Sync configuration version"
            );
            let parsed_token = u64::from_str_radix(&token, 16)?;
            ensure!(
                token == token_text(parsed_token)?,
                "Invalid Sync reservation token"
            );
            Some(ProducerCapture {
                _claim_generation: claim_generation,
                _parent_generation: parent_generation,
                _worker: worker,
                _fence_digest: fence_digest,
                _incarnation: incarnation,
                _token: token,
                _configuration_version: configuration_version,
                authority_actor,
                _authority_basis_digest: authority_basis_digest,
                _activation_digest: activation_digest,
                _path_digest: path_digest,
            })
        }
        _ => return Err(anyhow!("Sync producer capture incomplete")),
    };
    let receipt_values = [
        receipt_id.is_some(),
        receipt_hash.is_some(),
        receipt_target.is_some(),
    ];
    ensure!(
        receipt_values.iter().all(|present| *present)
            || receipt_values.iter().all(|present| !*present),
        "Sync receipt capture incomplete"
    );
    let receipt = match (receipt_id, receipt_hash, receipt_target) {
        (Some(receipt_id), Some(content_hash), Some(target)) => {
            ensure!(valid_digest(&content_hash), "Invalid Sync receipt digest");
            ensure!(valid_receipt_id(&receipt_id), "Invalid Local receipt ID");
            let receipt = crate::jobs::ResultArtifact {
                receipt_id,
                content_hash,
                target,
            };
            ensure!(
                receipt.target == format!("source:{source_id}"),
                "Sync receipt Source mismatch"
            );
            Some(receipt)
        }
        (None, None, None) => None,
        _ => unreachable!(),
    };
    let result = match (result_json, result_digest) {
        (Some(result_json), Some(result_digest)) => {
            ensure!(valid_digest(&result_digest), "Invalid Sync result digest");
            let result: SourceScanResult = serde_json::from_str(&result_json)?;
            let canonical_result = serde_json::to_string(&result)?;
            ensure!(
                result_json == canonical_result,
                "Sync result is not canonical"
            );
            ensure!(
                digest(canonical_result.as_bytes()) == result_digest,
                "Sync result digest mismatch"
            );
            ensure!(
                result.project_id == u64::try_from(source_id).ok(),
                "Sync result Source mismatch"
            );
            Some(result)
        }
        (None, None) => None,
        _ => return Err(anyhow!("Sync result capture incomplete")),
    };
    let failure = || -> Result<SourceSyncFailure> {
        let detail = failure_detail
            .clone()
            .ok_or_else(|| anyhow!("Sync failure detail missing"))?;
        ensure!(
            !detail.is_empty() && detail.len() <= 1024,
            "Invalid Sync failure detail"
        );
        Ok(SourceSyncFailure {
            project_id: u64::try_from(source_id).ok(),
            name: source_name.clone(),
            error: detail,
        })
    };
    let outcome = match state.as_str() {
        "pending" => {
            ensure!(
                capture.is_none()
                    && result.is_none()
                    && receipt.is_none()
                    && failure_code.is_none()
                    && failure_detail.is_none()
                    && acknowledged == 0,
                "Invalid pending Sync target"
            );
            ValidatedTargetOutcome::Pending
        }
        "claimed" => {
            ensure!(
                capture.is_some()
                    && result.is_none()
                    && receipt.is_none()
                    && failure_code.is_none()
                    && failure_detail.is_none()
                    && acknowledged == 0,
                "Invalid claimed Sync target"
            );
            ValidatedTargetOutcome::Claimed
        }
        "succeeded" => {
            ensure!(
                capture.is_some() && failure_code.is_none() && failure_detail.is_none(),
                "Invalid successful Sync target"
            );
            ValidatedTargetOutcome::Succeeded {
                result: result.ok_or_else(|| anyhow!("Sync result missing"))?,
                _receipt: receipt.ok_or_else(|| anyhow!("Sync receipt missing"))?,
            }
        }
        "failed" => {
            ensure!(
                result.is_none() && receipt.is_none(),
                "Failed Sync target retained success evidence"
            );
            let code = failure_code
                .clone()
                .ok_or_else(|| anyhow!("Sync failure code missing"))?;
            match code.as_str() {
                "unavailable" | "unsupported_kind" => {
                    ensure!(
                        capture.is_none() && acknowledged == 1,
                        "Invalid unreserved Sync failure"
                    );
                }
                "local_read"
                | "local_settlement"
                | "configuration_changed"
                | "authority_refused" => {
                    ensure!(
                        capture.is_some(),
                        "Reserved Sync failure lacks producer capture"
                    );
                }
                _ => return Err(anyhow!("Invalid Sync failure code")),
            }
            ValidatedTargetOutcome::Failed {
                failure: failure()?,
                code,
            }
        }
        "not_run" => {
            ensure!(
                capture.is_none()
                    && result.is_none()
                    && receipt.is_none()
                    && failure_code.as_deref() == Some("batch_halt")
                    && acknowledged == 0,
                "Invalid not-run Sync target"
            );
            let _ = failure()?;
            ValidatedTargetOutcome::NotRun
        }
        "reconciliation_required" => {
            ensure!(
                capture.is_some(),
                "Sync reconciliation lacks producer capture"
            );
            ensure!(
                matches!(
                    failure_code.as_deref(),
                    Some(
                        "local_read"
                            | "local_settlement"
                            | "configuration_changed"
                            | "authority_refused"
                    )
                ),
                "Invalid Sync reconciliation reason"
            );
            if result.is_some() || receipt.is_some() {
                ensure!(
                    result.is_some() && receipt.is_some(),
                    "Sync reconciliation receipt/result mismatch"
                );
            }
            let _ = failure()?;
            ValidatedTargetOutcome::ReconciliationRequired { result }
        }
        _ => return Err(anyhow!("Invalid Sync target state")),
    };
    Ok(ValidatedTarget {
        ordinal: key.ordinal,
        source_id,
        acknowledged: acknowledged == 1,
        failure_detail,
        capture,
        outcome,
    })
}

/// Seals the batch's targets on first open. A later attempt revalidates the
/// sealed basis and returns earlier attempts' unsettled claims to pending.
/// A recognized authority refusal is returned for reporting after commit.
pub(crate) fn open(
    tx: &Transaction<'_>,
    lease: &AttemptLease,
    policy: crate::auth::AuthPolicy,
) -> Result<Option<crate::jobs::CommittedRefusal>> {
    let job = require_sync_job(tx, lease)?;
    ensure!(!job.cancel_requested, "Sync was cancelled");
    if let Err(error) = crate::jobs::require_original_authority(tx, &job, policy) {
        return refuse_authority(tx, job, error).map(Some);
    }
    if has_batch(tx, &job.job_id)? {
        validate_basis(tx, &job)?;
        tx.execute(
            "UPDATE source_sync_targets SET state='pending',parent_generation=NULL,attempt_worker=NULL,attempt_fence_digest=NULL,reservation_incarnation=NULL,reservation_token=NULL,configuration_version=NULL,authority_actor=NULL,authority_basis_digest=NULL,activation_observation_digest=NULL,path_observation_digest=NULL,updated_at=?2 WHERE job_id=?1 AND state='claimed' AND parent_generation<>?3 AND result_json IS NULL AND failure_code IS NULL",
            params![job.job_id, now(), job.generation],
        )?;
        return Ok(None);
    }
    let (selection_kind, source_ids) = match selected(&job.payload)? {
        Some(ids) => ("explicit", ids),
        None => (
            "all",
            tx.prepare(
                "SELECT id FROM projects WHERE tenant_id=?1 ORDER BY last_synced_at ASC,id ASC",
            )?
            .query_map([&job.tenant], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        ),
    };
    ensure!(source_ids.len() <= 1000, "Too many sources");
    tx.execute(
        "INSERT INTO source_sync_batches(job_id,tenant,selection_kind,target_basis_digest,target_count,phase,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'open',?6,?6)",
        params![
            job.job_id,
            job.tenant,
            selection_kind,
            basis_digest(&job.tenant, selection_kind, &source_ids)?,
            i64::try_from(source_ids.len())?,
            now()
        ],
    )?;
    for (ordinal, source_id) in source_ids.into_iter().enumerate() {
        tx.execute(
            "INSERT INTO source_sync_targets(job_id,ordinal,requested_source_id,tenant,state,updated_at) VALUES(?1,?2,?3,?4,'pending',?5)",
            params![job.job_id, i64::try_from(ordinal)?, source_id, job.tenant, now()],
        )?;
    }
    Ok(None)
}

pub(crate) fn has_batch(tx: &Transaction<'_>, job_id: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_sync_batches WHERE job_id=?1)",
        [job_id],
        |row| row.get(0),
    )?)
}

/// Fences the attempt and keeps any unsettled target evidence available for
/// reconciliation. Return the authority error only after committing the halt.
pub(crate) fn refuse_authority(
    tx: &Transaction<'_>,
    mut job: JobRecord,
    error: anyhow::Error,
) -> Result<crate::jobs::CommittedRefusal> {
    if !crate::jobs::set_sync_source_writer_authority_refusal(&mut job, &error) {
        return Err(error);
    }
    let job = halt_job(tx, job, "authority_refused", &format!("{error:#}"))?;
    Ok(crate::jobs::CommittedRefusal {
        job,
        failure: error.downcast()?,
    })
}

/// Narrows a halted batch's reservation only for targets it never claimed.
/// A target can be pending after an earlier attempt, so keep those with an
/// earlier claim generation even when the halt marked them as not run.
pub(crate) fn retained_authority_resource(tx: &Transaction<'_>, job: &JobRecord) -> Result<String> {
    validate_basis(tx, job)?;
    let sources = tx
        .prepare(
            "SELECT requested_source_id FROM source_sync_targets
             WHERE job_id=?1 AND NOT (state='not_run' AND claim_generation=0)",
        )?
        .query_map([&job.job_id], |row| Ok(row.get::<_, i64>(0)?.to_string()))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(!sources.is_empty(), "Retained Sync has no claimed targets");
    Ok(format!("source-set:{}", sources.join(",")))
}

fn progress(tx: &Transaction<'_>, job_id: &str) -> Result<u8> {
    let (done, total): (i64, i64) = tx.query_row(
        "SELECT count(*) FILTER (WHERE state IN ('succeeded','failed') AND reservation_acknowledged=1),count(*) FROM source_sync_targets WHERE job_id=?1",
        [job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(if total == 0 {
        100
    } else {
        u8::try_from((done * 100) / total)?
    })
}

/// Halts the live Sync attempt after a caller Source operation error. The
/// attempt must still own the Job. When no batch exists yet, returns `None`
/// without writing. Otherwise closes through [`halt_job`], preserving retained
/// target evidence and recording `detail` as the Job recovery message.
pub(crate) fn halt_after_error(
    tx: &Transaction<'_>,
    lease: &AttemptLease,
    code: &str,
    detail: &str,
) -> Result<Option<JobRecord>> {
    ensure!(
        matches!(
            code,
            "cancelled" | "authority_refused" | "fatal" | "uncertain_target"
        ),
        "Invalid Sync halt code"
    );
    let job = require_sync_job(tx, lease)?;
    if !has_batch(tx, &job.job_id)? {
        return Ok(None);
    }
    halt_job(tx, job, code, detail).map(Some)
}

/// Prune filter on `durable_jobs`. A terminal Job stays while any target is
/// pending, claimed or awaiting reconciliation, or holds a terminal result whose
/// reservation is not acknowledged. Cleanup of a settled batch drops its rows,
/// and the archived Job document keeps the finish result.
pub(crate) const PRUNE_HOLD: &str = " AND NOT EXISTS (SELECT 1 FROM source_sync_targets t WHERE t.job_id=durable_jobs.id AND (t.state IN ('pending','claimed','reconciliation_required') OR (t.state IN ('succeeded','failed') AND t.reservation_acknowledged=0)))";

/// Ends the Job and closes its batch. Pending targets become not-run. A
/// claimed or unacknowledged target, or a generic effect, leaves the Job for
/// reconciliation instead of a terminal state.
pub(crate) fn halt_job(
    tx: &Transaction<'_>,
    mut job: JobRecord,
    code: &str,
    detail: &str,
) -> Result<JobRecord> {
    ensure!(job.kind == JobKind::Sync, "Job is not Sync");
    ensure!(
        matches!(
            code,
            "cancelled" | "authority_refused" | "fatal" | "uncertain_target"
        ),
        "Invalid Sync halt code"
    );
    let detail: String = detail.chars().take(1024).collect();
    if code == "authority_refused"
        && matches!(
            job.state,
            PersistentState::Failed | PersistentState::ReconciliationRequired
        )
        && job.authority_refusal.as_ref().is_some_and(|refusal| {
            refusal.phase == crate::jobs::AuthorityRefusalPhase::SourceWriter
                && refusal.generation == job.generation - 1
        })
        && job.recovery.as_deref() == Some(detail.as_str())
    {
        return Ok(job);
    }
    let unresolved: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_sync_targets WHERE job_id=?1 AND (state IN ('claimed','reconciliation_required') OR (state IN ('succeeded','failed') AND reservation_acknowledged=0)))",
        [&job.job_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "UPDATE source_sync_targets SET state='not_run',failure_code='batch_halt',failure_detail=?2,updated_at=?3 WHERE job_id=?1 AND state='pending'",
        params![job.job_id, detail, now()],
    )?;
    if unresolved || !job.effects.is_empty() {
        let reason = if code == "authority_refused" {
            "authority_refused"
        } else {
            "local_settlement"
        };
        tx.execute(
            "UPDATE source_sync_targets SET state='reconciliation_required',failure_code=?2,failure_detail=?3,updated_at=?4 WHERE job_id=?1 AND state='claimed'",
            params![job.job_id, reason, detail, now()],
        )?;
        tx.execute(
            "UPDATE source_sync_batches SET phase='reconciliation_required',halt_code=NULL,updated_at=?2 WHERE job_id=?1 AND phase='open'",
            params![job.job_id, now()],
        )?;
        job.state = PersistentState::ReconciliationRequired;
    } else {
        tx.execute(
            "UPDATE source_sync_batches SET phase='halted',halt_code=?2,updated_at=?3 WHERE job_id=?1 AND phase='open'",
            params![job.job_id, code, now()],
        )?;
        job.state = if code == "cancelled" {
            PersistentState::Cancelled
        } else {
            PersistentState::Failed
        };
    }
    job.progress = Some(progress(tx, &job.job_id)?);
    job.public_result = None;
    job.result = None;
    job.recovery = Some(detail);
    job.generation += 1;
    job.fence = None;
    job.worker = None;
    job.lease_until = None;
    crate::jobs::save(tx, &mut job, "source_sync_halted")?;
    Ok(job)
}

fn failure(tx: &Transaction<'_>, key: &SyncTargetKey) -> Result<SourceSyncFailure> {
    Ok(tx.query_row(
        "SELECT requested_source_id,source_name,failure_detail FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2 AND state='failed'",
        params![key.job_id, key.ordinal],
        |row| {
            Ok(SourceSyncFailure {
                project_id: u64::try_from(row.get::<_, i64>(0)?).ok(),
                name: row.get(1)?,
                error: row.get(2)?,
            })
        },
    )?)
}

/// Claims the batch's next pending target. A missing or non-Local Source
/// fails the target at once. A Local target records the attempt, the Source
/// observation and a new reservation token, which is activated only after
/// the commit.
pub(crate) fn next(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    policy: crate::auth::AuthPolicy,
) -> Result<Next> {
    let loaded =
        crate::jobs::load(tx, lease.job_id())?.ok_or_else(|| anyhow!("Sync Job not found"))?;
    ensure!(loaded.kind == JobKind::Sync, "Attempt is not a Sync Job");
    ensure!(loaded.tenant == lease.tenant(), "Sync next tenant mismatch");
    if loaded.state == PersistentState::ReconciliationRequired || loaded.state.terminal() {
        return Ok(Next::Done(NextSyncTarget::Halted, loaded.state_version));
    }
    let mut job = require_sync_job(tx, lease)?;
    if job.cancel_requested {
        let halted = halt_job(tx, job, "cancelled", "Sync was cancelled")?;
        return Ok(Next::Done(NextSyncTarget::Halted, halted.state_version));
    }
    let authority = match crate::jobs::require_original_authority(tx, &job, policy) {
        Ok(authority) => authority,
        Err(error) => {
            if !crate::jobs::set_sync_source_writer_authority_refusal(&mut job, &error) {
                return Err(error);
            }
            let halted = halt_job(tx, job, "authority_refused", &format!("{error:#}"))?;
            return Ok(Next::Done(NextSyncTarget::Halted, halted.state_version));
        }
    };
    let phase: Option<String> = tx
        .query_row(
            "SELECT phase FROM source_sync_batches WHERE job_id=?1",
            [&job.job_id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(phase.as_deref() == Some("open"), "Sync batch is not open");
    let row: Option<(i64, i64, String, i64, i64)> = tx
        .query_row(
            "SELECT ordinal,requested_source_id,state,claim_generation,reservation_acknowledged FROM source_sync_targets WHERE job_id=?1 AND NOT (state IN ('succeeded','failed') AND reservation_acknowledged=1) ORDER BY ordinal LIMIT 1",
            [&job.job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((ordinal, source_id, target_state, claim_generation, acknowledged)) = row else {
        return Ok(Next::Done(NextSyncTarget::Complete, job.state_version));
    };
    if target_state == "failed" && acknowledged == 0 {
        let key = SyncTargetKey {
            job_id: job.job_id,
            ordinal: u32::try_from(ordinal)?,
        };
        return Ok(Next::Done(
            NextSyncTarget::RetainedFailure(failure(tx, &key)?),
            job.state_version,
        ));
    }
    ensure!(
        target_state == "pending",
        "Prior Sync target is not pending"
    );
    let key = SyncTargetKey {
        job_id: job.job_id.clone(),
        ordinal: u32::try_from(ordinal)?,
    };
    let source: Option<(String, String, String, String, String, Option<String>)> = tx
        .query_row(
            "SELECT name,source_type,source_kind,url,branch,tag FROM projects WHERE tenant_id=?1 AND id=?2",
            params![job.tenant, source_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    // `source_kind` selects the GitHub path. A row whose `source_type` was
    // rewritten to `github` while its kind stayed local is still unsupported.
    let (source_name, github) = match source {
        None => {
            tx.execute(
                "UPDATE source_sync_targets SET state='failed',failure_code='unavailable',failure_detail='Source not found',reservation_acknowledged=1,updated_at=?3 WHERE job_id=?1 AND ordinal=?2 AND state='pending'",
                params![key.job_id, key.ordinal, now()],
            )?;
            return ordinary_failure(tx, job, &key);
        }
        Some((name, _, kind, url, branch, tag)) if kind == "github" => (
            name.clone(),
            Some(GithubHandoff {
                url,
                branch,
                tag,
                name,
                token: github_token(tx, &job.tenant)?,
            }),
        ),
        Some((name, source_type, _, _, _, _)) if source_type != "local" => {
            tx.execute(
                "UPDATE source_sync_targets SET source_name=?3,state='failed',failure_code='unsupported_kind',failure_detail='Source kind is not supported by Local Sync',reservation_acknowledged=1,updated_at=?4 WHERE job_id=?1 AND ordinal=?2 AND state='pending'",
                params![key.job_id, key.ordinal, name, now()],
            )?;
            activity::record(tx, &job.tenant, source_id, SourceOutcome::SyncFailed)?;
            return ordinary_failure(tx, job, &key);
        }
        Some((name, _, _, _, _, _)) => (name, None),
    };
    let durably_held: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_sync_targets WHERE tenant=?1 AND requested_source_id=?2 AND (state='claimed' OR (state IN ('succeeded','failed') AND reservation_acknowledged=0)))",
        params![job.tenant, source_id],
        |row| row.get(0),
    )?;
    ensure!(
        !durably_held && !state.source_busy(&job.tenant, source_id),
        SourceBusy
    );
    let token = state.next_token()?;
    let observation = current_observation(tx, &job.tenant, source_id, authority.subject())?;
    ensure!(
        tx.execute(
            "UPDATE source_sync_targets SET source_name=?3,state='claimed',claim_generation=claim_generation+1,parent_generation=?4,attempt_worker=?5,attempt_fence_digest=?6,reservation_incarnation=?7,reservation_token=?8,configuration_version=?9,authority_actor=?10,authority_basis_digest=?11,activation_observation_digest=?12,path_observation_digest=?13,updated_at=?14 WHERE job_id=?1 AND ordinal=?2 AND state='pending' AND claim_generation=?15",
            params![
                key.job_id,
                key.ordinal,
                source_name,
                job.generation,
                lease.worker(),
                digest(lease.fence()),
                state.incarnation(),
                token_text(token)?,
                observation.configuration_version,
                observation.authority_actor,
                observation.authority_basis_digest,
                observation.activation_observation_digest,
                observation.path_observation_digest,
                now(),
                claim_generation
            ],
        )? == 1,
        "Sync target claim changed"
    );
    Ok(Next::Reserve(ReservedTarget {
        key,
        observation,
        claim_generation: claim_generation + 1,
        token,
        binding: SourceBinding {
            tenant: job.tenant,
            id: source_id,
        },
        version: job.state_version,
        github,
    }))
}

/// The observation a claim records and later rechecks. A GitHub Source hashes
/// its identity, not the revision and path activation writes, so settling
/// after publication still matches. Every other kind keeps the local scan.
fn current_observation(
    tx: &Transaction<'_>,
    tenant: &str,
    source_id: i64,
    actor: &str,
) -> Result<LocalScanObservation> {
    let kind: String = tx.query_row(
        "SELECT source_kind FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, source_id],
        |row| row.get(0),
    )?;
    if kind == "github" {
        crate::source_scan::github_observation(tx, tenant, source_id, actor)
    } else {
        crate::source_scan::project_observation(tx, tenant, source_id, actor)
    }
}

fn github_token(tx: &Transaction<'_>, tenant: &str) -> Result<Option<HeldToken>> {
    match crate::snapshot(tx, tenant, "github_pat")? {
        crate::SettingSnapshot::Stored { value } if !value.trim().is_empty() => {
            Ok(Some(HeldToken(value)))
        }
        _ => Ok(None),
    }
}

/// Binds a candidate to its stored claim, then checks its content against the
/// recorded basis. The attempt must be live, the batch open and its sealed
/// basis intact, the target claimed by this attempt at the candidate's
/// generation, and the held token the one recorded and still live for the
/// target's Source. Content must keep the recorded Source configuration,
/// observation and revision, and any result must carry that Source's
/// provenance. Writes nothing.
pub(crate) fn bind(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    candidate: &SyncCandidate,
) -> Result<BoundSyncTarget> {
    let job = require_sync_job(tx, lease)?;
    bind_in_job(tx, state, lease, candidate, &job)
}

fn bind_in_job(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    candidate: &SyncCandidate,
    job: &JobRecord,
) -> Result<BoundSyncTarget> {
    let key = &candidate.key;
    ensure!(key.job_id == job.job_id, "Sync target Job mismatch");
    ensure!(!job.cancel_requested, "Sync was cancelled");
    let phase: Option<String> = tx
        .query_row(
            "SELECT phase FROM source_sync_batches WHERE job_id=?1",
            [&job.job_id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(phase.as_deref() == Some("open"), "Sync batch is not open");
    let source_id = *validate_basis(tx, job)?
        .get(usize::try_from(key.ordinal)?)
        .ok_or_else(|| anyhow!("Sync target is outside the sealed basis"))?;
    type Claim = (
        String,
        i64,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let (target_state, claim_generation, parent_generation, worker, fence, incarnation, token): Claim = tx.query_row(
        "SELECT state,claim_generation,parent_generation,attempt_worker,attempt_fence_digest,reservation_incarnation,reservation_token FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2",
        params![key.job_id, key.ordinal],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    )?;
    ensure!(target_state == "claimed", "Sync target is not claimed");
    ensure!(
        claim_generation == candidate.claim_generation,
        "Sync target claim generation mismatch"
    );
    ensure!(
        parent_generation == Some(job.generation)
            && worker.as_deref() == Some(lease.worker())
            && fence == Some(digest(lease.fence())),
        "Sync target attempt mismatch"
    );
    let proof = &candidate.proof;
    ensure!(
        incarnation.as_deref() == Some(proof.incarnation.as_str())
            && token == Some(token_text(proof.token)?),
        "Sync target reservation mismatch"
    );
    crate::source_scan::require_live_reservation(
        state,
        proof,
        &SourceBinding {
            tenant: job.tenant.clone(),
            id: source_id,
        },
    )?;
    let bound = BoundSyncTarget {
        key: key.clone(),
        source_id,
        claim_generation,
    };
    validate_target(tx, job, &bound)?;
    Ok(bound)
}

/// Stores one successful result for a claimed target and marks that
/// reservation released. The attempt and the held Source proof must still be
/// the claim's. An invalid claim writes nothing. The Job row and the batch stay as
/// they were; the live token is retired only after this write commits.
pub(crate) fn settle(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    job: &JobRecord,
    candidate: SyncCandidate,
    settlement: SyncSettlement,
    github: Option<crate::uploads::GithubSnapshot>,
) -> Result<SettledSyncRelease> {
    let bound = bind_in_job(tx, state, lease, &candidate, job)?;
    let SyncSettlement { result, receipt } = settlement;
    let result_json = serde_json::to_string(&result)?;
    let result_digest = digest(result_json.as_bytes());
    validate_result_provenance(
        bound.source_id,
        Some(result_json.clone()),
        Some(result_digest.clone()),
        Some(receipt.receipt_id.clone()),
        Some(receipt.content_hash.clone()),
        Some(receipt.target.clone()),
    )?;
    if let Some(snapshot) = github {
        ensure!(
            receipt.content_hash == snapshot.manifest_digest,
            "Sync receipt does not match the GitHub snapshot"
        );
        crate::uploads::activate_github(tx, state, lease, &bound, snapshot)?;
    } else {
        let github: bool = tx.query_row(
            "SELECT source_kind='github' FROM projects WHERE tenant_id=?1 AND id=?2",
            params![job.tenant, bound.source_id],
            |row| row.get(0),
        )?;
        ensure!(!github, "GitHub Sync settlement requires a snapshot");
    }
    ensure!(
        tx.execute(
            "UPDATE source_sync_targets SET state='succeeded',result_json=?3,result_digest=?4,receipt_id=?5,receipt_hash=?6,receipt_target=?7,reservation_acknowledged=1,updated_at=?8 WHERE job_id=?1 AND ordinal=?2 AND state='claimed' AND result_json IS NULL AND result_digest IS NULL AND receipt_id IS NULL AND receipt_hash IS NULL AND receipt_target IS NULL",
            params![
                bound.key.job_id,
                bound.key.ordinal,
                result_json,
                result_digest,
                receipt.receipt_id,
                receipt.content_hash,
                receipt.target,
                now(),
            ],
        )? == 1,
        "Sync target settlement changed"
    );
    activity::record(tx, &job.tenant, bound.source_id, SourceOutcome::Updated)?;
    let proof = candidate.proof;
    let binding = SourceBinding {
        tenant: lease.tenant().to_owned(),
        id: bound.source_id,
    };
    Ok(SettledSyncRelease {
        settled: SettledSyncTarget {
            key: bound.key,
            source_id: bound.source_id,
            result,
            receipt,
        },
        proof,
        binding,
    })
}

/// Records a local read failure for one claimed target and the Source failure
/// observation. The reservation stays held and unacknowledged, so progress
/// does not count this target. An invalid claim, including a target that already
/// stores a successful result, writes nothing.
pub(crate) fn fail(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    mut job: JobRecord,
    candidate: SyncCandidate,
    detail: &str,
) -> Result<FailedSyncTarget> {
    ensure!(
        !detail.is_empty() && detail.len() <= 1024,
        "Invalid Local Sync failure"
    );
    let bound = bind_in_job(tx, state, lease, &candidate, &job)?;
    let stores_result: bool = tx.query_row(
        "SELECT result_json IS NOT NULL OR result_digest IS NOT NULL OR receipt_id IS NOT NULL OR receipt_hash IS NOT NULL OR receipt_target IS NOT NULL FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2 AND state='claimed'",
        params![bound.key.job_id, bound.key.ordinal],
        |row| row.get(0),
    )?;
    ensure!(!stores_result, "Sync target already stores a result");
    crate::source_scan::mark_local_failure(tx, lease.tenant(), bound.source_id, detail)?;
    ensure!(
        tx.execute(
            "UPDATE source_sync_targets SET state='failed',failure_code='local_read',failure_detail=?3,updated_at=?4 WHERE job_id=?1 AND ordinal=?2 AND state='claimed' AND reservation_acknowledged=0 AND result_json IS NULL AND result_digest IS NULL AND receipt_id IS NULL AND receipt_hash IS NULL AND receipt_target IS NULL",
            params![bound.key.job_id, bound.key.ordinal, detail, now()],
        )? == 1,
        "Sync target failure changed"
    );
    activity::record(tx, &job.tenant, bound.source_id, SourceOutcome::SyncFailed)?;
    let failure = failure(tx, &bound.key)?;
    let recorded = progress(tx, &job.job_id)?;
    job.progress = Some(recorded);
    crate::jobs::save(tx, &mut job, "source_sync_progress")?;
    Ok(FailedSyncTarget {
        key: bound.key,
        failure,
        progress: recorded,
        version: job.state_version,
    })
}

/// Acknowledges one retained local-read failure when the presented observation
/// is the claim's and this attempt still owns the target. The stored failure
/// stays in place. Exact replay returns it and writes nothing. Any other
/// refusal writes nothing. The caller retires the live token after commit.
pub(crate) fn acknowledge(
    tx: &Transaction<'_>,
    state: &State,
    lease: &AttemptLease,
    candidate: SyncCandidate,
    observation: &LocalScanObservation,
) -> Result<AcknowledgedSyncRelease> {
    let SyncCandidate {
        key,
        claim_generation: presented_generation,
        proof,
    } = candidate;
    let mut job = require_sync_job(tx, lease)?;
    ensure!(key.job_id == job.job_id, "Sync target Job mismatch");
    let phase: Option<String> = tx
        .query_row(
            "SELECT phase FROM source_sync_batches WHERE job_id=?1",
            [&job.job_id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(phase.as_deref() == Some("open"), "Sync batch is not open");
    let row = tx
        .query_row(
            "SELECT requested_source_id,state,claim_generation,parent_generation,attempt_worker,attempt_fence_digest,reservation_incarnation,reservation_token,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest,reservation_acknowledged FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2",
            params![key.job_id, key.ordinal],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("Sync target not found"))?;
    let (
        source_id,
        target_state,
        claim_generation,
        parent_generation,
        worker,
        fence,
        incarnation,
        token,
        configuration_version,
        authority_actor,
        authority_basis_digest,
        activation_observation_digest,
        path_observation_digest,
        acknowledged,
    ) = row;
    ensure!(
        target_state == "failed",
        "Sync target is not retained evidence"
    );
    let (
        Some(parent_generation),
        Some(worker),
        Some(fence),
        Some(incarnation),
        Some(token),
        Some(configuration_version),
        Some(authority_actor),
        Some(authority_basis_digest),
        Some(activation_observation_digest),
        Some(path_observation_digest),
    ) = (
        parent_generation,
        worker,
        fence,
        incarnation,
        token,
        configuration_version,
        authority_actor,
        authority_basis_digest,
        activation_observation_digest,
        path_observation_digest,
    )
    else {
        return Err(anyhow!("Sync producer capture incomplete"));
    };
    ensure!(
        claim_generation == presented_generation,
        "Sync target claim generation mismatch"
    );
    ensure!(
        parent_generation == job.generation
            && worker == lease.worker()
            && fence == digest(lease.fence()),
        "Sync target attempt mismatch"
    );
    ensure!(
        incarnation == proof.incarnation && token == token_text(proof.token)?,
        "Sync target acknowledgement mismatch"
    );
    ensure!(
        observation.source_id() == source_id
            && observation.configuration_version == configuration_version
            && observation.authority_actor == authority_actor
            && observation.authority_basis_digest == authority_basis_digest
            && observation.activation_observation_digest == activation_observation_digest
            && observation.path_observation_digest == path_observation_digest,
        "Sync acknowledgement observation mismatch"
    );
    let binding = SourceBinding {
        tenant: job.tenant.clone(),
        id: source_id,
    };
    let live = match state.classify_issued(&proof.incarnation, proof.token) {
        IssuedTokenState::Active(actual) if actual == &binding => true,
        IssuedTokenState::Retired => false,
        IssuedTokenState::Active(_)
        | IssuedTokenState::NeverIssued
        | IssuedTokenState::WrongIncarnation => {
            return Err(anyhow!("Sync target acknowledgement mismatch"));
        }
    };
    ensure!(
        acknowledged == 1 || live,
        "Sync target acknowledgement mismatch"
    );
    let failure = failure(tx, &key)?;
    let (recorded, version) = if acknowledged == 0 {
        ensure!(
            tx.execute(
                "UPDATE source_sync_targets SET reservation_acknowledged=1,updated_at=?3 WHERE job_id=?1 AND ordinal=?2 AND state='failed' AND reservation_acknowledged=0",
                params![key.job_id, key.ordinal, now()],
            )? == 1,
            "Sync target acknowledgement changed"
        );
        let recorded = progress(tx, &job.job_id)?;
        job.progress = Some(recorded);
        crate::jobs::save(tx, &mut job, "source_sync_acknowledged")?;
        (recorded, job.state_version)
    } else {
        (progress(tx, &job.job_id)?, job.state_version)
    };
    Ok(AcknowledgedSyncRelease {
        acknowledged: AcknowledgedSyncTarget {
            key,
            failure,
            progress: recorded,
            version,
        },
        proof,
        binding,
    })
}

/// Returns one already-acknowledged successful Sync target result when this
/// attempt still owns the claim and the live Source observation matches the
/// capture stored at claim. Writes nothing. A changed observation or a stale
/// attempt is refused without writing, so the caller does not rerun Source work
/// under mismatched provenance.
pub(crate) fn adopt(
    tx: &Transaction<'_>,
    lease: &AttemptLease,
    key: &SyncTargetKey,
) -> Result<SettledSyncTarget> {
    let job = require_sync_job(tx, lease)?;
    ensure!(key.job_id == job.job_id, "Sync target Job mismatch");
    let phase: Option<String> = tx
        .query_row(
            "SELECT phase FROM source_sync_batches WHERE job_id=?1",
            [&job.job_id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(phase.as_deref() == Some("open"), "Sync batch is not open");
    let row = tx
        .query_row(
            "SELECT requested_source_id,state,parent_generation,attempt_worker,attempt_fence_digest,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest,result_json,result_digest,receipt_id,receipt_hash,receipt_target,reservation_acknowledged FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2",
            params![key.job_id, key.ordinal],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, i64>(15)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("Sync target not found"))?;
    let (
        source_id,
        target_state,
        parent_generation,
        worker,
        fence,
        configuration_version,
        authority_actor,
        authority_basis_digest,
        activation_observation_digest,
        path_observation_digest,
        result_json,
        result_digest,
        receipt_id,
        receipt_hash,
        receipt_target,
        acknowledged,
    ) = row;
    ensure!(
        target_state == "succeeded" && acknowledged == 1,
        "Sync target is not an acknowledged success"
    );
    let (
        Some(parent_generation),
        Some(worker),
        Some(fence),
        Some(configuration_version),
        Some(authority_actor),
        Some(authority_basis_digest),
        Some(activation_observation_digest),
        Some(path_observation_digest),
        Some(result_json),
        Some(result_digest),
        Some(receipt_id),
        Some(receipt_hash),
        Some(receipt_target),
    ) = (
        parent_generation,
        worker,
        fence,
        configuration_version,
        authority_actor,
        authority_basis_digest,
        activation_observation_digest,
        path_observation_digest,
        result_json,
        result_digest,
        receipt_id,
        receipt_hash,
        receipt_target,
    )
    else {
        return Err(anyhow!("Sync producer capture incomplete"));
    };
    ensure!(
        parent_generation == job.generation
            && worker == lease.worker()
            && fence == digest(lease.fence()),
        "Sync target attempt mismatch"
    );
    let observation = current_observation(tx, &job.tenant, source_id, &authority_actor)?;
    ensure!(
        observation.configuration_version == configuration_version
            && observation.authority_actor == authority_actor
            && observation.authority_basis_digest == authority_basis_digest
            && observation.activation_observation_digest == activation_observation_digest
            && observation.path_observation_digest == path_observation_digest,
        "Sync adoption provenance mismatch"
    );
    validate_result_provenance(
        source_id,
        Some(result_json.clone()),
        Some(result_digest),
        Some(receipt_id.clone()),
        Some(receipt_hash.clone()),
        Some(receipt_target.clone()),
    )?;
    let result: SourceScanResult = serde_json::from_str(&result_json)?;
    Ok(SettledSyncTarget {
        key: key.clone(),
        source_id,
        result,
        receipt: ResultArtifact {
            receipt_id,
            content_hash: receipt_hash,
            target: receipt_target,
        },
    })
}

fn ordinary_failure(tx: &Transaction<'_>, mut job: JobRecord, key: &SyncTargetKey) -> Result<Next> {
    let failure = failure(tx, key)?;
    job.progress = Some(progress(tx, &job.job_id)?);
    crate::jobs::save(tx, &mut job, "source_sync_progress")?;
    Ok(Next::Done(
        NextSyncTarget::OrdinaryFailure(failure),
        job.state_version,
    ))
}

struct InspectedSyncTargetRow {
    source_id: i64,
    tenant: String,
    source_name: Option<String>,
    state: String,
    result_json: Option<String>,
    result_digest: Option<String>,
    receipt_id: Option<String>,
    receipt_hash: Option<String>,
    receipt_target: Option<String>,
    failure_detail: Option<String>,
    acknowledged: i64,
}

/// Reads one live Sync target through the authorized attempt. The batch basis
/// and target tenant must match the Job. Writes nothing and does not touch
/// reservation ownership.
pub(crate) fn inspect(
    tx: &Transaction<'_>,
    lease: &AttemptLease,
    key: &SyncTargetKey,
    policy: crate::auth::AuthPolicy,
) -> Result<SyncTargetInspection> {
    let job = require_sync_job(tx, lease)?;
    ensure!(key.job_id == job.job_id, "Sync target Job mismatch");
    crate::jobs::require_original_authority(tx, &job, policy)?;
    let source_ids = validate_basis(tx, &job)?;
    let source_id = *source_ids
        .get(usize::try_from(key.ordinal)?)
        .ok_or_else(|| anyhow!("Sync target is outside the sealed basis"))?;
    let row = tx
        .query_row(
            "SELECT requested_source_id,tenant,source_name,state,result_json,result_digest,receipt_id,receipt_hash,receipt_target,failure_detail,reservation_acknowledged FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2",
            params![key.job_id, key.ordinal],
            |row| {
                Ok(InspectedSyncTargetRow {
                    source_id: row.get(0)?,
                    tenant: row.get(1)?,
                    source_name: row.get(2)?,
                    state: row.get(3)?,
                    result_json: row.get(4)?,
                    result_digest: row.get(5)?,
                    receipt_id: row.get(6)?,
                    receipt_hash: row.get(7)?,
                    receipt_target: row.get(8)?,
                    failure_detail: row.get(9)?,
                    acknowledged: row.get(10)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("Sync target not found"))?;
    ensure!(row.tenant == job.tenant, "Sync target tenant mismatch");
    ensure!(row.source_id == source_id, "Sync target Source mismatch");
    if matches!(row.state.as_str(), "succeeded" | "failed") && row.acknowledged == 0 {
        return Ok(SyncTargetInspection::ReconciliationRequired);
    }
    match row.state.as_str() {
        "pending" | "claimed" => Ok(SyncTargetInspection::Pending),
        "succeeded" => {
            validate_result_provenance(
                source_id,
                row.result_json.clone(),
                row.result_digest,
                row.receipt_id,
                row.receipt_hash,
                row.receipt_target,
            )?;
            let result_json = row
                .result_json
                .ok_or_else(|| anyhow!("Sync result missing"))?;
            let result: SourceScanResult = serde_json::from_str(&result_json)?;
            Ok(SyncTargetInspection::Succeeded(result))
        }
        "failed" => {
            let detail = row
                .failure_detail
                .ok_or_else(|| anyhow!("Sync failure detail missing"))?;
            ensure!(
                !detail.is_empty() && detail.len() <= 1024,
                "Invalid Sync failure detail"
            );
            Ok(SyncTargetInspection::Failed(SourceSyncFailure {
                project_id: u64::try_from(source_id).ok(),
                name: row.source_name,
                error: detail,
            }))
        }
        "not_run" => Ok(SyncTargetInspection::Halted),
        "reconciliation_required" => Ok(SyncTargetInspection::ReconciliationRequired),
        _ => Err(anyhow!("Invalid Sync target state")),
    }
}

/// Reads one retained Sync Job. Validates batch phase, sealed-basis bindings,
/// producer capture, observation digests and result provenance, then reports
/// dispositions without writing.
pub(crate) fn inspect_retained(
    tx: &Transaction<'_>,
    access: &AuthorizedSyncRead,
) -> Result<SyncRecoveryInspection> {
    let job = access.job();
    let source_ids = validate_basis(tx, job)?;
    let batch_phase: String = tx.query_row(
        "SELECT phase FROM source_sync_batches WHERE job_id=?1 AND tenant=?2",
        params![job.job_id, job.tenant],
        |row| row.get(0),
    )?;
    ensure!(
        matches!(
            (job.state, batch_phase.as_str()),
            (PersistentState::Succeeded, "completed")
                | (PersistentState::Failed, "failed" | "halted")
                | (PersistentState::Cancelled, "halted")
                | (
                    PersistentState::ReconciliationRequired,
                    "reconciliation_required"
                )
        ),
        "Sync recovery batch phase mismatch"
    );
    let mut targets = Vec::with_capacity(source_ids.len());
    for ordinal in 0..source_ids.len() {
        let target = validate_target_in_basis(
            tx,
            job,
            &SyncTargetKey {
                job_id: job.job_id.clone(),
                ordinal: u32::try_from(ordinal)?,
            },
            &source_ids,
        )?;
        if let (Some(expected), Some(capture)) =
            (access.original_subject(), target.capture.as_ref())
        {
            ensure!(
                expected == capture.authority_actor,
                "Sync recovery authority capture mismatch"
            );
        }
        let (disposition, result) = match target.outcome {
            ValidatedTargetOutcome::Pending => (SyncRecoveryDisposition::Pending, None),
            ValidatedTargetOutcome::Claimed => (SyncRecoveryDisposition::Claimed, None),
            ValidatedTargetOutcome::Succeeded { result, .. } => {
                (SyncRecoveryDisposition::Succeeded, Some(result))
            }
            ValidatedTargetOutcome::Failed { .. } => (SyncRecoveryDisposition::Failed, None),
            ValidatedTargetOutcome::NotRun => (SyncRecoveryDisposition::NotRun, None),
            ValidatedTargetOutcome::ReconciliationRequired { result } => {
                (SyncRecoveryDisposition::ReconciliationRequired, result)
            }
        };
        targets.push(SyncRecoveryTarget {
            ordinal: target.ordinal,
            requested_source_id: u64::try_from(target.source_id)?,
            disposition,
            failure: target.failure_detail,
            result,
            acknowledged: target.acknowledged,
        });
    }
    let state = match job.state {
        PersistentState::Succeeded => SyncRecoveryState::Completed,
        PersistentState::Failed => SyncRecoveryState::Failed,
        PersistentState::Cancelled => SyncRecoveryState::Cancelled,
        PersistentState::ReconciliationRequired => SyncRecoveryState::ReconciliationRequired,
        _ => return Err(anyhow!("Sync recovery Job is not retained")),
    };
    Ok(SyncRecoveryInspection {
        job_id: job.job_id.clone(),
        version: job.state_version,
        state,
        targets,
        original_authority: access.original_authority,
    })
}

fn validated_terminal_aggregate(tx: &Transaction<'_>, job: &JobRecord) -> Result<SourceSyncResult> {
    let source_ids = validate_basis(tx, job)?;
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for ordinal in 0..source_ids.len() {
        let target = validate_target_in_basis(
            tx,
            job,
            &SyncTargetKey {
                job_id: job.job_id.clone(),
                ordinal: u32::try_from(ordinal)?,
            },
            &source_ids,
        )?;
        ensure!(target.acknowledged, "Sync target is not acknowledged");
        match target.outcome {
            ValidatedTargetOutcome::Succeeded { result, .. } => results.push(result),
            ValidatedTargetOutcome::Failed { failure, code } => {
                ensure!(
                    matches!(
                        code.as_str(),
                        "unavailable" | "unsupported_kind" | "local_read"
                    ),
                    "Sync terminal failure requires reconciliation"
                );
                failures.push(failure);
            }
            _ => return Err(anyhow!("Sync target is not terminal")),
        }
    }
    Ok(SourceSyncResult {
        synced: u64::try_from(results.len())?,
        failed: u64::try_from(failures.len())?,
        results,
        failures,
    })
}

/// Finishes an open Sync batch once every target has a valid acknowledged
/// terminal result. Exact retry returns the same aggregate without writing.
pub(crate) fn finish(
    tx: &Transaction<'_>,
    lease: &AttemptLease,
    policy: crate::auth::AuthPolicy,
) -> Result<SyncCommandResult<SourceSyncResult>> {
    let loaded =
        crate::jobs::load(tx, lease.job_id())?.ok_or_else(|| anyhow!("Sync Job not found"))?;
    ensure!(loaded.kind == JobKind::Sync, "Attempt is not a Sync Job");
    ensure!(
        loaded.tenant == lease.tenant(),
        "Sync finish tenant mismatch"
    );
    if loaded.state == PersistentState::ReconciliationRequired {
        return Err(anyhow!("Inspect retained Sync recovery evidence"));
    }
    if loaded.state.terminal() {
        ensure!(
            loaded.generation == lease.generation(),
            "Sync finish generation mismatch"
        );
        ensure!(
            loaded.state_version == lease.version() + 1,
            "Sync finish retry version mismatch"
        );
        crate::jobs::require_original_authority(tx, &loaded, policy)?;
        let (phase, aggregate_json, aggregate_digest): (String, String, String) = tx.query_row(
            "SELECT phase,aggregate_json,aggregate_digest FROM source_sync_batches WHERE job_id=?1 AND tenant=?2",
            params![loaded.job_id, loaded.tenant],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        ensure!(
            matches!(phase.as_str(), "completed" | "failed"),
            "Sync finish retry batch mismatch"
        );
        let stored_aggregate: SourceSyncResult = serde_json::from_str(&aggregate_json)?;
        let canonical_aggregate = serde_json::to_string(&stored_aggregate)?;
        ensure!(
            aggregate_json == canonical_aggregate,
            "Sync finish retry aggregate is not canonical"
        );
        ensure!(
            digest(canonical_aggregate.as_bytes()) == aggregate_digest,
            "Sync finish retry aggregate mismatch"
        );
        let aggregate = validated_terminal_aggregate(tx, &loaded)?;
        ensure!(
            aggregate == stored_aggregate,
            "Sync finish retry target aggregate mismatch"
        );
        if aggregate.synced == 0 && aggregate.failed > 0 {
            ensure!(
                loaded.state == PersistentState::Failed && loaded.public_result.is_none(),
                "Sync finish retry failed aggregate mismatch"
            );
        } else {
            ensure!(
                matches!(&loaded.public_result, Some(CompletedResult::SourceSync(value)) if value == &aggregate),
                "Sync finish retry public result mismatch"
            );
        }
        return Ok(SyncCommandResult::read_only(aggregate, loaded));
    }
    let mut job = require_sync_job(tx, lease)?;
    ensure!(!job.cancel_requested, "Sync was cancelled");
    ensure!(job.effects.is_empty(), "Sync has generic effect evidence");
    crate::jobs::require_original_authority(tx, &job, policy)?;
    let (batch_tenant, selection_kind, target_basis_digest, target_count, batch_phase): (
        String,
        String,
        String,
        i64,
        String,
    ) = tx.query_row(
        "SELECT tenant,selection_kind,target_basis_digest,target_count,phase FROM source_sync_batches WHERE job_id=?1",
        [&job.job_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    ensure!(batch_tenant == job.tenant, "Sync batch tenant mismatch");
    ensure!(batch_phase == "open", "Sync batch is not open");
    let aggregate = validated_terminal_aggregate(tx, &job)?;
    let aggregate_json = serde_json::to_string(&aggregate)?;
    let aggregate_digest = digest(aggregate_json.as_bytes());
    let all_failed = aggregate.synced == 0 && aggregate.failed > 0;
    job.progress = Some(100);
    job.result = None;
    if all_failed {
        job.state = PersistentState::Failed;
        job.public_result = None;
        let joined = aggregate
            .failures
            .iter()
            .map(|failure| {
                format!(
                    "{}: {}",
                    failure
                        .project_id
                        .map_or_else(|| "unknown".into(), |id| id.to_string()),
                    failure.error
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        job.recovery = Some(joined.chars().take(1024).collect());
    } else {
        job.state = PersistentState::Succeeded;
        job.public_result = Some(CompletedResult::SourceSync(aggregate.clone()));
        job.recovery = None;
    }
    ensure!(
        tx.execute(
            "UPDATE source_sync_batches SET phase=?2,aggregate_json=?3,aggregate_digest=?4,updated_at=?5 WHERE job_id=?1 AND phase='open' AND tenant=?6 AND selection_kind=?7 AND target_basis_digest=?8 AND target_count=?9",
            params![job.job_id, if all_failed { "failed" } else { "completed" }, aggregate_json, aggregate_digest, now(), job.tenant, selection_kind, target_basis_digest, target_count],
        )? == 1,
        "Sync batch terminal update changed"
    );
    crate::jobs::save(
        tx,
        &mut job,
        if all_failed {
            "source_sync_failed"
        } else {
            "source_sync_completed"
        },
    )?;
    Ok(SyncCommandResult::committed(aggregate, job))
}

pub(crate) fn validate_schema(connection: &Connection, version: u64) -> Result<()> {
    let objects = |connection: &Connection| -> Result<Vec<(String, Option<String>)>> {
        Ok(connection
            .prepare("SELECT name,sql FROM sqlite_master WHERE name LIKE 'source_sync_%' OR name LIKE 'sqlite_autoindex_source_sync_%' ORDER BY name")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    };
    let actual = objects(connection)?;
    if version < 41 {
        ensure!(actual.is_empty(), "Unexpected Source Sync schema");
        return Ok(());
    }
    let expected = Connection::open_in_memory()?;
    expected.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE durable_jobs(id TEXT PRIMARY KEY); CREATE TABLE projects(id INTEGER PRIMARY KEY);")?;
    expected.execute_batch(include_str!("source_sync/schema.sql"))?;
    ensure!(actual == objects(&expected)?, "Source Sync schema mismatch");
    Ok(())
}

#[cfg(test)]
mod tests;
