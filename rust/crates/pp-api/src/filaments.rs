//! Custom filament writes and catalog reads, gated with the admin routes.
use crate::{
    accepted::{failure, read_error},
    admin::{AdminHttpConfig, Gate, response},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::Response,
    routing::{delete, get, post},
};
use pp_storage::{
    filaments::{FilamentClient, Outcome, Request},
    read_model::ReadClient,
};
use serde_json::{Value, json};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

/// Reads filament types only, using the core's checked outbound connector.
pub trait CatalogReader: Send + Sync {
    fn read(&self, integration: Value) -> Pin<Box<dyn Future<Output = Option<Vec<Value>>> + Send>>;
}
#[derive(Clone)]
struct App {
    client: FilamentClient,
    reader: Arc<dyn CatalogReader>,
    wait: Duration,
}
pub fn filament_router(
    config: AdminHttpConfig,
    reads: ReadClient,
    client: FilamentClient,
    reader: Arc<dyn CatalogReader>,
) -> Router {
    let Some((gate, wait)) = Gate::with_wait(config, reads) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/filaments/custom", post(add))
        .route("/filaments/custom/{id}", delete(remove))
        .route("/filaments/catalog", get(catalog));
    gate.guard(
        Router::new()
            .merge(routes.clone())
            .nest("/api/v1", routes)
            .layer(DefaultBodyLimit::max(1024 * 1024)),
    )
    .with_state(App {
        client,
        reader,
        wait,
    })
}
async fn execute(
    app: &App,
    credential: PresentedCredential,
    request: Request,
) -> Result<Outcome, Box<Response>> {
    let client = app.client.clone();
    let wait = app.wait;
    let result = match tokio::task::spawn_blocking(move || {
        client.submit(credential.into_credential(), request, wait)
    })
    .await
    {
        Ok(Ok(Outcome::Refused(status, detail, code))) => Err(if code.is_empty() {
            failure(
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                detail,
            )
        } else {
            crate::problem::coded(
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                detail,
                code,
            )
        }),
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => Err(read_error(&error)),
        Err(_) => Err(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
        )),
    };
    result.map_err(Box::new)
}
fn ok(body: Value) -> Response {
    response(
        StatusCode::OK,
        "application/json; charset=utf-8",
        body.to_string(),
    )
}
async fn add(
    State(app): State<App>,
    Extension(credential): Extension<PresentedCredential>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) => {
            return failure(
                if error.status() == StatusCode::UNPROCESSABLE_ENTITY {
                    StatusCode::BAD_REQUEST
                } else {
                    error.status()
                },
                "Request is invalid",
            );
        }
    };
    match execute(&app, credential, Request::Add(body)).await {
        Ok(Outcome::Added(body)) => ok(body),
        Err(reply) => *reply,
        _ => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}
async fn remove(
    State(app): State<App>,
    Extension(credential): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    match execute(&app, credential, Request::Delete(id)).await {
        Ok(Outcome::Deleted) => response(StatusCode::NO_CONTENT, "application/json", String::new()),
        Err(reply) => *reply,
        _ => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}
async fn catalog(
    State(app): State<App>,
    Extension(credential): Extension<PresentedCredential>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (custom, default_id, integration, allowed) =
        match execute(&app, credential.clone(), Request::Catalog(query.clone())).await {
            Ok(Outcome::Catalog {
                custom,
                default_id,
                integration,
                allowed,
            }) => (custom, default_id, integration, allowed),
            Err(reply) => return *reply,
            _ => return failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        };
    let Some(mut body) = catalog_body() else {
        return failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    if allowed {
        body["default_spoolman_integration_id"] = json!(default_id);
        if let Some(integration) = integration {
            let error = if integration.get("type").is_none() {
                Some(("not_found", "Spoolman integration not found"))
            } else if integration["type"] != "spoolman" {
                Some(("error", "Selected integration is not a Spoolman connector"))
            } else if integration["config"]["enabled"] == false {
                Some(("disabled", "Spoolman integration is disabled in Settings"))
            } else {
                None
            };
            if let Some((status, error)) = error {
                body["spoolman_status"] = json!(status);
                body["spoolman_error"] = json!(error);
            } else {
                match app.reader.read(integration).await {
                    Some(colors) => {
                        body["spoolman_status"] =
                            json!(if colors.is_empty() { "empty" } else { "ok" });
                        if colors.is_empty() {
                            body["spoolman_error"] = json!(
                                "Spoolman returned no filament types — add filaments in Spoolman (spools alone are not listed here)"
                            );
                        }
                        body["spoolman_colors"] = json!(colors);
                    }
                    None => {
                        body["spoolman_status"] = json!("error");
                        body["spoolman_error"] = json!("Spoolman catalog fetch failed");
                    }
                }
            }
        }
    }
    // Recheck access after awaiting the connector; withheld data must not escape.
    match execute(&app, credential, Request::Catalog(query)).await {
        Ok(Outcome::Catalog { allowed: false, .. }) => {
            if let Some(fields) = body.as_object_mut() {
                fields.remove("default_spoolman_integration_id");
            }
            body["spoolman_colors"] = json!([]);
            body["spoolman_status"] = json!("disabled");
            body["spoolman_error"] = json!("Feature spoolman is disabled.");
        }
        Err(reply) => return *reply,
        _ => {}
    }
    catalog_response(body, custom)
}

fn catalog_body() -> Option<Value> {
    let mut body: Value = serde_json::from_str(include_str!(
        // ast-grep-ignore: no-writer-family-literal -- Bundled catalog is read-only.
        "../../../../web/apps/server/src/data/ambrosia_fallback.json"
    ))
    .ok()?;
    for color in body["colors"].as_array_mut()? {
        let name = color["display_name"].as_str().unwrap_or("");
        let line = color["product_line"].as_str().unwrap_or("");
        color["combo_label"] = json!(if line.is_empty() {
            name.to_owned()
        } else {
            format!("{line} · {name}")
        });
        color.as_object_mut()?.retain(|key, _| {
            matches!(
                key.as_str(),
                "id" | "display_name" | "product_line" | "hex" | "combo_label" | "swatch_url"
            )
        });
        if color.get("swatch_url").is_none() {
            color["swatch_url"] = json!("");
        }
    }
    body["status"] = json!("ok");
    body["spoolman_colors"] = json!([]);
    Some(body)
}

fn catalog_response(body: Value, custom: String) -> Response {
    #[derive(serde::Serialize)]
    struct Catalog {
        #[serde(flatten)]
        body: Value,
        custom_colors: Box<serde_json::value::RawValue>,
    }
    let encoded = serde_json::value::RawValue::from_string(custom).and_then(|custom_colors| {
        serde_json::to_string(&Catalog {
            body,
            custom_colors,
        })
    });
    match encoded {
        Ok(body) => response(StatusCode::OK, "application/json; charset=utf-8", body),
        Err(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}
