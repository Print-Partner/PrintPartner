//! `POST /sources/{id}/upload-files` (GRE-428): loose files become a new
//! revision of an existing Source. Each multipart file part's file name is
//! its path inside the Source. The body streams to disk through
//! [`SourceUploads`]; the route answers `202 {job_id}` once the files are
//! stored and the import is recorded, and the import job reports the rest.

use super::{App, Principal, credentials, dispatch, guard, reject, source_id};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State},
    http::{StatusCode, header::CONTENT_LENGTH},
    middleware,
    response::{IntoResponse, Response},
    routing::post,
};
use pp_storage::{
    WriterFailure, WriterOwner,
    catalog::{Credentials, Outcome, Request as CatalogRequest},
};
use std::{
    io::{self, Read},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;

/// One file part as it streams in: its path inside the Source and its bytes.
pub type UploadPart = (String, Box<dyn Read + Send>);

/// The file parts of one upload, in order. An `Err` item means the body
/// broke, and the upload must not be admitted.
pub type UploadParts = Box<dyn Iterator<Item = io::Result<UploadPart>> + Send>;

/// Why an upload was refused or failed. Every refusal leaves no job and the
/// Source unchanged.
#[derive(Debug)]
pub enum UploadFailure {
    NoFiles,
    /// A file was refused; the text names it in words.
    InvalidFile(String),
    TooLarge,
    /// The upload would leave less free space than the reserve.
    InsufficientSpace,
    /// Every import slot is in use.
    Busy,
    /// Imports are not open: startup recovery has not finished, or the
    /// server is stopping.
    Unavailable,
    /// Storage refused or failed the import's admission. The route answers
    /// it as every storage family does, through
    /// [`crate::problem::writer_failure`]: a refused credential is 401 or 403,
    /// a busy or stopped writer is a retryable 503, and a storage fault is 500.
    Storage(WriterFailure),
    Failed,
}

/// What the upload route needs from the Source import service.
pub trait SourceUploads: Send + Sync + 'static {
    /// Refuses an upload of `bytes` that would leave less free space on the
    /// data volume than the reserve.
    fn check_space(&self, bytes: u64) -> Result<(), UploadFailure>;

    /// Whether imports are closing for shutdown. A streaming upload checks it
    /// while it waits for the client, so shutdown never waits on a slow body.
    fn closing(&self) -> bool;

    /// Stores `files` as a new revision of `source_id` and returns the import
    /// job's ID once the import is recorded. Blocks the calling thread.
    fn upload(
        &self,
        credential: Credentials,
        source_id: i64,
        files: UploadParts,
    ) -> Result<String, UploadFailure>;
}

/// Mounts `POST /sources/{id}/upload-files` at `/` and `/api/v1` behind the
/// catalog's guard when `PP_DESKTOP_REBUILD` was on at startup. The body has
/// no fixed limit; [`SourceUploads::check_space`] bounds it.
pub fn admit_upload_routes(
    config: super::CatalogHttpConfig,
    owner: Arc<Mutex<WriterOwner>>,
    uploads: Arc<dyn SourceUploads>,
) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let routes = Router::new().route("/sources/{id}/upload-files", post(upload_files));
    let app = App { config, owner };
    Router::new()
        .merge(routes.clone())
        .nest("/api/v1", routes)
        .layer(Extension(uploads))
        .layer(DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .route_layer(middleware::map_response(crate::problem::layer))
        .with_state(app)
}

async fn upload_files(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Extension(uploads): Extension<Arc<dyn SourceUploads>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let Some(id) = source_id(&id) else {
        return reject(StatusCode::NOT_FOUND, "Source not found");
    };
    // The Source and the credential are checked before any byte is read.
    match dispatch(
        &app,
        credentials(principal.clone()),
        CatalogRequest::Get { id },
    )
    .await
    {
        Ok(Outcome::Source(Some(_))) => {}
        Ok(_) => return reject(StatusCode::NOT_FOUND, "Source not found"),
        Err(response) => return *response,
    }
    let length = request.headers().get(CONTENT_LENGTH);
    let Some(length) = length.and_then(|value| value.to_str().ok()?.parse().ok()) else {
        return reject(StatusCode::LENGTH_REQUIRED, "Content-Length is required");
    };
    if let Err(failure) = uploads.check_space(length) {
        return refused(failure);
    }
    let Ok(multipart) = Multipart::from_request(request, &()).await else {
        return reject(
            StatusCode::BAD_REQUEST,
            "Expected a multipart/form-data body",
        );
    };
    let (parts, received) = mpsc::channel(1);
    let credential = credentials(principal);
    let service = Arc::clone(&uploads);
    let stored = tokio::task::spawn_blocking(move || {
        let parts = Parts(received, Arc::clone(&service));
        service.upload(credential, id, Box::new(parts))
    });
    // Answer as soon as the blocking side refuses, even while a stalled
    // client is still mid-body, so a slow client cannot hold the drain.
    let pumping = pump(multipart, parts, &uploads);
    tokio::pin!(pumping);
    let mut stored = stored;
    let answered = tokio::select! {
        streamed = &mut pumping => (streamed, stored.await),
        first = &mut stored => match first {
            // Success needs the whole body, so the pump has finished too.
            Ok(Ok(_)) => (pumping.await, first),
            refused => (Ok(()), refused),
        },
    };
    match answered {
        (Err(()), _) => reject(StatusCode::BAD_REQUEST, "The upload body could not be read"),
        (Ok(()), Ok(Ok(job_id))) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "job_id": job_id })),
        )
            .into_response(),
        (Ok(()), Ok(Err(failure))) => refused(failure),
        (Ok(()), Err(_)) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Upload failed"),
    }
}

