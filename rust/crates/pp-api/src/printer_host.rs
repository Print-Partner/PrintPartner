//! Printer-host reads for the Rust-only runtime (GRE-360), at the paths Node's
//! `routes/printers.ts` and `routes/integrations.ts` serve:
//!
//! - `GET /printers/{id}/capabilities`, `/printers/{id}/files?path=` and
//!   `/printers/{id}/cameras`, at the root and under `/api/v1`, where Node's
//!   core routes are registered
//! - `GET /printers/{id}/files/content?path=` and `/printers/{id}/cameras/view?id=`
//!   (GRE-1174), which stream the printer's reply through at the same mounts
//! - `POST /api/v1/integrations/{id}/test`, under `/api/v1` only, as in Node
//! - `GET /api/v1/integrations/{id}/tls-pin` and
//!   `POST /api/v1/integrations/{id}/tls-pin/reset` (GRE-1245, Rust-only): the
//!   certificate pin a Bambu Test connection took, and its deliberate reset.
//!   Neither contacts the printer.
//!
//! Every route is read-only on the printer: it lists, describes, probes or
//! streams a file or camera image, and never uploads, starts, deletes or
//! touches Checkoff. The drivers'
//! request counts and the PrusaLink 401 resend are described in
//! docs/printer-host-reads.md. `pp-core` implements [`PrinterHostPort`] over
//! storage's integration settings and the gateway's Moonraker and PrusaLink
//! drivers. Admission and
//! the credential proof are the admin routes' (`AdminHttpConfig`).

use crate::accepted::failure;
use crate::admin::{AdminHttpConfig, Gate};
use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pp_storage::read_model::ReadClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Which host surfaces this runtime can serve for a printer, as Node's
/// `IntegrationCapabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub files: bool,
    pub cameras: bool,
    pub status: bool,
}

impl Capabilities {
    pub const NONE: Self = Self {
        files: false,
        cameras: false,
        status: false,
    };
}

/// One port answer. The route maps each case to Node's status and detail.
#[derive(Debug, PartialEq)]
pub enum HostAnswer<T> {
    Ok(T),
    /// The printer (or, for the connection test, the integration) is not stored.
    NotFound,
    /// The printer has no linked host.
    NotLinked,
    /// The linked host is missing, disabled or of an unknown type.
    Unavailable,
    /// The host type has no such capability in this runtime.
    Unsupported,
    /// The printer request failed; the text is the public detail.
    Failed(String),
    /// Storage could not answer in time.
    Busy,
    /// The tenant has the integration's feature turned off.
    FeatureDisabled(&'static str),
    /// The stored feature choices cannot be read.
    FeatureInvalid,
}

pub type Answer<T> = Pin<Box<dyn Future<Output = HostAnswer<T>> + Send>>;

/// A printer reply head whose body streams through as the client reads it.
pub struct HostStream {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub body: Body,
}

pub trait PrinterHostPort: Send + Sync + 'static {
    fn capabilities(&self, printer: String) -> Answer<Capabilities>;
    /// `path` is already a safe storage path, or empty for the root.
    fn files(&self, printer: String, path: String) -> Answer<Value>;
    fn cameras(&self, printer: String) -> Answer<Value>;
    /// The printer's reply for a print file at `path`, a safe storage path;
    /// `Ok(None)` when the host does not list that file.
    fn file(&self, printer: String, path: String) -> Answer<Option<HostStream>>;
    /// The printer's reply for one camera's view; `Ok(None)` when the host
    /// lists no camera with that id.
    fn camera_view(&self, printer: String, camera: String) -> Answer<Option<HostStream>>;
    /// `Ok` holds Node's `{ok, message}` test result.
    fn test(&self, integration: String) -> Answer<Value>;
    /// `Ok` holds `{pinned, sha256_prefix, pinned_at}` for the integration.
    fn tls_pin(&self, integration: String) -> Answer<Value>;
    /// Deletes the integration's pin; `Ok` holds `{reset}`, whether one was
    /// stored.
    fn reset_tls_pin(&self, integration: String) -> Answer<Value>;
}

