//! Remaining settings and static help/document shapes from Node.
use crate::{
    accepted::failure,
    admin::{AdminHttpConfig, Gate, response},
    loopback::PresentedCredential,
    settings::{field, settled},
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
};
use pp_storage::{
    app_settings::{AppSettingsClient, Bindings, Key, Outcome},
    read_model::ReadClient,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[derive(Clone)]
struct App {
    settings: AppSettingsClient,
    wait: Duration,
    updates: Arc<crate::update_check::UpdateCheck>,
}

pub fn support_router(
    config: AdminHttpConfig,
    reads: ReadClient,
    settings: AppSettingsClient,
) -> Router {
    let wait = config.wait;
    let updates = Arc::new(crate::update_check::UpdateCheck::from_startup(
        config.server_updates,
    ));
    let Some(gate) = Gate::new(config, reads) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/settings/printer-plan-bindings", get(list).put(put))
        .route(
            "/settings/printer-plan-bindings/{id}",
            axum::routing::delete(delete),
        )
        .route("/help/workflow", get(help));
    // Node exempts OpenAPI from both API-key and session authentication.
    let public = Router::new()
        .route(
            "/openapi.json",
            get(|| async {
                Response::builder()
                    .status(StatusCode::FOUND)
                    .header("location", "/api/v1/openapi.json")
                    .header("cache-control", "private, no-store")
                    .body(axum::body::Body::empty())
                    .unwrap_or_else(|_| internal())
            }),
        )
        .route("/api/v1/openapi.json", get(document))
        .route("/api/v2/openapi.json", get(document));
    let update_routes = Router::new().route("/settings/update-check", get(update));
    let update_routes = gate.clone().authentication_only().guard(
        Router::new()
            .merge(update_routes.clone())
            .nest("/api/v1", update_routes),
    );
    gate.guard(
        Router::new()
            .merge(routes.clone())
            .nest("/api/v1", routes)
            .fallback(|| async { StatusCode::NOT_FOUND })
            .layer(DefaultBodyLimit::max(1024 * 1024)),
    )
    .merge(update_routes)
    .with_state(App {
        settings,
        wait,
        updates,
    })
    .merge(public)
}

const SHAPES: &str = include_str!("../../../../fixtures/admin/settings-support.json");
fn shapes() -> Option<Value> {
    serde_json::from_str(SHAPES).ok()
}
fn document_shape() -> Option<Value> {
    let fixture = shapes()?;
    let mut document = fixture["metadata"].clone();
    let mut paths = serde_json::Map::new();
    for group in fixture["route_groups"].as_array()? {
        for path in group["paths"].as_array()? {
            paths.insert(path.as_str()?.into(), group["shape"].clone());
        }
    }
    document["paths"] = Value::Object(paths);
    Some(document)
}
async fn document() -> Response {
    document_shape().map(ok).unwrap_or_else(internal)
}
async fn help(
    State(app): State<App>,
    Extension(credential): Extension<PresentedCredential>,
) -> Response {
    let outcome = tokio::task::spawn_blocking(move || {
        app.settings.read(
            credential.into_credential(),
            Key::ExternalAccess,
            &AtomicBool::new(false),
            app.wait,
        )
    })
    .await;
    match settled(outcome) {
        Ok(Outcome::Value(mode)) => {
            let key = if mode.as_deref() == Some("off") {
                "help_off"
            } else {
                "help_on"
            };
            match shapes().and_then(|shape| shape[key].as_str().map(str::to_owned)) {
                Some(text) => response(StatusCode::OK, "text/plain; charset=utf-8", text),
                None => internal(),
            }
        }
        Err(error) => *error,
        _ => internal(),
    }
}
async fn update(
    State(app): State<App>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    ok(app
        .updates
        .check(query.get("refresh").is_some_and(|v| v == "1"))
        .await)
}
async fn list(
    State(app): State<App>,
    Extension(credential): Extension<PresentedCredential>,
) -> Response {
    change(app, credential, Bindings::Get).await
}
async fn put(
    State(app): State<App>,
    Extension(credential): Extension<PresentedCredential>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Reuse the existing settings media-type and malformed-body contract.
    if let Err(error) = field(&headers, &body, "integration_id") {
        return *error;
    }
    let is_json = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    let body = if is_json {
        serde_json::from_slice(&body).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    change(app, credential, Bindings::Put(body)).await
}
async fn delete(
    State(app): State<App>,
    Extension(credential): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    change(app, credential, Bindings::Delete(id)).await
}
async fn change(app: App, credential: PresentedCredential, change: Bindings) -> Response {
    let outcome = tokio::task::spawn_blocking(move || {
        app.settings.bindings(
            credential.into_credential(),
            change,
            &AtomicBool::new(false),
            app.wait,
        )
    })
    .await;
    match settled(outcome) {
        Ok(Outcome::Json(body)) => ok(body),
        Err(error) => *error,
        _ => internal(),
    }
}
fn ok(body: Value) -> Response {
    response(
        StatusCode::OK,
        "application/json; charset=utf-8",
        body.to_string(),
    )
}
fn internal() -> Response {
    failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
}
