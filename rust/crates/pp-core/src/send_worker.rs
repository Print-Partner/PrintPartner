//! The send queue's printer-upload worker (GRE-365, GRE-1215). It runs only
//! `printer-upload` jobs, and only the send queue enqueues those, each after
//! a user's Send now or Send ready. Per job it:
//!
//! 1. checks the job's original authority, resolves the printer's Moonraker
//!    or PrusaLink host, and reads the staged file from under `exports`;
//! 2. admits one printer effect in storage before any request:
//!    `printer_upload`, or `printer_upload_and_start` when the item starts
//!    the print;
//! 3. sends the file once through [`HostSends`] and records the result. A
//!    success is confirmed and finished, with the Build's Checkoff link. A
//!    definite refusal denies the effect, so the job fails and the printer is
//!    free. An unknown outcome fails the attempt, which leaves the job
//!    awaiting the user's reconciliation. Nothing is ever resent.
//!
//! A failure before the effect, such as an unlinked printer or a missing
//! file, fails the job with its reason and sends nothing.
use anyhow::{Context, Result, anyhow};
use axum::http::StatusCode;
use pp_api::send_queue::{Prepared, SendTargets};
use pp_gateway::{
    MoonrakerStatus,
    printer_host::{HostSends, UploadResult},
};
use pp_storage::{
    SettingsClient, WriterOwner,
    auth::{AuthFailure, AuthPolicy, AuthorityFailure},
    jobs::{
        AttemptLease, ClaimedAttempt, CompletedResult, EffectIntent, EffectOperation, JobFailure,
        JobKind, Payload, PrinterUploadResult, ResultArtifact, ServerWorkerClient, WorkerAdmission,
        WorkerOperation,
    },
    printer_hosts::{self, PrinterLink},
    read_model::farm::{HostState, Observation},
    send_queue::writes::MISSING_ARTIFACT,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

/// Node's `MAX_PRINT_FILE_UPLOAD_BYTES`.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const READER_WAIT: Duration = Duration::from_millis(500);
const INVALID_ARTIFACT: &str = "Invalid artifact path";
const SETTINGS_UNAVAILABLE: &str = "Printer settings unavailable";
const NOT_AUTHORIZED: &str =
    "The account that sent this can no longer send it. Nothing was sent to the printer.";
/// Storage could not admit the send (for example a busy writer). Failing it
/// now, rather than letting its lease expire, keeps it from starting minutes
/// later with no user action.
const NOT_STARTED: &str = pp_storage::send_queue::NOT_ADMITTED_ERROR;
const STOPPING: &str = "The send did not start because Print Partner was stopping. Nothing was sent to the printer. Send now sends it.";
/// Node's `isAllowedPrinterUploadFilename` refusal: only sliced files print.
/// Storage refuses to enqueue any other upload; the route says why first.
const NOT_SLICED: &str = "Only .gcode / .bgcode / .gco files can be sent to a printer";

fn sliced(filename: &str) -> bool {
    Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["gcode", "bgcode", "gco"].contains(&extension.to_ascii_lowercase().as_str())
        })
}
/// Longer than one upload and start (120 s and 15 s in the drivers).
const LEASE_SECONDS: u32 = 300;
/// How long an idle worker waits without a dispatch's wake.
const IDLE_RECHECK: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct SendWorker {
    worker: ServerWorkerClient,
    settings: SettingsClient,
    sends: HostSends,
    exports: PathBuf,
    wake: Arc<Notify>,
}

/// What one claimed upload ended as, for logs and tests.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    /// The printer confirmed it; the job succeeded.
    Done,
    /// Nothing printed; the job failed with this reason.
    Failed(String),
    /// The printer may have it; the job awaits reconciliation.
    Unknown(String),
    /// Storage refused the attempt, for example a revoked authority.
    Refused,
}

