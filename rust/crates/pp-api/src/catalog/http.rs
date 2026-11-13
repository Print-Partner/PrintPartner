use super::wire;
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pp_storage::{
    WriterOwner,
    auth::{AuthFailure, AuthPolicy, Secret},
    catalog::{CatalogFailure, Credentials, Outcome, Request as CatalogRequest},
};
use serde::Deserialize;
use std::{
    net::IpAddr,
    sync::{Arc, Mutex},
};

#[cfg(feature = "desktop_rebuild")]
pub(super) mod upload;

#[derive(Clone)]
pub struct CatalogHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    policy: AuthPolicy,
    enabled: bool,
}

impl CatalogHttpConfig {
    pub fn new(origin: &str, api_key_tenant: &str, policy: AuthPolicy) -> Result<Self> {
        Self::from_startup(
            origin,
            api_key_tenant,
            policy,
            std::env::var("PP_DESKTOP_REBUILD").ok().as_deref(),
        )
    }

    /// `flag` is the process `PP_DESKTOP_REBUILD` value captured at startup.
    pub fn from_startup(
        origin: &str,
        api_key_tenant: &str,
        policy: AuthPolicy,
        flag: Option<&str>,
    ) -> Result<Self> {
        let url = reqwest::Url::parse(origin)?;
        ensure!(
            url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "Listener must be an origin"
        );
        let ip: IpAddr = url
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .parse()?;
        ensure!(
            ip.is_loopback() && matches!(url.scheme(), "http" | "https"),
            "Listener must be loopback HTTP"
        );
        ensure!(
            !api_key_tenant.trim().is_empty() && api_key_tenant.len() <= 512,
            "Invalid API key tenant"
        );
        let origin = url.origin().ascii_serialization();
        let host = origin
            .split_once("://")
            .ok_or_else(|| anyhow::anyhow!("Listener must be an origin"))?
            .1
            .to_owned();
        Ok(Self {
            origin,
            host,
            api_key_tenant: api_key_tenant.into(),
            policy,
            enabled: matches!(flag, Some("1" | "true")),
        })
    }
}

#[derive(Clone)]
struct App {
    config: CatalogHttpConfig,
    owner: Arc<Mutex<WriterOwner>>,
}

/// Mounts Source list, detail, create, metadata patch, delete, import-rules,
/// Source activity, Source category settings, Source naming get/put, global STL
/// naming get/put, and STL naming preview at `/` and `/api/v1` when
/// `PP_DESKTOP_REBUILD` was exactly `1` or `true` at startup. Otherwise nothing
/// is mounted. Loose-file and ZIP upload are [`upload::admit_upload_routes`];
/// sync and location stay unregistered.
pub fn admit_catalog_routes(config: CatalogHttpConfig, owner: Arc<Mutex<WriterOwner>>) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let routes = Router::new()
        .route("/sources/activity", get(list_activity))
        .route("/sources", get(list).post(create))
        .route(
            "/sources/{id}",
            get(detail).patch(patch_source).delete(delete_source),
        )
        .route(
            "/sources/{id}/import-rules",
            get(get_import_rules).put(put_import_rules),
        )
        .route("/sources/{id}/naming", get(get_naming).put(put_naming))
        .route(
            "/settings/source-categories",
            get(get_categories).put(put_categories),
        )
        .route(
            "/settings/stl-naming",
            get(get_global_naming).put(put_global_naming),
        )
        .route("/settings/stl-naming/preview", post(preview_naming));
    let app = App { config, owner };
    Router::new()
        .merge(routes.clone())
        .nest("/api/v1", routes)
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .route_layer(middleware::map_response(crate::problem::layer))
        .with_state(app)
}

#[derive(Deserialize)]
struct ActivityQuery {
    limit: Option<String>,
}