/// Node's per-route `rateLimit` budgets, one window per route. Node keys them
/// per client IP; the Rust-only runtime only admits loopback clients.
const CAPABILITIES_LIMIT: u32 = 120;
const FILES_LIMIT: u32 = 60;
const CAMERAS_LIMIT: u32 = 60;
const FILE_CONTENT_LIMIT: u32 = 20;
const CAMERA_VIEW_LIMIT: u32 = 20;
const TEST_LIMIT: u32 = 5;
const PIN_LIMIT: u32 = 60;
const PIN_RESET_LIMIT: u32 = 5;
const WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Windows(Mutex<HashMap<&'static str, (Instant, u32)>>);

impl Windows {
    /// `None` when admitted, else the seconds until the window resets.
    fn consume(&self, route: &'static str, limit: u32) -> Option<u64> {
        let mut windows = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let window = windows.entry(route).or_insert((now, 0));
        if now.duration_since(window.0) >= WINDOW {
            *window = (now, 0);
        }
        if window.1 >= limit {
            let left = WINDOW.saturating_sub(now.duration_since(window.0));
            return Some(left.as_secs().max(1));
        }
        window.1 += 1;
        None
    }
}

#[derive(Clone)]
struct App {
    port: Arc<dyn PrinterHostPort>,
    windows: Arc<Windows>,
}

/// Mounts the printer-host reads when `PP_DESKTOP_REBUILD` was exactly `1`
/// or `true`; otherwise nothing.
pub fn printer_host_router(
    config: AdminHttpConfig,
    client: ReadClient,
    port: Arc<dyn PrinterHostPort>,
) -> Router {
    let Some(gate) = Gate::new(config, client) else {
        return Router::new();
    };
    gate.guard(routes(port))
}

/// Node registers the core routes at the root and again in the `/api/v1`
/// plugin, and each registration gets its own rate-limit store, so each
/// mount keeps its own windows.
fn routes(port: Arc<dyn PrinterHostPort>) -> Router {
    let printers = Router::new()
        .route("/printers/{id}/capabilities", get(capabilities))
        .route("/printers/{id}/files", get(files))
        .route("/printers/{id}/files/content", get(file_content))
        .route("/printers/{id}/cameras", get(cameras))
        .route("/printers/{id}/cameras/view", get(camera_view));
    let mount = |routes: Router<App>, port| {
        routes.with_state(App {
            port,
            windows: Arc::default(),
        })
    };
    let v1 = printers
        .clone()
        .route("/integrations/{id}/test", post(test))
        .route("/integrations/{id}/tls-pin", get(tls_pin))
        .route("/integrations/{id}/tls-pin/reset", post(reset_tls_pin));
    Router::new()
        .merge(mount(printers, port.clone()))
        .nest("/api/v1", mount(v1, port))
}

fn throttled(seconds: u64) -> Response {
    let after = if seconds == 60 {
        "1 minute".to_owned()
    } else if seconds == 1 {
        "1 second".to_owned()
    } else {
        format!("{seconds} seconds")
    };
    let mut response = failure(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("Rate limit exceeded, retry in {after}"),
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    response
}

/// Node's `publicCapabilityError` and the shared upstream-failure answer.
fn refusal<T>(answer: HostAnswer<T>, unsupported: impl FnOnce() -> Response) -> Response {
    match answer {
        HostAnswer::Ok(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        HostAnswer::NotFound => failure(StatusCode::NOT_FOUND, "Printer not found"),
        HostAnswer::NotLinked => failure(StatusCode::CONFLICT, "Printer is not linked to a host"),
        HostAnswer::Unavailable => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Printer host is unavailable",
        ),
        HostAnswer::Unsupported => unsupported(),
        HostAnswer::Failed(detail) => failure(StatusCode::BAD_GATEWAY, &detail),
        HostAnswer::Busy => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Printer settings are unavailable",
        ),
        HostAnswer::FeatureDisabled(feature) => crate::problem::coded(
            StatusCode::FORBIDDEN,
            &format!("Feature {feature} is disabled."),
            "FEATURE_DISABLED",
        ),
        HostAnswer::FeatureInvalid => crate::problem::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored feature toggles are invalid.",
            "FEATURE_TOGGLES_INVALID",
        ),
    }
}

