//! `PUT /api/v2/modules/{module}` (GRE-1341): the replacement Settings
//! screen turns one Module on or off. Rust-only runtime: Node keeps its own
//! `PATCH /settings/features` in mixed mode, and this route mounts only when
//! `PP_DESKTOP_REBUILD` is on, so the two never write the same setting.
//!
//! Storage writes the change and reads every Module's state back in one
//! transaction (`FeatureClient::set_module`), from the same rows `GET /health`
//! and send admission read.

use crate::accepted::{AcceptedHttpConfig, Gate, failure, ok_json, read_error};
use crate::loopback::PresentedCredential;
use crate::printer_host::{Windows, throttled};
use crate::problem::{ProblemType, admission_response, coded, typed};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::Response,
    routing::put,
};
use pp_storage::features::{
    FeatureClient, InvalidLayer,
    modules::{Module, ModuleStatus},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Requests a minute that reach the write, whatever storage answers; a
/// person flipping switches never comes close.
const LIMIT: u32 = 30;
const BODY_LIMIT: usize = 16 * 1024;

/// `PUT /api/v2/modules/{module}` body.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SetModuleRequest {
    pub on: bool,
}

/// Every Module's state after the change, in `GET /health`'s order.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ModuleList {
    pub modules: Vec<ModuleStatus>,
}

#[derive(Clone)]
struct App {
    gate: Gate,
    features: FeatureClient,
    windows: Arc<Windows>,
}

/// Mounts the route under `/api/v2` when `PP_DESKTOP_REBUILD` was exactly
/// `1` or `true`; otherwise nothing. The accepted gate admits loopback
/// requests with an exact Host and Origin and a credential; storage proves
/// the credential in the write's own transaction (401, or 403 when the
/// account needs an owner mapping).
pub fn modules_router(config: AcceptedHttpConfig, features: FeatureClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/api/v2/modules/{module}", put(set_module))
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    gate.guard(routes).with_state(App {
        gate,
        features,
        windows: Arc::default(),
    })
}

async fn set_module(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(module): Path<String>,
    body: Bytes,
) -> Response {
    let Ok(module) = serde_json::from_value::<Module>(serde_json::Value::String(module)) else {
        return failure(StatusCode::NOT_FOUND, "Module not found");
    };
    let Ok(SetModuleRequest { on }) = serde_json::from_slice(&body) else {
        return typed(
            ProblemType::MalformedRequest,
            StatusCode::BAD_REQUEST,
            "Send {\"on\": true} or {\"on\": false}",
        );
    };
    if let Some(after) = app.windows.consume("modules", LIMIT) {
        return throttled(after);
    }
    let features = app.features.clone();
    let changed = app
        .gate
        .blocking(move |cancelled, wait| {
            let credential = presented.into_credential();
            features.set_module(credential, module, on, &cancelled, wait)
        })
        .await;
    match changed {
        Ok(Ok(modules)) => ok_json(&ModuleList { modules }),
        Ok(Err(error)) => refused(&error),
        Err(_) => crate::accepted::internal(),
    }
}

/// Unreadable stored toggles answer as Node's `PATCH /settings/features`
/// does; a busy or stopping writer answers 503, since nothing was written.
fn refused(error: &anyhow::Error) -> Response {
    if let Some(response) = admission_response(error, "Module settings are unavailable") {
        return response;
    }
    if error.downcast_ref::<InvalidLayer>().is_some() {
        return coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored feature toggles are invalid.",
            "FEATURE_TOGGLES_INVALID",
        );
    }
    let busy = error.chain().any(|cause| {
        matches!(
            cause.to_string().as_str(),
            "Writer transaction admission deadline expired" | "database is locked"
        )
    });
    if busy {
        return coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "Module settings are busy; try again",
            "transaction_unavailable",
        );
    }
    read_error(error)
}