async fn guard(
    State(app): State<App>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    if let Err((status, detail)) = admit(&app.config, &request) {
        return reject(status, detail);
    }
    let Some(principal) = principal(request.headers(), &app.config.api_key_tenant) else {
        return reject(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn admit(
    config: &CatalogHttpConfig,
    request: &axum::extract::Request,
) -> std::result::Result<(), (StatusCode, &'static str)> {
    crate::NetworkAdmission::peer(request)?;
    let (expected_origin, expected_host) =
        crate::NetworkAdmission::authority(request, &config.origin, &config.host);
    let headers = request.headers();
    let host = headers.get("host").and_then(|value| value.to_str().ok());
    if headers.get_all("host").iter().count() != 1
        || host != Some(expected_host)
        || request
            .uri()
            .authority()
            .is_some_and(|authority| authority.as_str() != expected_host)
        || headers
            .keys()
            .any(|key| key == "forwarded" || key.as_str().starts_with("x-forwarded-"))
    {
        return Err((StatusCode::FORBIDDEN, "Invalid request host"));
    }
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    let reading = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if headers.get_all("origin").iter().count() > 1
        || (headers.contains_key("origin") && origin != Some(expected_origin))
        || (!reading && origin != Some(expected_origin) && !has_api_key(headers))
        || headers
            .get_all("sec-fetch-site")
            .iter()
            .any(|value| value == "cross-site")
    {
        return Err((StatusCode::FORBIDDEN, "Invalid request origin"));
    }
    Ok(())
}

#[derive(Clone)]
enum Principal {
    Session(String),
    ApiKey { tenant: String, secret: String },
}

fn principal(headers: &HeaderMap, tenant: &str) -> Option<Principal> {
    let mut session = None;
    for header in headers.get_all("cookie") {
        for item in header.to_str().ok()?.split(';') {
            if let Some(("pp_session", value)) = item.trim().split_once('=')
                && session.replace(value).is_some()
            {
                return None;
            }
        }
    }
    let auth = headers.get("authorization");
    let custom = headers.get("x-print-partner-api-key");
    if headers.get_all("authorization").iter().count() > 1
        || headers.get_all("x-print-partner-api-key").iter().count() > 1
        || usize::from(session.is_some())
            + usize::from(auth.is_some())
            + usize::from(custom.is_some())
            != 1
    {
        return None;
    }
    if let Some(value) = session {
        return (!value.is_empty() && value.len() <= 4096)
            .then(|| Principal::Session(value.into()));
    }
    let key = if let Some(value) = auth {
        value.to_str().ok()?.strip_prefix("Bearer ")?
    } else {
        custom?.to_str().ok()?
    }
    .trim();
    (!key.is_empty() && key.len() <= 4096).then(|| Principal::ApiKey {
        tenant: tenant.into(),
        secret: key.into(),
    })
}

fn has_api_key(headers: &HeaderMap) -> bool {
    matches!(principal(headers, ""), Some(Principal::ApiKey { .. }))
}

fn credentials(principal: Principal) -> Credentials {
    match principal {
        Principal::Session(token) => Credentials::Session(Secret::new(token)),
        Principal::ApiKey { tenant, secret } => Credentials::Key {
            tenant_id: tenant,
            key: Secret::new(secret),
        },
    }
}

async fn list(State(app): State<App>, Extension(principal): Extension<Principal>) -> Response {
    execute(&app, credentials(principal), CatalogRequest::List {}).await
}

async fn create(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    body: Bytes,
) -> Response {
    let Some(body) = object_body(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    if let Some(response) = refused_local_path(&body) {
        return response;
    }
    // A missing or null name reaches storage, which answers Node's "Source name
    // is required"; a name of another type is refused here.
    let mut body = body;
    if body.get("name").is_none_or(serde_json::Value::is_null) {
        body["name"] = "".into();
    }
    let source = match serde_json::from_value::<wire::CreateSourceRequest>(body) {
        Ok(request) => request.into(),
        Err(error) => return reject(StatusCode::BAD_REQUEST, format!("Invalid Source: {error}")),
    };
    execute(
        &app,
        credentials(principal),
        CatalogRequest::CreateCatalogSource { source },
    )
    .await
}

async fn delete_source(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let Some(id) = source_id(&id) else {
        return reject(StatusCode::NOT_FOUND, "Source not found");
    };
    execute(&app, credentials(principal), CatalogRequest::Delete { id }).await
}

async fn detail(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let Some(id) = source_id(&id) else {
        return reject(StatusCode::NOT_FOUND, "Source not found");
    };
    execute(&app, credentials(principal), CatalogRequest::Get { id }).await
}

async fn patch_source(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Some(id) = source_id(&id) else {
        return reject(StatusCode::NOT_FOUND, "Source not found");
    };
    let Some(body) = object_body(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    if let Some(response) = refused_local_path(&body) {
        return response;
    }
    let patch = match wire::source_patch(&body) {
        Ok(patch) => patch,
        Err(detail) => return reject(StatusCode::BAD_REQUEST, detail),
    };
    execute(
        &app,
        credentials(principal),
        CatalogRequest::UpdateCatalogSource { id, patch },
    )
    .await
}

async fn get_categories(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
) -> Response {
    execute(
        &app,
        credentials(principal),
        CatalogRequest::GetCategoriesWithTree {},
    )
    .await
}

async fn put_categories(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    body: Bytes,
) -> Response {
    let Some(body) = object_body(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let Some((categories, replacements)) = wire::category_save(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    execute(
        &app,
        credentials(principal),
        CatalogRequest::SaveCategoriesWithTree {
            categories,
            replacements,
        },
    )
    .await
}

async fn list_activity(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<ActivityQuery>,
) -> Response {
    execute(
        &app,
        credentials(principal),
        CatalogRequest::SourceActivity {
            limit: wire::activity_limit(query.limit.as_deref()),
        },
    )
    .await
}

async fn get_import_rules(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let Some(id) = source_id(&id) else {
        return reject(StatusCode::NOT_FOUND, "Source not found");
    };
    execute(
        &app,
        credentials(principal),
        CatalogRequest::GetImportRules { id },
    )
    .await
}

async fn put_import_rules(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Some(id) = source_id(&id) else {
        return reject(StatusCode::NOT_FOUND, "Source not found");
    };
    let Some(body) = object_body(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    if let Some(response) = refused_local_path(&body) {
        return response;
    }
    let Some(rules) = wire::import_rules_save(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    execute(
        &app,
        credentials(principal),
        CatalogRequest::SaveImportRules { id, rules },
    )
    .await
}

async fn get_naming(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let Some(id) = naming_source_id(&id) else {
        return invalid_naming("Source id must be a positive integer");
    };
    naming(&app, principal, CatalogRequest::GetNaming { id }).await
}

async fn put_naming(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Some(id) = naming_source_id(&id) else {
        return invalid_naming("Source id must be a positive integer");
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    if !body
        .get("use_defaults")
        .is_some_and(serde_json::Value::is_boolean)
    {
        return invalid_naming(
            "source naming request.use_defaults: Invalid discriminator value. \
             Expected 'true' | 'false'",
        );
    }
    let Some(settings) = wire::naming_save(&body) else {
        return invalid_naming("source naming request is invalid");
    };
    naming(&app, principal, CatalogRequest::SaveNaming { id, settings }).await
}

/// Source naming answers with Node's coded envelope, `{ code, detail }`.
async fn naming(app: &App, principal: Principal, request: CatalogRequest) -> Response {
    match dispatch_with(app, credentials(principal), request, naming_error).await {
        Ok(Outcome::Data(value)) => Json(value).into_response(),
        Ok(_) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
        Err(response) => *response,
    }
}

pub(crate) fn naming_error(error: anyhow::Error) -> Response {
    let coded = |status, code, detail| crate::problem::coded(status, detail, code);
    match error.downcast_ref::<CatalogFailure>() {
        Some(CatalogFailure::NotFound) => coded(
            StatusCode::NOT_FOUND,
            "source_not_found",
            "Source not found",
        ),
        Some(CatalogFailure::Conflict) => coded(
            StatusCode::CONFLICT,
            "source_naming_conflict",
            "Source metadata changed while saving naming rules",
        ),
        Some(CatalogFailure::InvalidStoredNaming) => coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_source_naming_state",
            "Stored Source naming settings are invalid",
        ),
        Some(CatalogFailure::Input(detail)) => invalid_naming(detail),
        _ => catalog_error(error),
    }
}

fn invalid_naming(detail: &str) -> Response {
    crate::problem::coded(StatusCode::BAD_REQUEST, detail, "invalid_source_naming")
}

/// Node's `sourceIdFromParams`: digits only, a safe integer above zero.
fn naming_source_id(raw: &str) -> Option<i64> {
    let id = raw
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| raw.parse::<i64>().ok())??;
    (1..=9_007_199_254_740_991).contains(&id).then_some(id)
}

async fn preview_naming(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    body: Bytes,
) -> Response {
    let Some(body) = object_body(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let Some((relative_path, profile)) = wire::naming_preview(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    execute(
        &app,
        credentials(principal),
        CatalogRequest::PreviewNaming {
            relative_path,
            profile,
        },
    )
    .await
}

async fn get_global_naming(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
) -> Response {
    match dispatch(
        &app,
        credentials(principal),
        CatalogRequest::GetGlobalNaming {},
    )
    .await
    {
        Ok(Outcome::Data(profile)) => Json(wire::global_naming(profile)).into_response(),
        Ok(_) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
        Err(response) => *response,
    }
}

async fn put_global_naming(
    State(app): State<App>,
    Extension(principal): Extension<Principal>,
    body: Bytes,
) -> Response {
    let Some(body) = object_body(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let Some(profile) = wire::global_naming_save(&body) else {
        return reject(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    match dispatch(
        &app,
        credentials(principal),
        CatalogRequest::SaveGlobalNaming { profile },
    )
    .await
    {
        Ok(Outcome::Data(profile)) => Json(wire::global_naming(profile)).into_response(),
        Ok(_) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
        Err(response) => *response,
    }
}

fn object_body(bytes: &[u8]) -> Option<serde_json::Value> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .filter(serde_json::Value::is_object)
}

fn refused_local_path(body: &serde_json::Value) -> Option<Response> {
    body.get("local_path")
        .is_some_and(|value| !value.is_null())
        .then(|| {
            reject(
                StatusCode::BAD_REQUEST,
                "Raw local_path requires native Source selection",
            )
        })
}

fn source_id(raw: &str) -> Option<i64> {
    let id = raw.parse::<i64>().ok()?;
    (raw == id.to_string()).then_some(id)
}

async fn execute(app: &App, credentials: Credentials, request: CatalogRequest) -> Response {
    match dispatch(app, credentials, request).await {
        Ok(outcome) => match outcome {
            Outcome::Sources(sources) => Json(wire::SourceList { sources }).into_response(),
            Outcome::Source(Some(row)) => match wire::source(*row) {
                Ok(value) => Json(value).into_response(),
                Err(_) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
            },
            Outcome::Source(None) => reject(StatusCode::NOT_FOUND, "Source not found"),
            Outcome::Categories(settings) => match wire::categories(settings) {
                Ok(value) => Json(value).into_response(),
                Err(_) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
            },
            Outcome::Data(value) => Json(value).into_response(),
            Outcome::Activity(events) => {
                match serde_json::to_value(wire::SourceActivity { events }) {
                    Ok(value) => Json(value).into_response(),
                    Err(_) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
                }
            }
            Outcome::Preview(preview) => match wire::naming_preview_body(preview) {
                Ok(value) => Json(value).into_response(),
                Err(_) => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
            },
            Outcome::Deletion(outcome) => deletion_response(wire::source_deletion(outcome)),
            _ => reject(StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
        },
        Err(response) => *response,
    }
}

async fn dispatch(
    app: &App,
    credentials: Credentials,
    request: CatalogRequest,
) -> Result<Outcome, Box<Response>> {
    dispatch_with(app, credentials, request, catalog_error).await
}

async fn dispatch_with(
    app: &App,
    credentials: Credentials,
    request: CatalogRequest,
    on_error: fn(anyhow::Error) -> Response,
) -> Result<Outcome, Box<Response>> {
    let policy = app.config.policy;
    let client = {
        let Ok(owner) = app.owner.lock() else {
            return Err(Box::new(reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Catalog unavailable",
            )));
        };
        match owner.source_catalog_with_policy(credentials, policy) {
            Ok(client) => client,
            Err(_) => {
                return Err(Box::new(reject(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Catalog unavailable",
                )));
            }
        }
    };
    match tokio::task::spawn_blocking(move || client.execute(request)).await {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => Err(Box::new(on_error(error))),
        Err(_) => Err(Box::new(reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Catalog unavailable",
        ))),
    }
}

fn deletion_response(mapped: wire::DeletionHttp) -> Response {
    match mapped {
        wire::DeletionHttp::Deleted => StatusCode::NO_CONTENT.into_response(),
        wire::DeletionHttp::Refused {
            status: 404,
            detail,
        } => reject(StatusCode::NOT_FOUND, detail),
        wire::DeletionHttp::Refused {
            status: 409,
            detail,
        } => reject(StatusCode::CONFLICT, detail),
        wire::DeletionHttp::Refused { detail, .. } => {
            reject(StatusCode::INTERNAL_SERVER_ERROR, detail)
        }
    }
}

pub(crate) fn catalog_error(error: anyhow::Error) -> Response {
    if let Some(failure) = error.downcast_ref::<AuthFailure>() {
        return match failure {
            AuthFailure::QueueFull => crate::problem::overloaded("Catalog temporarily unavailable"),
            AuthFailure::CommitUnknown => crate::problem::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "Catalog temporarily unavailable",
                "outcome_unknown",
            ),
            AuthFailure::Stopped => reject(
                StatusCode::SERVICE_UNAVAILABLE,
                "Catalog temporarily unavailable",
            ),
            AuthFailure::CredentialChanged => reject(
                StatusCode::UNAUTHORIZED,
                "Credential changed; sign in again",
            ),
            _ => reject(StatusCode::UNAUTHORIZED, "Authentication required"),
        };
    }
    if let Some(failure) = error.downcast_ref::<CatalogFailure>() {
        let (status, detail) = match failure {
            CatalogFailure::NotFound => (StatusCode::NOT_FOUND, "Source not found".to_owned()),
            CatalogFailure::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "Catalog request too large".to_owned(),
            ),
            CatalogFailure::InvalidStoredNaming => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Stored Source naming settings are invalid".to_owned(),
            ),
            CatalogFailure::Input(_)
            | CatalogFailure::DuplicateName(_)
            // The typed failure's own text, never an outer context's.
            | CatalogFailure::Referenced => (StatusCode::BAD_REQUEST, failure.to_string()),
            CatalogFailure::Conflict => {
                return crate::problem::coded(
                    StatusCode::CONFLICT,
                    &failure.to_string(),
                    "catalog_metadata_conflict",
                );
            }
        };
        return reject(status, detail);
    }
    let (status, detail) = match error.to_string().as_str() {
        "Authentication required" | "Invalid session" => {
            (StatusCode::UNAUTHORIZED, "Authentication required")
        }
        "Writer queue full" => {
            return crate::problem::overloaded("Catalog temporarily unavailable");
        }
        "Storage stopped" | "Cancelled before admission" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Catalog temporarily unavailable",
        ),
        "Source not found" => (StatusCode::NOT_FOUND, "Source not found"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "Catalog unavailable"),
    };
    reject(status, detail)
}

fn reject(status: StatusCode, detail: impl AsRef<str>) -> Response {
    crate::problem::response(status, detail.as_ref())
}