fn refused(failure: UploadFailure) -> Response {
    let (status, detail) = match failure {
        UploadFailure::InvalidFile(detail) => return reject(StatusCode::BAD_REQUEST, detail),
        UploadFailure::NoFiles => (StatusCode::BAD_REQUEST, "The upload has no file part"),
        UploadFailure::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "The upload is too large"),
        UploadFailure::InsufficientSpace => (
            StatusCode::INSUFFICIENT_STORAGE,
            "Not enough free disk space",
        ),
        UploadFailure::Busy => (
            StatusCode::CONFLICT,
            "Another import is running; try again later",
        ),
        UploadFailure::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "Imports are closed"),
        UploadFailure::Storage(failure) => return crate::problem::writer_failure(&failure),
        UploadFailure::Failed => (StatusCode::INTERNAL_SERVER_ERROR, "Upload failed"),
    };
    reject(status, detail)
}

/// A part on its way to the blocking side; `None` marks the clean end.
type Sent<T> = io::Result<Option<T>>;

/// Feeds the file parts of `multipart` to the blocking side, one at a time.
/// Each stream ends with an explicit `None`, so a dropped sender (a client
/// that went away, a stopping server) reads as a broken body, never as a
/// shorter one. Returns `Err` when the body itself could not be read; a
/// receiver that stopped early is the blocking side's refusal, not an error
/// here.
async fn pump(
    mut multipart: Multipart,
    parts: mpsc::Sender<Sent<UploadPart>>,
    uploads: &Arc<dyn SourceUploads>,
) -> Result<(), ()> {
    let broken = || io::Error::new(io::ErrorKind::InvalidData, "Upload body unreadable");
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => {
                let _ = parts.send(Ok(None)).await;
                return Ok(());
            }
            Err(_) => {
                let _ = parts.send(Err(broken())).await;
                return Err(());
            }
        };
        // Form fields that are not files carry no Source content.
        let Some(name) = field.file_name().map(str::to_owned) else {
            continue;
        };
        let (chunks, received) = mpsc::channel(4);
        let reader = PartReader {
            chunks: received,
            current: Bytes::new(),
            done: false,
            uploads: Arc::clone(uploads),
        };
        if parts
            .send(Ok(Some((name, Box::new(reader)))))
            .await
            .is_err()
        {
            return Ok(());
        }
        loop {
            let chunk = match field.chunk().await {
                Ok(chunk) => chunk,
                Err(_) => {
                    let _ = chunks.send(Err(broken())).await;
                    return Err(());
                }
            };
            let end = chunk.is_none();
            if chunks.send(Ok(chunk)).await.is_err() {
                return Ok(());
            }
            if end {
                break;
            }
        }
    }
}

/// Waits on the blocking side for the next message from the pump, waking
/// every 250 ms so a slow client cannot hold up shutdown. `Err` once imports
/// are closing; `None` once the pump has gone.
fn next_sent<T>(
    receiver: &mut mpsc::Receiver<T>,
    uploads: &Arc<dyn SourceUploads>,
) -> io::Result<Option<T>> {
    let runtime = tokio::runtime::Handle::current();
    loop {
        if uploads.closing() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Imports are closing",
            ));
        }
        let waited = tokio::time::timeout(Duration::from_millis(250), receiver.recv());
        if let Ok(message) = runtime.block_on(waited) {
            return Ok(message);
        }
    }
}

struct Parts(mpsc::Receiver<Sent<UploadPart>>, Arc<dyn SourceUploads>);

impl Iterator for Parts {
    type Item = io::Result<UploadPart>;

    fn next(&mut self) -> Option<Self::Item> {
        match next_sent(&mut self.0, &self.1) {
            Ok(Some(Ok(Some(part)))) => Some(Ok(part)),
            Ok(Some(Ok(None))) => None,
            Ok(Some(Err(error))) | Err(error) => Some(Err(error)),
            Ok(None) => Some(Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Upload ended early",
            ))),
        }
    }
}

struct PartReader {
    chunks: mpsc::Receiver<Sent<Bytes>>,
    current: Bytes,
    done: bool,
    uploads: Arc<dyn SourceUploads>,
}

impl Read for PartReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.current.is_empty() {
            if self.done {
                return Ok(0);
            }
            match next_sent(&mut self.chunks, &self.uploads)? {
                Some(Ok(Some(bytes))) => self.current = bytes,
                Some(Ok(None)) => self.done = true,
                Some(Err(error)) => return Err(error),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Upload ended early",
                    ));
                }
            }
        }
        let read = buffer.len().min(self.current.len());
        buffer[..read].copy_from_slice(&self.current.split_to(read));
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::{UploadFailure, refused};
    use axum::http::StatusCode;
    use pp_storage::{WriterFailure, auth::AuthFailure};

    fn status(failure: UploadFailure) -> StatusCode {
        refused(failure).status()
    }

    #[test]
    fn a_storage_fault_is_a_server_error() {
        let fault = WriterFailure::Internal(anyhow::anyhow!("disk I/O error"));
        assert_eq!(
            status(UploadFailure::Storage(fault)),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn a_backed_up_or_stopped_writer_is_retryable() {
        for failure in [
            WriterFailure::Busy,
            WriterFailure::QueueFull,
            WriterFailure::Stopped,
        ] {
            assert_eq!(
                status(UploadFailure::Storage(failure)),
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
    }

    #[test]
    fn a_refused_credential_answers_like_the_auth_seam() {
        let refused = WriterFailure::Auth(AuthFailure::SessionRequired);
        assert_eq!(
            status(UploadFailure::Storage(refused)),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn only_a_busy_import_slot_is_a_conflict() {
        assert_eq!(status(UploadFailure::Busy), StatusCode::CONFLICT);
        assert_eq!(
            status(UploadFailure::Failed),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