impl SendWorker {
    pub fn new(owner: &WriterOwner, policy: AuthPolicy, sends: HostSends) -> Result<Self> {
        let exports = owner
            .lease()
            .context("Storage lease unavailable")?
            .data_dir()
            .join("exports");
        Ok(Self {
            worker: owner.job_worker_with_policy(
                policy,
                WorkerAdmission {
                    kinds: vec![(JobKind::PrinterUpload, 4)],
                    total: 4,
                    per_resource: 1,
                    lease_seconds: LEASE_SECONDS,
                },
            )?,
            settings: owner.client(),
            sends,
            exports,
            wake: Arc::new(Notify::new()),
        })
    }

    /// Wakes an idle worker after a dispatch commits.
    pub fn waker(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Claims and runs printer uploads until `cancelled` is set. With nothing
    /// to claim it waits for a dispatch's wake, or [`IDLE_RECHECK`] for an
    /// attempt whose lease expired. Storage calls run on blocking threads.
    pub fn spawn(self, cancelled: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while !cancelled.load(Ordering::Acquire) {
                let worker = self.worker.clone();
                let flag = Arc::clone(&cancelled);
                let claimed = tokio::task::spawn_blocking(move || {
                    worker
                        .claim_pending(&flag, Duration::from_secs(1))?
                        .receive()
                })
                .await;
                match claimed {
                    Ok(Ok(Some(claimed))) => {
                        if let Err(error) = self.run(claimed, &cancelled).await {
                            tracing::warn!(error = %error, "printer upload stopped");
                        }
                    }
                    Ok(Ok(None)) => {
                        let _ = tokio::time::timeout(IDLE_RECHECK, self.wake.notified()).await;
                    }
                    Ok(Err(_)) | Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
                }
            }
        })
    }

    /// Claims one waiting printer upload and runs it, or returns `None`.
    pub async fn run_next(&self) -> Result<Option<Sent>> {
        let worker = self.worker.clone();
        let claimed = tokio::task::spawn_blocking(move || worker.claim()).await??;
        match claimed {
            Some(claimed) => self.run(claimed, &AtomicBool::new(false)).await.map(Some),
            None => Ok(None),
        }
    }

    /// Runs a claimed upload. Shutdown that begins before the upload starts
    /// fails it with [`STOPPING`] without contacting the printer.
    async fn run(&self, claimed: ClaimedAttempt, cancelled: &AtomicBool) -> Result<Sent> {
        let ClaimedAttempt { job, mut lease, .. } = claimed;
        let Payload::PrinterUpload {
            printer_id,
            artifact_path,
            filename,
            start,
            host_name,
            ..
        } = job.payload.clone()
        else {
            return Err(anyhow!("Printer worker claimed another job kind"));
        };
        let (worker, attempt) = (self.worker.clone(), lease.clone());
        if let Err(error) = tokio::task::spawn_blocking(move || worker.authorize(&attempt)).await? {
            let reason = authorize_failure(&error);
            return Ok(self.give_up(&mut lease, reason).await);
        }
        let prepared = self.prepare(&printer_id, &artifact_path).await;
        let (target, kind, integration_id, file) = match prepared {
            Ok(prepared) => prepared,
            Err(reason) => {
                self.advance(&mut lease, WorkerOperation::FailWith(reason.clone()))
                    .await?;
                return Ok(Sent::Failed(reason));
            }
        };
        let content_hash = hex::encode(Sha256::digest(&file));
        let estimate = pp_storage::slicer_grams::estimate(&file);
        let intent = intent(&job.job_id, &printer_id, &filename, start, &content_hash);
        if let Some(refused) = self.begin(&mut lease, intent, cancelled).await? {
            return Ok(refused);
        }
        let result = self.sends.upload(&target, file, &filename, start).await;
        let sent = classify(&result, start);
        match &sent {
            Sent::Done => {
                let remote_path = result
                    .remote_path
                    .clone()
                    .unwrap_or_else(|| filename.clone());
                self.advance(
                    &mut lease,
                    WorkerOperation::ConfirmEffect(ResultArtifact {
                        receipt_id: receipt_id(&remote_path),
                        content_hash,
                        target: printer_id.clone(),
                    }),
                )
                .await?;
                let (settings, printer) = (self.settings.clone(), printer_id.clone());
                // The printer has the file: labels are display text, so a failed
                // read must not keep the send from finishing.
                let labels = tokio::task::spawn_blocking(move || {
                    printer_hosts::send_labels(&settings, READER_WAIT, &printer)
                })
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
                let host_name = host_name
                    .or(labels.host_name)
                    .unwrap_or_else(|| "Printer".into());
                self.advance(
                    &mut lease,
                    WorkerOperation::FinishPublic {
                        artifact: None,
                        result: Box::new(CompletedResult::PrinterUpload(PrinterUploadResult {
                            printer_name: labels.printer_name.unwrap_or_else(|| printer_id.clone()),
                            printer_id,
                            integration_id,
                            integration_type: kind,
                            host_name,
                            filename,
                            remote_path,
                            started: result.started.unwrap_or(false),
                            message: Some(result.message.clone()),
                            checkoff_link_id: None,
                            checkoff_units: 0,
                            filament_estimate_g: estimate.map(|e| e.grams),
                            filament_grams_per_mm: estimate.and_then(|e| e.grams_per_mm),
                        })),
                    },
                )
                .await?;
                let exports = self.exports.clone();
                let _ =
                    tokio::task::spawn_blocking(move || discard(&exports, &artifact_path)).await;
            }
            Sent::Failed(reason) => {
                self.advance(&mut lease, WorkerOperation::DenyEffect(reason.clone()))
                    .await?;
            }
            Sent::Unknown(_) => {
                self.advance(&mut lease, WorkerOperation::Fail).await?;
            }
            Sent::Refused => {}
        }
        Ok(sent)
    }

    /// Admits the send's effect, or returns how the attempt ended instead. A
    /// refusal, such as Farm turned off since the dispatch, fails the job:
    /// nothing was sent. Shutdown seen before or right after admission fails
    /// it with [`STOPPING`], denying an admitted effect.
    async fn begin(
        &self,
        lease: &mut AttemptLease,
        intent: EffectIntent,
        cancelled: &AtomicBool,
    ) -> Result<Option<Sent>> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(Some(self.give_up(lease, STOPPING).await));
        }
        let Err(error) = self
            .advance(lease, WorkerOperation::BeginEffect(intent))
            .await
        else {
            if !cancelled.load(Ordering::Acquire) {
                return Ok(None);
            }
            self.advance(lease, WorkerOperation::DenyEffect(STOPPING.into()))
                .await?;
            return Ok(Some(Sent::Failed(STOPPING.into())));
        };
        let reason = match error.downcast_ref() {
            Some(failure @ JobFailure::FeatureDisabled { .. }) => failure.to_string(),
            _ => NOT_STARTED.to_owned(),
        };
        Ok(Some(self.give_up(lease, &reason).await))
    }

    /// Fails an attempt that sent nothing, so its item settles as `error` and
    /// its printer is free at once; `Refused` when storage refuses that too.
    async fn give_up(&self, lease: &mut AttemptLease, reason: &str) -> Sent {
        match self
            .advance(lease, WorkerOperation::FailWith(reason.to_owned()))
            .await
        {
            Ok(()) => Sent::Failed(reason.to_owned()),
            Err(_) => Sent::Refused,
        }
    }

    /// Applies one worker operation off the async runtime and keeps the
    /// lease's new version.
    async fn advance(&self, lease: &mut AttemptLease, operation: WorkerOperation) -> Result<()> {
        let worker = self.worker.clone();
        let mut attempt = lease.clone();
        let (attempt, result) = tokio::task::spawn_blocking(move || {
            let result = worker.update(&mut attempt, operation);
            (attempt, result)
        })
        .await?;
        *lease = attempt;
        result.map(drop)
    }

    /// The host, its type and id, and the staged file, or the reason the job
    /// fails without sending.
    async fn prepare(
        &self,
        printer_id: &str,
        artifact_path: &str,
    ) -> std::result::Result<
        (
            pp_gateway::printer_host::HostTarget,
            String,
            String,
            Vec<u8>,
        ),
        String,
    > {
        let settings = self.settings.clone();
        let printer = printer_id.to_owned();
        let link = tokio::task::spawn_blocking(move || {
            printer_hosts::printer_link(&settings, READER_WAIT, &printer)
        })
        .await
        .map_err(|_| SETTINGS_UNAVAILABLE.to_owned())?
        .map_err(|_| SETTINGS_UNAVAILABLE.to_owned())?;
        let integration = match link {
            PrinterLink::Linked(integration) => integration,
            PrinterLink::PrinterNotFound => return Err("Fleet printer not found".into()),
            PrinterLink::NotLinked => return Err("Printer is not linked to a host".into()),
            PrinterLink::NotAvailable => return Err("Linked host not found".into()),
        };
        let (kind, id) = (integration.kind.clone(), integration.id.clone());
        let Some(crate::printer_host::Host::Read(target)) = crate::printer_host::host(integration)
        else {
            return Err(format!("Upload is not supported for {kind}"));
        };
        let file = read_artifact(&self.exports, Path::new(artifact_path)).await?;
        Ok((target, kind, id, file))
    }
}

