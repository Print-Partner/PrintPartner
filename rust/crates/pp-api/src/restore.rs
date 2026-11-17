//! `POST /api/v2/installation/restore/stage` and `POST /api/v2/installation/restore`
//! (GRE-1161). Mounted only when `PP_DESKTOP_REBUILD` is exactly `1` or `true`.
//!
//! Stage streams an uploaded backup archive into
//! [`pp_storage::restore::InstallationClient::begin_upload`] and stages it.
//! Restore activates a staged restore by its id once the request repeats
//! [`CONFIRMATION`]. The client sends no path. One request runs at a time.

use crate::admin::{AdminHttpConfig, Gate};
use crate::loopback::PresentedCredential;
use crate::problem::{self, FieldError, Problem, ProblemCode, ProblemType, ToProblem};
use axum::{
    Extension, Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    response::Response,
    routing::post,
};
use http_body::Body as _;
use pp_storage::{
    WriterFailure, WriterOwner,
    read_model::ReadClient,
    restore::{CONFIRMATION, InstallationClient, Refusal, RestoreError, Staged, Upload},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

/// Bytes handed to storage per blocking write.
const WRITE_CHUNK: usize = 1024 * 1024;
/// A restore request is two short strings.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// The stage route's body: what the archive holds and the text the restore
/// request must repeat. The generated client calls the route
/// `stageInstallationRestore`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema, serde::Deserialize))]
pub struct RestorePreview {
    /// 32 lowercase hex digits. Valid until Print Partner restarts or another
    /// backup is staged; after that, upload the backup again.
    pub restore_id: String,
    pub created_at: String,
    pub app_version: String,
    /// Schema version of the archived database.
    pub schema_version: u64,
    /// The text `RestoreRequest.confirmation` must repeat. It says a restore
    /// cannot reverse prints already sent or Spoolman usage already recorded.
    pub confirmation: String,
}

/// The restore route's request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema, serde::Serialize))]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    pub restore_id: String,
    pub confirmation: String,
}

/// The restore route's body. The generated client calls the route
/// `restoreInstallation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema, serde::Deserialize))]
pub struct RestoreOutcome {
    /// Schema version of the database as restored.
    pub restored_schema_version: u64,
    /// Schema version after the restored database was opened and migrated.
    pub schema_version: u64,
}

/// Why a stage or restore request was refused.
#[derive(Debug)]
enum RestoreFailure {
    LengthRequired,
    BodyUnreadable,
    /// The restore request is not a `RestoreRequest`.
    Invalid {
        readable: bool,
        errors: Vec<FieldError>,
    },
    /// This router is already staging or restoring.
    Busy,
    Refused(Refusal),
    RolledBack,
    RestartRequired,
    Writer(WriterFailure),
    Internal,
}

impl From<RestoreError> for RestoreFailure {
    fn from(error: RestoreError) -> Self {
        match error {
            RestoreError::Refused(refusal) => Self::Refused(refusal),
            RestoreError::RolledBack => Self::RolledBack,
            RestoreError::RestartRequired => Self::RestartRequired,
            RestoreError::Writer(failure) => Self::Writer(failure),
        }
    }
}

/// Staging reports a [`Refusal`] inside `anyhow`. Anything else is logged
/// here and answered without its text, which can hold paths.
fn staging_failure(error: anyhow::Error) -> RestoreFailure {
    match error.downcast::<Refusal>() {
        Ok(refusal) => RestoreFailure::Refused(refusal),
        Err(error) => {
            log::warn!("Restore staging failed: {error:#}");
            RestoreFailure::Internal
        }
    }
}

