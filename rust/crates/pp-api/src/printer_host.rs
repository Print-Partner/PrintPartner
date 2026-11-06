//! Printer-host reads for the Rust-only runtime (GRE-360), at the paths Node's
//! `routes/printers.ts` and `routes/integrations.ts` serve:
//!
//! - `GET /printers/{id}/capabilities`, `/printers/{id}/files?path=` and
//!   `/printers/{id}/cameras`, at the root and under `/api/v1`, where Node's
//!   core routes are registered
//! - `POST /api/v1/integrations/{id}/test`, under `/api/v1` only, as in Node
//!
//! Every route is read-only on the printer: it lists, describes or probes,
//! once per request, and never uploads, starts, deletes, retries or touches
//! Checkoff. `pp-core` implements [`PrinterHostPort`] over storage's
//! integration settings and the gateway's Moonraker driver. Admission and
//! the credential proof are the admin routes' (`AdminHttpConfig`).

use crate::accepted::failure;
use crate::admin::{AdminHttpConfig, Gate};
use axum::{
    Router,
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
}

pub type Answer<T> = Pin<Box<dyn Future<Output = HostAnswer<T>> + Send>>;

pub trait PrinterHostPort: Send + Sync + 'static {
    fn capabilities(&self, printer: String) -> Answer<Capabilities>;
    /// `path` is already a safe storage path, or empty for the root.
    fn files(&self, printer: String, path: String) -> Answer<Value>;
    fn cameras(&self, printer: String) -> Answer<Value>;
    /// `Ok` holds Node's `{ok, message}` test result.
    fn test(&self, integration: String) -> Answer<Value>;
}

/// Node's per-route `rateLimit` budgets, one window per route. Node keys them
/// per client IP; the Rust-only runtime only admits loopback clients.
const CAPABILITIES_LIMIT: u32 = 120;
const FILES_LIMIT: u32 = 60;
const CAMERAS_LIMIT: u32 = 60;
const TEST_LIMIT: u32 = 5;
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
        .route("/printers/{id}/cameras", get(cameras));
    let mount = |routes: Router<App>, port| {
        routes.with_state(App {
            port,
            windows: Arc::default(),
        })
    };
    let v1 = printers
        .clone()
        .route("/integrations/{id}/test", post(test));
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

async fn test(State(app): State<App>, Path(id): Path<String>) -> Response {
    if let Some(after) = app.windows.consume("test", TEST_LIMIT) {
        return throttled(after);
    }
    match app.port.test(id).await {
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

#[cfg(test)]
mod tests;