/// The staged file's real path, when it is under `exports` inside a
/// `printer-uploads` directory, as Node's `assertPrinterUploadArtifactPath`
/// requires; otherwise Node's reason.
fn staged(exports: &Path, path: &Path) -> std::result::Result<PathBuf, &'static str> {
    let root = exports.canonicalize().map_err(|_| MISSING_ARTIFACT)?;
    let file = path.canonicalize().map_err(|_| MISSING_ARTIFACT)?;
    let inside = file.starts_with(&root)
        && file
            .ancestors()
            .take_while(|ancestor| *ancestor != root)
            .any(|ancestor| {
                ancestor
                    .file_name()
                    .is_some_and(|name| name == "printer-uploads")
            });
    if !inside {
        return Err(INVALID_ARTIFACT);
    }
    if !std::fs::metadata(&file).is_ok_and(|metadata| metadata.is_file()) {
        return Err(MISSING_ARTIFACT);
    }
    Ok(file)
}

/// The staged file's bytes, or the reason the send fails.
async fn read_artifact(exports: &Path, path: &Path) -> std::result::Result<Vec<u8>, String> {
    let (exports, path) = (exports.to_owned(), path.to_owned());
    tokio::task::spawn_blocking(move || {
        let file = staged(&exports, &path)?;
        let metadata = std::fs::metadata(&file).map_err(|_| MISSING_ARTIFACT)?;
        if metadata.len() > MAX_FILE_BYTES {
            return Err("Print file exceeds the 64 MiB upload limit");
        }
        std::fs::read(&file).map_err(|_| MISSING_ARTIFACT)
    })
    .await
    .map_err(|_| MISSING_ARTIFACT.to_owned())?
    .map_err(str::to_owned)
}