fn refusal_problem(refusal: &Refusal) -> Problem {
    let (kind, status, detail) = match refusal {
        Refusal::Unmarked => (
            ProblemType::Conflict,
            409,
            "Restore needs a Rust-owned installation",
        ),
        Refusal::TooLarge(_) => (
            ProblemType::MalformedRequest,
            413,
            "The upload is larger than its Content-Length",
        ),
        Refusal::NoSpace(_) | Refusal::InsufficientSpace(_) => {
            (ProblemType::Overloaded, 503, "Not enough free disk space")
        }
        Refusal::LargerThanDeclared(_) | Refusal::InvalidArchive(_) => {
            (ProblemType::Validation, 422, "Invalid backup archive")
        }
        Refusal::UnsupportedSchema(_) => (
            ProblemType::Validation,
            422,
            "This release cannot restore the backup's database",
        ),
        Refusal::Confirmation => (
            ProblemType::Validation,
            400,
            "Restore confirmation does not match",
        ),
        Refusal::UnknownRestore => {
            return Problem::new(
                ProblemType::NotFound,
                404,
                "No staged restore with that id; upload the backup again",
            )
            .with_problem_code(ProblemCode::RestoreNotFound);
        }
        Refusal::Busy => (
            ProblemType::Overloaded,
            503,
            "Storage work is still active; try again",
        ),
    };
    Problem::new(kind, status, detail)
}

impl ToProblem for RestoreFailure {
    fn to_problem(&self) -> Problem {
        match self {
            Self::LengthRequired => Problem::new(
                ProblemType::MalformedRequest,
                411,
                "Content-Length is required",
            ),
            Self::BodyUnreadable => Problem::new(
                ProblemType::MalformedRequest,
                400,
                "The upload body could not be read",
            ),
            Self::Invalid { readable, errors } => {
                let kind = if *readable {
                    ProblemType::Validation
                } else {
                    ProblemType::MalformedRequest
                };
                Problem::new(kind, 400, "Request is invalid").with_errors(errors.clone())
            }
            Self::Busy => {
                Problem::new(ProblemType::Overloaded, 503, "A restore is already running")
            }
            Self::Refused(refusal) => refusal_problem(refusal),
            Self::RolledBack => Problem::new(
                ProblemType::Internal,
                500,
                "Restore failed; the previous installation is kept",
            )
            .with_problem_code(ProblemCode::RestoreRolledBack),
            Self::RestartRequired => Problem::new(
                ProblemType::Unavailable,
                503,
                "Restore did not finish; restart Print Partner to recover",
            )
            .with_problem_code(ProblemCode::RestartRequired),
            Self::Writer(failure) => failure.to_problem(),
            Self::Internal => Problem::new(ProblemType::Internal, 500, "Internal Server Error"),
        }
    }
}

fn refused(failure: &RestoreFailure) -> Response {
    let space = match failure {
        RestoreFailure::Refused(Refusal::NoSpace(space)) => Some((space.needed, space.available)),
        RestoreFailure::Refused(Refusal::InsufficientSpace(space)) => {
            Some((space.needed, space.available))
        }
        _ => None,
    };
    let mut extensions = Map::new();
    if let Some((needed, available)) = space {
        extensions.insert("needed".into(), json!(needed));
        extensions.insert("available".into(), json!(available));
    }
    problem::build(failure.to_problem(), extensions)
}

#[derive(Clone)]
struct App {
    owner: Arc<Mutex<WriterOwner>>,
    /// One stage or restore at a time.
    operation: Arc<Semaphore>,
}

/// Mounts the restore routes under `/api/v2` when `config` was built with
/// `PP_DESKTOP_REBUILD` exactly `1` or `true`. Otherwise the router is empty.
pub fn restore_router(
    config: AdminHttpConfig,
    owner: Arc<Mutex<WriterOwner>>,
    reads: ReadClient,
) -> Router {
    let Some(gate) = Gate::new(config, reads) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/installation/restore/stage", post(stage))
        .route("/installation/restore", post(restore));
    gate.guard(Router::new().nest("/api/v2", routes))
        .with_state(App {
            owner,
            operation: Arc::new(Semaphore::new(1)),
        })
}

fn installation(owner: &Mutex<WriterOwner>) -> Result<InstallationClient, RestoreFailure> {
    owner
        .lock()
        .map_err(|_| RestoreFailure::Internal)?
        .installation()
        .map_err(|_| RestoreFailure::Writer(WriterFailure::Stopped))
}

