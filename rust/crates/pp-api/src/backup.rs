//! Whole-installation backup and confirmed restore for replacement Settings.
//!
//! The routes take no filesystem paths. A backup is written to a private
//! temporary directory and downloaded from one slot; a restore is uploaded,
//! validated and staged, then activated by repeating the confirmation text.
//! Storage does the work in `pp_storage::restore`.

use crate::accepted::{failure, read_error};
use crate::loopback::{
    LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
    require_api_key_tenant,
};
use anyhow::Result;
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body::Body as _;
use pp_storage::{
    read_model::ReadClient,
    restore::{BackupFile, CONFIRMATION, InstallationClient, Refusal, Upload},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::sync::{Mutex, Semaphore};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const WRITE_CHUNK: usize = 1024 * 1024;

#[derive(Clone)]
pub struct BackupHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    wait: Duration,
    max_upload_bytes: u64,
}

impl BackupHttpConfig {
    /// `flag` is the process `PP_DESKTOP_REBUILD` value captured at startup.
    pub fn from_startup(origin: &str, api_key_tenant: &str, flag: Option<&str>) -> Result<Self> {
        let listener = LoopbackOrigin::parse(origin)?;
        require_api_key_tenant(api_key_tenant)?;
        Ok(Self {
            origin: listener.origin,
            host: listener.host,
            api_key_tenant: api_key_tenant.into(),
            enabled: rebuild_flag_enabled(flag),
            wait: Duration::from_millis(100),
            // The restore validator's decompressed limit; a larger upload
            // cannot pass it.
            max_upload_bytes: 20 * 1024 * 1024 * 1024,
        })
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct InstallationBackup {
    pub file_name: String,
    pub created_at: String,
    pub app_version: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct RestorePreview {
    pub restore_id: String,
    pub created_at: String,
    pub app_version: String,
    pub schema_version: u64,
    /// The text `RestoreRequest.confirmation` must repeat. It states that a
    /// restore cannot reverse printer or Spoolman effects.
    pub confirmation: String,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    pub restore_id: String,
    pub confirmation: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct RestoreOutcome {
    /// Schema version of the restored database before it was opened.
    pub restored_schema_version: u64,
    /// Schema version after opening and migrating it.
    pub schema_version: u64,
}

#[derive(Clone)]
struct App {
    config: BackupHttpConfig,
    reads: ReadClient,
    installation: InstallationClient,
    /// One backup or restore operation at a time.
    operation: Arc<Semaphore>,
    backup: Arc<Mutex<Option<BackupFile>>>,
}

/// Mounts the backup and restore routes under `/api/v2/installation` when
/// `PP_DESKTOP_REBUILD` was exactly `1` or `true`; otherwise nothing. Every
/// route needs a session or API key.
pub fn backup_router(
    config: BackupHttpConfig,
    reads: ReadClient,
    installation: InstallationClient,
) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let app = App {
        config,
        reads,
        installation,
        operation: Arc::new(Semaphore::new(1)),
        backup: Arc::new(Mutex::new(None)),
    };
    Router::new()
        .route("/api/v2/installation/backup", post(create_backup))
        .route("/api/v2/installation/backup/archive", get(download_backup))
        .route("/api/v2/installation/restore/stage", post(stage_restore))
        .route("/api/v2/installation/restore", post(activate_restore))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    if let Err((status, detail)) = admit(
        &app.config.origin,
        &app.config.host,
        &app.config.api_key_tenant,
        &request,
    ) {
        return failure(status, detail);
    }
    let Some(presented) = principal(request.headers(), &app.config.api_key_tenant) else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    if let Some(refused) = authenticate(&app, presented).await {
        return refused;
    }
    next.run(request).await
}

/// An empty read batch only resolves the credential.
async fn authenticate(app: &App, presented: PresentedCredential) -> Option<Response> {
    let reads = app.reads.clone();
    let wait = app.config.wait;
    let proof = tokio::task::spawn_blocking(move || {
        reads.read(
            presented.into_credential(),
            &[],
            &AtomicBool::new(false),
            wait,
        )
    })
    .await;
    match proof {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(read_error(&error)),
        Err(_) => Some(internal()),
    }
}

fn internal() -> Response {
    failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
}

fn busy() -> Response {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "Another backup or restore is running",
    )
}

fn storage_error(error: &anyhow::Error) -> Response {
    let Some(refusal) = error.downcast_ref::<Refusal>() else {
        return read_error(error);
    };
    let status = match refusal {
        Refusal::Unmarked => StatusCode::CONFLICT,
        Refusal::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        Refusal::InvalidArchive(_) | Refusal::UnsupportedSchema(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        Refusal::Confirmation => StatusCode::BAD_REQUEST,
        Refusal::UnknownRestore => StatusCode::NOT_FOUND,
        Refusal::Busy => StatusCode::SERVICE_UNAVAILABLE,
    };
    failure(status, &refusal.to_string())
}

/// Runs blocking storage work off the async workers.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| anyhow::anyhow!("Storage task failed"))?
}

async fn create_backup(State(app): State<App>) -> Response {
    let Ok(_permit) = app.operation.try_acquire() else {
        return busy();
    };
    let installation = app.installation.clone();
    let backup = match blocking(move || installation.backup_to_temporary(APP_VERSION)).await {
        Ok(backup) => backup,
        Err(error) => return storage_error(&error),
    };
    let body = InstallationBackup {
        file_name: backup
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        created_at: backup.metadata.created_at.clone(),
        app_version: backup.metadata.app_version.clone(),
        size_bytes: backup.size_bytes,
        sha256: backup.sha256.clone(),
    };
    // Replacing the slot removes the previous archive.
    *app.backup.lock().await = Some(backup);
    no_store(Json(body).into_response())
}

async fn download_backup(State(app): State<App>) -> Response {
    let slot = app.backup.lock().await;
    let Some(backup) = slot.as_ref() else {
        return failure(StatusCode::NOT_FOUND, "No backup has been created");
    };
    let name = backup
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // An open file keeps streaming if a newer backup replaces the slot.
    let Ok(file) = tokio::fs::File::open(&backup.path).await else {
        return internal();
    };
    let size = backup.size_bytes;
    drop(slot);
    no_store(
        Response::builder()
            .header(header::CONTENT_TYPE, "application/gzip")
            .header(header::CONTENT_LENGTH, size)
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            )
            .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))
            .unwrap_or_else(|_| internal()),
    )
}