/// The send queue routes' printer checks before a dispatch: the staged file,
/// the printer's Moonraker or PrusaLink host and, for an item that waits for
/// idle, the status watcher's latest state. Nothing here contacts a printer.
pub(crate) struct QueueTargets {
    pub(crate) settings: SettingsClient,
    pub(crate) exports: PathBuf,
    pub(crate) status: MoonrakerStatus,
    pub(crate) wake: Arc<Notify>,
}

impl SendTargets for QueueTargets {
    fn prepare(&self, item: &Value, idle: bool) -> Prepared {
        let refused = |status, detail: &str| Prepared::Refused {
            status,
            detail: detail.to_owned(),
        };
        if !sliced(item["filename"].as_str().unwrap_or_default()) {
            return refused(StatusCode::BAD_REQUEST, NOT_SLICED);
        }
        let path = Path::new(item["artifact_path"].as_str().unwrap_or_default());
        match staged(&self.exports, path) {
            Ok(_) => {}
            Err(MISSING_ARTIFACT) => return Prepared::Missing,
            Err(reason) => return refused(StatusCode::BAD_REQUEST, reason),
        }
        let printer = item["printer_id"].as_str().unwrap_or_default();
        let texts = [
            "Printer is not linked to a host",
            "Linked host not found",
            "Upload is not supported for",
        ];
        let (kind, id) = match readable(&self.settings, printer, texts) {
            Ok(host) => host,
            Err((status, detail)) => return Prepared::Refused { status, detail },
        };
        if idle {
            let wait = "wait for Idle or force dispatch";
            let busy = match (
                kind.as_str(),
                crate::printer_status::observed(&self.status, &id),
            ) {
                (
                    "moonraker" | "prusalink",
                    Observation::Seen {
                        state: HostState::Idle | HostState::Complete,
                        ..
                    },
                ) => None,
                ("moonraker" | "prusalink", Observation::Seen { state, .. }) => Some(format!(
                    "Printer is {} — {wait}",
                    format!("{state:?}").to_lowercase()
                )),
                ("moonraker" | "prusalink", _) => {
                    Some(format!("Printer status is unavailable — {wait}"))
                }
                _ => Some(format!(
                    "Printer status is not watched for {kind} hosts — {wait}"
                )),
            };
            if let Some(detail) = busy {
                return refused(StatusCode::CONFLICT, &detail);
            }
        }
        match printer_hosts::send_labels(&self.settings, READER_WAIT, printer) {
            Ok(labels) => Prepared::Ready {
                host_name: labels.host_name.unwrap_or_default(),
            },
            Err(_) => refused(StatusCode::SERVICE_UNAVAILABLE, SETTINGS_UNAVAILABLE),
        }
    }