/// Runs blocking storage work off the async workers.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, RestoreFailure> + Send + 'static,
) -> Result<T, RestoreFailure> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or(Err(RestoreFailure::Internal))
}

async fn stage(
    State(app): State<App>,
    Extension(_presented): Extension<PresentedCredential>,
    request: Request,
) -> Response {
    let Ok(_permit) = app.operation.try_acquire() else {
        return refused(&RestoreFailure::Busy);
    };
    match staged(&app, request).await {
        Ok(staged) => json_ok(&RestorePreview {
            restore_id: staged.restore_id,
            created_at: staged.metadata.created_at,
            app_version: staged.metadata.app_version,
            schema_version: staged.schema_version,
            confirmation: CONFIRMATION.into(),
        }),
        Err(failure) => refused(&failure),
    }
}

/// Writes the body to an upload of exactly its declared length, then
/// stages it. Dropping the upload on any failure removes its file.
async fn staged(app: &App, request: Request) -> Result<Staged, RestoreFailure> {
    let length = request.headers().get(header::CONTENT_LENGTH);
    let length = length
        .and_then(|value| value.to_str().ok()?.parse::<u64>().ok())
        .ok_or(RestoreFailure::LengthRequired)?;
    let owner = Arc::clone(&app.owner);
    let (client, mut upload) = blocking(move || {
        let client = installation(&owner)?;
        let upload = client.begin_upload(length).map_err(staging_failure)?;
        Ok((client, upload))
    })
    .await?;
    let mut body = request.into_body();
    let mut pending = Vec::new();
    loop {
        let done = match next_data(&mut body).await? {
            Some(data) => {
                pending.extend_from_slice(&data);
                false
            }
            None => true,
        };
        if done || pending.len() >= WRITE_CHUNK {
            let bytes = std::mem::take(&mut pending);
            upload = blocking(move || write(upload, &bytes)).await?;
        }
        if done {
            break;
        }
    }
    blocking(move || {
        let staged = client.stage(upload).map_err(staging_failure)?;
        if let Some(error) = &staged.cleanup_error {
            log::warn!("An earlier restore stage was not removed: {error}");
        }
        Ok(staged)
    })
    .await
}

