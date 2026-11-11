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

async fn probe(State(app): State<App>, Json(request): Json<PrinterProbeRequest>) -> Response {
    if request.kind.len() > 32 || request.address.is_empty() || request.address.len() > 2048 {
        return crate::problem::coded(
            StatusCode::BAD_REQUEST,
            "Invalid printer kind or address",
            "invalid_printer_probe",
        );
    }
    if let Some(after) = app.calls.consume("probe", 10) {
        return super::throttled(after);
    }
    let Ok(_active) = app.active.try_acquire() else {
        return crate::problem::coded(
            StatusCode::TOO_MANY_REQUESTS,
            "A printer probe is already running",
            "printer_probe_busy",
        );
    };
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

/// Metadata for generation and the core mounted-route contract test.
pub const CLIENT_ROUTE: crate::client_contract::ClientRoute = crate::client_contract::ClientRoute {
    name: "probePrinter",
    method: "POST",
    path: "/api/v2/printers/probe",
    auth: crate::client_contract::ClientAuth::Credential,
    request: Some("PrinterProbeRequest"),
    response: "PrinterProbeResult",
    text_params: &[],
    headers: &[],
};

#[cfg(feature = "codegen")]
impl Serialize for crate::client_contract::ClientRoute {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut route = serializer.serialize_struct("ClientRoute", 8)?;
        route.serialize_field("name", &self.name)?;
        route.serialize_field("method", &self.method)?;
        route.serialize_field("path", &self.path)?;
        route.serialize_field("auth", self.auth.as_str())?;
        route.serialize_field("request", &self.request)?;
        route.serialize_field("response", &self.response)?;
        route.serialize_field("text_params", &self.text_params)?;
        route.serialize_field("headers", &self.headers)?;
        route.end()
    }
}

#[cfg(feature = "codegen")]
pub fn client_document(mut document: serde_json::Value) -> Result<String, serde_json::Error> {
    use serde_json::json;
    document["dtos"]["PrinterProbeRequest"] =
        serde_json::to_value(schemars::schema_for!(PrinterProbeRequest))?;
    let mut result_schema = schemars::schema_for!(PrinterProbeResult);
    // Responses always serialize kind, including its null value.
    if let Some(object) = result_schema.schema.object.as_mut() {
        object.required.insert("kind".into());
    }
    document["dtos"]["PrinterProbeResult"] = serde_json::to_value(result_schema)?;
    if let Some(routes) = document["routes"].as_array_mut() {
        let route = CLIENT_ROUTE;
        routes.push(serde_json::to_value(route)?);
    }
    if let Some(fixtures) = document["fixtures"].as_array_mut() {
        let request = PrinterProbeRequest {
            kind: "moonraker".into(),
            address: "http://fixture.invalid".into(),
        };
        let result = PrinterProbeResult {
            kind: Some("moonraker".into()),
            reachable: true,
            credentials_needed: false,
            reason: "Printer responded successfully.".into(),
            capabilities: vec!["status".into()],
        };
        let identified = serde_json::to_value(&result)?;
        let unavailable = PrinterProbeResult {
            kind: None,
            reachable: false,
            reason: "Printer probe timed out.".into(),
            capabilities: vec![],
            ..result
        };
        for (dto, name, value) in [
            (
                "PrinterProbeRequest",
                "request",
                serde_json::to_value(request)?,
            ),
            ("PrinterProbeResult", "identified", identified),
            (
                "PrinterProbeResult",
                "unavailable",
                serde_json::to_value(unavailable)?,
            ),
        ] {
            fixtures.push(json!({"dto":dto,"name":name,"value":value,"rust_accepts":true}));
        }
    }
    document.sort_all_objects();
    Ok(format!("{}\n", serde_json::to_string_pretty(&document)?))
}
