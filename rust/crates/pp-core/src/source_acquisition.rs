use crate::uploads::{SourceImports, Through};
use anyhow::{Context, Result, anyhow, ensure};
use pp_api::catalog::UploadKind;
use pp_source::{
    SourcePath,
    retained_capture::{
        CaptureCandidate, FreezeResult, FrozenCapture, RetainedCapturePhase, RetainedCaptureVault,
    },
};
use pp_storage::{
    WriterFailure, WriterOwner,
    auth::AuthPolicy,
    jobs::Credential,
    uploads::{
        AdmissionLimits, CaptureId, CapturedPayloadV1, File, Operation, PreflightedCapture,
        PreparedCapturedAdmission, Target,
    },
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt,
    io::{self, Read},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// One file of a capture request, read into the retained capture.
pub struct CapturedFile {
    pub path: SourcePath,
    pub input: Box<dyn Read + Send>,
}

/// A captured Source import request: the admission it asks for and the files
/// that make up its input.
pub struct CaptureRequest {
    pub credential: Credential,
    pub operation_key: String,
    pub target: Target,
    pub payload: CapturedPayloadV1,
    pub limits: AdmissionLimits,
    pub files: Vec<CapturedFile>,
}

/// A captured import whose files arrive one at a time, such as the parts of
/// an upload. Each item names the file's path inside the Source; an `Err`
/// item means the stream broke, and nothing is admitted. A ZIP takes exactly
/// one item, and the import unpacks it.
pub struct StreamedRequest<F> {
    pub credential: Credential,
    pub operation_key: String,
    pub target: Target,
    pub kind: UploadKind,
    pub limits: AdmissionLimits,
    pub files: F,
}

/// Why [`SourceAcquisitions`] refused or failed an acquisition. Only
/// [`AcquisitionError::Failed`] can follow a write to the journal.
#[derive(Debug)]
pub enum AcquisitionError {
    /// Recovery has not opened ingress, or shutdown has begun.
    Closed(&'static str),
    /// Every capacity slot is held.
    Busy,
    /// The operation key is already in flight.
    InProgress,
    /// The request held no file.
    NoFiles,
    /// The files are over the import's input limit.
    TooLarge,
    /// A file was refused: an unsafe or repeated path, or a broken stream.
    /// The text says which, in words.
    Input(String),
    /// Storage refused the admission: the Source, the credential, or a
    /// storage fault, sorted by [`WriterFailure`].
    Refused(WriterFailure),
    /// The acquisition gate's lock was poisoned by a panicking thread.
    GateUnavailable,
    /// Any other failure, such as the capture vault or freezing the files.
    Failed(anyhow::Error),
}

impl AcquisitionError {
    fn context(self, context: impl fmt::Display + Send + Sync + 'static) -> Self {
        match self {
            Self::Failed(error) => Self::Failed(error.context(context)),
            // A refusal of the request stays as worded; cleanup is the
            // service's concern.
            refusal => refusal,
        }
    }

    /// Sorts a failure while writing the file at `path`: refusals of the
    /// input itself are the caller's, everything else is the service's.
    fn writing(error: pp_source::Error, path: &str) -> Self {
        match error {
            pp_source::Error::Limit => Self::TooLarge,
            pp_source::Error::UnsafePath | pp_source::Error::UnsafeAncestor => {
                Self::Input(format!("File path {path:?} is not allowed"))
            }
            pp_source::Error::DuplicatePath => {
                Self::Input(format!("File path {path:?} appears twice"))
            }
            pp_source::Error::StoredPathTooLong { .. } => {
                Self::Input(format!("File path {path:?} is too long"))
            }
            error => Self::Failed(anyhow::Error::from(error)),
        }
    }
}

impl fmt::Display for AcquisitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Failed(error) if formatter.alternate() => {
                return write!(formatter, "{error:#}");
            }
            Self::Failed(error) => return write!(formatter, "{error}"),
            Self::Refused(error) => {
                return write!(formatter, "Source acquisition refused: {error}");
            }
            Self::GateUnavailable => "Source acquisition gate poisoned",
            Self::Closed(text) => text,
            Self::Input(text) => text,
            Self::Busy => "Source acquisition capacity exhausted",
            Self::InProgress => "Source acquisition already in progress for this key",
            Self::NoFiles => "Source acquisition has no files",
            Self::TooLarge => "Source acquisition input is too large",
        })
    }
}