/// The next data frame of `body`, `None` at its end. Trailers are skipped.
async fn next_data(body: &mut Body) -> Result<Option<axum::body::Bytes>, RestoreFailure> {
    loop {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await;
        match frame {
            None => return Ok(None),
            Some(Err(_)) => return Err(RestoreFailure::BodyUnreadable),
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

fn write(mut upload: Upload, bytes: &[u8]) -> Result<Upload, RestoreFailure> {
    upload.write(bytes).map_err(staging_failure)?;
    Ok(upload)
}

async fn restore(
    State(app): State<App>,
    Extension(_presented): Extension<PresentedCredential>,
    request: Request,
) -> Response {
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_REQUEST_BYTES).await else {
        return refused(&RestoreFailure::BodyUnreadable);
    };
    let body: RestoreRequest = match problem::parse_body(&bytes) {
        Ok(body) => body,
        Err(errors) => {
            let readable = problem::is_json_object(&bytes) || !errors.is_empty();
            return refused(&RestoreFailure::Invalid { readable, errors });
        }
    };
    let Ok(_permit) = app.operation.try_acquire() else {
        return refused(&RestoreFailure::Busy);
    };
    let owner = Arc::clone(&app.owner);
    let activated = blocking(move || {
        // The owner lock is released before the activation waits.
        let client = installation(&owner)?;
        Ok(client.activate(&body.restore_id, &body.confirmation)?)
    })
    .await;
    match activated {
        Ok(activated) => json_ok(&RestoreOutcome {
            restored_schema_version: activated.previous_schema_version,
            schema_version: activated.schema_version,
        }),
        Err(failure) => refused(&failure),
    }
}

fn json_ok(body: &impl Serialize) -> Response {
    let Ok(bytes) = serde_json::to_vec(body) else {
        return refused(&RestoreFailure::Internal);
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| refused(&RestoreFailure::Internal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use pp_storage::archive::NoSpace;
    use serde_json::Value;

    async fn document(failure: &RestoreFailure) -> (u16, Value) {
        let response = refused(failure);
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn each_failure_has_one_problem_body() {
        use ProblemType as T;
        let space = NoSpace {
            needed: 30,
            available: 4,
        };
        let (status, body) = document(&RestoreFailure::Refused(Refusal::NoSpace(space))).await;
        assert_eq!(status, 503);
        assert_eq!(body["detail"], "Not enough free disk space");
        assert_eq!(
            (body["needed"].clone(), body["available"].clone()),
            (json!(30), json!(4))
        );
        let path = "/tmp/secret-path-canary".to_owned();
        let cases = [
            (
                RestoreFailure::LengthRequired,
                411,
                T::MalformedRequest,
                "Content-Length is required",
                None,
            ),
            (
                RestoreFailure::BodyUnreadable,
                400,
                T::MalformedRequest,
                "The upload body could not be read",
                None,
            ),
            (
                RestoreFailure::Busy,
                503,
                T::Overloaded,
                "A restore is already running",
                None,
            ),
            (
                RestoreFailure::Refused(Refusal::Unmarked),
                409,
                T::Conflict,
                "Restore needs a Rust-owned installation",
                None,
            ),
            (
                RestoreFailure::Refused(Refusal::TooLarge(9)),
                413,
                T::MalformedRequest,
                "The upload is larger than its Content-Length",
                None,
            ),
            (
                RestoreFailure::Refused(Refusal::LargerThanDeclared(9)),
                422,
                T::Validation,
                "Invalid backup archive",
                None,
            ),
            (
                RestoreFailure::Refused(Refusal::InvalidArchive(path.clone())),
                422,
                T::Validation,
                "Invalid backup archive",
                None,
            ),
            (
                RestoreFailure::Refused(Refusal::UnsupportedSchema(path)),
                422,
                T::Validation,
                "This release cannot restore the backup's database",
                None,
            ),
            (
                RestoreFailure::Refused(Refusal::Confirmation),
                400,
                T::Validation,
                "Restore confirmation does not match",
                None,
            ),
            (
                RestoreFailure::Refused(Refusal::UnknownRestore),
                404,
                T::NotFound,
                "No staged restore with that id; upload the backup again",
                Some("restore_not_found"),
            ),
            (
                RestoreFailure::Refused(Refusal::Busy),
                503,
                T::Overloaded,
                "Storage work is still active; try again",
                None,
            ),
            (
                RestoreFailure::RolledBack,
                500,
                T::Internal,
                "Restore failed; the previous installation is kept",
                Some("restore_rolled_back"),
            ),
            (
                RestoreFailure::RestartRequired,
                503,
                T::Unavailable,
                "Restore did not finish; restart Print Partner to recover",
                Some("restart_required"),
            ),
            (
                RestoreFailure::Internal,
                500,
                T::Internal,
                "Internal Server Error",
                None,
            ),
        ];
        for (failure, status, kind, detail, code) in cases {
            let (answered, body) = document(&failure).await;
            assert_eq!(answered, status, "{detail}");
            let mut expected = json!({
                "type": serde_json::to_value(kind).unwrap(),
                "title": kind.title(),
                "status": status,
                "detail": detail,
            });
            if let Some(code) = code {
                expected["code"] = json!(code);
            }
            assert_eq!(body, expected);
        }
        let leaked = staging_failure(anyhow::anyhow!("open /tmp/secret-path-canary"));
        let (status, body) = document(&leaked).await;
        assert_eq!(status, 500);
        assert!(!body.to_string().contains("secret-path-canary"), "{body}");
        let refusal = staging_failure(anyhow::Error::from(Refusal::UnknownRestore));
        assert!(matches!(
            refusal,
            RestoreFailure::Refused(Refusal::UnknownRestore)
        ));
    }
}