async fn stage_restore(State(app): State<App>, request: Request) -> Response {
    let Ok(_permit) = app.operation.try_acquire() else {
        return busy();
    };
    let limit = app.config.max_upload_bytes;
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok()?.parse::<u64>().ok());
    if declared.is_some_and(|length| length > limit) {
        return storage_error(&Refusal::TooLarge(limit).into());
    }
    let installation = app.installation.clone();
    let mut upload = match blocking(move || installation.begin_upload(limit)).await {
        Ok(upload) => upload,
        Err(error) => return storage_error(&error),
    };
    let mut body = request.into_body();
    let mut pending = Vec::new();
    loop {
        let frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await;
        let done = match frame {
            None => true,
            Some(Err(_)) => return failure(StatusCode::BAD_REQUEST, "Upload interrupted"),
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    pending.extend_from_slice(&data);
                }
                false
            }
        };
        if done || pending.len() >= WRITE_CHUNK {
            let bytes = std::mem::take(&mut pending);
            upload = match blocking(move || write(upload, bytes)).await {
                Ok(upload) => upload,
                Err(error) => return storage_error(&error),
            };
        }
        if done {
            break;
        }
    }
    let installation = app.installation.clone();
    match blocking(move || installation.stage(upload)).await {
        Ok(staged) => no_store(
            Json(RestorePreview {
                restore_id: staged.restore_id,
                created_at: staged.metadata.created_at,
                app_version: staged.metadata.app_version,
                schema_version: staged.schema_version,
                confirmation: CONFIRMATION.into(),
            })
            .into_response(),
        ),
        Err(error) => storage_error(&error),
    }
}

fn write(mut upload: Upload, bytes: Vec<u8>) -> Result<Upload> {
    upload.write(&bytes)?;
    Ok(upload)
}

async fn activate_restore(State(app): State<App>, request: Request) -> Response {
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_REQUEST_BYTES).await else {
        return failure(StatusCode::BAD_REQUEST, "Invalid restore request");
    };
    let Ok(body) = serde_json::from_slice::<RestoreRequest>(&bytes) else {
        return failure(StatusCode::BAD_REQUEST, "Invalid restore request");
    };
    let Ok(_permit) = app.operation.try_acquire() else {
        return busy();
    };
    let installation = app.installation.clone();
    match blocking(move || installation.activate(&body.restore_id, &body.confirmation)).await {
        Ok(activated) => no_store(
            Json(RestoreOutcome {
                restored_schema_version: activated.previous_schema_version,
                schema_version: activated.schema_version,
            })
            .into_response(),
        ),
        Err(error) => storage_error(&error),
    }
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    response
}
