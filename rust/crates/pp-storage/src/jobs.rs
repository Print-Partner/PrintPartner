#[cfg(feature = "desktop_rebuild")]
mod checklist;
mod filename;
mod model;
#[cfg(feature = "desktop_rebuild")]
mod results;
#[cfg(feature = "desktop_rebuild")]
mod subscriptions;
use crate::{
    Envelope, SettingsClient, WriterOwner,
    auth::{self, AuthPolicy, Secret},
};
use anyhow::{Result, anyhow, ensure};
pub use filename::*;
pub use model::*;
#[cfg(feature = "desktop_rebuild")]
pub use results::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[cfg(feature = "desktop_rebuild")]
pub(crate) use subscriptions::JobSubscriptions;
#[cfg(feature = "desktop_rebuild")]
pub use subscriptions::{
    FrameAdmission, JobSubscription, NonblockingSend, PreparedEvent, PreparedFrame, StreamClose,
};

#[derive(Clone, Copy)]
pub(crate) enum EffectAdmissionPolicy {
    Legacy,
    #[cfg(feature = "desktop_rebuild")]
    EnforceFeatures,
}

pub struct PhysicalOwner {
    storage: Arc<crate::Shared>,
}
#[cfg(feature = "desktop_rebuild")]
impl PhysicalOwner {
    pub(crate) fn storage_identity(&self) -> &Arc<crate::Shared> {
        &self.storage
    }
}
pub enum Credential {
    Session(Secret),
    RoutedKey { tenant: String, key: Secret },
    PhysicalOwner(PhysicalOwner),
}
#[derive(Clone)]
pub struct AtomicJobClient {
    storage: SettingsClient,
    policy: AuthPolicy,
}
/// A tenant's retained public snapshots, newest first.
#[cfg(feature = "desktop_rebuild")]
#[derive(Clone)]
pub struct RetainedJobs {
    snapshots: Arc<[JobSnapshot]>,
}
#[cfg(feature = "desktop_rebuild")]
impl std::ops::Deref for RetainedJobs {
    type Target = [JobSnapshot];