    fn host(&self, printer_id: &str) -> Result<String, (StatusCode, String)> {
        queue_host(&self.settings, printer_id)
    }

    fn stage(&self, name: &str, file: &[u8]) -> std::io::Result<String> {
        stage(&self.exports, name, file)
    }

    fn dispatched(&self) {
        self.wake.notify_one();
    }

    fn discard(&self, item: &Value) {
        discard(
            &self.exports,
            item["artifact_path"].as_str().unwrap_or_default(),
        );
    }
}

/// The type and id of `printer`'s linked host when it is a
/// [`Host::Read`](crate::printer_host::Host::Read) host, or the refusal,
/// worded with `texts`: not linked, host missing, and the prefix before the
/// host type that is not read.
fn readable(
    settings: &SettingsClient,
    printer: &str,
    texts: [&str; 3],
) -> Result<(String, String), (StatusCode, String)> {
    let integration = match printer_hosts::printer_link(settings, READER_WAIT, printer) {
        Ok(PrinterLink::Linked(integration)) => integration,
        Ok(PrinterLink::PrinterNotFound) => {
            return Err((StatusCode::NOT_FOUND, "Fleet printer not found".into()));
        }
        Ok(PrinterLink::NotLinked) => return Err((StatusCode::BAD_REQUEST, texts[0].into())),
        Ok(PrinterLink::NotAvailable) => return Err((StatusCode::BAD_REQUEST, texts[1].into())),
        Err(_) => return Err((StatusCode::SERVICE_UNAVAILABLE, SETTINGS_UNAVAILABLE.into())),
    };
    let (kind, id) = (integration.kind.clone(), integration.id.clone());
    match crate::printer_host::host(integration) {
        Some(crate::printer_host::Host::Read(_)) => Ok((kind, id)),
        _ => Err((StatusCode::BAD_REQUEST, format!("{} {kind}", texts[2]))),
    }
}