impl std::error::Error for AcquisitionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Failed(error) => error.source(),
            Self::Refused(error) => Some(error),
            _ => None,
        }
    }
}

/// The result of an accepted acquisition, delivered once its operation is
/// recorded.
pub struct AcquisitionReply {
    admitted: mpsc::Receiver<Result<Operation, AcquisitionError>>,
    result: mpsc::Receiver<Result<Operation>>,
}

impl AcquisitionReply {
    fn ready(operation: Operation) -> Self {
        let (admitted, admitted_receiver) = mpsc::channel();
        let (result, receiver) = mpsc::channel();
        let _ = admitted.send(Ok(operation.clone()));
        let _ = result.send(Ok(operation));
        Self {
            admitted: admitted_receiver,
            result: receiver,
        }
    }

    /// Blocks until the acquisition's admission is recorded, and returns the
    /// operation as admitted. The worker goes on to settle it.
    pub fn admitted(&self) -> Result<Operation, AcquisitionError> {
        self.admitted
            .recv()
            .map_err(|_| AcquisitionError::Failed(anyhow!("Source acquisition worker stopped")))?
    }

    /// Blocks until the acquisition's operation is recorded.
    pub fn wait(self) -> Result<Operation> {
        self.result
            .recv()
            .map_err(|_| anyhow!("Source acquisition worker stopped"))?
    }
}

/// What [`SourceAcquisitions::shutdown`] observed when it returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionDrain {
    /// Whether every acquisition finished within the budget.
    pub idle: bool,
    /// Operation keys whose workers stopped for shutdown after admission,
    /// sorted. Each keeps its journal row and reserved bytes; nothing resumes
    /// it in this instance. A key stopped while still admitted keeps its
    /// retained capture, which a later instance's
    /// [`SourceAcquisitions::recover_before_accepting`] settles; a key stopped
    /// after its capture became owned input is resumed by import job restart
    /// recovery when storage is next opened.
    pub interrupted: Vec<String>,
    /// Operation keys still in flight when the budget ran out, sorted.
    pub pending: Vec<String>,
}

const INTERRUPTED: &str = "Source acquisition interrupted by shutdown; retained for recovery";

struct Gate {
    open: bool,
    /// Set once by shutdown; refuses every later acquisition and recovery.
    shut_down: bool,
    active: usize,
    capacity: usize,
    /// Operation keys of every acquisition between preflight and its recorded
    /// result. Keys are held across tenants, so two tenants acquiring one key
    /// at the same moment see the later one refused.
    in_flight: HashSet<String>,
    interrupted: BTreeSet<String>,
}

/// The gate, signalled whenever an operation key is released, and the stop
/// flag every worker observes.
struct Shared {
    gate: Mutex<Gate>,
    released: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn gate(&self) -> Result<MutexGuard<'_, Gate>, AcquisitionError> {
        self.gate
            .lock()
            .map_err(|_| AcquisitionError::GateUnavailable)
    }

    /// Refuses unless ingress is open.
    fn open_gate(&self) -> Result<MutexGuard<'_, Gate>, AcquisitionError> {
        let gate = self.gate()?;
        if gate.shut_down {
            return Err(AcquisitionError::Closed(
                "Source acquisition is shutting down",
            ));
        }
        if !gate.open {
            return Err(AcquisitionError::Closed(
                "Source acquisition ingress is closed",
            ));
        }
        Ok(gate)
    }
}

/// One reserved operation key and, once taken, one capacity slot; dropping it
/// returns both.
struct Permit {
    shared: Arc<Shared>,
    key: String,
    slot: bool,
}

impl Permit {
    fn take_slot(&mut self) -> Result<(), AcquisitionError> {
        let mut gate = self.shared.open_gate()?;
        if gate.active >= gate.capacity {
            return Err(AcquisitionError::Busy);
        }
        gate.active += 1;
        self.slot = true;
        Ok(())
    }