    fn deref(&self) -> &Self::Target {
        &self.snapshots
    }
}
#[derive(Clone)]
pub struct ServerWorkerClient {
    storage: SettingsClient,
    identity: String,
    admission: Arc<WorkerAdmission>,
    #[cfg(feature = "desktop_rebuild")]
    policy: Option<AuthPolicy>,
}
#[derive(Clone)]
pub struct AttemptLease {
    job_id: String,
    tenant: String,
    generation: i64,
    version: i64,
    fence: String,
    worker: String,
}
#[cfg(feature = "desktop_rebuild")]
pub struct ClaimedAttempt {
    pub job: JobRecord,
    pub lease: AttemptLease,
    pub source_work: Option<crate::catalog::SourceWorkLease>,
}
#[cfg(not(feature = "desktop_rebuild"))]
pub type ClaimedAttempt = (JobRecord, AttemptLease);
impl AttemptLease {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
}
#[cfg(feature = "desktop_rebuild")]
impl AttemptLease {
    pub(crate) fn generation(&self) -> i64 {
        self.generation
    }
    pub(crate) fn version(&self) -> i64 {
        self.version
    }
    pub(crate) fn tenant(&self) -> &str {
        &self.tenant
    }
    pub(crate) fn worker(&self) -> &str {
        &self.worker
    }
    pub(crate) fn fence(&self) -> &str {
        &self.fence
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct WorkerAdmission {
    pub kinds: Vec<(JobKind, usize)>,
    pub total: usize,
    pub per_resource: usize,
    pub lease_seconds: u32,
}
impl WorkerAdmission {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.total > 0
                && self.total <= 128
                && self.per_resource > 0
                && self.per_resource <= self.total
                && (1..=3600).contains(&self.lease_seconds),
            "Invalid worker admission"
        );
        let mut kinds = std::collections::HashSet::new();
        for (kind, limit) in &self.kinds {
            ensure!(
                *limit > 0 && *limit <= self.total && kinds.insert(kind.name()),
                "Invalid kind admission"
            );
        }
        Ok(())
    }
}
pub enum UserOperation {
    Enqueue {
        key: String,
        payload_version: u32,
        payload: Payload,
    },
    Get {
        job_id: String,
    },
    #[cfg(feature = "desktop_rebuild")]
    GetPublic {
        job_id: String,
    },
    List(JobListQuery),
    #[cfg(feature = "desktop_rebuild")]
    ListRetained(LegacyListFilter),
    /// Operator-visible damaged rows, with uncertain reservations explicitly flagged.
    Quarantined {
        before: Option<(i64, String)>,
        limit: u16,
    },
    History {
        job_id: String,
        before_version: Option<i64>,
        limit: u16,
    },
    Reconciliations {
        job_id: String,
        before_version: Option<i64>,
        limit: u16,
    },
    Cancel {
        job_id: String,
    },
    Reconcile {
        job_id: String,
        expected_version: i64,
        expected_generation: i64,
        effect_hash: String,
        decision: Decision,
        receipt: Option<ResultArtifact>,
    },
}
pub enum WorkerOperation {
    Heartbeat,
    Progress(u8),
    BeginEffect(EffectIntent),
    ConfirmEffect(ResultArtifact),
    Finish(Option<ResultArtifact>),
    /// Finishes with the result the public job API reports.
    #[cfg(feature = "desktop_rebuild")]
    FinishPublic {
        artifact: Option<ResultArtifact>,
        result: Box<CompletedResult>,
    },
    Fail,
}
/// A damaged document is retained for inspection and repair, never executed.
#[derive(Debug)]
pub struct QuarantinedJob {
    pub job_id: String,
    pub kind: String,
    pub updated_at: i64,
    pub reason: String,
    /// This reservation remains held until an operator investigates the effect.
    pub uncertain_resource: Option<String>,
}
pub enum Outcome {
    #[cfg(feature = "desktop_rebuild")]
    LocalScanObservation(crate::source_scan::LocalScanObservation),
    #[cfg(feature = "desktop_rebuild")]
    LocalScanSettled(JobRecord, crate::source_scan::LocalScanApplied),
    #[cfg(feature = "desktop_rebuild")]
    LocalScanCompletion(crate::source_scan::LocalCompletionInspection),
    #[cfg(feature = "desktop_rebuild")]
    HeldLocalScan(crate::source_scan::HeldLocalScanInspection),
    #[cfg(feature = "desktop_rebuild")]
    LocalScanReconciled(JobRecord),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncOpened,
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncTarget(crate::source_sync::NextSyncTarget, i64),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncBound(crate::source_sync::BoundSyncTarget),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncSettled(crate::source_sync::SettledSyncTarget),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncFailed(crate::source_sync::FailedSyncTarget),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncAcknowledged(crate::source_sync::AcknowledgedSyncTarget),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncHalted(JobRecord),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncInspection(crate::source_sync::SyncTargetInspection),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncRecovery(crate::source_sync::SyncRecoveryInspection),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncFinish(crate::source_sync::SourceSyncFinishOutcome, LocalCommit),
    Job(JobRecord, LocalCommit),
    List(Vec<JobRecord>),
    #[cfg(feature = "desktop_rebuild")]
    PublicList(RetainedJobs),
    #[cfg(feature = "desktop_rebuild")]
    PublicJob(JobSnapshot),
    #[cfg(feature = "desktop_rebuild")]
    Observed(JobSubscription),
    Quarantined(Vec<QuarantinedJob>),
    History(Vec<HistoryEntry>),
    Reconciliations(Vec<ReconciliationRecord>),
    Claimed(Option<ClaimedAttempt>),
    Retained(usize),
    #[cfg(feature = "desktop_rebuild")]
    Authorized(ValidatedAuthority),
}
/// What the writer does once the transaction commits.
enum TransactionDecision {
    Outcome(Outcome),
    #[cfg(feature = "desktop_rebuild")]
    Claimed(ClaimPlan),
    #[cfg(feature = "desktop_rebuild")]
    LocalScanCompleted(crate::source_scan::PendingLocalCompletion),
    #[cfg(feature = "desktop_rebuild")]
    LocalScanReconciled(crate::source_scan::PendingLocalReconciliation),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncReserved(crate::source_sync::ReservedTarget, SettingsClient),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncSettled(crate::source_sync::SettledSyncRelease),
    #[cfg(feature = "desktop_rebuild")]
    SourceSyncAcknowledged(crate::source_sync::AcknowledgedSyncRelease),
    /// Report this error only after committing the refusals recorded before it.
    #[cfg(feature = "desktop_rebuild")]
    ErrorAfterCommit(anyhow::Error),
    /// Report the authority failure after committing the refusal it caused.
    #[cfg(feature = "desktop_rebuild")]
    Refused(CommittedRefusal),
    /// Register the stream only once the snapshot it starts from has committed.
    #[cfg(feature = "desktop_rebuild")]
    Observation {
        credential: Credential,
        authority: auth::StreamAuthority,
        policy: AuthPolicy,
        snapshot: JobSnapshot,
    },
}
#[cfg(feature = "desktop_rebuild")]
pub(crate) struct CommittedRefusal {
    pub(crate) job: JobRecord,
    pub(crate) failure: auth::AuthorityFailure,
}
/// The original credential of a claimed attempt, revalidated under the
/// current policy.
#[cfg(feature = "desktop_rebuild")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedAuthority {
    tenant: String,
    subject: String,
}
#[cfg(feature = "desktop_rebuild")]
impl ValidatedAuthority {
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
}
pub(crate) enum Command {
    User {
        credential: Credential,
        policy: AuthPolicy,
        operation: UserOperation,
        storage: Arc<crate::Shared>,
    },
    Claim {
        worker: String,
        admission: Arc<WorkerAdmission>,
        #[cfg(feature = "desktop_rebuild")]
        job_id: Option<String>,
        #[cfg(feature = "desktop_rebuild")]
        kind: Option<JobKind>,
        #[cfg(feature = "desktop_rebuild")]
        policy: Option<AuthPolicy>,
        #[cfg(feature = "desktop_rebuild")]
        storage: SettingsClient,
    },
    #[cfg(feature = "desktop_rebuild")]
    ClaimResolved {
        claim: crate::uploads::ResolvedCapturedClaim,
        worker: String,
        admission: Arc<WorkerAdmission>,
        policy: Option<AuthPolicy>,
        storage: SettingsClient,
    },
    Worker {
        lease: AttemptLease,
        operation: WorkerOperation,
        admission: Arc<WorkerAdmission>,
        #[cfg(feature = "desktop_rebuild")]
        policy: Option<AuthPolicy>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Authorize {
        lease: AttemptLease,
        policy: AuthPolicy,
    },
    #[cfg(feature = "desktop_rebuild")]
    Observe {
        credential: Credential,
        policy: AuthPolicy,
        job_id: String,
    },
    #[cfg(feature = "desktop_rebuild")]
    ObserveLocalScan {
        lease: AttemptLease,
        source_work: crate::catalog::SourceWorkProof,
        policy: AuthPolicy,
    },
    #[cfg(feature = "desktop_rebuild")]
    SettleLocalScan {
        lease: AttemptLease,
        source_work: crate::catalog::SourceWorkProof,
        policy: AuthPolicy,
        settlement: crate::source_scan::LocalScanSettlement,
    },
    #[cfg(feature = "desktop_rebuild")]
    CompleteLocalScan {
        lease: AttemptLease,
        source_work: crate::catalog::SourceWorkProof,
        policy: AuthPolicy,
        applied: crate::source_scan::LocalScanApplied,
        result: SourceScanResult,
    },
    #[cfg(feature = "desktop_rebuild")]
    InspectLocalScan {
        lease: AttemptLease,
        worker: String,
    },
    #[cfg(feature = "desktop_rebuild")]
    InspectHeldLocalScan {
        lease: AttemptLease,
        source_work: crate::catalog::SourceWorkProof,
        worker: String,
    },
    #[cfg(feature = "desktop_rebuild")]
    ReconcileLocalScan {
        lease: AttemptLease,
        source_work: crate::catalog::SourceWorkProof,
        applied: crate::source_scan::LocalScanApplied,
        reason: String,
    },
    #[cfg(feature = "desktop_rebuild")]
    OpenSourceSync {
        lease: AttemptLease,
        policy: AuthPolicy,
    },
    #[cfg(feature = "desktop_rebuild")]
    ClaimSourceSyncTarget {
        lease: AttemptLease,
        policy: AuthPolicy,
        storage: SettingsClient,
    },
    #[cfg(feature = "desktop_rebuild")]
    BindSourceSyncTarget {
        lease: AttemptLease,
        candidate: crate::source_sync::SyncCandidate,
    },
    #[cfg(feature = "desktop_rebuild")]
    SettleSourceSyncTarget {
        lease: AttemptLease,
        candidate: crate::source_sync::SyncCandidate,
        settlement: crate::source_sync::SyncSettlement,
    },
    #[cfg(feature = "desktop_rebuild")]
    FailSourceSyncTarget {
        lease: AttemptLease,
        candidate: crate::source_sync::SyncCandidate,
        detail: String,
    },
    #[cfg(feature = "desktop_rebuild")]
    AcknowledgeSourceSyncTarget {
        lease: AttemptLease,
        candidate: crate::source_sync::SyncCandidate,
        observation: crate::source_scan::LocalScanObservation,
    },
    #[cfg(feature = "desktop_rebuild")]
    AdoptSourceSyncTarget {
        lease: AttemptLease,
        key: crate::source_sync::SyncTargetKey,
    },
    #[cfg(feature = "desktop_rebuild")]
    HaltSourceSync {
        lease: AttemptLease,
        halt: crate::source_sync::SourceSyncHalt,
        detail: String,
    },
    #[cfg(feature = "desktop_rebuild")]
    InspectSourceSyncTarget {
        lease: AttemptLease,
        key: crate::source_sync::SyncTargetKey,
        policy: AuthPolicy,
    },
    #[cfg(feature = "desktop_rebuild")]
    FinishSourceSync {
        lease: AttemptLease,
        policy: AuthPolicy,
    },
    #[cfg(feature = "desktop_rebuild")]
    InspectRetainedSourceSync {
        credential: Credential,
        policy: AuthPolicy,
        job_id: String,
        expected_version: i64,
        storage: Arc<crate::Shared>,
    },
    Retain {
        per_tenant: usize,
        global: usize,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobFailure {
    #[cfg(feature = "desktop_rebuild")]
    FeatureDisabled {
        feature: pp_domain::features::Feature,
    },
    CommitUnknown,
    JobDocTooLarge {
        size: usize,
        limit: usize,
    },
}
impl std::fmt::Display for JobFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "desktop_rebuild")]
            Self::FeatureDisabled { feature } => {
                write!(f, "Feature {} is disabled.", feature.key())
            }
            Self::CommitUnknown => f.write_str("Job commit outcome unknown"),
            Self::JobDocTooLarge { size, limit } => write!(
                f,
                "Durable job document is {size} bytes, exceeding the {limit}-byte limit"
            ),
        }
    }
}
impl std::error::Error for JobFailure {}
#[cfg(feature = "desktop_rebuild")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobAccessFailure {
    NotFound,
    InternalOnly,
    Capacity,
    Stopped,
}
#[cfg(feature = "desktop_rebuild")]
impl std::fmt::Display for JobAccessFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotFound | Self::InternalOnly => "Job not found",
            Self::Capacity => "Job stream capacity reached",
            Self::Stopped => "Jobs owner stopped",
        })
    }
}
#[cfg(feature = "desktop_rebuild")]
impl std::error::Error for JobAccessFailure {}
pub struct PendingClaim(Pending);
impl PendingClaim {
    pub fn receive(self) -> Result<Option<ClaimedAttempt>> {
        match self.0.receive()? {
            Outcome::Claimed(value) => Ok(value),
            _ => unreachable!(),
        }
    }
}
pub struct Pending(mpsc::Receiver<Result<Outcome>>);
impl Pending {
    pub fn receive(self) -> Result<Outcome> {
        self.0
            .recv()
            .map_err(|_| anyhow!(JobFailure::CommitUnknown))?
    }
}
fn submit(
    storage: &SettingsClient,
    command: Command,
    cancelled: &AtomicBool,
    wait: Duration,
) -> Result<Pending> {
    let deadline = Instant::now() + wait;
    let mut queue = storage
        .shared
        .queue
        .lock()
        .map_err(|_| anyhow!("Writer admission poisoned"))?;
    loop {
        ensure!(!queue.closed, "Storage stopped");
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Cancelled before admission"
        );
        if queue.pending.len() < storage.shared.capacity {
            let (reply, receiver) = mpsc::channel();
            queue.pending.push_back(Envelope::Jobs { command, reply });
            storage.shared.changed.notify_all();
            return Ok(Pending(receiver));
        }
        ensure!(Instant::now() < deadline, "Writer queue full");
        queue = storage
            .shared
            .changed
            .wait_timeout(queue, Duration::from_millis(5))
            .map_err(|_| anyhow!("Writer admission poisoned"))?
            .0;
    }
}
impl WriterOwner {
    pub fn jobs(&self, policy: AuthPolicy) -> Result<AtomicJobClient> {
        self.auth_with_policy(policy)?;
        Ok(AtomicJobClient {
            storage: self.client(),
            policy,
        })
    }
    pub fn job_physical_owner(&self) -> PhysicalOwner {
        PhysicalOwner {
            storage: self.client.shared.clone(),
        }
    }
    pub fn job_worker(&self, admission: WorkerAdmission) -> Result<ServerWorkerClient> {
        Ok(ServerWorkerClient {
            storage: self.client(),
            identity: random(),
            admission: self.worker_admission(admission)?,
            #[cfg(feature = "desktop_rebuild")]
            policy: None,
        })
    }
    /// A worker that can claim jobs carrying an original credential. Each
    /// claim revalidates that credential under `policy`.
    #[cfg(feature = "desktop_rebuild")]
    pub fn job_worker_with_policy(
        &self,
        policy: AuthPolicy,
        admission: WorkerAdmission,
    ) -> Result<ServerWorkerClient> {
        self.auth_with_policy(policy)?;
        Ok(ServerWorkerClient {
            storage: self.client(),
            identity: random(),
            admission: self.worker_admission(admission)?,
            policy: Some(policy),
        })
    }
    fn worker_admission(&self, admission: WorkerAdmission) -> Result<Arc<WorkerAdmission>> {
        admission.validate()?;
        let mut configured = self
            .client
            .shared
            .job_admission
            .lock()
            .map_err(|_| anyhow!("Worker admission poisoned"))?;
        if let Some(existing) = configured.as_ref() {
            ensure!(
                existing.as_ref() == &admission,
                "Worker admission already configured"
            );
        }
        Ok(configured
            .get_or_insert_with(|| Arc::new(admission))
            .clone())
    }
    pub fn retain_jobs(&self, per_tenant: usize, global: usize) -> Result<usize> {
        ensure!(
            per_tenant > 0 && global >= per_tenant && global <= 10000,
            "Invalid retention bounds"
        );
        match submit(
            &self.client,
            Command::Retain { per_tenant, global },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::Retained(n) => Ok(n),
            _ => unreachable!(),
        }
    }
}
impl AtomicJobClient {
    pub fn submit(
        &self,
        credential: Credential,
        mut operation: UserOperation,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Pending> {
        if let UserOperation::Enqueue {
            key,
            payload_version,
            payload,
        } = &mut operation
        {
            model::text(key, 128)?;
            ensure!(*payload_version == 1, "Unsupported payload version");
            if let Payload::ExportStlPack {
                filename_grouping: Some(grouping),
                ..
            } = payload
            {
                grouping.normalize();
            }
            payload.validate()?;
            #[cfg(feature = "desktop_rebuild")]
            ensure!(
                !matches!(payload, Payload::SuppliedSourceImport { .. }),
                "Supplied imports require atomic domain admission"
            );
        }
        submit(
            &self.storage,
            Command::User {
                credential,
                policy: self.policy,
                operation,
                storage: self.storage.shared.clone(),
            },
            cancelled,
            wait,
        )
    }
    /// Reads the snapshot the public job API reports.
    #[cfg(feature = "desktop_rebuild")]
    pub fn read_public(
        &self,
        credential: Credential,
        job_id: String,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<JobSnapshot> {
        match self
            .submit(
                credential,
                UserOperation::GetPublic { job_id },
                cancelled,
                wait,
            )?
            .receive()?
        {
            Outcome::PublicJob(snapshot) => Ok(snapshot),
            _ => unreachable!(),
        }
    }
    /// Lists the tenant's retained jobs with Node's filters and ordering.
    #[cfg(feature = "desktop_rebuild")]
    pub fn list_retained(
        &self,
        credential: Credential,
        filter: LegacyListFilter,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<RetainedJobs> {
        match self
            .submit(
                credential,
                UserOperation::ListRetained(filter),
                cancelled,
                wait,
            )?
            .receive()?
        {
            Outcome::PublicList(snapshots) => Ok(snapshots),
            _ => unreachable!(),
        }
    }
    /// Streams the job's committed snapshots, starting from the current one,
    /// for as long as the credential stays valid.
    #[cfg(feature = "desktop_rebuild")]
    pub fn subscribe(
        &self,
        credential: Credential,
        job_id: String,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<JobSubscription> {
        match submit(
            &self.storage,
            Command::Observe {
                credential,
                policy: self.policy,
                job_id,
            },
            cancelled,
            wait,
        )?
        .receive()?
        {
            Outcome::Observed(subscription) => Ok(subscription),
            _ => unreachable!(),
        }
    }

    /// Reads retained Sync recovery evidence for one Job at an exact version.
    /// Validates target/result/observation bindings and reports dispositions
    /// without adopting or deleting evidence.
    #[cfg(feature = "desktop_rebuild")]
    pub fn inspect_source_sync_recovery(
        &self,
        credential: Credential,
        job_id: String,
        expected_version: i64,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<crate::source_sync::SyncRecoveryInspection> {
        match submit(
            &self.storage,
            Command::InspectRetainedSourceSync {
                credential,
                policy: self.policy,
                job_id,
                expected_version,
                storage: self.storage.shared.clone(),
            },
            cancelled,
            wait,
        )?
        .receive()?
        {
            Outcome::SourceSyncRecovery(value) => Ok(value),
            _ => unreachable!(),
        }
    }
}
impl ServerWorkerClient {
    pub fn begin_source_work(
        &self,
        lease: &AttemptLease,
        source_id: Option<i64>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<crate::catalog::SourceWorkLease> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        crate::catalog::begin_job_work(&self.storage, lease.clone(), source_id, cancelled, wait)
    }

    pub fn claim_pending(&self, cancelled: &AtomicBool, wait: Duration) -> Result<PendingClaim> {
        Ok(PendingClaim(submit(
            &self.storage,
            Command::Claim {
                worker: self.identity.clone(),
                admission: self.admission.clone(),
                #[cfg(feature = "desktop_rebuild")]
                job_id: None,
                #[cfg(feature = "desktop_rebuild")]
                kind: None,
                #[cfg(feature = "desktop_rebuild")]
                policy: self.policy,
                #[cfg(feature = "desktop_rebuild")]
                storage: self.storage.clone(),
            },
            cancelled,
            wait,
        )?))
    }
    pub fn claim(&self) -> Result<Option<ClaimedAttempt>> {
        self.claim_pending(&AtomicBool::new(false), Duration::from_secs(5))?
            .receive()
    }
    /// Claims the oldest admissible job of one admitted kind.
    #[cfg(feature = "desktop_rebuild")]
    pub fn claim_kind(&self, kind: JobKind) -> Result<Option<ClaimedAttempt>> {
        ensure!(
            self.admission
                .kinds
                .iter()
                .any(|(admitted, _)| *admitted == kind),
            "Job kind is not admitted"
        );
        PendingClaim(submit(
            &self.storage,
            Command::Claim {
                worker: self.identity.clone(),
                admission: self.admission.clone(),
                job_id: None,
                kind: Some(kind),
                policy: self.policy,
                storage: self.storage.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?)
        .receive()
    }
    /// Claims one queued supplied import by id. A refused original authority
    /// commits the refusal and returns the typed failure instead of moving on.
    /// A busy Source returns `SourceBusy` and leaves the queued job unchanged.
    #[cfg(feature = "desktop_rebuild")]
    pub fn claim_import(&self, job_id: &str) -> Result<Option<ClaimedAttempt>> {
        PendingClaim(submit(
            &self.storage,
            Command::Claim {
                worker: self.identity.clone(),
                admission: self.admission.clone(),
                job_id: Some(job_id.into()),
                kind: Some(JobKind::SuppliedSourceImport),
                policy: self.policy,
                storage: self.storage.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?)
        .receive()
    }
    /// Runs one phase of a claimed supplied import with a live reservation for
    /// its tenant and Source. A refused original authority
    /// commits the refusal and its Source observation before returning the typed
    /// failure.
    #[cfg(feature = "desktop_rebuild")]
    pub fn import_phase(
        &self,
        lease: &AttemptLease,
        source_work: &crate::catalog::SourceWorkLease,
        phase: crate::uploads::Phase,
    ) -> Result<crate::uploads::Operation> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match crate::uploads::submit(
            &self.storage,
            crate::uploads::Command::Phase {
                lease: lease.clone(),
                source_work: source_work.proof()?,
                phase,
                policy,
            },
            &AtomicBool::new(false),
        )? {
            crate::uploads::UploadsOutcome::Operation(operation) => Ok(*operation),
            crate::uploads::UploadsOutcome::Preflighted(_)
            | crate::uploads::UploadsOutcome::CaptureCorrelation(_) => {
                Err(anyhow!("Unexpected capture preflight reply"))
            }
        }
    }

    /// Claims a queued captured import only after the retained claim still
    /// matches the admitted journal row, job payload, inventory, and Source.
    #[cfg(feature = "desktop_rebuild")]
    pub fn claim_resolved_import(
        &self,
        claim: crate::uploads::ResolvedCapturedClaim,
    ) -> Result<Option<ClaimedAttempt>> {
        PendingClaim(submit(
            &self.storage,
            Command::ClaimResolved {
                claim,
                worker: self.identity.clone(),
                admission: self.admission.clone(),
                policy: self.policy,
                storage: self.storage.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?)
        .receive()
    }

    /// Revalidates the original credential of a live attempt. The check does
    /// not change the job.
    #[cfg(feature = "desktop_rebuild")]
    pub fn authorize(&self, lease: &AttemptLease) -> Result<ValidatedAuthority> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match submit(
            &self.storage,
            Command::Authorize {
                lease: lease.clone(),
                policy,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::Authorized(authority) => Ok(authority),
            _ => unreachable!(),
        }
    }
    pub fn update(
        &self,
        lease: &mut AttemptLease,
        operation: WorkerOperation,
    ) -> Result<JobRecord> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        match submit(
            &self.storage,
            Command::Worker {
                lease: lease.clone(),
                operation,
                admission: self.admission.clone(),
                #[cfg(feature = "desktop_rebuild")]
                policy: self.policy,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::Job(record, _) => {
                lease.version = record.state_version;
                Ok(record)
            }
            _ => unreachable!(),
        }
    }
}
#[cfg(feature = "desktop_rebuild")]
impl ServerWorkerClient {
    pub fn observe_local_scan(
        &self,
        lease: &AttemptLease,
        source_work: &crate::catalog::SourceWorkLease,
    ) -> Result<crate::source_scan::LocalScanObservation> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match submit(
            &self.storage,
            Command::ObserveLocalScan {
                lease: lease.clone(),
                source_work: source_work.proof()?,
                policy,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::LocalScanObservation(observation) => Ok(observation),
            _ => unreachable!(),
        }
    }

    pub fn settle_local_scan(
        &self,
        lease: &mut AttemptLease,
        source_work: &crate::catalog::SourceWorkLease,
        settlement: crate::source_scan::LocalScanSettlement,
    ) -> Result<crate::source_scan::LocalScanApplied> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match submit(
            &self.storage,
            Command::SettleLocalScan {
                lease: lease.clone(),
                source_work: source_work.proof()?,
                policy,
                settlement,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::LocalScanSettled(job, applied) => {
                lease.version = job.state_version;
                Ok(applied)
            }
            _ => unreachable!(),
        }
    }

    pub fn complete_local_scan(
        &self,
        lease: &mut AttemptLease,
        source_work: &mut crate::catalog::SourceWorkLease,
        applied: crate::source_scan::LocalScanApplied,
        result: SourceScanResult,
    ) -> Result<crate::source_scan::LocalCompletionInspection> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        let outcome = match submit(
            &self.storage,
            Command::CompleteLocalScan {
                lease: lease.clone(),
                source_work: source_work.proof()?,
                policy,
                applied,
                result,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::LocalScanCompletion(outcome) => outcome,
            _ => unreachable!(),
        };
        if matches!(
            outcome,
            crate::source_scan::LocalCompletionInspection::Completed(_)
        ) {
            source_work.consume_retired()?;
        }
        Ok(outcome)
    }

    pub fn inspect_local_scan_completion(
        &self,
        lease: &AttemptLease,
    ) -> Result<crate::source_scan::LocalCompletionInspection> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        match submit(
            &self.storage,
            Command::InspectLocalScan {
                lease: lease.clone(),
                worker: self.identity.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::LocalScanCompletion(outcome) => Ok(outcome),
            _ => unreachable!(),
        }
    }

    pub fn inspect_local_scan_while_held(
        &self,
        lease: &mut AttemptLease,
        source_work: &crate::catalog::SourceWorkLease,
    ) -> Result<crate::source_scan::HeldLocalScanInspection> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let outcome = match submit(
            &self.storage,
            Command::InspectHeldLocalScan {
                lease: lease.clone(),
                source_work: source_work.proof()?,
                worker: self.identity.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::HeldLocalScan(outcome) => outcome,
            _ => unreachable!(),
        };
        lease.version = match &outcome {
            crate::source_scan::HeldLocalScanInspection::NotApplied(job)
            | crate::source_scan::HeldLocalScanInspection::Applied(job, _) => job.state_version,
        };
        Ok(outcome)
    }

    pub fn record_local_scan_reconciliation(
        &self,
        lease: &AttemptLease,
        source_work: &mut crate::catalog::SourceWorkLease,
        applied: crate::source_scan::LocalScanApplied,
        reason: String,
    ) -> Result<JobRecord> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let job = match submit(
            &self.storage,
            Command::ReconcileLocalScan {
                lease: lease.clone(),
                source_work: source_work.proof()?,
                applied,
                reason,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::LocalScanReconciled(job) => job,
            _ => unreachable!(),
        };
        source_work.consume_retired()?;
        Ok(job)
    }

    pub fn open_source_sync(&self, lease: &AttemptLease) -> Result<()> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match submit(
            &self.storage,
            Command::OpenSourceSync {
                lease: lease.clone(),
                policy,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncOpened => Ok(()),
            _ => unreachable!(),
        }
    }

    /// Claims the next target of an open batch. A Local target's lease holds
    /// its Source reservation; release or drop it promptly.
    pub fn claim_next_source_sync_target(
        &self,
        lease: &mut AttemptLease,
    ) -> Result<crate::source_sync::NextSyncTarget> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match submit(
            &self.storage,
            Command::ClaimSourceSyncTarget {
                lease: lease.clone(),
                policy,
                storage: self.storage.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncTarget(target, version) => {
                lease.version = version;
                Ok(target)
            }
            _ => unreachable!(),
        }
    }

    /// Checks a claimed target against its stored claim before the worker
    /// trusts it. A refusal changes nothing.
    pub fn bind_source_sync_target(
        &self,
        lease: &AttemptLease,
        target: &crate::source_sync::SyncTargetLease,
    ) -> Result<crate::source_sync::BoundSyncTarget> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        match submit(
            &self.storage,
            Command::BindSourceSyncTarget {
                lease: lease.clone(),
                candidate: target.candidate()?,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncBound(target) => Ok(target),
            _ => unreachable!(),
        }
    }

    /// Stores one successful target result and releases that target's Source
    /// reservation. A stale attempt, or a Source proof that is not the claim's
    /// live reservation, is refused and writes nothing.
    pub fn settle_source_sync_target(
        &self,
        lease: &AttemptLease,
        target: &mut crate::source_sync::SyncTargetLease,
        settlement: crate::source_sync::SyncSettlement,
    ) -> Result<crate::source_sync::SettledSyncTarget> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let settled = match submit(
            &self.storage,
            Command::SettleSourceSyncTarget {
                lease: lease.clone(),
                candidate: target.candidate()?,
                settlement,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncSettled(settled) => settled,
            _ => unreachable!(),
        };
        target.source_work.consume_retired()?;
        Ok(settled)
    }

    /// Records one claimed target's local read failure and the Source failure
    /// observation. The reservation stays held. A refusal writes nothing.
    pub fn fail_source_sync_target(
        &self,
        lease: &mut AttemptLease,
        target: &crate::source_sync::SyncTargetLease,
        detail: &str,
    ) -> Result<crate::source_sync::FailedSyncTarget> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let failed = match submit(
            &self.storage,
            Command::FailSourceSyncTarget {
                lease: lease.clone(),
                candidate: target.candidate()?,
                detail: detail.to_owned(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncFailed(failed) => failed,
            _ => unreachable!(),
        };
        lease.version = failed.version;
        Ok(failed)
    }

    /// Acknowledges one retained local-read failure when `observation` matches
    /// the claim. Exact replay returns that failure and writes nothing. A
    /// refusal writes nothing. The live token is retired after commit.
    pub fn acknowledge_source_sync_target(
        &self,
        lease: &mut AttemptLease,
        target: &crate::source_sync::SyncTargetLease,
        observation: &crate::source_scan::LocalScanObservation,
    ) -> Result<crate::source_sync::AcknowledgedSyncTarget> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let acknowledged = match submit(
            &self.storage,
            Command::AcknowledgeSourceSyncTarget {
                lease: lease.clone(),
                candidate: target.candidate()?,
                observation: observation.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncAcknowledged(acknowledged) => acknowledged,
            _ => unreachable!(),
        };
        lease.version = acknowledged.version;
        Ok(acknowledged)
    }

    /// Returns one already-acknowledged successful Sync target result when this
    /// attempt still owns the claim and the live Source observation matches the
    /// claim capture. Writes nothing. A changed observation or stale attempt is
    /// refused without writing.
    pub fn adopt_source_sync_target(
        &self,
        lease: &AttemptLease,
        key: crate::source_sync::SyncTargetKey,
    ) -> Result<crate::source_sync::SettledSyncTarget> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        match submit(
            &self.storage,
            Command::AdoptSourceSyncTarget {
                lease: lease.clone(),
                key,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncSettled(settled) => Ok(settled),
            _ => unreachable!(),
        }
    }

    /// Halts the open Sync batch after a caller Source operation error. The
    /// detail becomes the Job recovery message. Retained target evidence stays
    /// in place; a claimed or unacknowledged target leaves the Job for
    /// reconciliation. A stale attempt is refused without writing.
    pub fn halt_source_sync(
        &self,
        lease: &AttemptLease,
        halt: crate::source_sync::SourceSyncHalt,
        detail: String,
    ) -> Result<JobRecord> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        match submit(
            &self.storage,
            Command::HaltSourceSync {
                lease: lease.clone(),
                halt,
                detail,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncHalted(job) => Ok(job),
            _ => unreachable!(),
        }
    }

    /// Reads one live Sync target for this authorized attempt. Writes nothing
    /// and leaves reservation ownership unchanged. A foreign tenant row or
    /// stale attempt is refused without writing.
    pub fn inspect_source_sync_target(
        &self,
        lease: &AttemptLease,
        key: crate::source_sync::SyncTargetKey,
    ) -> Result<crate::source_sync::SyncTargetInspection> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match submit(
            &self.storage,
            Command::InspectSourceSyncTarget {
                lease: lease.clone(),
                key,
                policy,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncInspection(inspection) => Ok(inspection),
            _ => unreachable!(),
        }
    }

    /// Finishes an open Sync batch after every target has a valid acknowledged
    /// terminal result. Exact retry returns the same aggregate and writes
    /// nothing. An unsettled target is refused without writing.
    pub fn finish_source_sync(
        &self,
        lease: &AttemptLease,
    ) -> Result<crate::source_sync::SourceSyncFinishOutcome> {
        ensure!(lease.worker == self.identity, "Foreign worker lease");
        let policy = self.policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
        match submit(
            &self.storage,
            Command::FinishSourceSync {
                lease: lease.clone(),
                policy,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
        {
            Outcome::SourceSyncFinish(outcome, _) => Ok(outcome),
            _ => unreachable!(),
        }
    }
}

fn random() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}
fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}
/// Rounds the start up to a whole second so a lease checked against the
/// truncated `now()` lasts at least `lease_seconds` of real time.
fn lease_expiry(at: time::OffsetDateTime, lease_seconds: u32) -> i64 {
    at.unix_timestamp() + i64::from(at.nanosecond() > 0) + i64::from(lease_seconds)
}
pub const JOB_DOC_SIZE_LIMIT: usize = 131072;

fn check_document_size(size: usize) -> Result<()> {
    ensure!(
        size <= JOB_DOC_SIZE_LIMIT,
        JobFailure::JobDocTooLarge {
            size,
            limit: JOB_DOC_SIZE_LIMIT
        }
    );
    Ok(())
}

fn decode_row(
    id: &str,
    document: &str,
    diagnostics: &mut DocumentDiagnostics,
) -> Option<JobRecord> {
    match decode(document) {
        Ok(job) => Some(job),
        Err(error) => {
            diagnostics.record(id, &error);
            None
        }
    }
}

fn document_failure_reason(error: &anyhow::Error) -> String {
    match error.downcast_ref::<JobFailure>() {
        Some(error @ JobFailure::JobDocTooLarge { .. }) => error.to_string(),
        _ => "Invalid durable job document; operator inspection required".into(),
    }
}

#[derive(Default)]
struct DocumentDiagnostics {
    failures: usize,
}
impl DocumentDiagnostics {
    fn record(&mut self, id: &str, error: &anyhow::Error) {
        self.failures += 1;
        if self.failures <= 8 {
            let job_ref = hex::encode(Sha256::digest(id.as_bytes()));
            eprintln!(
                "Skipping invalid durable job row job_ref={}: {}",
                &job_ref[..16],
                document_failure_reason(error)
            );
        }
    }
}
impl Drop for DocumentDiagnostics {
    fn drop(&mut self) {
        if self.failures > 8 {
            eprintln!(
                "Additional invalid durable job rows skipped: {}",
                self.failures - 8
            );
        }
    }
}

fn encode(job: &JobRecord) -> Result<String> {
    let mut document = serde_json::to_value(job)?;
    document["_attempt_worker"] = serde_json::to_value(&job.worker)?;
    document["_attempt_fence"] = serde_json::to_value(&job.fence)?;
    #[cfg(feature = "desktop_rebuild")]
    {
        match job.authority_disposition {
            AuthorityDisposition::Original => {
                document["_authority"] = auth::authority::encode(
                    job.authority
                        .as_ref()
                        .ok_or_else(|| anyhow!("Missing durable authority"))?,
                )?;
            }
            AuthorityDisposition::PhysicalOwner => {
                document["_authority"] = serde_json::Value::Null;
            }
            AuthorityDisposition::LegacyMissing => {}
        }
        if let Some(observation) = &job.authority_refusal {
            document["_authority_refusal"] = serde_json::to_value(observation)?;
        }
    }
    let document = serde_json::to_string(&document)?;
    check_document_size(document.len())?;
    Ok(document)
}
pub(crate) fn decode(document: &str) -> Result<JobRecord> {
    check_document_size(document.len())?;
    let value: serde_json::Value = serde_json::from_str(document)?;
    let mut job: JobRecord = serde_json::from_value(value.clone())?;
    job.fence = serde_json::from_value(
        value
            .get("_attempt_fence")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )?;
    job.worker = serde_json::from_value(
        value
            .get("_attempt_worker")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )?;
    #[cfg(feature = "desktop_rebuild")]
    {
        (job.authority, job.authority_disposition) = match value.get("_authority") {
            Some(serde_json::Value::Null) => (None, AuthorityDisposition::PhysicalOwner),
            Some(authority) => (
                Some(auth::authority::decode(authority.clone())?),
                AuthorityDisposition::Original,
            ),
            None => (None, AuthorityDisposition::LegacyMissing),
        };
        job.authority_refusal = value
            .get("_authority_refusal")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?;
    }
    for timestamp in [
        Some(job.created_at),
        Some(job.updated_at),
        job.finished_at,
        job.lease_until,
    ]
    .into_iter()
    .flatten()
    {
        ensure!(
            time::OffsetDateTime::from_unix_timestamp(timestamp).is_ok(),
            "Invalid job timestamp"
        );
    }
    job.payload.validate_stored()?;
    job.validate_state()?;
    Ok(job)
}
pub(crate) fn load(tx: &Transaction<'_>, id: &str) -> Result<Option<JobRecord>> {
    let document: Option<String> = tx
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1 AND state!='quarantined'",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    document
        .map(|document| {
            let job = decode(&document)?;
            #[cfg(feature = "desktop_rebuild")]
            ensure!(job.job_id == id, "Stored Job identity mismatch");
            Ok(job)
        })
        .transpose()
}

#[cfg(feature = "desktop_rebuild")]
pub(crate) fn load_for_capture_claim(tx: &Transaction<'_>, id: &str) -> Result<JobRecord> {
    load(tx, id)?.ok_or_else(|| anyhow!("Captured import job requires repair"))
}
fn owned(tx: &Transaction<'_>, id: &str, tenant: &str) -> Result<JobRecord> {
    #[cfg(not(feature = "desktop_rebuild"))]
    let missing = || anyhow!("Job not found");
    #[cfg(feature = "desktop_rebuild")]
    let missing = || anyhow!(JobAccessFailure::NotFound);
    load(tx, id)?
        .filter(|record| record.tenant == tenant)
        .ok_or_else(missing)
}
fn retained_owned(tx: &Transaction<'_>, id: &str, tenant: &str) -> Result<JobRecord> {
    if let Some(record) = load(tx, id)? {
        ensure!(
            record.tenant == tenant && record.job_id == id,
            "Job not found"
        );
        return Ok(record);
    }
    let document: Option<String> = tx
        .query_row(
            "SELECT archived_document FROM durable_job_keys WHERE tenant=?1 AND job_id=?2 AND archived_document IS NOT NULL LIMIT 1",
            params![tenant, id],
            |row| row.get(0),
        )
        .optional()?;
    document
        .map(|value| decode(&value))
        .transpose()?
        .map(|record| {
            ensure!(
                record.tenant == tenant && record.job_id == id,
                "Job not found"
            );
            Ok(record)
        })
        .transpose()?
        .ok_or_else(|| anyhow!("Job not found"))
}
fn bind_printer_start(tx: &Transaction<'_>, tenant: &str, mut payload: Payload) -> Result<Payload> {
    let Payload::PrinterStart(request) = &payload else {
        return Ok(payload);
    };
    let parent_id = request.uploaded_job_id.clone();
    let parent = retained_owned(tx, &parent_id, tenant)?;
    ensure!(
        parent.state == PersistentState::UploadedOnly,
        "Parent job is not an uploaded-only print"
    );
    let proof = parent
        .uploaded_only_proof()
        .ok_or_else(|| anyhow!("Parent job is not an uploaded-only print"))?;
    let upload = proof.upload.clone();
    let printer_id = proof.printer_id.to_owned();
    let documents = tx
        .prepare(
            "SELECT document FROM durable_jobs WHERE tenant=?1 AND json_extract(document,'$.payload.payload.uploaded_job_id')=?2
             UNION ALL
             SELECT archived_document FROM durable_job_keys WHERE tenant=?1 AND archived_document IS NOT NULL AND json_extract(archived_document,'$.payload.payload.uploaded_job_id')=?2",
        )?
        .query_map(params![tenant, parent_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for document in documents {
        let existing = decode(&document)?;
        if matches!(
            &existing.payload,
            Payload::PrinterStart(value) if value.uploaded_job_id == parent_id
        ) && !matches!(
            existing.state,
            PersistentState::Failed | PersistentState::Cancelled
        ) {
            return Err(anyhow!("Printer start already requested"));
        }
    }
    payload.bind_printer_start(printer_id, upload)?;
    Ok(payload)
}
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn fail_source_unavailable(tx: &Transaction<'_>, job: &mut JobRecord) -> Result<()> {
    job.state = PersistentState::Failed;
    job.recovery = Some("Source is unavailable for this job".into());
    save(tx, job, "source_unavailable")
}
pub(crate) fn save(tx: &Transaction<'_>, job: &mut JobRecord, event: &str) -> Result<()> {
    job.payload.validate_stored()?;
    job.state_version += 1;
    job.updated_at = now();
    if job.state.terminal() {
        job.finished_at = Some(job.updated_at);
        job.lease_until = None;
        job.fence = None;
        job.worker = None;
    }
    job.validate_state()?;
    tx.execute("INSERT INTO durable_jobs(id,tenant,kind,state,resource,version,generation,lease_until,created,updated,document) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(id) DO UPDATE SET state=excluded.state,resource=excluded.resource,version=excluded.version,generation=excluded.generation,lease_until=excluded.lease_until,updated=excluded.updated,document=excluded.document",params![job.job_id,job.tenant,job.kind.name(),job.state.name(),job.payload.resource(),job.state_version,job.generation,job.lease_until,job.created_at,job.updated_at,encode(job)?])?;
    tx.execute("DELETE FROM durable_job_history WHERE job_id=?1 AND event IN ('heartbeat','progress') AND version<?2",params![job.job_id,job.state_version-128])?;
    tx.execute(
        "INSERT INTO durable_job_history(job_id,version,at,state,event) VALUES(?1,?2,?3,?4,?5)",
        params![
            job.job_id,
            job.state_version,
            job.updated_at,
            job.state.name(),
            event
        ],
    )?;
    #[cfg(feature = "desktop_rebuild")]
    if job.kind == JobKind::PrinterUpload {
        crate::send_queue::settle(tx, job, event)?;
    }
    #[cfg(feature = "desktop_rebuild")]
    subscriptions::record(job);
    Ok(())
}
/// Fails the printer upload `job_id` when it is queued or its outcome is
/// unknown, so it never runs and no longer reserves its printer, and returns
/// whether it did. The send queue calls this when the user removes the item
/// the upload was dispatched for, and at open for an upload that never
/// started. Any other job, or an id with no Rust job (a send Node started),
/// is left alone.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn release_printer_upload(
    tx: &Transaction<'_>,
    tenant: &str,
    job_id: &str,
    reason: &str,
    event: &str,
) -> Result<bool> {
    let stored: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM durable_jobs WHERE id=?1 AND tenant=?2 AND kind=?3 AND state IN ('queued','reconciliation_required'))",
        params![job_id, tenant, JobKind::PrinterUpload.name()],
        |row| row.get(0),
    )?;
    if !stored {
        return Ok(false);
    }
    let mut job = owned(tx, job_id, tenant)?;
    job.state = PersistentState::Failed;
    job.recovery = Some(reason.into());
    save(tx, &mut job, event)?;
    Ok(true)
}
/// The state of the tenant's printer upload `job_id`, or `None` when no Rust
/// job has that id (a send Node started).
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn printer_upload_state(
    tx: &Transaction<'_>,
    tenant: &str,
    job_id: &str,
) -> Result<Option<PersistentState>> {
    let stored: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM durable_jobs WHERE id=?1 AND tenant=?2 AND kind=?3)",
        params![job_id, tenant, JobKind::PrinterUpload.name()],
        |row| row.get(0),
    )?;
    if !stored {
        return Ok(None);
    }
    Ok(Some(owned(tx, job_id, tenant)?.state))
}
/// Records the user's answer for a printer upload awaiting reconciliation:
/// it printed (succeeded) or it did not (failed). The answer is kept in the
/// reconciliation history against the last effect, and the job stops
/// reserving its printer. Nothing is resent.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn reconcile_printer_upload(
    tx: &Transaction<'_>,
    tenant: &str,
    subject: &str,
    job_id: &str,
    printed: bool,
) -> Result<()> {
    let mut job = owned(tx, job_id, tenant)?;
    ensure!(
        job.kind == JobKind::PrinterUpload && job.state == PersistentState::ReconciliationRequired,
        "Printer upload is not awaiting reconciliation"
    );
    let effect = job
        .effects
        .last()
        .ok_or_else(|| anyhow!("Reconciliation effect required"))?;
    let decision = if printed {
        "UserPrinted"
    } else {
        "UserDidNotPrint"
    };
    tx.execute(
        "INSERT INTO durable_job_reconciliations(job_id,version,subject,generation,effect_hash,basis_hash,target,decision,receipt,at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'null',?9)",
        params![
            job.job_id,
            job.state_version + 1,
            subject,
            job.generation,
            effect.intent.content_hash,
            effect.intent.basis_hash,
            effect.intent.target,
            decision,
            now()
        ],
    )?;
    job.state = if printed {
        PersistentState::Succeeded
    } else {
        PersistentState::Failed
    };
    job.recovery = Some(if printed {
        "You reconciled this send: it printed".into()
    } else {
        "You reconciled this send: it did not print".into()
    });
    save(tx, &mut job, &format!("reconciled:{decision}"))
}
/// True when a queued, running or unresolved job holds `printer_id`.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn printer_reserved(
    tx: &Transaction<'_>,
    tenant: &str,
    printer_id: &str,
) -> Result<bool> {
    let resource = format!("printer:{printer_id}");
    Ok(reserved_resources(tx, tenant, true)?
        .iter()
        .any(|reserved| resources_overlap(&resource, reserved)))
}
/// Collects the job changes another family's transaction makes, so they
/// publish like a job command's once that transaction commits.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn begin_job_collection() {
    subscriptions::begin_collection();
}
/// Publishes the collected changes when the transaction committed. A commit
/// with an unknown outcome stops every stream, as a job command's does.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn end_job_collection(
    subscriptions: &JobSubscriptions,
    committed: bool,
    error: Option<&anyhow::Error>,
) {
    if committed {
        subscriptions::mark_committed();
        subscriptions.publish(subscriptions::finish_collection());
        return;
    }
    subscriptions::finish_collection();
    if error.is_some_and(|error| matches!(error.downcast_ref(), Some(JobFailure::CommitUnknown))) {
        subscriptions.stop();
    }
}
fn quarantine(tx: &Transaction<'_>, id: &str) -> Result<()> {
    // Preserve the original document and resource; only the attempt index changes.
    let changed = tx.execute(
        "UPDATE durable_jobs SET state='quarantined',lease_until=NULL,version=version+1,updated=?2 WHERE id=?1 AND state!='quarantined'",
        params![id, now()],
    )?;
    if changed != 0 {
        tx.execute(
            "INSERT INTO durable_job_history(job_id,version,at,state,event)
             SELECT id,version,updated,state,CASE WHEN resource='' THEN 'document_quarantined' ELSE 'document_quarantined_resource_uncertain' END FROM durable_jobs WHERE id=?1",
            [id],
        )?;
    }
    Ok(())
}
/// The state an interrupted attempt recovers to, and the recovery detail.
/// A recorded effect always needs reconciliation and is never re-queued;
/// the job kind's [`RecoveryPolicy`] names why.
fn recovery_decision(job: &JobRecord) -> (PersistentState, Option<&'static str>) {
    let owner_cleanup = job.awaits_owner_cleanup();
    let never_resend = job.kind.recovery_policy() == RecoveryPolicy::NeverResendAfterEffect;
    if job.effects.iter().any(|effect| !effect.confirmed) {
        let detail = if never_resend {
            "The send may have reached the printer; it is never resent automatically. Inspect and reconcile"
        } else {
            "Inspect effect receipts and reconcile explicitly"
        };
        (PersistentState::ReconciliationRequired, Some(detail))
    } else if !job.effects.is_empty() {
        let detail = if never_resend {
            "The printer accepted the send; it is never resent automatically. Reconcile to finish"
        } else {
            "Resume from confirmed receipts requires handler reconciliation"
        };
        (PersistentState::ReconciliationRequired, Some(detail))
    } else if job.cancel_requested && !owner_cleanup {
        (PersistentState::Cancelled, None)
    } else if job.attempt >= 100 && !owner_cleanup {
        (PersistentState::Failed, Some("Attempt limit reached"))
    } else {
        (PersistentState::Queued, None)
    }
}

fn recover_attempt(tx: &Transaction<'_>, mut job: JobRecord) -> Result<()> {
    let (state, detail) = recovery_decision(&job);
    job.state = state;
    #[cfg(feature = "desktop_rebuild")]
    if job.kind == JobKind::Sync {
        let halt = if job.state == PersistentState::Cancelled {
            Some(("cancelled", "Sync was cancelled"))
        } else {
            detail.map(|detail| ("fatal", detail))
        };
        if let Some((code, detail)) = halt {
            crate::source_sync::halt_job(tx, job, code, detail)?;
            return Ok(());
        }
    }
    job.generation += 1;
    job.fence = None;
    job.worker = None;
    job.lease_until = None;
    if let Some(detail) = detail {
        job.recovery = Some(detail.into());
    }
    save(tx, &mut job, "attempt_recovered")
}

fn recover_in(tx: &Transaction<'_>, all: bool) -> Result<()> {
    let mut diagnostics = DocumentDiagnostics::default();
    let rows=tx.prepare("SELECT id,lease_until,state FROM durable_jobs WHERE state IN ('running','effect_admitted') OR (?1 AND state!='quarantined')")?.query_map([all],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, lease_until, state) in rows {
        let document: String = tx.query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [&id],
            |row| row.get(0),
        )?;
        let Some(job) = decode_row(&id, &document, &mut diagnostics) else {
            quarantine(tx, &id)?;
            continue;
        };
        if !matches!(state.as_str(), "running" | "effect_admitted") {
            continue;
        }
        if !all && !lease_until.is_some_and(|until| until <= now()) {
            continue;
        }
        if let Err(error) = recover_attempt(tx, job) {
            if !matches!(
                error.downcast_ref::<JobFailure>(),
                Some(JobFailure::JobDocTooLarge { .. })
            ) {
                return Err(error);
            }
            diagnostics.record(&id, &error);
            quarantine(tx, &id)?;
        }
    }
    Ok(())
}
pub(crate) fn recover(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    recover_in(&tx, true)?;
    #[cfg(feature = "desktop_rebuild")]
    crate::uploads::settle_ended(&tx)?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn execute(
    connection: &mut Connection,
    command: Command,
    effect_policy: EffectAdmissionPolicy,
    admission: crate::writer_admission::Deadline,
    #[cfg(feature = "desktop_rebuild")] subscriptions: &JobSubscriptions,
    #[cfg(feature = "desktop_rebuild")] catalog_state: &mut crate::catalog::State,
) -> Result<Outcome> {
    #[cfg(feature = "desktop_rebuild")]
    subscriptions::begin_collection();
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    let decision = match command {
        Command::User {
            credential,
            policy,
            operation,
            storage,
        } => {
            let auth_committed = matches!(&credential, Credential::RoutedKey { .. });
            #[cfg(not(feature = "desktop_rebuild"))]
            let mut outcome = {
                let (tenant, subject) = actor(&tx, credential, policy, &storage)?;
                user(&tx, &tenant, &subject, operation)?
            };
            #[cfg(feature = "desktop_rebuild")]
            let mut outcome = if matches!(operation, UserOperation::Enqueue { .. }) {
                let (tenant, subject, authority) =
                    auth::authority::admit(&tx, credential, policy, &storage)?;
                user(&tx, &tenant, &subject, authority, operation)?
            } else {
                let (tenant, subject) = actor(&tx, credential, policy, &storage)?;
                user(&tx, &tenant, &subject, None, operation)?
            };
            if auth_committed && let Outcome::Job(_, commit) = &mut outcome {
                *commit = LocalCommit::Committed;
            }
            TransactionDecision::Outcome(outcome)
        }
        #[cfg(not(feature = "desktop_rebuild"))]
        Command::Claim { worker, admission } => claim(&tx, &worker, &admission)?,
        #[cfg(feature = "desktop_rebuild")]
        Command::Claim {
            worker,
            admission,
            job_id,
            kind,
            policy,
            storage,
        } => claim(
            &tx,
            &worker,
            &admission,
            ClaimFilter {
                job_id: job_id.as_deref(),
                kind,
            },
            policy,
            catalog_state,
            storage,
        )?,
        #[cfg(feature = "desktop_rebuild")]
        Command::ClaimResolved {
            claim: resolved,
            worker,
            admission,
            policy,
            storage,
        } => {
            let job_id = crate::uploads::validate_resolved_claim(&tx, &resolved)?;
            claim(
                &tx,
                &worker,
                &admission,
                ClaimFilter {
                    job_id: Some(&job_id),
                    kind: Some(JobKind::SuppliedSourceImport),
                },
                policy,
                catalog_state,
                storage,
            )?
        }
        #[cfg(not(feature = "desktop_rebuild"))]
        Command::Worker {
            lease,
            operation,
            admission,
        } => advance(&tx, lease, operation, &admission, effect_policy)?,
        #[cfg(feature = "desktop_rebuild")]
        Command::Worker {
            lease,
            operation,
            admission,
            policy,
        } => advance(&tx, lease, operation, &admission, effect_policy, policy)?,
        #[cfg(feature = "desktop_rebuild")]
        Command::Authorize { lease, policy } => {
            let job = claimed_job(&tx, &lease)?;
            TransactionDecision::Outcome(Outcome::Authorized(require_original_authority(
                &tx, &job, policy,
            )?))
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::Observe {
            credential,
            policy,
            job_id,
        } => {
            let authority = auth::resolve_stream_authority(&tx, &credential, policy, true)?;
            let job = owned(&tx, &job_id, &authority.tenant)?;
            ensure!(!job.internal_only(), JobAccessFailure::NotFound);
            TransactionDecision::Observation {
                credential,
                authority,
                policy,
                snapshot: job.snapshot(),
            }
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::ObserveLocalScan {
            lease,
            source_work,
            policy,
        } => TransactionDecision::Outcome(Outcome::LocalScanObservation(
            crate::source_scan::observe(&tx, catalog_state, &lease, &source_work, policy)?,
        )),
        #[cfg(feature = "desktop_rebuild")]
        Command::SettleLocalScan {
            lease,
            source_work,
            policy,
            settlement,
        } => {
            let (job, applied) = crate::source_scan::settle(
                &tx,
                catalog_state,
                &lease,
                &source_work,
                policy,
                settlement,
            )?;
            TransactionDecision::Outcome(Outcome::LocalScanSettled(job, applied))
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::CompleteLocalScan {
            lease,
            source_work,
            policy,
            applied,
            result,
        } => TransactionDecision::LocalScanCompleted(crate::source_scan::complete(
            &tx,
            catalog_state,
            &lease,
            source_work,
            policy,
            applied,
            result,
        )?),
        #[cfg(feature = "desktop_rebuild")]
        Command::InspectLocalScan { lease, worker } => {
            TransactionDecision::Outcome(Outcome::LocalScanCompletion(crate::source_scan::inspect(
                &tx,
                catalog_state,
                &worker,
                &lease,
            )?))
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::InspectHeldLocalScan {
            lease,
            source_work,
            worker,
        } => TransactionDecision::Outcome(Outcome::HeldLocalScan(
            crate::source_scan::inspect_held(&tx, catalog_state, &worker, &lease, &source_work)?,
        )),
        #[cfg(feature = "desktop_rebuild")]
        Command::ReconcileLocalScan {
            lease,
            source_work,
            applied,
            reason,
        } => TransactionDecision::LocalScanReconciled(crate::source_scan::reconcile(
            &tx,
            catalog_state,
            &lease,
            source_work,
            &applied,
            &reason,
        )?),
        #[cfg(feature = "desktop_rebuild")]
        Command::OpenSourceSync { lease, policy } => {
            match crate::source_sync::open(&tx, &lease, policy)? {
                Some(refusal) => TransactionDecision::Refused(refusal),
                None => TransactionDecision::Outcome(Outcome::SourceSyncOpened),
            }
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::ClaimSourceSyncTarget {
            lease,
            policy,
            storage,
        } => match crate::source_sync::next(&tx, catalog_state, &lease, policy)? {
            crate::source_sync::Next::Reserve(target) => {
                TransactionDecision::SourceSyncReserved(target, storage)
            }
            crate::source_sync::Next::Done(target, version) => {
                TransactionDecision::Outcome(Outcome::SourceSyncTarget(target, version))
            }
        },
        #[cfg(feature = "desktop_rebuild")]
        Command::BindSourceSyncTarget { lease, candidate } => {
            TransactionDecision::Outcome(Outcome::SourceSyncBound(crate::source_sync::bind(
                &tx,
                catalog_state,
                &lease,
                &candidate,
            )?))
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::SettleSourceSyncTarget {
            lease,
            candidate,
            settlement,
        } => TransactionDecision::SourceSyncSettled(crate::source_sync::settle(
            &tx,
            catalog_state,
            &lease,
            candidate,
            settlement,
        )?),
        #[cfg(feature = "desktop_rebuild")]
        Command::FailSourceSyncTarget {
            lease,
            candidate,
            detail,
        } => TransactionDecision::Outcome(Outcome::SourceSyncFailed(crate::source_sync::fail(
            &tx,
            catalog_state,
            &lease,
            candidate,
            &detail,
        )?)),
        #[cfg(feature = "desktop_rebuild")]
        Command::AcknowledgeSourceSyncTarget {
            lease,
            candidate,
            observation,
        } => TransactionDecision::SourceSyncAcknowledged(crate::source_sync::acknowledge(
            &tx,
            catalog_state,
            &lease,
            candidate,
            &observation,
        )?),
        #[cfg(feature = "desktop_rebuild")]
        Command::AdoptSourceSyncTarget { lease, key } => TransactionDecision::Outcome(
            Outcome::SourceSyncSettled(crate::source_sync::adopt(&tx, &lease, &key)?),
        ),
        #[cfg(feature = "desktop_rebuild")]
        Command::HaltSourceSync {
            lease,
            halt,
            detail,
        } => {
            ensure!(
                !detail.is_empty() && detail.len() <= 1024,
                "Invalid Sync halt detail"
            );
            let job = crate::source_sync::halt_after_error(&tx, &lease, halt.stored(), &detail)?
                .ok_or_else(|| anyhow!("Sync batch is not open"))?;
            TransactionDecision::Outcome(Outcome::SourceSyncHalted(job))
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::InspectSourceSyncTarget { lease, key, policy } => TransactionDecision::Outcome(
            Outcome::SourceSyncInspection(crate::source_sync::inspect(&tx, &lease, &key, policy)?),
        ),
        #[cfg(feature = "desktop_rebuild")]
        Command::FinishSourceSync { lease, policy } => {
            let result = crate::source_sync::finish(&tx, &lease, policy)?;
            let outcome = crate::source_sync::SourceSyncFinishOutcome::Completed(Box::new(
                crate::source_sync::CompletedSourceSync {
                    job: result.job,
                    aggregate: result.value,
                },
            ));
            TransactionDecision::Outcome(Outcome::SourceSyncFinish(outcome, result.commit))
        }
        #[cfg(feature = "desktop_rebuild")]
        Command::InspectRetainedSourceSync {
            credential,
            policy,
            job_id,
            expected_version,
            storage,
        } => {
            let (tenant, actor) = actor(&tx, credential, policy, &storage)?;
            let job = owned(&tx, &job_id, &tenant)?;
            let (subject, status) = match require_original_authority(&tx, &job, policy) {
                Ok(authority) => (
                    Some(authority.subject().to_owned()),
                    crate::source_sync::OriginalAuthorityStatus::Valid,
                ),
                Err(error) => crate::source_sync::original_authority_status(&error).ok_or(error)?,
            };
            let access = crate::source_sync::AuthorizedSyncRead::new(
                job,
                tenant,
                actor,
                expected_version,
                subject,
                status,
            )?;
            TransactionDecision::Outcome(Outcome::SourceSyncRecovery(
                crate::source_sync::inspect_retained(&tx, &access)?,
            ))
        }
        Command::Retain { per_tenant, global } => {
            TransactionDecision::Outcome(Outcome::Retained(prune(&tx, per_tenant, global)?))
        }
    };
    let terminal = match &decision {
        TransactionDecision::Outcome(Outcome::Job(job, LocalCommit::Committed)) => {
            job.state.terminal()
        }
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::Outcome(Outcome::SourceSyncHalted(job)) => job.state.terminal(),
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::Outcome(Outcome::SourceSyncFinish(_, LocalCommit::Committed)) => true,
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::Refused(CommittedRefusal { job, .. }) => job.state.terminal(),
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::LocalScanCompleted(_) => true,
        _ => false,
    };
    if terminal {
        prune(&tx, 1000, 10000)?;
    }
    #[cfg(not(feature = "desktop_rebuild"))]
    tx.commit()
        .map_err(|_| anyhow!(JobFailure::CommitUnknown))?;
    #[cfg(feature = "desktop_rebuild")]
    {
        if tx.commit().is_err() {
            subscriptions::finish_collection();
            subscriptions.stop();
            return Err(anyhow!(JobFailure::CommitUnknown));
        }
        subscriptions::mark_committed();
        subscriptions.publish(subscriptions::finish_collection());
    }
    match decision {
        TransactionDecision::Outcome(outcome) => Ok(outcome),
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::Claimed(plan) => {
            Ok(Outcome::Claimed(Some(plan.into_claimed(catalog_state)?)))
        }
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::ErrorAfterCommit(error) => Err(error),
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::Refused(CommittedRefusal { failure, .. }) => Err(failure.into()),
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::LocalScanCompleted(pending) => {
            use crate::catalog::IssuedTokenState;
            let outcome = match catalog_state
                .classify_issued(&pending.proof.incarnation, pending.proof.token)
            {
                IssuedTokenState::Active(actual) if actual == &pending.binding => {
                    catalog_state.retire_issued(&pending.proof, &pending.binding)?;
                    crate::source_scan::LocalCompletionInspection::Completed(pending.completion)
                }
                IssuedTokenState::Retired => {
                    crate::source_scan::LocalCompletionInspection::Completed(pending.completion)
                }
                IssuedTokenState::Active(_) => {
                    crate::source_scan::LocalCompletionInspection::CommittedReservationConflict(
                        pending.completion,
                    )
                }
                IssuedTokenState::NeverIssued | IssuedTokenState::WrongIncarnation => {
                    crate::source_scan::LocalCompletionInspection::CommittedReservationCorrupt(
                        pending.completion,
                    )
                }
            };
            Ok(Outcome::LocalScanCompletion(outcome))
        }
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::LocalScanReconciled(pending) => {
            use crate::catalog::IssuedTokenState;
            match catalog_state.classify_issued(&pending.proof.incarnation, pending.proof.token) {
                IssuedTokenState::Active(actual) if actual == &pending.binding => {
                    catalog_state.retire_issued(&pending.proof, &pending.binding)?;
                }
                IssuedTokenState::Retired => {}
                IssuedTokenState::Active(_) => {
                    return Err(anyhow!(
                        "Committed Local scan reconciliation has a Source reservation conflict"
                    ));
                }
                IssuedTokenState::NeverIssued | IssuedTokenState::WrongIncarnation => {
                    return Err(anyhow!(
                        "Committed Local scan reconciliation has a corrupt Source reservation"
                    ));
                }
            }
            Ok(Outcome::LocalScanReconciled(pending.job))
        }
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::SourceSyncReserved(target, storage) => {
            let (target, version) = target.activate(catalog_state, storage)?;
            Ok(Outcome::SourceSyncTarget(target, version))
        }
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::SourceSyncSettled(pending) => {
            catalog_state.retire_issued(&pending.proof, &pending.binding)?;
            Ok(Outcome::SourceSyncSettled(pending.settled))
        }
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::SourceSyncAcknowledged(pending) => {
            catalog_state.retire_issued(&pending.proof, &pending.binding)?;
            Ok(Outcome::SourceSyncAcknowledged(pending.acknowledged))
        }
        #[cfg(feature = "desktop_rebuild")]
        TransactionDecision::Observation {
            credential,
            authority,
            policy,
            snapshot,
        } => subscriptions
            .register(credential, authority, policy, snapshot)
            .map(Outcome::Observed),
    }
}
/// Runs an authority write under the stream admission gate, then revokes the
/// streams whose credential no longer resolves to the same tenant and subject.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn authority_change<T>(
    connection: &mut Connection,
    subscriptions: &JobSubscriptions,
    change: impl FnOnce(&mut Connection) -> Result<T>,
) -> Result<T> {
    subscriptions.authority_change(|| {
        let result = change(connection);
        if subscriptions.revalidate(connection).is_err() {
            subscriptions.stop_under_authority_gate();
        }
        result
    })
}
pub(crate) fn actor(
    tx: &Transaction<'_>,
    credential: Credential,
    policy: AuthPolicy,
    storage: &Arc<crate::Shared>,
) -> Result<(String, String)> {
    match credential {
        Credential::Session(token) => auth::job_session_actor(tx, token.expose(), policy),
        Credential::RoutedKey { tenant, key } => auth::job_key_actor(tx, &tenant, key),
        Credential::PhysicalOwner(owner) => {
            ensure!(
                Arc::ptr_eq(storage, &owner.storage),
                "Foreign physical owner"
            );
            Ok(("default".into(), "physical-owner".into()))
        }
    }
}
fn prune(tx: &Transaction<'_>, per_tenant: usize, global: usize) -> Result<usize> {
    #[cfg(feature = "desktop_rebuild")]
    let held = crate::source_sync::PRUNE_HOLD;
    #[cfg(not(feature = "desktop_rebuild"))]
    let held = "";
    let mut statement=tx.prepare(&format!(
        "WITH ranked AS (
            SELECT id,document,updated,rowid AS sequence,
                   ROW_NUMBER() OVER(PARTITION BY tenant ORDER BY updated DESC,rowid DESC) AS local_rank
            FROM durable_jobs WHERE state IN ('uploaded_only','succeeded','failed','cancelled'){held}
         )
         SELECT id,document FROM ranked
         WHERE updated<=?3 OR local_rank>?1 OR id NOT IN (
             SELECT id FROM ranked WHERE local_rank<=?1 AND updated>?3
             ORDER BY updated DESC,sequence DESC LIMIT ?2
         )"))?;
    let removed = statement
        .query_map(
            params![per_tenant as i64, global as i64, now() - 86400],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, document) in &removed {
        tx.execute(
            "UPDATE durable_job_keys SET archived_document=?2 WHERE job_id=?1",
            params![id, document],
        )?;
        tx.execute("DELETE FROM durable_jobs WHERE id=?1", [id])?;
    }
    Ok(removed.len())
}
pub(crate) fn user(
    tx: &Transaction<'_>,
    tenant: &str,
    subject: &str,
    #[cfg(feature = "desktop_rebuild")] authority: Option<auth::authority::AuthorityBasis>,
    operation: UserOperation,
) -> Result<Outcome> {
    match operation {
        UserOperation::Enqueue {
            key,
            payload_version,
            mut payload,
        } => {
            model::text(&key, 128)?;
            ensure!(payload_version == 1, "Unsupported payload version");
            payload.validate()?;
            let intent = hex::encode(Sha256::digest(serde_json::to_vec(&(
                payload_version,
                &payload,
            ))?));
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT intent,job_id FROM durable_job_keys WHERE tenant=?1 AND key=?2",
                    params![tenant, key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((hash, id)) = prior {
                ensure!(hash == intent, "Idempotency conflict");
                return Ok(Outcome::Job(
                    retained_owned(tx, &id, tenant)?,
                    LocalCommit::ReadOnly,
                ));
            }
            payload = bind_printer_start(tx, tenant, payload)?;
            let active:i64=tx.query_row("SELECT COUNT(*) FROM durable_jobs WHERE state NOT IN ('uploaded_only','succeeded','failed','cancelled')",[],|r|r.get(0))?;
            ensure!(active < 1024, "Durable queue full");
            let mut job = JobRecord {
                job_id: random(),
                tenant: tenant.into(),
                kind: payload.kind(),
                payload_version,
                payload,
                state: PersistentState::Queued,
                state_version: 0,
                attempt: 0,
                generation: 0,
                lease_until: None,
                created_at: now(),
                updated_at: now(),
                finished_at: None,
                cancel_requested: false,
                progress: None,
                effects: vec![],
                result: None,
                #[cfg(feature = "desktop_rebuild")]
                public_result: None,
                recovery: None,
                #[cfg(feature = "desktop_rebuild")]
                authority_disposition: if authority.is_some() {
                    AuthorityDisposition::Original
                } else {
                    AuthorityDisposition::PhysicalOwner
                },
                #[cfg(feature = "desktop_rebuild")]
                authority,
                #[cfg(feature = "desktop_rebuild")]
                authority_refusal: None,
                fence: None,
                worker: None,
            };
            save(tx, &mut job, "enqueued")?;
            tx.execute(
                "INSERT INTO durable_job_keys(tenant,key,intent,job_id) VALUES(?1,?2,?3,?4)",
                params![tenant, key, intent, job.job_id],
            )?;
            Ok(Outcome::Job(job, LocalCommit::Committed))
        }
        UserOperation::Get { job_id } => Ok(Outcome::Job(
            owned(tx, &job_id, tenant)?,
            LocalCommit::ReadOnly,
        )),
        #[cfg(feature = "desktop_rebuild")]
        UserOperation::GetPublic { job_id } => {
            let job = owned(tx, &job_id, tenant)?;
            ensure!(!job.internal_only(), JobAccessFailure::NotFound);
            Ok(Outcome::PublicJob(job.snapshot()))
        }
        #[cfg(feature = "desktop_rebuild")]
        UserOperation::ListRetained(filter) => {
            prune(tx, 1000, 10000)?;
            let rows = tx
                .prepare("SELECT rowid,id,document FROM durable_jobs WHERE tenant=?1 AND state!='quarantined'")?
                .query_map([tenant], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut diagnostics = DocumentDiagnostics::default();
            let mut jobs = rows
                .into_iter()
                .filter_map(|(sequence, id, document)| {
                    decode_row(&id, &document, &mut diagnostics).map(|job| (sequence, job))
                })
                .filter(|(_, job)| {
                    if job.internal_only() {
                        return false;
                    }
                    let status = match &filter.status {
                        LegacyStatusFilter::Any => true,
                        LegacyStatusFilter::Exact(status) => job.snapshot().status == status,
                        LegacyStatusFilter::NoMatch => false,
                    };
                    let since = filter.since_millis.is_none_or(|cutoff| {
                        job.finished_at
                            .unwrap_or(job.updated_at)
                            .saturating_mul(1000)
                            >= cutoff
                    });
                    let profile = match filter.profile {
                        LegacyProfileFilter::Any => true,
                        LegacyProfileFilter::Exact(profile) => {
                            job.payload.profile_id() == Some(profile)
                        }
                        LegacyProfileFilter::NoMatch => false,
                    };
                    status && since && profile
                })
                .collect::<Vec<_>>();
            // Newest first; ties keep enqueue order, as Node's stable sort does.
            jobs.sort_by(|(left_sequence, left), (right_sequence, right)| {
                right
                    .finished_at
                    .unwrap_or(right.updated_at)
                    .cmp(&left.finished_at.unwrap_or(left.updated_at))
                    .then_with(|| left_sequence.cmp(right_sequence))
            });
            Ok(Outcome::PublicList(RetainedJobs {
                snapshots: jobs.into_iter().map(|(_, job)| job.snapshot()).collect(),
            }))
        }
        UserOperation::List(query) => {
            prune(tx, 1000, 10000)?;
            ensure!((1..=200).contains(&query.limit), "Invalid list limit");
            if let Some(status) = &query.status {
                ensure!(
                    ["pending", "running", "done", "error", "cancelled"].contains(&status.as_str()),
                    "Invalid job status"
                );
            }
            if let Some(id) = query.profile_id {
                ensure!(
                    id > 0 && id <= 9_007_199_254_740_991,
                    "Invalid profile identifier"
                );
            }
            if let Some((_, id)) = &query.before {
                model::text(id, 128)?;
            }
            let mut statement = tx.prepare("SELECT id,document FROM durable_jobs WHERE tenant=?1 AND state!='quarantined' AND (?2 IS NULL OR CASE state WHEN 'queued' THEN 'pending' WHEN 'running' THEN 'running' WHEN 'effect_admitted' THEN 'running' WHEN 'uploaded_only' THEN 'done' WHEN 'succeeded' THEN 'done' WHEN 'cancelled' THEN 'cancelled' ELSE 'error' END=?2) AND (?3 IS NULL OR updated>=?3) AND (?4 IS NULL OR json_extract(document,'$.payload.payload.profile_id')=?4) AND (?5 IS NULL OR updated<?5 OR (updated=?5 AND id<?6)) ORDER BY updated DESC,id DESC")?;
            let mut rows = statement.query(params![
                tenant,
                query.status,
                query.since,
                query.profile_id.map(|v| v as i64),
                query.before.as_ref().map(|c| c.0),
                query.before.as_ref().map(|c| c.1.as_str()),
            ])?;
            let mut jobs = Vec::with_capacity(usize::from(query.limit));
            let mut diagnostics = DocumentDiagnostics::default();
            while jobs.len() < usize::from(query.limit) {
                let Some(row) = rows.next()? else {
                    break;
                };
                let id: String = row.get(0)?;
                let document: String = row.get(1)?;
                if let Some(job) = decode_row(&id, &document, &mut diagnostics) {
                    jobs.push(job);
                }
            }
            Ok(Outcome::List(jobs))
        }
        UserOperation::Quarantined { before, limit } => {
            ensure!((1..=200).contains(&limit), "Invalid quarantine list limit");
            if let Some((_, id)) = &before {
                model::text(id, 128)?;
            }
            let mut statement = tx.prepare(
                "SELECT id,kind,updated,resource,document FROM durable_jobs WHERE tenant=?1 AND state='quarantined'
                 AND (?2 IS NULL OR updated<?2 OR (updated=?2 AND id<?3)) ORDER BY updated DESC,id DESC LIMIT ?4",
            )?;
            let records = statement
                .query_map(
                    params![
                        tenant,
                        before.as_ref().map(|c| c.0),
                        before.as_ref().map(|c| c.1.as_str()),
                        limit
                    ],
                    |row| {
                        let resource: String = row.get(3)?;
                        let document: String = row.get(4)?;
                        Ok(QuarantinedJob {
                            job_id: row.get(0)?,
                            kind: row.get(1)?,
                            updated_at: row.get(2)?,
                            reason: decode(&document)
                                .err()
                                .map(|e| document_failure_reason(&e))
                                .unwrap_or_else(|| {
                                    "Document quarantined; operator inspection required".into()
                                }),
                            uncertain_resource: (!resource.is_empty()).then_some(resource),
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Outcome::Quarantined(records))
        }
        UserOperation::History {
            job_id,
            before_version,
            limit,
        } => {
            owned(tx, &job_id, tenant)?;
            ensure!((1..=200).contains(&limit), "Invalid history limit");
            let entries=tx.prepare("SELECT version,at,state,event FROM durable_job_history WHERE job_id=?1 AND (?2 IS NULL OR version<?2) ORDER BY version DESC LIMIT ?3")?.query_map(params![job_id,before_version,limit],|r|Ok(HistoryEntry{version:r.get(0)?,at:r.get(1)?,state:r.get(2)?,event:r.get(3)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Outcome::History(entries))
        }
        UserOperation::Reconciliations {
            job_id,
            before_version,
            limit,
        } => {
            owned(tx, &job_id, tenant)?;
            ensure!((1..=200).contains(&limit), "Invalid reconciliation limit");
            let entries=tx.prepare("SELECT version,subject,generation,effect_hash,basis_hash,target,decision,receipt,at FROM durable_job_reconciliations WHERE job_id=?1 AND (?2 IS NULL OR version<?2) ORDER BY version DESC LIMIT ?3")?.query_map(params![job_id,before_version,limit],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,i64>(8)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Outcome::Reconciliations(
                entries
                    .into_iter()
                    .map(
                        |(
                            version,
                            subject,
                            generation,
                            effect_hash,
                            basis_hash,
                            target,
                            decision,
                            receipt,
                            at,
                        )| {
                            Ok(ReconciliationRecord {
                                version,
                                subject,
                                generation,
                                effect_hash,
                                basis_hash,
                                target,
                                decision,
                                receipt: serde_json::from_str(&receipt)?,
                                at,
                            })
                        },
                    )
                    .collect::<Result<_>>()?,
            ))
        }
        UserOperation::Cancel { job_id } => {
            let mut job = owned(tx, &job_id, tenant)?;
            if job.state.terminal() || job.cancel_requested {
                // An import that ended before its cleanup phase settles here.
                #[cfg(feature = "desktop_rebuild")]
                if crate::uploads::settle_terminal(tx, &job)? {
                    return Ok(Outcome::Job(job, LocalCommit::Committed));
                }
                return Ok(Outcome::Job(job, LocalCommit::ReadOnly));
            }
            job.cancel_requested = true;
            #[cfg(feature = "desktop_rebuild")]
            if job.kind == JobKind::Sync {
                if job.state == PersistentState::ReconciliationRequired
                    && job.authority_refusal.as_ref().is_some_and(|refusal| {
                        refusal.phase == AuthorityRefusalPhase::SourceWriter
                            && refusal.generation == job.generation - 1
                    })
                {
                    save(tx, &mut job, "cancellation_requested")?;
                    return Ok(Outcome::Job(job, LocalCommit::Committed));
                }
                let job = crate::source_sync::halt_job(tx, job, "cancelled", "Sync was cancelled")?;
                return Ok(Outcome::Job(job, LocalCommit::Committed));
            }
            // The import's owner reclaims it to release its input and quota.
            #[cfg(feature = "desktop_rebuild")]
            if job.kind == JobKind::SuppliedSourceImport {
                job.state = PersistentState::Queued;
                job.generation += 1;
                job.fence = None;
                job.worker = None;
                job.lease_until = None;
                save(tx, &mut job, "cancellation_requested")?;
                return Ok(Outcome::Job(job, LocalCommit::Committed));
            }
            if job.effects.is_empty() {
                job.state = PersistentState::Cancelled;
                job.generation += 1;
            } else {
                job.state = PersistentState::ReconciliationRequired;
                job.generation += 1;
                job.fence = None;
                job.worker = None;
                job.lease_until = None;
                job.recovery = Some("Cancellation cannot undo an admitted effect".into());
            }
            save(tx, &mut job, "cancellation_requested")?;
            Ok(Outcome::Job(job, LocalCommit::Committed))
        }
        UserOperation::Reconcile {
            job_id,
            expected_version,
            expected_generation,
            effect_hash,
            decision,
            receipt,
        } => {
            let mut job = owned(tx, &job_id, tenant)?;
            // The Sync batch owns its retained evidence and reconciliation.
            #[cfg(feature = "desktop_rebuild")]
            ensure!(
                job.kind != JobKind::Sync,
                "Sync cannot use generic effect reconciliation"
            );
            #[cfg(feature = "desktop_rebuild")]
            ensure!(
                decision != Decision::ConfirmSucceeded
                    || job
                        .effects
                        .iter()
                        .all(|effect| effect.checklist_completion.is_none()),
                "Checklist completion requires its owning reconciler"
            );
            ensure!(
                job.state == PersistentState::ReconciliationRequired
                    && job.state_version == expected_version
                    && job.generation == expected_generation,
                "Stale reconciliation"
            );
            let effect_index = job
                .effects
                .len()
                .checked_sub(1)
                .ok_or_else(|| anyhow!("Missing effect"))?;
            ensure!(
                job.effects[effect_index].intent.content_hash == effect_hash,
                "Wrong reconciliation subject"
            );
            if let Some(receipt) = &receipt {
                receipt.validate()?;
                ensure!(
                    receipt.content_hash == job.effects[effect_index].intent.content_hash
                        && receipt.target == job.effects[effect_index].intent.target,
                    "Receipt mismatch"
                );
            }
            let prior_result = job.result.clone();
            let mut subject_receipt = None;
            match decision {
                Decision::ConfirmSucceeded => {
                    let receipt =
                        receipt.ok_or_else(|| anyhow!("Confirmation receipt required"))?;
                    let effect = &mut job.effects[effect_index];
                    match effect.outcome()? {
                        EffectOutcome::Confirmed(stored) => {
                            ensure!(stored == &receipt, "Receipt mismatch")
                        }
                        EffectOutcome::Unresolved => effect.confirm(receipt.clone())?,
                        #[cfg(feature = "desktop_rebuild")]
                        EffectOutcome::Observed(_) => effect.confirm(receipt.clone())?,
                        EffectOutcome::Denied => return Err(anyhow!("Effect already resolved")),
                    }
                    subject_receipt = Some(receipt.clone());
                    job.result = Some(receipt);
                    if let Some(primary) = proven_primary_printer_receipt(&job).cloned() {
                        job.result = Some(primary);
                        job.state = PersistentState::Succeeded;
                    } else if completion_proven(&job, job.result.as_ref()) {
                        job.state = PersistentState::Succeeded;
                    } else if !settle_uploaded_only(&mut job) {
                        if resolved_printer_failure(&job) {
                            job.result = prior_result;
                            job.state = PersistentState::Failed;
                        } else {
                            return Err(anyhow!(
                                "Remaining job effects require an explicit decision"
                            ));
                        }
                    }
                }
                Decision::ConfirmNoEffect => {
                    ensure!(receipt.is_none(), "No-effect decision cannot carry receipt");
                    job.effects[effect_index].deny()?;
                    if let Some(primary) = proven_primary_printer_receipt(&job).cloned() {
                        job.result = Some(primary);
                        job.state = PersistentState::Succeeded;
                    } else if !settle_uploaded_only(&mut job) {
                        job.result = prior_result;
                        job.state = PersistentState::Failed;
                    }
                }
                Decision::Abandon => {
                    job.state = PersistentState::ReconciliationRequired;
                }
            }
            let effect = job
                .effects
                .last()
                .ok_or_else(|| anyhow!("Reconciliation effect required"))?;
            tx.execute("INSERT INTO durable_job_reconciliations(job_id,version,subject,generation,effect_hash,basis_hash,target,decision,receipt,at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![job.job_id,job.state_version+1,subject,expected_generation,effect_hash,effect.intent.basis_hash,effect.intent.target,format!("{decision:?}"),serde_json::to_string(&subject_receipt)?,now()])?;
            job.recovery = Some(format!("User reconciliation: {decision:?}"));
            save(
                tx,
                &mut job,
                &format!(
                    "reconciled:{decision:?}:{}:{}",
                    expected_generation, effect_hash
                ),
            )?;
            Ok(Outcome::Job(job, LocalCommit::Committed))
        }
    }
}
#[cfg(feature = "desktop_rebuild")]
struct SourceReservation {
    token: u64,
    tenant: String,
    id: i64,
    client: SettingsClient,
}
#[cfg(feature = "desktop_rebuild")]
struct ClaimPlan {
    job: JobRecord,
    lease: AttemptLease,
    source: Option<SourceReservation>,
}
#[cfg(feature = "desktop_rebuild")]
impl ClaimPlan {
    fn into_claimed(self, state: &mut crate::catalog::State) -> Result<ClaimedAttempt> {
        let source_work = self
            .source
            .map(|source| -> Result<_> {
                state.activate(source.token, source.tenant, source.id);
                let proof = state.proof(source.token)?;
                Ok(crate::catalog::lease(source.client, proof))
            })
            .transpose()?;
        Ok(ClaimedAttempt {
            job: self.job,
            lease: self.lease,
            source_work,
        })
    }
}
#[cfg(feature = "desktop_rebuild")]
enum SourceBinding {
    None,
    Individual(i64),
    Wildcard,
}
/// Sync reserves each target's Source when it claims the target.
#[cfg(feature = "desktop_rebuild")]
fn source_binding(payload: &Payload) -> Result<SourceBinding> {
    Ok(match payload {
        Payload::ImportScan { project_id }
        | Payload::ExtractSourceDocs { project_id }
        | Payload::SuppliedSourceImport { project_id, .. } => {
            SourceBinding::Individual(i64::try_from(*project_id)?)
        }
        Payload::CheckSourceUpdates {} => SourceBinding::Wildcard,
        _ => SourceBinding::None,
    })
}
#[cfg(feature = "desktop_rebuild")]
struct ClaimFilter<'a> {
    job_id: Option<&'a str>,
    kind: Option<JobKind>,
}
fn claim(
    tx: &Transaction<'_>,
    worker: &str,
    admission: &WorkerAdmission,
    #[cfg(feature = "desktop_rebuild")] filter: ClaimFilter<'_>,
    #[cfg(feature = "desktop_rebuild")] policy: Option<AuthPolicy>,
    #[cfg(feature = "desktop_rebuild")] catalog_state: &crate::catalog::State,
    #[cfg(feature = "desktop_rebuild")] storage: SettingsClient,
) -> Result<TransactionDecision> {
    let mut diagnostics = DocumentDiagnostics::default();
    recover_in(tx, false)?;
    let active: i64 = tx.query_row(
        "SELECT COUNT(*) FROM durable_jobs WHERE state IN ('running','effect_admitted')",
        [],
        |r| r.get(0),
    )?;
    if active >= admission.total as i64 {
        return Ok(TransactionDecision::Outcome(Outcome::Claimed(None)));
    }
    #[cfg(feature = "desktop_rebuild")]
    let mut refused = false;
    for (kind, capacity) in &admission.kinds {
        #[cfg(feature = "desktop_rebuild")]
        if filter.kind.is_some_and(|requested| requested != *kind) {
            continue;
        }
        let count:i64=tx.query_row("SELECT COUNT(*) FROM durable_jobs WHERE kind=?1 AND state IN ('running','effect_admitted')",[kind.name()],|r|r.get(0))?;
        if count >= *capacity as i64 {
            continue;
        }
        let documents=tx.prepare("SELECT id,document FROM durable_jobs WHERE state='queued' AND kind=?1 ORDER BY created,id")?.query_map([kind.name()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, document) in documents {
            #[cfg(feature = "desktop_rebuild")]
            if filter.job_id.is_some_and(|target| target != id) {
                continue;
            }
            let Some(mut job) = decode_row(&id, &document, &mut diagnostics) else {
                continue;
            };
            #[cfg(feature = "desktop_rebuild")]
            if filter.job_id.is_none() && crate::uploads::awaits_owner_input(tx, &job)? {
                continue;
            }
            let resource = job.payload.resource();
            if !resource.is_empty() {
                let count = reserved_resources(tx, &job.tenant, false)?
                    .iter()
                    .filter(|reserved| resources_overlap(&resource, reserved))
                    .count();
                if count >= admission.per_resource {
                    continue;
                }
            }
            ensure!(
                job.attempt < 100 || job.awaits_owner_cleanup(),
                "Job attempt limit reached"
            );
            #[cfg(feature = "desktop_rebuild")]
            {
                let refusal = match job.authority_disposition {
                    AuthorityDisposition::PhysicalOwner => None,
                    AuthorityDisposition::LegacyMissing => {
                        Some(AuthorityRefusalReason::MissingOriginal)
                    }
                    AuthorityDisposition::Original => {
                        let Some(policy) = policy else {
                            if refused {
                                prune(tx, 1000, 10000)?;
                            }
                            return Ok(TransactionDecision::ErrorAfterCommit(
                                auth::AuthorityFailure::PolicyRequired.into(),
                            ));
                        };
                        refusal_reason(tx, &job, policy)?
                    }
                };
                if let Some(reason) = refusal {
                    if filter.job_id.is_some() {
                        crate::uploads::record_targeted_authority_refusal(tx, &job)?;
                    } else if job.kind == JobKind::SuppliedSourceImport {
                        crate::uploads::record_recovered_authority_refusal(tx, &job)?;
                    }
                    let phase = if filter.job_id.is_some() {
                        AuthorityRefusalPhase::TargetedClaim
                    } else {
                        AuthorityRefusalPhase::Claim
                    };
                    record_refusal(tx, &mut job, reason, phase, None)?;
                    if filter.job_id.is_some() {
                        return Ok(TransactionDecision::Refused(CommittedRefusal {
                            job,
                            failure: reason.failure(),
                        }));
                    }
                    refused |= job.state.terminal();
                    continue;
                }
            }
            #[cfg(feature = "desktop_rebuild")]
            let source = match source_binding(&job.payload)? {
                SourceBinding::Individual(id) => {
                    if crate::catalog::get(tx, &job.tenant, id)?.is_none() {
                        fail_source_unavailable(tx, &mut job)?;
                        continue;
                    }
                    if catalog_state.source_busy(&job.tenant, id) {
                        if filter.job_id.is_some() {
                            return Ok(TransactionDecision::ErrorAfterCommit(anyhow!(
                                crate::catalog::SourceBusy
                            )));
                        }
                        continue;
                    }
                    Some(SourceReservation {
                        token: catalog_state.next_token()?,
                        tenant: job.tenant.clone(),
                        id,
                        client: storage,
                    })
                }
                SourceBinding::None | SourceBinding::Wildcard => None,
            };
            job.state = PersistentState::Running;
            job.attempt += 1;
            job.generation += 1;
            let fence = random();
            job.lease_until = Some(lease_expiry(
                time::OffsetDateTime::now_utc(),
                admission.lease_seconds,
            ));
            job.fence = Some(fence.clone());
            job.worker = Some(worker.into());
            save(tx, &mut job, "claimed")?;
            let lease = AttemptLease {
                job_id: job.job_id.clone(),
                tenant: job.tenant.clone(),
                generation: job.generation,
                version: job.state_version,
                fence,
                worker: worker.into(),
            };
            #[cfg(feature = "desktop_rebuild")]
            if refused {
                prune(tx, 1000, 10000)?;
            }
            #[cfg(feature = "desktop_rebuild")]
            return Ok(TransactionDecision::Claimed(ClaimPlan {
                job,
                lease,
                source,
            }));
            #[cfg(not(feature = "desktop_rebuild"))]
            return Ok(TransactionDecision::Outcome(Outcome::Claimed(Some((
                job, lease,
            )))));
        }
    }
    #[cfg(feature = "desktop_rebuild")]
    if refused {
        prune(tx, 1000, 10000)?;
    }
    Ok(TransactionDecision::Outcome(Outcome::Claimed(None)))
}
fn proven_primary_printer_receipt(job: &JobRecord) -> Option<&ResultArtifact> {
    if let Payload::PrinterStart(request) = &job.payload {
        let upload = request.upload_effect()?;
        return job.effects.iter().find_map(|effect| {
            if effect.intent.operation != EffectOperation::PrinterStart
                || effect.intent.basis_hash != upload.intent.basis_hash
                || effect.intent.content_hash != upload.intent.content_hash
                || effect.intent.target != upload.intent.target
            {
                return None;
            }
            match effect.outcome().ok()? {
                EffectOutcome::Confirmed(receipt) => Some(receipt),
                _ => None,
            }
        });
    }
    let Payload::PrinterUpload {
        printer_id, start, ..
    } = &job.payload
    else {
        return None;
    };
    let confirmed = |operation| {
        job.effects.iter().find_map(|effect| {
            if effect.intent.operation != operation || effect.intent.target != *printer_id {
                return None;
            }
            match effect.outcome().ok()? {
                EffectOutcome::Confirmed(receipt) => Some(receipt),
                _ => None,
            }
        })
    };
    if let Some(receipt) = confirmed(EffectOperation::PrinterUploadAndStart) {
        return Some(receipt);
    }
    let upload = confirmed(EffectOperation::PrinterUpload)?;
    if *start {
        confirmed(EffectOperation::PrinterStart)
    } else {
        Some(upload)
    }
}
fn completion_proven(job: &JobRecord, result: Option<&ResultArtifact>) -> bool {
    match &job.payload {
        #[cfg(feature = "desktop_rebuild")]
        Payload::SuppliedSourceImport { .. } => false,
        Payload::PrinterUpload { .. } => {
            proven_primary_printer_receipt(job).is_some_and(|primary| result == Some(primary))
        }
        Payload::PrinterStart(request) => request.upload_effect().is_some_and(|upload| {
            job.effects.iter().any(|effect| {
                effect.intent.operation == EffectOperation::PrinterStart
                    && effect.intent.basis_hash == upload.intent.basis_hash
                    && effect.intent.content_hash == upload.intent.content_hash
                    && effect.intent.target == upload.intent.target
                    && effect.confirmed
                    && effect.receipt.as_ref() == result
            })
        }),
        Payload::ExportStlPack { .. }
        | Payload::ExportChecklistHtml { .. }
        | Payload::ExportKitBundle { .. }
        | Payload::ExportAcceptedPlate3mf { .. }
        | Payload::ExportDirect3mf { .. } => result.is_some(),
        _ => true,
    }
}
fn settle_uploaded_only(job: &mut JobRecord) -> bool {
    let prior = job.result.clone();
    job.result = job
        .effects
        .iter()
        .find(|effect| effect.intent.operation == EffectOperation::PrinterUpload)
        .and_then(|effect| match effect.outcome().ok()? {
            EffectOutcome::Confirmed(receipt) => Some(receipt.clone()),
            _ => None,
        });
    if job.uploaded_only_proof().is_some() {
        job.state = PersistentState::UploadedOnly;
        true
    } else {
        job.result = prior;
        false
    }
}
fn resolved_printer_failure(job: &JobRecord) -> bool {
    matches!(job.payload, Payload::PrinterUpload { .. })
        && !job.effects.is_empty()
        && job.effects.iter().all(|effect| {
            matches!(
                effect.intent.operation,
                EffectOperation::PrinterUpload
                    | EffectOperation::PrinterStart
                    | EffectOperation::PrinterUploadAndStart
                    | EffectOperation::SpoolmanDeduction
            ) && matches!(
                effect.outcome(),
                Ok(EffectOutcome::Confirmed(_) | EffectOutcome::Denied)
            )
        })
        && !job.effects.iter().any(|effect| {
            matches!(effect.outcome(), Ok(EffectOutcome::Confirmed(_)))
                && matches!(
                    effect.intent.operation,
                    EffectOperation::PrinterUpload
                        | EffectOperation::PrinterStart
                        | EffectOperation::PrinterUploadAndStart
                )
        })
}
pub(crate) fn claimed_job(tx: &Transaction<'_>, lease: &AttemptLease) -> Result<JobRecord> {
    let job = owned(tx, &lease.job_id, &lease.tenant)?;
    ensure!(
        job.worker.as_deref() == Some(lease.worker.as_str())
            && job.generation == lease.generation
            && job.state_version == lease.version
            && job.fence.as_deref() == Some(&lease.fence)
            && job.lease_until.is_some_and(|until| until > now())
            && matches!(
                job.state,
                PersistentState::Running | PersistentState::EffectAdmitted
            ),
        "Stale attempt fence"
    );
    Ok(job)
}
pub(crate) fn claimed_source(
    tx: &Transaction<'_>,
    lease: &AttemptLease,
    source_id: Option<i64>,
) -> Result<(String, i64)> {
    let job = claimed_job(tx, lease)?;
    #[cfg(not(feature = "desktop_rebuild"))]
    let id = match job.payload {
        Payload::ImportScan { project_id } | Payload::ExtractSourceDocs { project_id } => {
            ensure!(
                source_id.is_none(),
                "Individual Source attempt uses its stored Source"
            );
            i64::try_from(project_id)?
        }
        Payload::Sync { project_ids } => {
            let id = source_id.ok_or_else(|| anyhow!("Sync attempt requires Source"))?;
            if let Some(ids) = project_ids {
                ensure!(
                    u64::try_from(id).is_ok_and(|id| ids.contains(&id)),
                    "Source outside Sync scope"
                );
            }
            id
        }
        Payload::CheckSourceUpdates {} => {
            source_id.ok_or_else(|| anyhow!("Wildcard Source attempt requires Source"))?
        }
        _ => return Err(anyhow!("Attempt is not Source work")),
    };
    #[cfg(feature = "desktop_rebuild")]
    let id = match source_binding(&job.payload)? {
        SourceBinding::Individual(id) => {
            ensure!(
                source_id.is_none(),
                "Individual Source attempt uses its stored Source"
            );
            id
        }
        SourceBinding::Wildcard => {
            source_id.ok_or_else(|| anyhow!("Wildcard Source attempt requires Source"))?
        }
        SourceBinding::None => return Err(anyhow!("Attempt is not Source work")),
    };
    Ok((job.tenant, id))
}
pub(crate) fn source_reserved(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<bool> {
    let resource = format!("source:{id}");
    Ok(reserved_resources(tx, tenant, true)?
        .iter()
        .any(|reserved| resources_overlap(&resource, reserved)))
}
// The column is retained for damaged rows. Healthy legacy scoped Sync rows may
// still have source:* in the column; their validated payload supplies the scope.
fn reserved_resources(tx: &Transaction<'_>, tenant: &str, queued: bool) -> Result<Vec<String>> {
    let mut diagnostics = DocumentDiagnostics::default();
    let rows = tx.prepare("SELECT id,document,resource,state FROM durable_jobs WHERE tenant=?1 AND (state IN ('running','effect_admitted','reconciliation_required','quarantined') OR (?2 AND state='queued')) AND resource<>''")?
        .query_map(params![tenant, queued], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, document, resource, state)| {
            if state == "quarantined" {
                return resource;
            }
            decode_row(&id, &document, &mut diagnostics)
                .map(|job| job.payload.resource())
                .unwrap_or(resource)
        })
        .collect())
}
fn resources_overlap(left: &str, right: &str) -> bool {
    let sources = |resource: &str| -> Option<Vec<String>> {
        if let Some(ids) = resource.strip_prefix("source-set:") {
            Some(
                ids.split(',')
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )
        } else {
            resource
                .strip_prefix("source:")
                .map(|id| vec![id.to_owned()])
        }
    };
    match (sources(left), sources(right)) {
        (Some(left), Some(right)) => {
            !left.is_empty()
                && !right.is_empty()
                && (left.iter().any(|id| id == "*")
                    || right.iter().any(|id| id == "*")
                    || left.iter().any(|id| right.contains(id)))
        }
        _ => left == right,
    }
}
fn advance(
    tx: &Transaction<'_>,
    lease: AttemptLease,
    operation: WorkerOperation,
    admission: &WorkerAdmission,
    _effect_policy: EffectAdmissionPolicy,
    #[cfg(feature = "desktop_rebuild")] policy: Option<AuthPolicy>,
) -> Result<TransactionDecision> {
    let mut job = claimed_job(tx, &lease)?;
    #[cfg(feature = "desktop_rebuild")]
    if job.kind == JobKind::Sync {
        match &operation {
            WorkerOperation::BeginEffect(_)
            | WorkerOperation::ConfirmEffect(_)
            | WorkerOperation::Finish(_)
            | WorkerOperation::FinishPublic { .. } => {
                return Err(anyhow!("Sync operation belongs to its journal owner"));
            }
            WorkerOperation::Heartbeat | WorkerOperation::Progress(_) | WorkerOperation::Fail => {}
        }
    }
    #[cfg(feature = "desktop_rebuild")]
    if let Some(refusal) = refuse_advance(tx, &mut job, &operation, policy)? {
        return Ok(TransactionDecision::Refused(refusal));
    }
    #[cfg(feature = "desktop_rebuild")]
    ensure!(
        job.kind != JobKind::SuppliedSourceImport
            || matches!(
                operation,
                WorkerOperation::Heartbeat | WorkerOperation::Progress(_)
            ),
        "Supplied imports advance through owned phases"
    );
    #[cfg(feature = "desktop_rebuild")]
    if job.kind == JobKind::Sync && matches!(operation, WorkerOperation::Fail) {
        let job = crate::source_sync::halt_job(tx, job, "fatal", "Source Sync worker failed")?;
        return Ok(TransactionDecision::Outcome(Outcome::Job(
            job,
            LocalCommit::Committed,
        )));
    }
    let event = match operation {
        WorkerOperation::Heartbeat => "heartbeat",
        WorkerOperation::Progress(progress) => {
            ensure!(progress <= 100, "Invalid progress");
            job.progress = Some(progress);
            "progress"
        }
        WorkerOperation::BeginEffect(intent) => {
            ensure!(
                !job.cancel_requested
                    && job.state == PersistentState::Running
                    && job.effects.len() < 16,
                "Effect admission denied"
            );
            model::digest(&intent.basis_hash)?;
            model::digest(&intent.content_hash)?;
            model::text(&intent.target, 1024)?;
            let allowed = match intent.operation {
                EffectOperation::PrinterUpload
                | EffectOperation::PrinterUploadAndStart
                | EffectOperation::PrinterStart
                | EffectOperation::SpoolmanDeduction => job.kind == JobKind::PrinterUpload,
                EffectOperation::SourceRefresh => matches!(
                    job.kind,
                    JobKind::Sync
                        | JobKind::ImportScan
                        | JobKind::ExtractSourceDocs
                        | JobKind::CheckSourceUpdates
                ),
                EffectOperation::LocalArtifact => job.kind != JobKind::PrinterUpload,
            };
            ensure!(allowed, "Effect does not belong to job kind");
            if let Payload::PrinterStart(request) = &job.payload {
                let upload = request
                    .upload_effect()
                    .ok_or_else(|| anyhow!("Missing printer start binding"))?;
                ensure!(
                    intent.operation == EffectOperation::PrinterStart
                        && intent.basis_hash == upload.intent.basis_hash
                        && intent.content_hash == upload.intent.content_hash
                        && intent.target == upload.intent.target,
                    "Effect does not match uploaded artifact"
                );
            }
            ensure!(
                !job.effects
                    .iter()
                    .any(|e| e.intent.operation == intent.operation
                        && e.intent.target == intent.target),
                "Effect already admitted"
            );
            if let Payload::PrinterUpload {
                printer_id, start, ..
            } = &job.payload
            {
                if matches!(
                    intent.operation,
                    EffectOperation::PrinterUpload
                        | EffectOperation::PrinterUploadAndStart
                        | EffectOperation::PrinterStart
                ) {
                    ensure!(&intent.target == printer_id, "Effect target mismatch");
                }
                if intent.operation == EffectOperation::PrinterUploadAndStart {
                    ensure!(*start, "Combined start not requested");
                    ensure!(
                        !job.effects.iter().any(|e| matches!(
                            e.intent.operation,
                            EffectOperation::PrinterUpload | EffectOperation::PrinterStart
                        )),
                        "Printer transfer already admitted"
                    );
                }
                if matches!(
                    intent.operation,
                    EffectOperation::PrinterUpload | EffectOperation::PrinterStart
                ) {
                    ensure!(
                        !job.effects
                            .iter()
                            .any(|e| e.intent.operation == EffectOperation::PrinterUploadAndStart),
                        "Combined printer transfer already admitted"
                    );
                }
                if intent.operation == EffectOperation::PrinterStart {
                    ensure!(
                        *start
                            && job
                                .effects
                                .iter()
                                .any(|e| e.intent.operation == EffectOperation::PrinterUpload
                                    && e.confirmed),
                        "Printer start requires confirmed upload"
                    );
                }
            }
            #[cfg(feature = "desktop_rebuild")]
            if matches!(_effect_policy, EffectAdmissionPolicy::EnforceFeatures)
                && intent.operation == EffectOperation::SpoolmanDeduction
            {
                require_spoolman(tx, &job)?;
            }
            job.effects.push(EffectReceipt {
                intent,
                attempt: job.attempt,
                generation: job.generation,
                confirmed: false,
                no_effect: false,
                receipt: None,
                #[cfg(feature = "desktop_rebuild")]
                checklist_completion: None,
            });
            job.state = PersistentState::EffectAdmitted;
            "effect_intent"
        }
        WorkerOperation::ConfirmEffect(receipt) => {
            ensure!(
                job.state == PersistentState::EffectAdmitted,
                "No admitted effect"
            );
            receipt.validate()?;
            let effect = job
                .effects
                .last_mut()
                .ok_or_else(|| anyhow!("No effect intent"))?;
            ensure!(
                !effect.confirmed
                    && receipt.content_hash == effect.intent.content_hash
                    && receipt.target == effect.intent.target,
                "Receipt mismatch"
            );
            effect.confirm(receipt)?;
            job.state = PersistentState::Running;
            "effect_confirmed"
        }
        WorkerOperation::Finish(result) => finish(&mut job, result)?,
        #[cfg(feature = "desktop_rebuild")]
        WorkerOperation::FinishPublic { artifact, result } => {
            ensure!(result.kind() == job.kind, "Completed result kind mismatch");
            let event = finish(&mut job, artifact)?;
            job.public_result = Some(*result);
            event
        }
        WorkerOperation::Fail => {
            if job.state == PersistentState::EffectAdmitted {
                job.state = PersistentState::ReconciliationRequired;
                job.recovery = Some("Effect outcome unknown".into());
                job.fence = None;
                job.worker = None;
                job.lease_until = None;
            } else if !job.effects.is_empty() {
                job.state = PersistentState::ReconciliationRequired;
                job.recovery = Some("Confirmed effects require reconciliation".into());
                job.fence = None;
                job.worker = None;
                job.lease_until = None;
            } else {
                job.state = PersistentState::Failed;
            }
            "failed"
        }
    };
    if matches!(
        job.state,
        PersistentState::Running | PersistentState::EffectAdmitted
    ) {
        job.lease_until = Some(lease_expiry(
            time::OffsetDateTime::now_utc(),
            admission.lease_seconds,
        ));
    }
    save(tx, &mut job, event)?;
    Ok(TransactionDecision::Outcome(Outcome::Job(
        job,
        LocalCommit::Committed,
    )))
}

fn finish(job: &mut JobRecord, result: Option<ResultArtifact>) -> Result<&'static str> {
    #[cfg(feature = "desktop_rebuild")]
    ensure!(
        job.effects
            .iter()
            .all(|effect| effect.checklist_completion.is_none()),
        "Job requires its owning finalizer"
    );
    ensure!(
        job.state == PersistentState::Running && !job.cancel_requested,
        "Job cannot finish"
    );
    let result = result.or_else(|| proven_primary_printer_receipt(job).cloned());
    if let Some(result) = &result {
        result.validate()?;
        ensure!(
            job.effects
                .iter()
                .any(|e| e.confirmed && e.receipt.as_ref() == Some(result)),
            "Result requires confirmed artifact receipt"
        );
    }
    ensure!(
        completion_proven(job, result.as_ref()),
        "Completion requires effect receipts"
    );
    job.result = result;
    job.progress = Some(100);
    job.state = PersistentState::Succeeded;
    Ok("finished")
}

/// Revalidates the attempt's original authority before any worker operation.
/// A refusal fences the attempt. A receipt that matches the admitted effect is
/// kept as observed evidence for reconciliation; it never confirms the effect.
#[cfg(feature = "desktop_rebuild")]
fn refuse_advance(
    tx: &Transaction<'_>,
    job: &mut JobRecord,
    operation: &WorkerOperation,
    policy: Option<AuthPolicy>,
) -> Result<Option<CommittedRefusal>> {
    let reason = match job.authority_disposition {
        AuthorityDisposition::PhysicalOwner => return Ok(None),
        AuthorityDisposition::LegacyMissing => AuthorityRefusalReason::MissingOriginal,
        AuthorityDisposition::Original => {
            let policy = policy.ok_or(auth::AuthorityFailure::PolicyRequired)?;
            match refusal_reason(tx, job, policy)? {
                Some(reason) => reason,
                None => return Ok(None),
            }
        }
    };
    let matching_receipt = match operation {
        WorkerOperation::ConfirmEffect(receipt) => {
            ensure!(
                job.state == PersistentState::EffectAdmitted,
                "No admitted effect"
            );
            receipt.validate()?;
            let effect = job
                .effects
                .last()
                .ok_or_else(|| anyhow!("No effect intent"))?;
            ensure!(
                !effect.confirmed
                    && receipt.content_hash == effect.intent.content_hash
                    && receipt.target == effect.intent.target,
                "Receipt mismatch"
            );
            Some(receipt.clone())
        }
        _ => None,
    };
    if job.kind == JobKind::Sync {
        let error = reason.failure().into();
        set_sync_source_writer_authority_refusal(job, &error);
        *job = crate::source_sync::halt_job(
            tx,
            job.clone(),
            "authority_refused",
            &format!("{error:#}"),
        )?;
    } else {
        record_refusal(
            tx,
            job,
            reason,
            AuthorityRefusalPhase::WorkerAdvance,
            matching_receipt,
        )?;
    }
    Ok(Some(CommittedRefusal {
        job: job.clone(),
        failure: reason.failure(),
    }))
}

#[cfg(feature = "desktop_rebuild")]
fn require_spoolman(tx: &Transaction<'_>, job: &JobRecord) -> Result<()> {
    use pp_domain::{PositiveId, features::Feature};

    let Payload::PrinterUpload { profile_id, .. } = &job.payload else {
        return Err(anyhow!("Spoolman deduction requires printer upload"));
    };
    let plan = profile_id
        .map(PositiveId::new)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let view = crate::features::resolve_for_tenant(tx, &job.tenant, plan)?;
    ensure!(
        view.effective().enabled(Feature::Spoolman),
        JobFailure::FeatureDisabled {
            feature: Feature::Spoolman
        }
    );
    Ok(())
}

/// Revalidates the credential that enqueued `job` under the current policy.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn require_original_authority(
    tx: &Transaction<'_>,
    job: &JobRecord,
    policy: AuthPolicy,
) -> Result<ValidatedAuthority> {
    ensure!(
        job.authority_disposition == AuthorityDisposition::Original,
        auth::AuthorityFailure::Missing
    );
    let basis = job
        .authority
        .as_ref()
        .ok_or(auth::AuthorityFailure::Missing)?;
    let (tenant, subject) = auth::authority::resolve(tx, basis, policy)?;
    ensure!(tenant == job.tenant, auth::AuthorityFailure::TenantChanged);
    Ok(ValidatedAuthority { tenant, subject })
}

#[cfg(feature = "desktop_rebuild")]
pub(crate) fn revalidate_source_authority(
    tx: &Transaction<'_>,
    job: &JobRecord,
    policy: AuthPolicy,
) -> Result<()> {
    if job.authority_disposition == AuthorityDisposition::PhysicalOwner {
        return Ok(());
    }
    require_original_authority(tx, job, policy).map(drop)
}

#[cfg(feature = "desktop_rebuild")]
pub(crate) fn commit_source_authority_refusal(
    tx: &Transaction<'_>,
    job: &mut JobRecord,
    error: &anyhow::Error,
) -> Result<bool> {
    let Some(reason) = AuthorityRefusalReason::of_source_writer(job, error) else {
        return Ok(false);
    };
    record_refusal(tx, job, reason, AuthorityRefusalPhase::SourceWriter, None)?;
    Ok(true)
}

/// Records a Source-writer refusal on a Sync Job without saving it. The
/// Sync halt that follows saves the Job.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn set_sync_source_writer_authority_refusal(
    job: &mut JobRecord,
    error: &anyhow::Error,
) -> bool {
    let Some(reason) = AuthorityRefusalReason::of_source_writer(job, error) else {
        return false;
    };
    job.authority_refusal = Some(AuthorityRefusalObservation {
        version: 1,
        reason,
        phase: AuthorityRefusalPhase::SourceWriter,
        observed_at: now(),
        generation: job.generation,
    });
    true
}

#[cfg(feature = "desktop_rebuild")]
impl AuthorityRefusalReason {
    /// The refusal to record for a failed revalidation. Other failures stay
    /// hard errors and change nothing.
    fn of(error: &anyhow::Error) -> Option<Self> {
        match error.downcast_ref::<auth::AuthorityFailure>()? {
            auth::AuthorityFailure::CredentialInvalid => Some(Self::CredentialInvalid),
            auth::AuthorityFailure::PolicyChanged => Some(Self::PolicyChanged),
            auth::AuthorityFailure::TenantChanged => Some(Self::TenantChanged),
            auth::AuthorityFailure::SubjectChanged => Some(Self::SubjectChanged),
            auth::AuthorityFailure::Missing | auth::AuthorityFailure::PolicyRequired => None,
        }
    }
    fn of_source_writer(job: &JobRecord, error: &anyhow::Error) -> Option<Self> {
        if job.authority_disposition == AuthorityDisposition::LegacyMissing {
            Some(Self::MissingOriginal)
        } else {
            Self::of(error)
        }
    }
    fn failure(self) -> auth::AuthorityFailure {
        match self {
            Self::MissingOriginal => auth::AuthorityFailure::Missing,
            Self::CredentialInvalid => auth::AuthorityFailure::CredentialInvalid,
            Self::PolicyChanged => auth::AuthorityFailure::PolicyChanged,
            Self::TenantChanged => auth::AuthorityFailure::TenantChanged,
            Self::SubjectChanged => auth::AuthorityFailure::SubjectChanged,
        }
    }
}

#[cfg(feature = "desktop_rebuild")]
fn refusal_reason(
    tx: &Transaction<'_>,
    job: &JobRecord,
    policy: AuthPolicy,
) -> Result<Option<AuthorityRefusalReason>> {
    match require_original_authority(tx, job, policy) {
        Ok(_) => Ok(None),
        Err(error) => AuthorityRefusalReason::of(&error).ok_or(error).map(Some),
    }
}

/// Records why the original authority was refused and fences the attempt. A job
/// without effects fails; one with effects waits for reconciliation, keeping
/// `matching_receipt` on the last effect as observed evidence.
#[cfg(feature = "desktop_rebuild")]
fn record_refusal(
    tx: &Transaction<'_>,
    job: &mut JobRecord,
    reason: AuthorityRefusalReason,
    phase: AuthorityRefusalPhase,
    matching_receipt: Option<ResultArtifact>,
) -> Result<()> {
    if let Some(receipt) = matching_receipt {
        job.effects
            .last_mut()
            .ok_or_else(|| anyhow!("No effect intent"))?
            .receipt = Some(receipt);
    }
    job.authority_refusal = Some(AuthorityRefusalObservation {
        version: 1,
        reason,
        phase,
        observed_at: now(),
        generation: job.generation,
    });
    job.generation += 1;
    job.fence = None;
    job.worker = None;
    job.lease_until = None;
    if job.effects.is_empty() {
        job.state = PersistentState::Failed;
        job.recovery = Some("Original authority is no longer valid".into());
    } else {
        job.state = PersistentState::ReconciliationRequired;
        job.recovery = Some("Inspect retained effect evidence and reconcile explicitly".into());
    }
    save(tx, job, "authority_refused")
}

pub(crate) fn validate_schema(connection: &Connection, version: u64) -> Result<()> {
    let objects = |connection: &Connection| -> Result<Vec<(String, Option<String>)>> {
        Ok(connection.prepare("SELECT name,sql FROM sqlite_master WHERE name LIKE 'durable_job%' OR name LIKE 'sqlite_autoindex_durable_job%' ORDER BY name")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
    };
    let actual = objects(connection)?;
    if version < 35 {
        ensure!(actual.is_empty(), "Unversioned durable job objects");
        return Ok(());
    }
    let legacy = Connection::open_in_memory()?;
    legacy.execute_batch(include_str!("jobs/schema.sql"))?;
    let indexed = Connection::open_in_memory()?;
    indexed.execute_batch(include_str!("jobs/schema.sql"))?;
    indexed.execute_batch(include_str!("jobs/printer-start-indexes.sql"))?;
    ensure!(
        actual == objects(&legacy)? || actual == objects(&indexed)?,
        "Durable job schema mismatch"
    );
    let mut query=connection.prepare("SELECT id,tenant,kind,state,resource,version,generation,lease_until,created,updated,document FROM durable_jobs")?;
    let mut rows = query.query([])?;
    let mut diagnostics = DocumentDiagnostics::default();
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(3)? == "quarantined" {
            continue;
        }
        let Some(job) = decode_row(
            &row.get::<_, String>(0)?,
            &row.get::<_, String>(10)?,
            &mut diagnostics,
        ) else {
            continue;
        };
        job.payload.validate_stored()?;
        ensure!(
            job.payload_version == 1
                && job.kind == job.payload.kind()
                && job.job_id == row.get::<_, String>(0)?
                && job.tenant == row.get::<_, String>(1)?
                && job.kind.name() == row.get::<_, String>(2)?
                && job.state.name() == row.get::<_, String>(3)?
                && (job.payload.resource() == row.get::<_, String>(4)?
                    || (matches!(
                        job.payload,
                        Payload::Sync {
                            project_ids: Some(_)
                        }
                    ) && row.get::<_, String>(4)? == "source:*"))
                && job.state_version == row.get::<_, i64>(5)?
                && job.generation == row.get::<_, i64>(6)?
                && job.lease_until == row.get::<_, Option<i64>>(7)?
                && job.created_at == row.get::<_, i64>(8)?
                && job.updated_at == row.get::<_, i64>(9)?,
            "Durable job record mismatch"
        );
        let running = matches!(
            job.state,
            PersistentState::Running | PersistentState::EffectAdmitted
        );
        ensure!(
            job.state_version > 0
                && job.attempt >= 0
                && job.generation >= job.attempt
                && running == job.fence.is_some()
                && running == job.lease_until.is_some()
                && job.effects.len() <= 16,
            "Invalid durable job state"
        );
    }
    Ok(())
}

#[cfg(feature = "desktop_rebuild")]
fn printer_upload_conflicts(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    profile_id: Option<u64>,
    checkoff_units: &[CheckoffUnit],
) -> Result<bool> {
    let mut owners = std::collections::HashSet::new();
    let mut unresolved = false;
    for unit in checkoff_units {
        let owner: Option<i64> = tx
            .query_row(
                "SELECT p.profile_id FROM parts p JOIN build_profiles b ON b.id=p.profile_id AND b.tenant_id=p.tenant_id WHERE p.tenant_id=? AND p.id=?",
                params![tenant, unit.part_id as i64],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = owner {
            owners.insert(owner);
        } else {
            unresolved = true;
        }
    }
    ensure!(
        owners.len() <= 1
            && profile_id.is_none_or(|id| owners.iter().all(|owner| *owner as u64 == id)),
        "Job coordinate ownership mismatch"
    );
    Ok(unresolved || profile_id == Some(profile as u64) || owners.contains(&profile))
}

#[cfg(feature = "desktop_rebuild")]
pub(crate) fn publication_conflicts(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
) -> Result<Vec<pp_contracts::publication::ExecutionConflict>> {
    let records = tx
        .prepare("SELECT id,kind,state,document FROM durable_jobs WHERE tenant=? ORDER BY id")?
        .query_map([tenant], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Vec::new();
    for (id, kind, state, document) in records {
        let record = decode(&document)?;
        ensure!(
            record.tenant == tenant
                && record.job_id == id
                && record.kind == record.payload.kind()
                && record.kind.name() == kind
                && record.state.name() == state,
            "Job identity mismatch"
        );
        if record.state.terminal() {
            continue;
        }
        let affected = match &record.payload {
            Payload::Sync { .. }
            | Payload::ImportScan { .. }
            | Payload::ExtractSourceDocs { .. }
            | Payload::CheckSourceUpdates {} => false,
            Payload::SuppliedSourceImport { .. } => false,
            Payload::ExportStlPack { profile_id, .. }
            | Payload::ExportChecklistHtml { profile_id }
            | Payload::ExportKitBundle { profile_id, .. }
            | Payload::ExportAcceptedPlate3mf { profile_id, .. }
            | Payload::ExportDirect3mf { profile_id, .. } => *profile_id == profile as u64,
            Payload::PrinterUpload {
                profile_id,
                checkoff_units,
                ..
            } => printer_upload_conflicts(tx, tenant, profile, *profile_id, checkoff_units)?,
            Payload::PrinterStart(request) => {
                let parent = retained_owned(tx, &request.uploaded_job_id, tenant)?;
                ensure!(
                    parent.kind == JobKind::PrinterUpload
                        && parent.state == PersistentState::UploadedOnly,
                    "Parent job is not an uploaded-only print"
                );
                let proof = parent
                    .uploaded_only_proof()
                    .ok_or_else(|| anyhow!("Parent job is not an uploaded-only print"))?;
                ensure!(
                    request.printer_id() == Some(proof.printer_id)
                        && request.upload_effect() == Some(proof.upload),
                    "Printer start binding mismatch"
                );
                let Payload::PrinterUpload {
                    profile_id,
                    checkoff_units,
                    ..
                } = &parent.payload
                else {
                    return Err(anyhow!("Parent job is not an uploaded-only print"));
                };
                printer_upload_conflicts(tx, tenant, profile, *profile_id, checkoff_units)?
            }
        };
        if affected {
            result.push(pp_contracts::publication::ExecutionConflict {
                operation_id: id,
                kind,
                state,
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/claimed_attempt.rs"
    ));
    use super::*;

    fn record() -> JobRecord {
        JobRecord {
            job_id: "size-boundary".into(),
            tenant: "default".into(),
            kind: JobKind::CheckSourceUpdates,
            payload_version: 1,
            payload: Payload::CheckSourceUpdates {},
            state: PersistentState::Queued,
            state_version: 0,
            attempt: 0,
            generation: 0,
            lease_until: None,
            created_at: now(),
            updated_at: now(),
            finished_at: None,
            cancel_requested: false,
            progress: None,
            effects: vec![],
            result: None,
            #[cfg(feature = "desktop_rebuild")]
            public_result: None,
            recovery: Some(String::new()),
            #[cfg(feature = "desktop_rebuild")]
            authority: None,
            #[cfg(feature = "desktop_rebuild")]
            authority_disposition: AuthorityDisposition::PhysicalOwner,
            #[cfg(feature = "desktop_rebuild")]
            authority_refusal: None,
            fence: None,
            worker: None,
        }
    }

    #[cfg(not(feature = "desktop_rebuild"))]
    fn owner_user(
        tx: &Transaction<'_>,
        subject: &str,
        operation: UserOperation,
    ) -> Result<Outcome> {
        user(tx, "default", subject, operation)
    }
    #[cfg(feature = "desktop_rebuild")]
    fn owner_user(
        tx: &Transaction<'_>,
        subject: &str,
        operation: UserOperation,
    ) -> Result<Outcome> {
        user(tx, "default", subject, None, operation)
    }

    fn sized_record(size: usize) -> JobRecord {
        let mut job = record();
        let padding = size - encode(&job).unwrap().len();
        // Non-ASCII text makes the boundary a byte count, not a character count.
        job.recovery = Some("é".repeat(padding / 2) + &"a".repeat(padding % 2));
        job
    }

    #[test]
    fn ticket_t_28_claims_sqlite_full_enqueue_rolls_back_and_retries() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("jobs/schema.sql"))
            .unwrap();
        let pages: i64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        connection
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
        let enqueue = |key: &str| UserOperation::Enqueue {
            key: key.into(),
            payload_version: 1,
            payload: Payload::CheckSourceUpdates {},
        };
        let mut committed = 0_i64;
        let mut failed_key = None;
        for index in 0..128 {
            let key = format!("disk-full-{index}");
            let result = {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                match owner_user(&tx, "disk-full-test", enqueue(&key)) {
                    Ok(outcome) => tx.commit().map(|()| outcome).map_err(anyhow::Error::from),
                    Err(error) => Err(error),
                }
            };
            match result {
                Ok(_) => committed += 1,
                Err(error) => {
                    assert!(
                        matches!(error.downcast_ref::<rusqlite::Error>(),
                        Some(rusqlite::Error::SqliteFailure(code, _))
                            if code.code == rusqlite::ErrorCode::DiskFull),
                        "{error:?}"
                    );
                    failed_key = Some(key);
                    break;
                }
            }
        }
        let failed_key = failed_key.expect("SQLite page budget must produce SQLITE_FULL");
        for table in ["durable_jobs", "durable_job_keys", "durable_job_history"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, committed, "partial enqueue in {table}");
        }
        let failed_keys: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM durable_job_keys WHERE key=?1",
                [&failed_key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failed_keys, 0);
        connection
            .pragma_update(None, "max_page_count", pages + 64)
            .unwrap();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let Outcome::Job(retried, LocalCommit::Committed) =
            owner_user(&tx, "disk-full-test", enqueue(&failed_key)).unwrap()
        else {
            panic!("retry must commit one job");
        };
        tx.commit().unwrap();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let Outcome::Job(duplicate, LocalCommit::ReadOnly) =
            owner_user(&tx, "disk-full-test", enqueue(&failed_key)).unwrap()
        else {
            panic!("duplicate must replay the committed job");
        };
        tx.commit().unwrap();
        assert_eq!(duplicate.job_id, retried.job_id);
        for table in ["durable_jobs", "durable_job_keys", "durable_job_history"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, committed + 1, "retry duplicated {table}");
        }
    }

    static REJECT_NEXT_COMMIT: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn reject_next_commit(_: *mut std::ffi::c_void) -> i32 {
        i32::from(REJECT_NEXT_COMMIT.swap(false, Ordering::SeqCst))
    }

    fn enqueue_job(connection: &mut Connection, key: &str) -> JobRecord {
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let Outcome::Job(job, LocalCommit::Committed) = owner_user(
            &tx,
            "disk-full-claim",
            UserOperation::Enqueue {
                key: key.into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
        )
        .unwrap() else {
            panic!("enqueue");
        };
        tx.commit().unwrap();
        job
    }

    fn run(connection: &mut Connection, command: Command) -> Result<Outcome> {
        execute(
            connection,
            command,
            EffectAdmissionPolicy::Legacy,
            crate::writer_admission::Deadline::at_dequeue(),
            #[cfg(feature = "desktop_rebuild")]
            &JobSubscriptions::new(),
            #[cfg(feature = "desktop_rebuild")]
            &mut crate::catalog::State::new(std::env::temp_dir()),
        )
    }
    fn claim_command(worker: &str) -> Command {
        Command::Claim {
            worker: worker.into(),
            admission: Arc::new(WorkerAdmission {
                kinds: vec![(JobKind::CheckSourceUpdates, 4)],
                total: 4,
                per_resource: 4,
                lease_seconds: 60,
            }),
            #[cfg(feature = "desktop_rebuild")]
            job_id: None,
            #[cfg(feature = "desktop_rebuild")]
            kind: None,
            #[cfg(feature = "desktop_rebuild")]
            policy: None,
            #[cfg(feature = "desktop_rebuild")]
            storage: test_storage(),
        }
    }
    #[cfg(feature = "desktop_rebuild")]
    fn test_storage() -> SettingsClient {
        let root = std::env::temp_dir().join(format!("pp-claim-unit-{}", random()));
        let (owner, _) = WriterOwner::open(&root, crate::Limits::default()).unwrap();
        let client = owner.client();
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        client
    }

    fn job_row(connection: &Connection, id: &str) -> (String, i64, i64) {
        let (state, generation, document): (String, i64, String) = connection
            .query_row(
                "SELECT state,generation,document FROM durable_jobs WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let attempt = decode(&document).unwrap().attempt;
        (state, generation, attempt)
    }

    #[test]
    fn ticket_t_28_claims_disk_full_claim_keeps_one_active_attempt() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("jobs/schema.sql"))
            .unwrap();
        let held = enqueue_job(&mut connection, "held");
        let Outcome::Claimed(Some(claimed_attempt!(claimed, _))) =
            run(&mut connection, claim_command("holder")).unwrap()
        else {
            panic!("held claim");
        };
        assert_eq!(claimed.job_id, held.job_id);
        assert_eq!(claimed.attempt, 1);
        let victim = enqueue_job(&mut connection, "victim");
        let pages: i64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        connection
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
        let mut stored = decode(
            &connection
                .query_row(
                    "SELECT document FROM durable_jobs WHERE id=?1",
                    [&victim.job_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        let mut width = 64;
        let mut blocked = false;
        while width <= 8192 {
            stored.recovery = Some("a".repeat(width));
            let encoded = encode(&stored).unwrap();
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            match tx.execute(
                "UPDATE durable_jobs SET document=?1 WHERE id=?2",
                params![encoded, victim.job_id],
            ) {
                Ok(_) => {
                    tx.commit().unwrap();
                    width += 64;
                }
                Err(error) => {
                    assert!(
                        matches!(
                            error,
                            rusqlite::Error::SqliteFailure(code, _)
                                if code.code == rusqlite::ErrorCode::DiskFull
                        ),
                        "{error:?}"
                    );
                    blocked = true;
                    break;
                }
            }
        }
        assert!(blocked, "page budget never rejected a larger job document");
        let error = match run(&mut connection, claim_command("second")) {
            Err(error) => error,
            Ok(_) => panic!("in-flight claim committed under the page budget"),
        };
        assert!(
            matches!(
                error.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(code, _))
                    if code.code == rusqlite::ErrorCode::DiskFull
            ),
            "{error:?}"
        );
        assert_eq!(
            job_row(&connection, &held.job_id),
            ("running".into(), claimed.generation, 1)
        );
        assert_eq!(
            job_row(&connection, &victim.job_id),
            ("queued".into(), 0, 0)
        );
        let running: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM durable_jobs WHERE state='running'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(running, 1);
        let limit: i64 = connection
            .query_row("PRAGMA max_page_count", [], |row| row.get(0))
            .unwrap();
        connection
            .pragma_update(None, "max_page_count", limit + 64)
            .unwrap();
        let Outcome::Claimed(Some(claimed_attempt!(retried, _))) =
            run(&mut connection, claim_command("second")).unwrap()
        else {
            panic!("claim after raising the quota");
        };
        assert_eq!(retried.job_id, victim.job_id);
        assert_eq!(retried.attempt, 1);
        assert_eq!(
            job_row(&connection, &held.job_id),
            ("running".into(), claimed.generation, 1)
        );
        let running: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM durable_jobs WHERE state='running'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(running, 2);
    }

    #[test]
    fn ticket_t_28_claims_commit_unknown_rolls_back_in_flight_claim() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("jobs/schema.sql"))
            .unwrap();
        let held = enqueue_job(&mut connection, "held");
        let Outcome::Claimed(Some(claimed_attempt!(claimed, _))) =
            run(&mut connection, claim_command("holder")).unwrap()
        else {
            panic!("held claim");
        };
        assert_eq!(claimed.job_id, held.job_id);
        let victim = enqueue_job(&mut connection, "victim");
        REJECT_NEXT_COMMIT.store(true, Ordering::SeqCst);
        unsafe {
            rusqlite::ffi::sqlite3_commit_hook(
                connection.handle(),
                Some(reject_next_commit),
                std::ptr::null_mut(),
            );
        }
        let error = match run(&mut connection, claim_command("second")) {
            Err(error) => error,
            Ok(_) => panic!("in-flight claim committed"),
        };
        assert_eq!(
            error.downcast_ref::<JobFailure>(),
            Some(&JobFailure::CommitUnknown)
        );
        unsafe {
            rusqlite::ffi::sqlite3_commit_hook(connection.handle(), None, std::ptr::null_mut());
        }
        assert_eq!(
            job_row(&connection, &held.job_id),
            ("running".into(), claimed.generation, 1)
        );
        assert_eq!(
            job_row(&connection, &victim.job_id),
            ("queued".into(), 0, 0)
        );
        let running: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM durable_jobs WHERE state='running'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(running, 1);
        let Outcome::Claimed(Some(claimed_attempt!(retried, _))) =
            run(&mut connection, claim_command("second")).unwrap()
        else {
            panic!("claim after the commit failure");
        };
        assert_eq!(retried.job_id, victim.job_id);
        assert_eq!(retried.attempt, 1);
        assert_eq!(
            job_row(&connection, &held.job_id),
            ("running".into(), claimed.generation, 1)
        );
    }

    #[test]
    fn job_document_exact_size_limit_saves_and_decodes() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("jobs/schema.sql"))
            .unwrap();
        let tx = connection.transaction().unwrap();
        let mut job = sized_record(JOB_DOC_SIZE_LIMIT);
        save(&tx, &mut job, "enqueued").unwrap();
        tx.commit().unwrap();
        let document: String = connection
            .query_row("SELECT document FROM durable_jobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(document.len(), JOB_DOC_SIZE_LIMIT);
        assert_eq!(decode(&document).unwrap().recovery, job.recovery);
        validate_schema(&connection, 35).unwrap();
    }

    #[test]
    fn job_document_over_size_limit_rejects_without_any_writes() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("jobs/schema.sql"))
            .unwrap();
        let tx = connection.transaction().unwrap();
        let mut job = sized_record(JOB_DOC_SIZE_LIMIT + 1);
        let error = save(&tx, &mut job, "enqueued").unwrap_err();
        assert_eq!(
            error.downcast_ref::<JobFailure>(),
            Some(&JobFailure::JobDocTooLarge {
                size: JOB_DOC_SIZE_LIMIT + 1,
                limit: JOB_DOC_SIZE_LIMIT,
            })
        );
        // Even committing after the error cannot persist a document or history row.
        tx.commit().unwrap();
        for table in ["durable_jobs", "durable_job_history", "durable_job_keys"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
        }

        let tx = connection.transaction().unwrap();
        let mut job = record();
        save(&tx, &mut job, "enqueued").unwrap();
        let original = encode(&job).unwrap();
        job.recovery = sized_record(JOB_DOC_SIZE_LIMIT + 1).recovery;
        assert!(save(&tx, &mut job, "progress").is_err());
        tx.commit().unwrap();
        let stored: String = connection
            .query_row("SELECT document FROM durable_jobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, original);
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM durable_job_history", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    /// The reference stores a rich result past 128 KiB because it predates
    /// main's document limit; here the limit also bounds the public result.
    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn rich_completed_result_round_trips_through_durable_codec_within_the_limit() {
        let skipped = |reason: String| {
            Some(CompletedResult::SourceUpdates(SourceUpdatesResult {
                checked: vec![],
                skipped: vec![SkippedSourceUpdate {
                    source_id: 1,
                    name: "Source".into(),
                    reason,
                }],
                updates_available: 0,
                checked_count: 0,
            }))
        };
        let mut job = record();
        job.state = PersistentState::Succeeded;
        job.state_version = 1;
        job.progress = Some(100);
        job.finished_at = Some(job.updated_at);
        job.public_result = skipped("x".repeat(16 * 1024));
        job.validate_state().unwrap();
        let decoded = decode(&encode(&job).unwrap()).unwrap();
        assert_eq!(decoded.public_result, job.public_result);

        job.public_result = skipped("x".repeat(JOB_DOC_SIZE_LIMIT));
        assert!(matches!(
            encode(&job).unwrap_err().downcast_ref::<JobFailure>(),
            Some(JobFailure::JobDocTooLarge { .. })
        ));
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn snapshot_defaults_progress_only_for_queued_jobs_and_reports_the_public_result() {
        let mut job = record();
        job.created_at = 1;
        job.updated_at = 1;
        assert_eq!(job.snapshot().progress, Some(0));
        assert_eq!(job.snapshot().created_at, "1970-01-01T00:00:01.000Z");
        for state in [
            PersistentState::Running,
            PersistentState::Failed,
            PersistentState::Cancelled,
        ] {
            job.state = state;
            assert_eq!(job.snapshot().progress, None, "{state:?}");
        }
        job.state = PersistentState::Running;
        job.progress = Some(37);
        assert_eq!(job.snapshot().progress, Some(37));
        job.state = PersistentState::Succeeded;
        job.progress = Some(100);
        job.result = Some(ResultArtifact {
            receipt_id: "internal-receipt".into(),
            content_hash: "b".repeat(64),
            target: "internal-target".into(),
        });
        assert_eq!(job.snapshot().progress, Some(100));
        assert_eq!(job.snapshot().result, None);
        let result = CompletedResult::SourceUpdates(SourceUpdatesResult {
            checked: vec![],
            skipped: vec![],
            updates_available: 0,
            checked_count: 0,
        });
        job.public_result = Some(result.clone());
        assert_eq!(job.snapshot().result, Some(result));
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn observed_spoolman_receipt_does_not_prove_uploaded_only() {
        let path = std::env::temp_dir().join(format!(
            "pp-observed-spoolman-unit-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(&path).unwrap();
        let owner = WriterOwner::open(&path, crate::Limits::default())
            .unwrap()
            .0;
        owner
            .jobs(AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
                first_user: auth::FirstUserTenant::NewUser,
            })
            .unwrap()
            .submit(
                Credential::PhysicalOwner(owner.job_physical_owner()),
                UserOperation::Enqueue {
                    key: "observed-spoolman".into(),
                    payload_version: 1,
                    payload: Payload::PrinterUpload {
                        printer_id: "observed-printer".into(),
                        artifact_path: "exports/observed.gcode".into(),
                        filename: "observed.gcode".into(),
                        start: true,
                        profile_id: None,
                        host_name: None,
                        checkoff_units: vec![],
                        unlabeled_names: vec![],
                    },
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive()
            .unwrap();
        let worker = owner
            .job_worker(WorkerAdmission {
                kinds: vec![(JobKind::PrinterUpload, 1)],
                total: 1,
                per_resource: 1,
                lease_seconds: 60,
            })
            .unwrap();
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        let upload = EffectIntent {
            operation: EffectOperation::PrinterUpload,
            basis_hash: "a".repeat(64),
            content_hash: "b".repeat(64),
            target: "observed-printer".into(),
        };
        worker
            .update(&mut lease, WorkerOperation::BeginEffect(upload.clone()))
            .unwrap();
        worker
            .update(
                &mut lease,
                WorkerOperation::ConfirmEffect(ResultArtifact {
                    receipt_id: "observed-upload".into(),
                    content_hash: upload.content_hash,
                    target: upload.target,
                }),
            )
            .unwrap();
        let spoolman = EffectIntent {
            operation: EffectOperation::SpoolmanDeduction,
            basis_hash: "c".repeat(64),
            content_hash: "d".repeat(64),
            target: "spoolman:observed".into(),
        };
        worker
            .update(&mut lease, WorkerOperation::BeginEffect(spoolman.clone()))
            .unwrap();
        let mut uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
        uncertain.effects.last_mut().unwrap().receipt = Some(ResultArtifact {
            receipt_id: "observed-deduction".into(),
            content_hash: spoolman.content_hash,
            target: spoolman.target,
        });
        uncertain.authority_refusal = Some(AuthorityRefusalObservation {
            version: 1,
            reason: AuthorityRefusalReason::CredentialInvalid,
            phase: AuthorityRefusalPhase::WorkerAdvance,
            observed_at: now(),
            generation: uncertain.generation,
        });
        uncertain.validate_state().unwrap();
        assert!(uncertain.uploaded_only_proof().is_none());
        uncertain.authority_refusal = None;
        assert!(uncertain.validate_state().is_err());
        owner.shutdown().unwrap();
    }
    #[test]
    fn lease_expiry_grants_at_least_lease_seconds_at_second_boundaries() {
        const SECOND: i128 = 1_000_000_000;
        let k = 1_800_000_000_i128 * SECOND;
        for offset in [0, 1, 1_000_000, 999_000_000, SECOND - 1] {
            let start = time::OffsetDateTime::from_unix_timestamp_nanos(k + offset).unwrap();
            for lease_seconds in [1, 60] {
                let granted =
                    i128::from(lease_expiry(start, lease_seconds)) * SECOND - (k + offset);
                let wanted = i128::from(lease_seconds) * SECOND;
                assert!(
                    (wanted..wanted + SECOND).contains(&granted),
                    "offset {offset} ns, lease {lease_seconds} s: granted {granted} ns"
                );
            }
        }
    }

    #[cfg(not(feature = "desktop_rebuild"))]
    #[test]
    fn sync_settle_is_absent_without_desktop_rebuild() {
        let root =
            std::env::temp_dir().join(format!("pp-sync-settle-off-{}", rand::random::<u64>()));
        let (owner, ready) = crate::WriterOwner::open(&root, crate::Limits::default()).unwrap();
        assert_eq!(ready.version, 35);
        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'source_sync_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        owner.shutdown().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod ticket_t_28_recovery {
    use super::*;

    fn interrupted(kind: JobKind) -> JobRecord {
        JobRecord {
            job_id: "interrupted".into(),
            tenant: "default".into(),
            kind,
            payload_version: 1,
            payload: Payload::CheckSourceUpdates {},
            state: PersistentState::Running,
            state_version: 3,
            attempt: 1,
            generation: 1,
            lease_until: None,
            created_at: now(),
            updated_at: now(),
            finished_at: None,
            cancel_requested: false,
            progress: None,
            effects: vec![],
            result: None,
            #[cfg(feature = "desktop_rebuild")]
            public_result: None,
            recovery: None,
            #[cfg(feature = "desktop_rebuild")]
            authority: None,
            #[cfg(feature = "desktop_rebuild")]
            authority_disposition: AuthorityDisposition::PhysicalOwner,
            #[cfg(feature = "desktop_rebuild")]
            authority_refusal: None,
            fence: None,
            worker: None,
        }
    }

    fn admitted(operation: EffectOperation, confirmed: bool) -> EffectReceipt {
        EffectReceipt {
            intent: EffectIntent {
                operation,
                basis_hash: "basis".into(),
                content_hash: "content".into(),
                target: "printer-a".into(),
            },
            attempt: 1,
            generation: 1,
            confirmed,
            no_effect: false,
            receipt: None,
            #[cfg(feature = "desktop_rebuild")]
            checklist_completion: None,
        }
    }

    #[test]
    fn every_job_kind_has_a_named_recovery_policy() {
        for kind in JobKind::ALL {
            let policy = kind.recovery_policy();
            assert!(!policy.name().is_empty(), "{kind:?}");
            let expected = match kind.effect_class() {
                EffectClass::UncertainExternal => RecoveryPolicy::NeverResendAfterEffect,
                EffectClass::RecoverableExternal => RecoveryPolicy::RequeueUntilEffect,
                _ => RecoveryPolicy::RequeueLocal,
            };
            assert_eq!(policy, expected, "{kind:?}");
        }
        assert_eq!(
            JobKind::PrinterUpload.recovery_policy(),
            RecoveryPolicy::NeverResendAfterEffect
        );
        assert_eq!(
            JobKind::CheckSourceUpdates.recovery_policy(),
            RecoveryPolicy::RequeueUntilEffect
        );
        assert_eq!(
            JobKind::ExportStlPack.recovery_policy(),
            RecoveryPolicy::RequeueLocal
        );
        assert_eq!(
            serde_json::to_value(RecoveryPolicy::NeverResendAfterEffect).unwrap(),
            serde_json::json!("never_resend_after_effect")
        );
    }

    #[test]
    fn printer_send_with_an_admitted_effect_is_never_requeued() {
        for confirmed in [false, true] {
            let mut job = interrupted(JobKind::PrinterUpload);
            job.effects
                .push(admitted(EffectOperation::PrinterUpload, confirmed));
            let (state, detail) = recovery_decision(&job);
            assert_eq!(
                state,
                PersistentState::ReconciliationRequired,
                "{confirmed}"
            );
            assert!(
                detail.unwrap().contains("never resent automatically"),
                "{detail:?}"
            );
            // Even a job at the attempt limit or with cancel requested stays
            // uncertain: the remote effect cannot be undone or assumed absent.
            job.attempt = 100;
            job.cancel_requested = true;
            assert_eq!(
                recovery_decision(&job).0,
                PersistentState::ReconciliationRequired
            );
        }
    }

    #[test]
    fn printer_send_interrupted_before_its_intent_is_requeued() {
        // No admitted intent means nothing was sent, so the attempt is safe to repeat.
        let job = interrupted(JobKind::PrinterUpload);
        assert_eq!(recovery_decision(&job), (PersistentState::Queued, None));
    }

    #[test]
    fn local_and_read_jobs_requeue_and_keep_the_generic_reconcile_detail() {
        for kind in [JobKind::ExportStlPack, JobKind::CheckSourceUpdates] {
            assert_eq!(
                recovery_decision(&interrupted(kind)),
                (PersistentState::Queued, None),
                "{kind:?}"
            );
        }
        let mut read = interrupted(JobKind::CheckSourceUpdates);
        read.effects
            .push(admitted(EffectOperation::PrinterUpload, false));
        assert_eq!(
            recovery_decision(&read),
            (
                PersistentState::ReconciliationRequired,
                Some("Inspect effect receipts and reconcile explicitly")
            )
        );
        let mut cancelled = interrupted(JobKind::ExportStlPack);
        cancelled.cancel_requested = true;
        assert_eq!(
            recovery_decision(&cancelled),
            (PersistentState::Cancelled, None)
        );
        let mut exhausted = interrupted(JobKind::ExportStlPack);
        exhausted.attempt = 100;
        assert_eq!(
            recovery_decision(&exhausted),
            (PersistentState::Failed, Some("Attempt limit reached"))
        );
    }
}