async fn capabilities(State(app): State<App>, Path(id): Path<String>) -> Response {
    if let Some(after) = app.windows.consume("capabilities", CAPABILITIES_LIMIT) {
        return throttled(after);
    }
    match app.port.capabilities(id).await {
        HostAnswer::Ok(capabilities) => axum::Json(capabilities).into_response(),
        // A printer with no reachable host has no capabilities rather than an
        // error, so the client can ask about every printer unconditionally.
        HostAnswer::NotLinked | HostAnswer::Unavailable | HostAnswer::Unsupported => {
            axum::Json(Capabilities::NONE).into_response()
        }
        other => refusal(other, || axum::Json(Capabilities::NONE).into_response()),
    }
}

#[derive(Deserialize)]
struct PathQuery {
    path: Option<String>,
}

/// Node's `safeStoragePath(raw, { trimTrailing: true })`.
pub fn safe_storage_path(raw: &str) -> Option<String> {
    if raw.contains(['\\', '\0']) {
        return None;
    }
    let normalized = raw.trim_matches('/');
    let bad = |segment: &str| segment.is_empty() || segment == "." || segment == "..";
    (!normalized.is_empty() && !normalized.split('/').any(bad)).then(|| normalized.to_owned())
}

async fn files(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Response {
    if let Some(after) = app.windows.consume("files", FILES_LIMIT) {
        return throttled(after);
    }
    let requested = query.path.unwrap_or_default();
    let requested = requested.trim();
    // No path, or an empty one, means the host's storage root.
    let path = if requested.is_empty() {
        String::new()
    } else {
        match safe_storage_path(requested) {
            Some(path) => path,
            None => {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "path is not a valid printer storage path",
                );
            }
        }
    };
    match app.port.files(id, path).await {
        HostAnswer::Ok(listing) => axum::Json(listing).into_response(),
        other => refusal(other, || {
            failure(
                StatusCode::NOT_IMPLEMENTED,
                "Printer host does not support file browsing",
            )
        }),
    }
}

async fn cameras(State(app): State<App>, Path(id): Path<String>) -> Response {
    if let Some(after) = app.windows.consume("cameras", CAMERAS_LIMIT) {
        return throttled(after);
    }
    match app.port.cameras(id).await {
        HostAnswer::Ok(cameras) => axum::Json(json!({ "cameras": cameras })).into_response(),
        // Camera controls appear only for capabilities the host can serve, so
        // a host without cameras reports none rather than failing.
        other => refusal(other, || {
            axum::Json(json!({ "cameras": [] })).into_response()
        }),
    }
}

/// What a proxied reply is served as.
#[derive(Clone, Copy)]
enum Served {
    /// A print file: a download, never a page.
    File,
    /// A camera view for an `<img>`: only image and MJPEG types pass.
    Camera,
}

/// Camera types a browser renders as an image without running script.
const CAMERA_TYPES: [&str; 5] = [
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
    "multipart/x-mixed-replace",
];

