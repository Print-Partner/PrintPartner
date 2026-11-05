//! `GET`/`HEAD /api/v1/integrations/{id}/status` for the Rust-only runtime,
//! answered from the Moonraker status watchers through [`PrinterStatusPort`].
//! Node registers this route under `/api/v1` only. The route reads cached
//! observations: it never polls a printer on demand, sends, retries, or
//! touches Checkoff. Admission and the credential proof are the admin
//! routes' (`AdminHttpConfig`).

use crate::accepted::failure;
use crate::admin::{AdminHttpConfig, Gate};
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use pp_storage::read_model::ReadClient;
use std::{future::Future, pin::Pin, sync::Arc};

/// What the watchers hold for one integration id.
#[derive(Debug, PartialEq)]
pub enum StatusLookup {
    /// The `PrinterHostStatus` JSON object, as Node returns it.
    Status(serde_json::Value),
    /// The id is not a Moonraker host in the stored settings.
    NotWatched,
    /// A Moonraker host with no current status (no valid snapshot, or its
    /// poller is stopped).
    Unavailable,
}

/// Read-only access to the watchers' cached statuses. `pp-core` implements
/// it over the gateway's watchers.
pub trait PrinterStatusPort: Send + Sync + 'static {
    fn lookup(&self, id: String) -> Pin<Box<dyn Future<Output = StatusLookup> + Send>>;
}

/// Mounts the status route when `PP_DESKTOP_REBUILD` was exactly `1` or
/// `true`; otherwise nothing. The credential is proved with an empty
/// `ReadClient` batch before the lookup, as Node's auth hook runs first.
pub fn printer_status_router(
    config: AdminHttpConfig,
    client: ReadClient,
    port: Arc<dyn PrinterStatusPort>,
) -> Router {
    let Some(gate) = Gate::new(config, client) else {
        return Router::new();
    };
    gate.guard(Router::new().route("/api/v1/integrations/{id}/status", get(status)))
        .with_state(port)
}

async fn status(
    State(port): State<Arc<dyn PrinterStatusPort>>,
    Path(id): Path<String>,
) -> Response {
    match port.lookup(id).await {
        StatusLookup::Status(body) => axum::Json(body).into_response(),
        StatusLookup::NotWatched => failure(StatusCode::NOT_FOUND, "Integration not found"),
        StatusLookup::Unavailable => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Printer monitoring unavailable",
        ),
    }
}
