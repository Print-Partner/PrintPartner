use anyhow::{Result, anyhow, ensure};
use pp_source::{
    ArtifactBudget, ContentFile, LocalFiles, MAX_ENTRIES, Selection, SnapshotRequest, SourcePath,
    TenantRepos,
    local_selection::{self, InputFile},
    media::MediaLimits,
    retained_capture::{FrozenCapture, RetainedInputTransfer},
};
use pp_storage::{
    WriterOwner,
    auth::AuthPolicy,
    jobs::{ClaimedAttempt, Credential, JobKind, ServerWorkerClient, WorkerAdmission},
    uploads::{
        Admission, AdmissionLimits, Artifact, CaptureJournalCorrelation, CapturedPayloadV1, File,
        ImportClient, Input, Operation, OwnedInput, Phase, PreflightedCapture,
        PreparedCapturedAdmission, State, Target, inspect_capture_manifest,
    },
};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Synchronous admission and lookup for Source imports. Supplied admits hash a
/// directory. Captured admits preflight, prepare, claim, copy retained inputs,
/// publish an immutable revision, and activate through journal receipts.
pub struct SourceImports {
    client: ImportClient,
    worker: ServerWorkerClient,
    repos: PathBuf,
}

/// Durable stop points for [`SourceImports::work_operation`] and
/// [`SourceImports::work_frozen`]. Later calls resume at the journaled phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Through {
    OwnedInput,
    Published,
    Activated,
    Settled,
}

impl SourceImports {
    pub fn new(owner: &WriterOwner, policy: AuthPolicy, quota: u64) -> Result<Self> {
        Ok(Self {
            client: owner.imports(policy, quota)?,
            worker: owner.job_worker_with_policy(policy, worker_admission())?,
            repos: owner.import_repos_root()?,
        })
    }

