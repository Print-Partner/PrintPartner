//! `POST /api/v2/installation/backup` and `GET /api/v2/installation/backup/archive`
//! (GRE-1371). Mounted only when `PP_DESKTOP_REBUILD` is exactly `1` or `true`.
//!
//! Create calls [`pp_storage::restore::InstallationClient::export_backup`], which
//! writes one full archive under `<data>/backups/`. The router keeps that one
//! archive and streams it. A later create drops the previous one. The client
//! sends no path. One request runs at a time.

use crate::admin::{AdminHttpConfig, Gate};
use crate::loopback::PresentedCredential;
use crate::problem::{self, Problem, ProblemType, ToProblem};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use pp_storage::{
    WriterFailure, WriterOwner,
    archive::NoSpace,
    read_model::ReadClient,
    restore::{BackupFile, BackupWarning, Refusal, RestoreError},
};
use serde::Serialize;
use serde_json::{Map, json};
use std::{
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    task::{Context, Poll},
};
use tokio::io::AsyncRead;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The create route's body. `name` is the archive's file name, never a path.
/// The generated client calls the route `createInstallationBackup`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema, serde::Deserialize))]
pub struct InstallationBackup {
    pub name: String,
    pub created_at: String,
    pub app_version: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub warnings: Vec<InstallationBackupWarning>,
}

/// A backup was made, and the user should know this about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema, serde::Deserialize))]
pub enum InstallationBackupWarning {
    Large,
}

impl From<BackupWarning> for InstallationBackupWarning {
    fn from(warning: BackupWarning) -> Self {
        match warning {
            BackupWarning::Large => Self::Large,
        }
    }
}

/// Why a backup request was refused.
#[derive(Debug)]
enum BackupFailure {
    NoSpace(NoSpace),
    /// Activation has paused restore work.
    StorageBusy,
    /// This router is already creating or sending a backup.
    Busy,
    NoneKept,
    Writer(WriterFailure),
    Internal,
}

impl From<RestoreError> for BackupFailure {
    fn from(error: RestoreError) -> Self {
        match error {
            RestoreError::Refused(Refusal::NoSpace(space)) => Self::NoSpace(space),
            RestoreError::Refused(Refusal::Busy) => Self::StorageBusy,
            RestoreError::Refused(_) | RestoreError::RolledBack | RestoreError::RestartRequired => {
                Self::Internal
            }
            RestoreError::Writer(failure) => Self::Writer(failure),
        }
    }
}

impl ToProblem for BackupFailure {
    fn to_problem(&self) -> Problem {
        match self {
            Self::NoSpace(_) => {
                Problem::new(ProblemType::Overloaded, 503, "Not enough free disk space")
            }
            Self::StorageBusy => {
                Problem::new(ProblemType::Overloaded, 503, "A restore is in progress")
            }
            Self::Busy => Problem::new(ProblemType::Overloaded, 503, "A backup is already running"),
            Self::NoneKept => {
                Problem::new(ProblemType::NotFound, 404, "No backup has been created")
            }
            Self::Writer(failure) => failure.to_problem(),
            Self::Internal => Problem::new(ProblemType::Internal, 500, "Internal Server Error"),
        }
    }
}

fn refused(failure: &BackupFailure) -> Response {
    match failure {
        BackupFailure::NoSpace(space) => {
            let mut extensions = Map::new();
            extensions.insert("needed".into(), json!(space.needed));
            extensions.insert("available".into(), json!(space.available));
            problem::build(failure.to_problem(), extensions)
        }
        other => problem::refused(other),
    }
}

#[derive(Default)]
struct Inner {
    busy: bool,
    kept: Option<Arc<BackupFile>>,
}

struct Slot {
    inner: Mutex<Inner>,
}

/// Clears `busy` when the create finishes or the download body is dropped.
struct Flight {
    slot: Arc<Slot>,
    kept: Option<Arc<BackupFile>>,
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.slot.lock().busy = false;
    }
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(self: &Arc<Self>) -> Result<Flight, BackupFailure> {
        let mut inner = self.lock();
        if inner.busy {
            return Err(BackupFailure::Busy);
        }
        inner.busy = true;
        Ok(Flight {
            kept: inner.kept.clone(),
            slot: Arc::clone(self),
        })
    }

    fn keep(&self, file: BackupFile) {
        self.lock().kept = Some(Arc::new(file));
    }
}

#[derive(Clone)]
struct App {
    owner: Arc<Mutex<WriterOwner>>,
    slot: Arc<Slot>,
}

/// Mounts the backup routes under `/api/v2` when `config` was built with
/// `PP_DESKTOP_REBUILD` exactly `1` or `true`. Otherwise the router is empty.
pub fn backup_router(
    config: AdminHttpConfig,
    owner: Arc<Mutex<WriterOwner>>,
    reads: ReadClient,
) -> Router {
    let Some(gate) = Gate::new(config, reads) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/installation/backup", post(create))
        .route("/installation/backup/archive", get(download));
    gate.guard(Router::new().nest("/api/v2", routes))
        .with_state(App {
            owner,
            slot: Arc::new(Slot {
                inner: Mutex::new(Inner::default()),
            }),
        })
}

fn export(owner: &Mutex<WriterOwner>) -> Result<BackupFile, BackupFailure> {
    let client = owner
        .lock()
        .map_err(|_| BackupFailure::Internal)?
        .installation()
        .map_err(|_| BackupFailure::Internal)?;
    client
        .export_backup(APP_VERSION)
        .map_err(BackupFailure::from)
}

fn described(file: &BackupFile) -> Result<InstallationBackup, BackupFailure> {
    Ok(InstallationBackup {
        name: archive_name(&file.path)?.to_owned(),
        created_at: file.metadata.created_at.clone(),
        app_version: file.metadata.app_version.clone(),
        size_bytes: file.size_bytes,
        sha256: file.sha256.clone(),
        warnings: file.warnings.iter().copied().map(Into::into).collect(),
    })
}