/// Add to queue's printer check, in Node's words: a fleet printer linked to
/// a host the send worker can upload to. The answer is the host's name.
fn queue_host(settings: &SettingsClient, printer: &str) -> Result<String, (StatusCode, String)> {
    let texts = [
        "Printer is not linked to a host. Link a Moonraker or PrusaLink host in Settings.",
        "Linked printer host was not found",
        "Queue is not supported for",
    ];
    readable(settings, printer, texts)?;
    printer_hosts::send_labels(settings, READER_WAIT, printer)
        .map(|labels| labels.host_name.unwrap_or_default())
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, SETTINGS_UNAVAILABLE.into()))
}

/// Writes `file` as `name` in a new directory under
/// `exports/printer-uploads/`, as Node's `streamPrinterUploadArtifact` does.
fn stage(exports: &Path, name: &str, file: &[u8]) -> std::io::Result<String> {
    let mut parts = Path::new(name).components();
    if !matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Staged file name must be one path component",
        ));
    }
    let directory = exports
        .join("printer-uploads")
        .join(hex::encode(rand::random::<[u8; 16]>()));
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(name);
    if let Err(error) = std::fs::write(&path, file) {
        let _ = std::fs::remove_dir_all(&directory);
        return Err(error);
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Deletes a staged file's own directory, as Node's
/// `cleanupPrinterUploadArtifactDir` does after a sent or removed item.
fn discard(exports: &Path, path: &str) {
    if let Ok(file) = staged(exports, Path::new(path))
        && let Some(directory) = file.parent()
        && directory.parent().and_then(Path::file_name) == Some("printer-uploads".as_ref())
    {
        let _ = std::fs::remove_dir_all(directory);
    }
}

/// Why a send whose authority check failed did not start: the account's
/// refusal, or storage that could not answer.
fn authorize_failure(error: &anyhow::Error) -> &'static str {
    let refused = error.downcast_ref::<AuthorityFailure>().is_some()
        || matches!(
            error.downcast_ref::<AuthFailure>(),
            Some(
                AuthFailure::OwnerMappingRequired
                    | AuthFailure::SessionRequired
                    | AuthFailure::InvalidCredentials
                    | AuthFailure::CredentialChanged
            )
        );
    if refused { NOT_AUTHORIZED } else { NOT_STARTED }
}

/// The effect one send admits: the upload, or the upload and its start,
/// bound to this job's printer, file and choice.
fn intent(
    job_id: &str,
    printer_id: &str,
    filename: &str,
    start: bool,
    content_hash: &str,
) -> EffectIntent {
    EffectIntent {
        operation: if start {
            EffectOperation::PrinterUploadAndStart
        } else {
            EffectOperation::PrinterUpload
        },
        basis_hash: hex::encode(Sha256::digest(format!(
            "{job_id}\n{printer_id}\n{filename}\n{start}"
        ))),
        content_hash: content_hash.to_owned(),
        target: printer_id.to_owned(),
    }
}

/// How the driver's answer settles the admitted effect. A start that was
/// requested but definitely did not happen means nothing printed.
fn classify(result: &UploadResult, start: bool) -> Sent {
    if result.outcome_unknown {
        return Sent::Unknown(result.message.clone());
    }
    if result.ok && (!start || result.started == Some(true)) {
        return Sent::Done;
    }
    Sent::Failed(result.message.clone())
}

/// The remote path cut to the 128 bytes a receipt id may hold, on a
/// character boundary.
fn receipt_id(remote_path: &str) -> String {
    let mut end = remote_path.len().min(128);
    while !remote_path.is_char_boundary(end) {
        end -= 1;
    }
    remote_path[..end].to_owned()
}

#[cfg(test)]
mod tests;