    /// Records this key as stopped by shutdown, before the key is released.
    fn interrupt(&self) {
        if let Ok(mut gate) = self.shared.gate() {
            gate.interrupted.insert(self.key.clone());
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut gate) = self.shared.gate() {
            gate.in_flight.remove(&self.key);
            if self.slot {
                gate.active -= 1;
            }
        }
        self.shared.released.notify_all();
    }
}

/// The admission a capture's manifest is bound to.
struct Bound {
    operation_key: String,
    payload: CapturedPayloadV1,
    limits: AdmissionLimits,
}

/// A frozen capture with its prepared admission, owned by the service until
/// its operation is recorded.
struct Accepted {
    prepared: PreparedCapturedAdmission,
    frozen: FrozenCapture,
    unstable: bool,
    permit: Permit,
}

/// Captured Source acquisitions over the retained capture vault. Ingress stays
/// closed until [`SourceAcquisitions::recover_before_accepting`] has settled
/// every retained capture left by an earlier process. At most `capacity`
/// acquisitions hold a slot at once; a request beyond that is refused before
/// it reads input or writes a capture or journal row. After
/// [`SourceAcquisitions::shutdown`] the instance refuses all work; a new
/// instance recovers what it left.
pub struct SourceAcquisitions {
    imports: Arc<SourceImports>,
    data_dir: PathBuf,
    shared: Arc<Shared>,
}

impl SourceAcquisitions {
    pub fn new(
        owner: &WriterOwner,
        policy: AuthPolicy,
        quota: u64,
        capacity: usize,
    ) -> Result<Self> {
        ensure!(capacity > 0, "Source acquisition capacity must be positive");
        let data_dir = owner
            .import_repos_root()?
            .parent()
            .ok_or_else(|| anyhow!("Storage data directory unavailable"))?
            .to_owned();
        Ok(Self {
            imports: Arc::new(SourceImports::new(owner, policy, quota)?),
            data_dir,
            shared: Arc::new(Shared {
                gate: Mutex::new(Gate {
                    open: false,
                    shut_down: false,
                    active: 0,
                    capacity,
                    in_flight: HashSet::new(),
                    interrupted: BTreeSet::new(),
                }),
                released: Condvar::new(),
                stop: AtomicBool::new(false),
            }),
        })
    }

    /// Begins shutdown and drains acquisition workers within one `budget`
    /// shared by every acquisition. Ingress closes for good: later
    /// acquisitions, including replies for recorded keys, and recovery are
    /// refused without reading input or writing anything. An acquisition
    /// still capturing its input finishes its capture unless its own
    /// cancellation stops it. Every worker is told to stop: one that has
    /// admitted its capture but not settled it stops, leaves its journal row
    /// and retained capture as they are, and replies that it was interrupted.
    /// Nothing replays interrupted work here; a new instance's
    /// [`SourceAcquisitions::recover_before_accepting`] settles a retained
    /// capture, after a restart when its import job was already claimed.
    /// Returns once no
    /// acquisition is in flight or the budget runs out; repeating it waits
    /// again and reports every interruption so far.
    pub fn shutdown(&self, budget: Duration) -> Result<AcquisitionDrain> {
        let deadline = Instant::now() + budget;
        let mut gate = self.gate()?;
        gate.shut_down = true;
        gate.open = false;
        self.shared.stop.store(true, Ordering::Release);
        while !gate.in_flight.is_empty() {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            gate = self
                .shared
                .released
                .wait_timeout(gate, left)
                .map_err(|_| anyhow!("Source acquisition gate poisoned"))?
                .0;
        }
        let mut pending = gate.in_flight.iter().cloned().collect::<Vec<_>>();
        pending.sort();
        Ok(AcquisitionDrain {
            idle: pending.is_empty(),
            interrupted: gate.interrupted.iter().cloned().collect(),
            pending,
        })
    }

