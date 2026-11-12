//! Authenticated first-run checks; a probe never saves an integration.
use crate::admin::{AdminHttpConfig, Gate};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use pp_storage::read_model::ReadClient;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PrinterProbeRequest {
    pub kind: String,
    pub address: String,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct PrinterProbeResult {
    pub kind: Option<String>,
    pub reachable: bool,
    pub credentials_needed: bool,
    pub reason: String,
    pub capabilities: Vec<String>,
}

pub trait PrinterProbePort: Send + Sync + 'static {
    fn probe(
        &self,
        request: PrinterProbeRequest,
    ) -> Pin<Box<dyn Future<Output = PrinterProbeResult> + Send>>;
}

#[derive(Clone)]
struct App {
    port: Arc<dyn PrinterProbePort>,
    calls: Arc<super::Windows>,
    active: Arc<tokio::sync::Semaphore>,
}

pub fn printer_probe_router(
    config: AdminHttpConfig,
    client: ReadClient,
    port: Arc<dyn PrinterProbePort>,
) -> Router {
    let Some(gate) = Gate::new(config, client) else {
        return Router::new();
    };
    gate.guard(
        Router::new()
            .route("/api/v2/printers/probe", post(probe))
            .layer(DefaultBodyLimit::max(4096)),
    )
    .with_state(App {
        port,
        calls: Arc::new(super::Windows::default()),
        active: Arc::new(tokio::sync::Semaphore::new(1)),
    })
}

async fn probe(
    State(app): State<App>,
    body: Result<Json<PrinterProbeRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request = match body {
        Ok(Json(request)) => request,
        Err(error) => {
            return crate::problem::coded(
                error.status(),
                "Invalid printer probe request",
                "invalid_printer_probe",
            );
        }
    };
    if request.kind.len() > 32 || request.address.is_empty() || request.address.len() > 2048 {
        return crate::problem::coded(
            StatusCode::BAD_REQUEST,
            "Invalid printer kind or address",
            "invalid_printer_probe",
        );
    }
    let Ok(_active) = app.active.try_acquire() else {
        return crate::problem::coded(
            StatusCode::TOO_MANY_REQUESTS,
            "A printer probe is already running",
            "printer_probe_busy",
        );
    };
    if let Some(after) = app.calls.consume("probe", 10) {
        return super::throttled(after);
    }
    match tokio::time::timeout(Duration::from_secs(8), app.port.probe(request)).await {
        Ok(result) => Json(result).into_response(),
        Err(_) => Json(PrinterProbeResult {
            kind: None,
            reachable: false,
            credentials_needed: false,
            reason: "Printer probe timed out.".into(),
            capabilities: vec![],
        })
        .into_response(),
    }
}