/// The archive leaf name. A name with a slash, a quote or `..` is refused so
/// the response cannot carry a path.
fn archive_name(path: &Path) -> Result<&str, BackupFailure> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(BackupFailure::Internal)?;
    let safe = !name.is_empty()
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    safe.then_some(name).ok_or(BackupFailure::Internal)
}

async fn create(
    State(app): State<App>,
    Extension(_presented): Extension<PresentedCredential>,
) -> Response {
    let flight = match app.slot.begin() {
        Ok(flight) => flight,
        Err(failure) => return refused(&failure),
    };
    let owner = Arc::clone(&app.owner);
    let created = tokio::task::spawn_blocking(move || {
        let result = (|| {
            let file = export(&owner)?;
            let described = described(&file)?;
            flight.slot.keep(file);
            Ok(described)
        })();
        drop(flight);
        result
    })
    .await;
    match created {
        Ok(Ok(body)) => json_ok(&body),
        Ok(Err(failure)) => refused(&failure),
        Err(_) => refused(&BackupFailure::Internal),
    }
}

async fn download(
    State(app): State<App>,
    Extension(_presented): Extension<PresentedCredential>,
) -> Response {
    let flight = match app.slot.begin() {
        Ok(flight) => flight,
        Err(failure) => return refused(&failure),
    };
    let Some(kept) = flight.kept.clone() else {
        return refused(&BackupFailure::NoneKept);
    };
    let name = match archive_name(&kept.path) {
        Ok(name) => name.to_owned(),
        Err(failure) => return refused(&failure),
    };
    let opened = tokio::fs::File::open(&kept.path).await;
    let file = match opened {
        Ok(file) => file,
        Err(_) => return refused(&BackupFailure::Internal),
    };
    archive_response(file, kept.size_bytes, &name, flight)
}

fn json_ok(body: &InstallationBackup) -> Response {
    let bytes = match serde_json::to_vec(body) {
        Ok(bytes) => bytes,
        Err(_) => return refused(&BackupFailure::Internal),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| refused(&BackupFailure::Internal))
}

fn archive_response(file: tokio::fs::File, length: u64, name: &str, flight: Flight) -> Response {
    let disposition = format!("attachment; filename=\"{name}\"");
    let Ok(disposition) = HeaderValue::from_str(&disposition) else {
        return refused(&BackupFailure::Internal);
    };
    let body = axum::body::Body::new(ArchiveBody {
        file,
        remaining: length,
        _flight: flight,
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(body)
        .unwrap_or_else(|_| refused(&BackupFailure::Internal))
}

struct ArchiveBody {
    file: tokio::fs::File,
    remaining: u64,
    _flight: Flight,
}

impl http_body::Body for ArchiveBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let body = self.get_mut();
        if body.remaining == 0 {
            return Poll::Ready(None);
        }
        let mut buf = [0u8; 64 * 1024];
        let want = usize::try_from(body.remaining)
            .unwrap_or(buf.len())
            .min(buf.len());
        let filled = {
            let mut read = tokio::io::ReadBuf::new(&mut buf[..want]);
            match Pin::new(&mut body.file).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(Ok(())) => read.filled().len(),
            }
        };
        if filled == 0 {
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Backup archive ended early",
            ))));
        }
        body.remaining -= filled as u64;
        Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::copy_from_slice(
            &buf[..filled],
        )))))
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> http_body::SizeHint {
        let mut hint = http_body::SizeHint::new();
        hint.set_exact(self.remaining);
        hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::Value;

    async fn document(failure: &BackupFailure) -> (u16, Value) {
        let response = refused(failure);
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn each_failure_has_one_problem_body() {
        let space = NoSpace {
            needed: 30,
            available: 4,
        };
        let (status, body) = document(&BackupFailure::NoSpace(space)).await;
        assert_eq!(status, 503);
        assert_eq!(
            body,
            json!({
                "type": "urn:print-partner:problem:overloaded",
                "title": "Service Overloaded",
                "status": 503,
                "detail": "Not enough free disk space",
                "needed": 30,
                "available": 4,
            })
        );
        let cases = [
            (
                BackupFailure::StorageBusy,
                503,
                "A restore is in progress",
                ProblemType::Overloaded,
            ),
            (
                BackupFailure::Busy,
                503,
                "A backup is already running",
                ProblemType::Overloaded,
            ),
            (
                BackupFailure::NoneKept,
                404,
                "No backup has been created",
                ProblemType::NotFound,
            ),
            (
                BackupFailure::Internal,
                500,
                "Internal Server Error",
                ProblemType::Internal,
            ),
        ];
        for (failure, status, detail, kind) in cases {
            let (answered, body) = document(&failure).await;
            assert_eq!(answered, status, "{detail}");
            assert_eq!(
                body,
                json!({
                    "type": serde_json::to_value(kind).unwrap(),
                    "title": kind.title(),
                    "status": status,
                    "detail": detail,
                })
            );
        }
        let leaked = BackupFailure::Writer(WriterFailure::Internal(anyhow::anyhow!(
            "select * from secrets where path = '/tmp/secret-path-canary'"
        )));
        let (status, body) = document(&leaked).await;
        assert_eq!(status, 500);
        let text = body.to_string();
        assert!(!text.contains("secret-path-canary"), "{text}");
        assert!(!text.contains("select"), "{text}");
        let stopped = document(&BackupFailure::Writer(WriterFailure::Stopped)).await;
        assert_eq!(stopped.0, 503);
        assert_eq!(stopped.1["detail"], "Service temporarily unavailable");
    }
}