    /// Closes ingress, finishes interrupted vault deletions, discards
    /// candidates a stopped process left half-written, and drives every
    /// frozen capture that exactly matches an admitted journal row through
    /// settlement. Captures whose journal row is already owned or later, or
    /// absent, are removed. Conflicting and malformed captures are
    /// left byte-for-byte unchanged; while any remain, ingress stays closed and
    /// this returns an error naming each remaining capture and its cause.
    /// Refuses while an acquisition still holds a slot, leaving ingress as it
    /// was, and refuses once shutdown has begun. Returns the number of
    /// captures reconciled.
    pub fn recover_before_accepting(&self) -> Result<usize> {
        {
            let mut gate = self.gate()?;
            ensure!(!gate.shut_down, "Source acquisitions are shut down");
            ensure!(gate.in_flight.is_empty(), "Source acquisitions are active");
            gate.open = false;
        }
        let vault = RetainedCaptureVault::open(&self.data_dir)?;
        // A malformed slot stops vault deletion cleanup; it is reported below
        // with every other capture that remains.
        let deleting = vault.finish_deleting().err();
        let entries = vault.scan()?;
        let mut causes = HashMap::new();
        for entry in &entries {
            if entry.phase() == RetainedCapturePhase::Candidate {
                // Admission follows the freeze, so nothing refers to it.
                if let Err(error) = vault.discard_candidate(entry.capture_id()) {
                    causes.insert(entry.capture_id().as_str().to_owned(), error.to_string());
                }
                continue;
            }
            if entry.phase() != RetainedCapturePhase::Frozen {
                continue;
            }
            let worked = vault
                .open_frozen(entry.capture_id())
                .map_err(anyhow::Error::from)
                .and_then(|frozen| {
                    self.imports
                        .work_frozen(frozen, Through::Settled, &AtomicBool::new(false))
                });
            if let Err(error) = worked {
                causes.insert(entry.capture_id().as_str().to_owned(), error.to_string());
            }
        }
        let remaining = vault.scan()?;
        if !remaining.is_empty() {
            let causes = remaining
                .iter()
                .map(|entry| {
                    let cause = causes
                        .remove(entry.capture_id().as_str())
                        .unwrap_or_else(|| {
                            match entry.phase() {
                                // Settlement returned without error but left the
                                // capture: its job is claimed until restart recovery
                                // requeues it.
                                RetainedCapturePhase::Frozen => {
                                    "Import job is held by another attempt".to_owned()
                                }
                                RetainedCapturePhase::Candidate => {
                                    "Capture candidate was never frozen".to_owned()
                                }
                                RetainedCapturePhase::Deleting => match &deleting {
                                    Some(error) => format!("Capture deletion stopped: {error}"),
                                    None => "Capture deletion stopped".to_owned(),
                                },
                                RetainedCapturePhase::Malformed => {
                                    "Malformed retained capture".to_owned()
                                }
                            }
                        });
                    format!("{}: {cause}", entry.capture_id().as_str())
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(anyhow!(
                "Retained captures require repair ({} remaining): {causes}",
                remaining.len()
            ));
        }
        self.set_ingress(true)?;
        Ok(entries.len())
    }

    /// Admits a prepared captured import once recovery has opened ingress.
    /// Refused admissions write no journal rows.
    pub fn admit_captured(&self, prepared: PreparedCapturedAdmission) -> Result<Operation> {
        ensure!(self.gate()?.open, "Source acquisition ingress is closed");
        self.imports.admit_captured(prepared)
    }

    /// Settles imports a stopped process left after their capture became owned
    /// input; opening storage requeued their jobs. Call it after
    /// [`SourceAcquisitions::recover_before_accepting`], which settles the
    /// imports still held as retained captures. A failed import is recorded
    /// as failed on its job and not counted. Returns the number of imports
    /// settled; a failure to claim ends the call with the count so far.
    pub fn resume_imports(&self) -> Result<usize> {
        let mut settled = 0;
        // Each claim leases its job, so a job that fails is not claimed again
        // here; the bound only guards against a storage fault.
        for _ in 0..1024 {
            // A failure to claim (a busy, full or stopped writer) ends the
            // loop: nothing was taken, and retrying would only repeat it.
            let claim = match self.imports.claim_next() {
                Ok(Some(claim)) => claim,
                Ok(None) => return Ok(settled),
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "Source imports not claimed");
                    return Ok(settled);
                }
            };
            match self
                .imports
                .work_claim(claim, Through::Settled, &self.shared.stop)
            {
                Ok(_) => settled += 1,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "Source import did not resume");
                }
            }
        }
        Ok(settled)
    }

    /// Acquires a captured import. A key whose operation is already recorded
    /// replies with that operation as recorded, which may not be settled yet,
    /// and writes nothing. The key is reserved before preflight, so a retry of
    /// a key still in flight is refused rather than freezing a second capture.
    /// Otherwise the request takes a capacity slot and
    /// keeps its key reserved, its files are written and frozen as a retained
    /// capture, and a worker admits and settles it; slot and key are held until
    /// the operation is recorded. A request refused for closed ingress,
    /// exhausted capacity, a key already in flight, or preflight reads no input
    /// and writes no capture and no journal row; one that fails before freezing
    /// removes its candidate.
    pub fn acquire(
        &self,
        request: CaptureRequest,
        cancelled: &AtomicBool,
    ) -> Result<AcquisitionReply, AcquisitionError> {
        let CaptureRequest {
            credential,
            operation_key,
            target,
            payload,
            limits,
            files,
        } = request;
        let mut permit = self.reserve(operation_key.clone())?;
        let authority = self
            .imports
            .preflight_capture(
                credential,
                operation_key.clone(),
                target,
                payload.clone(),
                limits,
            )
            // Converted before any context, so a busy, full, stopped or
            // refusing writer keeps its type.
            .map_err(|error| AcquisitionError::Refused(WriterFailure::from(error)))?;
        if let Some(operation) = authority.replay() {
            return Ok(AcquisitionReply::ready(operation.clone()));
        }
        permit.take_slot()?;
        if files.is_empty() {
            return Err(AcquisitionError::NoFiles);
        }
        let mut candidate = self.allocate()?;
        for file in files {
            if let Err(error) = write(&mut candidate, file.path, file.input, limits, cancelled) {
                return Err(self.abort(candidate, error));
            }
        }
        let bound = Bound {
            operation_key,
            payload,
            limits,
        };
        let accepted = self.freeze(authority, permit, bound, candidate, cancelled)?;
        self.spawn(accepted)
    }

    /// Acquires a captured import whose files arrive one at a time. Unlike
    /// [`SourceAcquisitions::acquire`], the paths are known only once every
    /// file is written, so the key and a capacity slot are taken before any
    /// input is read and preflight runs after. A refusal at any step removes
    /// the candidate and writes no journal row. Returns once the files are
    /// frozen and the worker is admitting them.
    ///
    /// A failure once shutdown has begun is [`AcquisitionError::Closed`]: the
    /// stop flag also stops the writes, so the candidate is discarded and the
    /// key and slot are released.
    pub fn acquire_streamed<F, R>(
        &self,
        request: StreamedRequest<F>,
    ) -> Result<AcquisitionReply, AcquisitionError>
    where
        F: Iterator<Item = io::Result<(String, R)>>,
        R: Read,
    {
        match self.streamed(request) {
            Err(
                AcquisitionError::Input(_)
                | AcquisitionError::Refused(_)
                | AcquisitionError::Failed(_),
            ) if self.closing() => Err(AcquisitionError::Closed(
                "Source acquisition is shutting down",
            )),
            result => result,
        }
    }

    /// Whether shutdown has begun.
    pub fn closing(&self) -> bool {
        self.shared.stop.load(Ordering::Acquire)
    }

    fn streamed(
        &self,
        request: StreamedRequest<impl Iterator<Item = io::Result<(String, impl Read)>>>,
    ) -> Result<AcquisitionReply, AcquisitionError> {
        let StreamedRequest {
            credential,
            operation_key,
            target,
            kind,
            limits,
            files,
        } = request;
        let mut permit = self.reserve(operation_key.clone())?;
        permit.take_slot()?;
        let cancelled = &self.shared.stop;
        let mut candidate = self.allocate()?;
        let mut paths = Vec::new();
        for file in files {
            if kind == UploadKind::Zip && !paths.is_empty() {
                let second = AcquisitionError::Input("A ZIP upload takes one file".into());
                return Err(self.abort(candidate, second));
            }
            let written = file
                .map_err(|_| AcquisitionError::Input("The upload body could not be read".into()))
                .and_then(|(path, input)| {
                    let path = SourcePath::try_from(path.clone()).map_err(|_| {
                        AcquisitionError::Input(format!("File path {path:?} is not allowed"))
                    })?;
                    paths.push(path.as_str().to_owned());
                    write(&mut candidate, path, input, limits, cancelled)
                });
            if let Err(error) = written {
                return Err(self.abort(candidate, error));
            }
        }
        if paths.is_empty() {
            return Err(self.abort(candidate, AcquisitionError::NoFiles));
        }
        let payload = match kind {
            UploadKind::Files => CapturedPayloadV1::files(paths),
            UploadKind::Zip => CapturedPayloadV1::zip(paths.swap_remove(0)),
        };
        let preflight = self
            .imports
            .preflight_capture(
                credential,
                operation_key.clone(),
                target,
                payload.clone(),
                limits,
            )
            .map_err(|error| AcquisitionError::Refused(WriterFailure::from(error)));
        let authority = match preflight {
            Ok(authority) if authority.replay().is_none() => authority,
            Ok(_) => {
                let replayed = anyhow!("Source acquisition key already recorded");
                return Err(self.abort(candidate, AcquisitionError::Failed(replayed)));
            }
            Err(refused) => return Err(self.abort(candidate, refused)),
        };
        let bound = Bound {
            operation_key,
            payload,
            limits,
        };
        let accepted = self.freeze(authority, permit, bound, candidate, cancelled)?;
        self.spawn(accepted)
    }

    /// Allocates a candidate; one that cannot be cleaned up closes ingress
    /// until recovery repairs it.
    fn allocate(&self) -> Result<CaptureCandidate, AcquisitionError> {
        let vault = RetainedCaptureVault::open(&self.data_dir)
            .map_err(|error| AcquisitionError::Failed(error.into()))?;
        vault.allocate_owned().map_err(|error| {
            if error.retained().is_some() {
                let _ = self.set_ingress(false);
            }
            AcquisitionError::Failed(error.into())
        })
    }

    /// Binds the written candidate's manifest to the preflighted admission
    /// and freezes it. Before the freeze, any failure removes the candidate; a
    /// candidate that cannot be removed closes ingress until recovery repairs
    /// it.
    fn freeze(
        &self,
        authority: PreflightedCapture,
        permit: Permit,
        bound: Bound,
        candidate: CaptureCandidate,
        cancelled: &AtomicBool,
    ) -> Result<Accepted, AcquisitionError> {
        let limits = bound.limits;
        let prepared = (|| -> Result<PreparedCapturedAdmission> {
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Source acquisition cancelled"
            );
            let inventory = candidate
                .inventory(limits.max_input_bytes)
                .context("Source acquisition inventory failed")?
                .into_iter()
                .map(|file| File {
                    path: file.path.as_str().to_owned(),
                    size: file.size,
                    sha256: file.sha256,
                    kind: "input".into(),
                })
                .collect();
            let capture_id = CaptureId::new(candidate.capture_id().as_str().to_owned())?;
            let prepared = authority
                .prepare(
                    capture_id,
                    bound.operation_key,
                    bound.payload,
                    limits,
                    inventory,
                )
                .context("Source acquisition binding failed")?;
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Source acquisition cancelled"
            );
            Ok(prepared)
        })();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return Err(self.abort(candidate, AcquisitionError::Failed(error))),
        };
        match candidate.freeze_owned(prepared.manifest()) {
            FreezeResult::Frozen(frozen) => Ok(Accepted {
                prepared,
                frozen,
                unstable: false,
                permit,
            }),
            FreezeResult::FrozenWithError { capture, .. } => Ok(Accepted {
                prepared,
                frozen: capture,
                unstable: true,
                permit,
            }),
            FreezeResult::CandidateError { candidate, error } => Err(self.abort(
                candidate,
                AcquisitionError::Failed(
                    anyhow::Error::from(error).context("Source acquisition freeze failed"),
                ),
            )),
        }
    }

    fn abort(&self, candidate: CaptureCandidate, error: AcquisitionError) -> AcquisitionError {
        match candidate.abort() {
            Ok(()) => error.context("Owned capture candidate removed"),
            Err(cleanup) => {
                let _ = self.set_ingress(false);
                tracing::warn!(%cleanup, "Capture candidate kept; uploads closed until repaired");
                error.context(format!(
                    "Owned capture cleanup retained repair receipt {:?}: {cleanup}",
                    cleanup.receipt()
                ))
            }
        }
    }

    /// Admits and settles an accepted capture on its own worker. The slot is
    /// returned once the operation is recorded, before the reply is sent. A
    /// worker that sees shutdown after admission stops before settling.
    fn spawn(&self, accepted: Accepted) -> Result<AcquisitionReply, AcquisitionError> {
        let receipt = accepted.frozen.receipt();
        let imports = self.imports.clone();
        let shared = self.shared.clone();
        let (sender, result) = mpsc::channel();
        let (admission, admitted) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("pp-source-acquisition".into())
            .spawn(move || {
                let Accepted {
                    prepared,
                    frozen,
                    unstable,
                    permit,
                } = accepted;
                let stabilized = if unstable {
                    frozen.stabilize().map_err(anyhow::Error::from)
                } else {
                    Ok(())
                };
                // The route gets the admission's own typed failure; `wait`
                // keeps a text copy of it.
                let admitted = match stabilized {
                    Ok(()) => imports
                        .admit_captured(prepared)
                        .map_err(|error| AcquisitionError::Refused(WriterFailure::from(error))),
                    Err(error) => Err(AcquisitionError::Failed(error)),
                };
                let admitted = match admitted {
                    Ok(operation) => {
                        let _ = admission.send(Ok(operation.clone()));
                        Ok(operation)
                    }
                    Err(error) => {
                        let shown = anyhow!("{error:#}");
                        let _ = admission.send(Err(error));
                        Err(shown)
                    }
                };
                let retained = admitted.is_ok();
                let stopped = || retained && shared.stop.load(Ordering::Acquire);
                let outcome = if stopped() {
                    Err(anyhow!(INTERRUPTED))
                } else {
                    match imports.work_frozen(frozen, Through::Settled, &shared.stop) {
                        Ok(Some(operation)) => Ok(operation),
                        Ok(None) => admitted.and_then(|operation| {
                            ensure!(
                                operation.cleanup_settled,
                                "Source acquisition did not settle"
                            );
                            Ok(operation)
                        }),
                        Err(error) => Err(error),
                    }
                };
                // A failure seen after shutdown stopped an admitted worker
                // leaves the import for recovery rather than failing it.
                let outcome = match outcome {
                    Err(_) if stopped() => {
                        permit.interrupt();
                        Err(anyhow!(INTERRUPTED))
                    }
                    outcome => outcome,
                };
                drop(permit);
                let _ = sender.send(outcome);
            });
        if let Err(error) = spawned {
            self.set_ingress(false)?;
            return Err(AcquisitionError::Failed(anyhow!(
                "Source acquisition worker unavailable; retained repair receipt {receipt:?}: {error}"
            )));
        }
        Ok(AcquisitionReply { admitted, result })
    }

    fn reserve(&self, key: String) -> Result<Permit, AcquisitionError> {
        let mut gate = self.gate()?;
        if gate.shut_down {
            return Err(AcquisitionError::Closed(
                "Source acquisition is shutting down",
            ));
        }
        if !gate.in_flight.insert(key.clone()) {
            return Err(AcquisitionError::InProgress);
        }
        Ok(Permit {
            shared: self.shared.clone(),
            key,
            slot: false,
        })
    }

    fn set_ingress(&self, open: bool) -> Result<(), AcquisitionError> {
        self.gate()?.open = open;
        Ok(())
    }

    fn gate(&self) -> Result<MutexGuard<'_, Gate>, AcquisitionError> {
        self.shared.gate()
    }
}

/// Writes one file into `candidate`, stopping when `cancelled` is set.
fn write(
    candidate: &mut CaptureCandidate,
    path: SourcePath,
    input: impl Read,
    limits: AdmissionLimits,
    cancelled: &AtomicBool,
) -> Result<(), AcquisitionError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(AcquisitionError::Failed(anyhow!(
            "Source acquisition cancelled"
        )));
    }
    let shown = path.as_str().to_owned();
    candidate
        .write_file_from_cancelled(path, input, limits.max_input_bytes, cancelled)
        .map(drop)
        .map_err(|error| {
            if cancelled.load(Ordering::Acquire) {
                AcquisitionError::Failed(anyhow!("Source acquisition cancelled"))
            } else {
                AcquisitionError::writing(error, &shown)
            }
        })
}
