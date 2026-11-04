//! Legal documents and the custom filament list (GRE-438), as Node's
//! `routes/legal.ts` and `GET /filaments/custom` in `routes/settings.ts` serve them.

use crate::accepted::{failure, read_error};
use crate::loopback::{
    LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
    require_api_key_tenant,
};
use anyhow::Result;
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use pp_storage::{
    admin::{AdminFiles, CustomFilaments},
    read_model::ReadClient,
};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// The files the Node build copies to `dist/data/legal`; `legal.ts` reads that
/// copy first, so it always serves these bytes.
const LEGAL: [(&str, &str); 4] = [
    (
        "/legal/summary",
        include_str!("../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md"),
    ),
    (
        "/legal/license",
        include_str!("../../../../web/apps/server/src/data/legal/LICENSE"),
    ),
    (
        "/legal/attribution",
        include_str!("../../../../web/apps/server/src/data/legal/ATTRIBUTION.md"),
    ),
    (
        "/legal/third-party",
        include_str!("../../../../web/apps/server/src/data/legal/THIRD_PARTY_NOTICES.md"),
    ),
];

#[derive(Clone)]
pub struct AdminHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    wait: Duration,
}

impl AdminHttpConfig {
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
        })
    }
}

#[derive(Clone)]
struct App {
    config: AdminHttpConfig,
    client: ReadClient,
    files: AdminFiles,
    admission: Arc<tokio::sync::Semaphore>,
}

/// Mounts `GET`/`HEAD` `/legal/{summary,license,attribution,third-party}` and
/// `/filaments/custom` at `/` and `/api/v1`, where Node registers its core
/// routes, when `PP_DESKTOP_REBUILD` was exactly `1` or `true`; otherwise
/// nothing. Every route first proves the credential with an empty
/// `ReadClient` batch, as Node's auth hook runs before each handler.
pub fn admin_router(config: AdminHttpConfig, client: ReadClient, files: AdminFiles) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let app = App {
        config,
        client,
        files,
        admission: Arc::new(tokio::sync::Semaphore::new(64)),
    };
    let mut routes = Router::new().route("/filaments/custom", get(custom_filaments));
    for (path, text) in LEGAL {
        routes = routes.route(
            path,
            get(move || async move {
                response(StatusCode::OK, "text/plain; charset=utf-8", text.into())
            }),
        );
    }
    Router::new()
        .merge(routes.clone())
        .nest("/api/v1", routes)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .route_layer(middleware::map_response(crate::problem::layer))
        .with_state(app)
}

async fn guard(State(app): State<App>, request: axum::extract::Request, next: Next) -> Response {
    let Ok(_permit) = app.admission.try_acquire() else {
        return crate::problem::overloaded("Service temporarily unavailable");
    };
    if request.uri().to_string().len() > 16_384 {
        return failure(StatusCode::URI_TOO_LONG, "Request target too long");
    }
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

/// An empty batch reads no Plan; it only resolves the credential's tenant.
/// Returns the refusal when the credential does not hold.
async fn authenticate(app: &App, presented: PresentedCredential) -> Option<Response> {
    let client = app.client.clone();
    let wait = app.config.wait;
    let proof = tokio::task::spawn_blocking(move || {
        client.read(
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
        Err(_) => Some(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
        )),
    }
}

async fn custom_filaments(State(app): State<App>) -> Response {
    let files = app.files.clone();
    match tokio::task::spawn_blocking(move || files.custom_filaments()).await {
        Ok(CustomFilaments::Body(body)) => {
            response(StatusCode::OK, "application/json; charset=utf-8", body)
        }
        // Node answers Fastify's TypeError text here; Rust keeps it out.
        Ok(CustomFilaments::NullStore) | Err(_) => {
            failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
    }
}

/// Node's `onSend` hook marks every reply `private, no-store`.
fn response(status: StatusCode, content_type: &str, body: String) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}