/// Node's `sendUpstreamResponse`: a 2xx reply streams through with its
/// type and length and `private, no-store`; anything else is refused and its
/// body is never read. Unlike Node, the reply comes from Print Partner's own
/// origin with `nosniff` and a sandboxing CSP, a print file as an
/// attachment, and a camera type outside `CAMERA_TYPES` as
/// `application/octet-stream`, so printer-controlled HTML or SVG never runs
/// as a page of the app.
fn proxied(stream: HostStream, served: Served) -> Response {
    match stream.status {
        200..=299 => {}
        404 => return failure(StatusCode::NOT_FOUND, "Printer resource not found"),
        status => {
            return failure(
                StatusCode::BAD_GATEWAY,
                &format!("Printer host returned HTTP {status}"),
            );
        }
    }
    let mut response = Response::new(stream.body);
    let headers = response.headers_mut();
    let content_type = stream.content_type.filter(|value| match served {
        Served::File => true,
        Served::Camera => {
            let essence = value.split(';').next().unwrap_or_default().trim();
            CAMERA_TYPES
                .iter()
                .any(|allowed| essence.eq_ignore_ascii_case(allowed))
        }
    });
    let content_type = match (content_type, served) {
        (Some(value), _) => HeaderValue::from_str(&value).ok(),
        (None, Served::Camera) => Some(HeaderValue::from_static("application/octet-stream")),
        (None, Served::File) => None,
    };
    if let Some(value) = content_type {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    if let Served::File = served {
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
    if let Some(length) = stream.content_length {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

#[derive(Deserialize)]
struct ContentQuery {
    path: Option<String>,
    profile_id: Option<String>,
}

async fn file_content(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<ContentQuery>,
) -> Response {
    if let Some(after) = app.windows.consume("files_content", FILE_CONTENT_LIMIT) {
        return throttled(after);
    }
    // Node captures a Build's inspection snapshot here; its only redeemer,
    // the print-file assignment route, is not served by this runtime yet.
    if query.profile_id.is_some() {
        return failure(
            StatusCode::NOT_IMPLEMENTED,
            "Print-file inspection for a Build is not available in this runtime yet",
        );
    }
    let requested = query.path.unwrap_or_default();
    let requested = requested.trim();
    if requested.is_empty() {
        return failure(StatusCode::BAD_REQUEST, "path is required");
    }
    let Some(path) = safe_storage_path(requested) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "path is not a valid printer storage path",
        );
    };
    match app.port.file(id, path).await {
        HostAnswer::Ok(Some(stream)) => proxied(stream, Served::File),
        HostAnswer::Ok(None) => failure(StatusCode::NOT_FOUND, "Printer file not found"),
        other => refusal(other, || {
            failure(
                StatusCode::NOT_IMPLEMENTED,
                "Printer host does not support file browsing",
            )
        }),
    }
}

#[derive(Deserialize)]
struct CameraQuery {
    id: Option<String>,
}

async fn camera_view(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<CameraQuery>,
) -> Response {
    if let Some(after) = app.windows.consume("cameras_view", CAMERA_VIEW_LIMIT) {
        return throttled(after);
    }
    let camera = query.id.unwrap_or_default();
    let camera = camera.trim();
    if camera.is_empty() {
        return failure(StatusCode::BAD_REQUEST, "id is required");
    }
    let missing = || failure(StatusCode::NOT_FOUND, "Printer camera not found");
    match app.port.camera_view(id, camera.to_owned()).await {
        HostAnswer::Ok(Some(stream)) => proxied(stream, Served::Camera),
        HostAnswer::Ok(None) => missing(),
        other => refusal(other, missing),
    }
}

async fn test(State(app): State<App>, Path(id): Path<String>) -> Response {
    if let Some(after) = app.windows.consume("test", TEST_LIMIT) {
        return throttled(after);
    }
    integration_answer(app.port.test(id).await)
}

/// An integration's answer, or Node's 404 for an unknown integration.
fn integration_answer(answer: HostAnswer<Value>) -> Response {
    match answer {
        HostAnswer::Ok(result) => axum::Json(result).into_response(),
        HostAnswer::NotFound => failure(StatusCode::NOT_FOUND, "Integration not found"),
        other => refusal(other, || {
            failure(
                StatusCode::NOT_IMPLEMENTED,
                "Connection test is not available for this integration type",
            )
        }),
    }
}

async fn tls_pin(State(app): State<App>, Path(id): Path<String>) -> Response {
    if let Some(after) = app.windows.consume("tls-pin", PIN_LIMIT) {
        return throttled(after);
    }
    integration_answer(app.port.tls_pin(id).await)
}

async fn reset_tls_pin(State(app): State<App>, Path(id): Path<String>) -> Response {
    if let Some(after) = app.windows.consume("tls-pin-reset", PIN_RESET_LIMIT) {
        return throttled(after);
    }
    integration_answer(app.port.reset_tls_pin(id).await)
}

#[cfg(test)]
mod tests;