    /// Admits `request` once per key, hashing its named files from `supplied`.
    pub fn admit(
        &self,
        credential: Credential,
        request: Admission,
        supplied: &Path,
        cancelled: &AtomicBool,
    ) -> Result<Operation> {
        ensure!(
            request.max_input_bytes > 0 && request.max_input_bytes <= 256 * 1024 * 1024,
            "Invalid import input limit"
        );
        let requested = match &request.input {
            Input::Files { paths } => paths.clone(),
            Input::Zip { path } => vec![path.clone()],
            Input::Captured(_) => {
                anyhow::bail!("Captured imports require prepared admission")
            }
        };
        let paths = requested
            .into_iter()
            .map(SourcePath::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        scanned(cancelled, Ok(()))?;
        let local = scanned(cancelled, LocalFiles::open(supplied))?;
        let inventory = local.inventory_until(&paths, request.max_input_bytes, cancelled);
        let expected_files = scanned(cancelled, inventory)?
            .into_iter()
            .map(input_file)
            .collect();
        let accounting_epoch = self.client.accounting_epoch();
        let disk = scanned(
            cancelled,
            local_selection::stored_bytes(&self.repos, cancelled),
        )?;
        self.client.admit(
            credential,
            request,
            expected_files,
            disk,
            accounting_epoch,
            cancelled,
        )
    }

    /// Preflights a captured import without writing journal rows or files.
    pub fn preflight_capture(
        &self,
        credential: Credential,
        operation_key: String,
        target: Target,
        payload: CapturedPayloadV1,
        limits: AdmissionLimits,
    ) -> Result<PreflightedCapture> {
        let disk = local_selection::stored_bytes(&self.repos, &AtomicBool::new(false))?;
        self.client
            .preflight_capture(credential, operation_key, target, payload, limits, disk)
    }

    /// Admits a prepared captured import once per key.
    pub fn admit_captured(&self, prepared: PreparedCapturedAdmission) -> Result<Operation> {
        let accounting_epoch = self.client.accounting_epoch();
        let disk = local_selection::stored_bytes(&self.repos, &AtomicBool::new(false))?;
        self.client.admit_prepared(prepared, disk, accounting_epoch)
    }

    /// Correlates a frozen capture with the journal and claims it only when the
    /// retained claim still matches and a current Source proof is available.
    /// Refuses before any retained-input file copy.
    pub fn prepare_frozen_claim(&self, frozen: &FrozenCapture) -> Result<ClaimedAttempt> {
        let manifest = frozen.manifest()?;
        let inspected = inspect_capture_manifest(&manifest)?;
        let paths = inspected
            .paths()
            .iter()
            .cloned()
            .map(SourcePath::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let inventory = frozen
            .inventory(&paths, inspected.max_input_bytes())?
            .into_iter()
            .map(input_file)
            .collect();
        match self
            .client
            .correlate_capture_manifest(manifest, inventory)?
        {
            CaptureJournalCorrelation::ExactAdmitted(claim) => {
                let claimed = self
                    .worker
                    .claim_resolved_import(claim)?
                    .ok_or_else(|| anyhow!("Captured import claim unavailable"))?;
                ensure!(
                    claimed.source_work.is_some(),
                    "Supplied import claim missing Source authority"
                );
                Ok(claimed)
            }
            CaptureJournalCorrelation::MismatchRepairRequired
            | CaptureJournalCorrelation::Absent
            | CaptureJournalCorrelation::ExactOwnedOrLater => {
                Err(anyhow!("Retained capture requires repair"))
            }
        }
    }

    /// Copies retained inputs for a claimed frozen capture into
    /// `repos/.pp-imports/{job_id}` through directory capabilities (`O_NOFOLLOW`
    /// and `O_EXCL`). Case-folded path collisions are rejected before any
    /// destination file is opened. Advances the journal to `OwnedInputReady`
    /// and removes the frozen capture. Does not publish or activate.
    pub fn write_retained_inputs(
        &self,
        claim: ClaimedAttempt,
        frozen: FrozenCapture,
        cancelled: &AtomicBool,
    ) -> Result<Operation> {
        let ClaimedAttempt {
            job: _,
            lease,
            source_work,
        } = claim;
        let live =
            source_work.ok_or_else(|| anyhow!("Supplied import claim missing Source authority"))?;
        let mut op = self.worker.import_phase(&lease, &live, Phase::Read)?;
        if op.state == State::Admitted {
            let paths = op
                .requested_files
                .iter()
                .map(|file| SourcePath::try_from(file.path.clone()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let inventory = frozen
                .transfer_owned_input(RetainedInputTransfer {
                    paths: &paths,
                    owned_root: &self.repos,
                    operation: &op.job_id,
                    max_bytes: op.max_input_bytes,
                    cancelled,
                })?
                .into_inventory();
            let files = inventory.into_iter().map(input_file).collect::<Vec<_>>();
            ensure!(
                files == op.requested_files
                    && hex::encode(Sha256::digest(serde_json::to_vec(&files)?))
                        == op.requested_digest,
                "Owned input requires repair"
            );
            op = self.worker.import_phase(
                &lease,
                &live,
                Phase::Owned(OwnedInput {
                    locator: format!(".pp-imports/{}", op.job_id),
                    digest: op.requested_digest.clone(),
                    files: op.requested_files.clone(),
                }),
            )?;
            frozen.remove(&paths)?;
        }
        Ok(op)
    }

    /// Claims `job_id` and advances through `through`, resuming at the durable
    /// journal phase without recopying owned inputs on exact retry.
    pub fn work_operation(
        &self,
        job_id: &str,
        supplied: Option<&Path>,
        through: Through,
        cancelled: &AtomicBool,
    ) -> Result<Option<Operation>> {
        self.work(Some(job_id), supplied, through, cancelled)
    }

    /// Claims the next supplied-import job and advances through `through`.
    pub fn work_next(&self, through: Through, cancelled: &AtomicBool) -> Result<Option<Operation>> {
        self.work(None, None, through, cancelled)
    }

    /// Correlates a frozen capture, claims an exact admitted match, and advances
    /// through `through`. Exact owned-or-later and absent captures are removed.
    pub fn work_frozen(
        &self,
        frozen: FrozenCapture,
        through: Through,
        cancelled: &AtomicBool,
    ) -> Result<Option<Operation>> {
        let manifest = frozen.manifest()?;
        let inspected = inspect_capture_manifest(&manifest)?;
        let paths = inspected
            .paths()
            .iter()
            .cloned()
            .map(SourcePath::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let inventory = frozen
            .inventory(&paths, inspected.max_input_bytes())?
            .into_iter()
            .map(input_file)
            .collect();
        match self
            .client
            .correlate_capture_manifest(manifest, inventory)?
        {
            CaptureJournalCorrelation::ExactAdmitted(claim) => {
                let Some(claim) = self.worker.claim_resolved_import(claim)? else {
                    return Ok(None);
                };
                self.work_claimed(claim, None, Some((frozen, paths)), through, cancelled)
            }
            CaptureJournalCorrelation::ExactOwnedOrLater | CaptureJournalCorrelation::Absent => {
                frozen.remove(&paths)?;
                Ok(None)
            }
            CaptureJournalCorrelation::MismatchRepairRequired => {
                Err(anyhow!("Retained capture requires repair"))
            }
        }
    }

    pub fn get(&self, credential: Credential, key: String) -> Result<Operation> {
        let op = self.client.get(credential, key)?;
        ensure!(
            self.artifact_intact(&op)?,
            "Accepted artifact requires repair"
        );
        Ok(op)
    }

    /// Names every regular file below `supplied`.
    pub fn directory_input(supplied: &Path) -> Result<Input> {
        Ok(Input::Files {
            paths: LocalFiles::open(supplied)?
                .paths()?
                .into_iter()
                .map(String::from)
                .collect(),
        })
    }

    /// Claims the next queued supplied import without working it.
    pub fn claim_next(&self) -> Result<Option<ClaimedAttempt>> {
        self.worker.claim_kind(JobKind::SuppliedSourceImport)
    }

    /// Works a claim from [`SourceImports::claim_next`] through `through`.
    pub fn work_claim(
        &self,
        claim: ClaimedAttempt,
        through: Through,
        cancelled: &AtomicBool,
    ) -> Result<Option<Operation>> {
        self.work_claimed(claim, None, None, through, cancelled)
    }

    fn work(
        &self,
        job_id: Option<&str>,
        supplied: Option<&Path>,
        through: Through,
        cancelled: &AtomicBool,
    ) -> Result<Option<Operation>> {
        let claim = match job_id {
            Some(id) => self.worker.claim_import(id)?,
            None => self.worker.claim_kind(JobKind::SuppliedSourceImport)?,
        };
        let Some(claim) = claim else {
            return Ok(None);
        };
        self.work_claimed(claim, supplied, None, through, cancelled)
    }

    fn work_claimed(
        &self,
        claim: ClaimedAttempt,
        supplied: Option<&Path>,
        retained: Option<(FrozenCapture, Vec<SourcePath>)>,
        through: Through,
        cancelled: &AtomicBool,
    ) -> Result<Option<Operation>> {
        let mut retained = retained;
        let ClaimedAttempt {
            job,
            lease,
            source_work,
        } = claim;
        let live =
            source_work.ok_or_else(|| anyhow!("Supplied import claim missing Source authority"))?;
        let mut op = self.worker.import_phase(&lease, &live, Phase::Read)?;
        let mut unverified = false;
        let result = (|| -> Result<()> {
            if job.cancel_requested && op.receipt.is_none() {
                op = self
                    .worker
                    .import_phase(&lease, &live, Phase::Fail("Cancelled".into()))?;
                self.cleanup(&op)?;
                op = self.worker.import_phase(&lease, &live, Phase::Cleanup)?;
                return Ok(());
            }
            if op.state == State::Admitted {
                // Refuse a missing input before touching `repos/.pp-imports`:
                // a refused call must leave every file as it found it.
                ensure!(
                    retained.is_some() || supplied.is_some(),
                    "Import requires explicitly supplied input"
                );
                local_selection::discard_owned(&self.repos, &op.job_id)?;
                let inventory = if let Some((frozen, paths)) = retained.as_ref() {
                    frozen
                        .transfer_owned_input(RetainedInputTransfer {
                            paths,
                            owned_root: &self.repos,
                            operation: &op.job_id,
                            max_bytes: op.max_input_bytes,
                            cancelled,
                        })?
                        .into_inventory()
                } else {
                    let supplied = supplied
                        .ok_or_else(|| anyhow!("Import requires explicitly supplied input"))?;
                    let paths = op
                        .requested_files
                        .iter()
                        .map(|file| SourcePath::try_from(file.path.clone()))
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    LocalFiles::open(supplied)?
                        .capture(
                            &paths,
                            &self.repos,
                            &op.job_id,
                            op.max_input_bytes,
                            cancelled,
                        )?
                        .1
                };
                let files = inventory.into_iter().map(input_file).collect::<Vec<_>>();
                ensure!(
                    files == op.requested_files
                        && hex::encode(Sha256::digest(serde_json::to_vec(&files)?))
                            == op.requested_digest,
                    "Owned input requires repair"
                );
                op = self.worker.import_phase(
                    &lease,
                    &live,
                    Phase::Owned(OwnedInput {
                        locator: format!(".pp-imports/{}", op.job_id),
                        digest: op.requested_digest.clone(),
                        files: op.requested_files.clone(),
                    }),
                )?;
                if let Some((frozen, paths)) = retained.take() {
                    frozen.remove(&paths)?;
                }
            }
            if through == Through::OwnedInput {
                return Ok(());
            }
            if op.state == State::OwnedInputReady {
                let owned = op
                    .owned
                    .as_ref()
                    .ok_or_else(|| anyhow!("Owned input missing"))?;
                let local = LocalFiles::open(&self.repos.join(&owned.locator))?;
                let paths = owned
                    .files
                    .iter()
                    .map(|file| SourcePath::try_from(file.path.clone()))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let inventory = local
                    .inventory(&paths, op.max_input_bytes)?
                    .into_iter()
                    .map(input_file)
                    .collect::<Vec<_>>();
                ensure!(
                    inventory == owned.files
                        && hex::encode(Sha256::digest(serde_json::to_vec(&inventory)?))
                            == owned.digest,
                    "Owned input requires repair"
                );
                let mut root = TenantRepos::open(op.tenant.clone(), &self.repos)?
                    .source(op.source_id.try_into()?)?;
                let prepared = root.prepare_media(
                    &local,
                    &local.paths()?,
                    &[],
                    MediaLimits {
                        max_total_bytes: op.max_prepared_bytes,
                        max_output_bytes: op.max_prepared_bytes.min(256 * 1024 * 1024),
                        ..Default::default()
                    },
                    cancelled,
                )?;
                let (key, files) = prepared.files().local_snapshot_selection(
                    local_selection::SelectionLimits {
                        // The number of STL files is structurally capped by the
                        // entry limit; the byte budgets come from the operation.
                        max_stl_files: MAX_ENTRIES as u64,
                        max_documentation_bytes: op.max_prepared_bytes,
                        max_bytes: op.max_prepared_bytes,
                    },
                )?;
                let snapshot = root.materialize(
                    SnapshotRequest {
                        upstream_revision_key: key,
                        files,
                        selection: Selection {
                            max_stl_files: MAX_ENTRIES as u64,
                            max_documentation_bytes: op.max_prepared_bytes,
                            omitted_files: vec![],
                        },
                    },
                    prepared.files(),
                    ArtifactBudget::new(op.reserved_bytes, op.max_prepared_bytes)?,
                )?;
                let artifact = Artifact {
                    tenant: snapshot.tenant_id,
                    source_id: snapshot.source_id.try_into()?,
                    upstream_key: snapshot.upstream_revision_key,
                    manifest_digest: snapshot.manifest_digest,
                    locator: snapshot.snapshot_locator,
                    stored_bytes: snapshot.stored_bytes,
                    files: artifact_files(&snapshot.files)?,
                    suggested_rules: prepared.receipt().suggested_import_rules.clone(),
                };
                op = self
                    .worker
                    .import_phase(&lease, &live, Phase::Published(artifact))?;
                prepared.discard()?;
            }
            if through == Through::Published {
                return Ok(());
            }
            if op.state == State::Published {
                if !self.artifact_intact(&op)? {
                    unverified = true;
                    return Err(anyhow!("Accepted artifact requires repair"));
                }
                op = self.worker.import_phase(&lease, &live, Phase::Activate)?;
            }
            if through == Through::Activated {
                return Ok(());
            }
            self.cleanup(&op)?;
            op = self.worker.import_phase(&lease, &live, Phase::Cleanup)?;
            Ok(())
        })();
        // A captured import that is still admitted keeps its retained capture
        // as the only copy, so a failed attempt leaves it for `work_frozen`.
        // Once the retained capture is consumed, a deterministic failure is
        // terminal: it must reach `Fail` and cleanup instead of requeueing
        // forever. A stop requested through `cancelled` is not deterministic,
        // so it leaves the import for restart recovery to resume, and it takes
        // precedence: a deterministic failure seen by a cancelled attempt
        // fails on the next attempt instead. A published import retries
        // activation, unless its artifact is missing or no longer matches.
        if let Err(error) = &result
            && !cancelled.load(Ordering::Acquire)
            && !(op.state == State::Admitted && matches!(op.input, Input::Captured(_)))
            && op.receipt.is_none()
            && (op.state != State::Published || unverified)
            && self
                .worker
                .import_phase(&lease, &live, Phase::Fail(failure_reason(error)))
                .is_ok()
            && self.cleanup(&op).is_ok()
        {
            let _ = self.worker.import_phase(&lease, &live, Phase::Cleanup);
        }
        drop(live);
        result.map(|()| Some(op))
    }

    /// Reports whether the published revision still matches the recorded
    /// artifact. A missing, corrupt, or mismatched revision is `Ok(false)`
    /// because a retry sees the same bytes; other I/O errors propagate so the
    /// import requeues.
    fn artifact_intact(&self, op: &Operation) -> Result<bool> {
        let Some(artifact) = &op.artifact else {
            return Ok(true);
        };
        let root =
            TenantRepos::open(op.tenant.clone(), &self.repos)?.source(op.source_id.try_into()?)?;
        let verified = match root.verify_published(&artifact.upstream_key) {
            Ok(verified) => verified,
            Err(error)
                if matches!(&error, pp_source::Error::Io(io)
                    if io.kind() != std::io::ErrorKind::NotFound) =>
            {
                return Err(error.into());
            }
            Err(_) => return Ok(false),
        };
        Ok(verified.stored_bytes == artifact.stored_bytes
            && verified.manifest_digest == artifact.manifest_digest
            && verified.snapshot_locator == artifact.locator
            && artifact_files(&verified.files)? == artifact.files)
    }

    fn cleanup(&self, op: &Operation) -> Result<()> {
        TenantRepos::open(op.tenant.clone(), &self.repos)?
            .source(op.source_id.try_into()?)?
            .discard_staging()?;
        local_selection::discard_owned(&self.repos, &op.job_id)?;
        Ok(())
    }
}

/// Says in words why an import failed, for its job's `error`. The Source
/// engine's refusals name the rule; other causes give their top message.
fn failure_reason(error: &anyhow::Error) -> String {
    let refusal = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<pp_source::Error>());
    match refusal {
        Some(pp_source::Error::UnsafePath | pp_source::Error::UnsafeAncestor) => {
            "A file path is not allowed in a Source".into()
        }
        Some(pp_source::Error::DuplicatePath) => "Two files have the same path".into(),
        Some(pp_source::Error::Limit) => "The import is over its size or file-count limit".into(),
        Some(pp_source::Error::CorruptSnapshot) => "The import's stored files are damaged".into(),
        // Until GRE-428 PR 2 keeps other files as artifacts.
        Some(pp_source::Error::InvalidIdentity) => {
            "A file has a type the import does not accept".into()
        }
        Some(
            refusal @ (pp_source::Error::StoredPathTooLong { .. }
            | pp_source::Error::AmbiguousRevision(_)),
        ) => refusal.to_string(),
        _ => error.to_string(),
    }
}

/// The one worker admission a storage owner accepts, shared by the printer
/// send worker and the Source import worker (GRE-428).
pub(crate) fn worker_admission() -> WorkerAdmission {
    WorkerAdmission {
        kinds: vec![
            (JobKind::PrinterUpload, 4),
            (JobKind::SuppliedSourceImport, 1),
        ],
        total: 5,
        per_resource: 1,
        lease_seconds: crate::send_worker::LEASE_SECONDS,
    }
}

fn artifact_files(files: &[ContentFile]) -> Result<Vec<File>> {
    files
        .iter()
        .map(|file| {
            Ok(File {
                path: file.path.as_str().into(),
                size: file.size_bytes,
                sha256: file.sha256.clone(),
                kind: serde_json::to_value(&file.kind)?
                    .as_str()
                    .ok_or_else(|| anyhow!("Artifact kind requires repair"))?
                    .into(),
            })
        })
        .collect()
}

fn input_file(file: InputFile) -> File {
    File {
        path: file.path.as_str().into(),
        size: file.size,
        sha256: file.sha256,
        kind: "input".into(),
    }
}

/// Reports a scan stopped by `cancelled` as a cancellation, not as its I/O
/// result.
fn scanned<T>(cancelled: &AtomicBool, result: Result<T, pp_source::Error>) -> Result<T> {
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Cancelled before admission"
    );
    Ok(result?)
}
