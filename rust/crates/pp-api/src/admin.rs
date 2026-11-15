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
    pub(crate) wait: Duration,
    pub(crate) server_updates: bool,
}

impl AdminHttpConfig {
    /// Only the standalone server opts into process-environment update checks.
    pub fn with_server_update_checks(mut self, enabled: bool) -> Self {
        self.server_updates = enabled;
        self
    }

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
            server_updates: false,
        })
    }
}

/// Loopback admission and credential proof shared by the routes that
/// authenticate with an empty `ReadClient` batch.
#[derive(Clone)]
pub(crate) struct Gate {
    config: AdminHttpConfig,
    client: ReadClient,
    admission: Arc<tokio::sync::Semaphore>,
    release_after_auth: bool,
}

impl Gate {
    /// `None` when `PP_DESKTOP_REBUILD` was not exactly `1` or `true`.
    pub(crate) fn new(config: AdminHttpConfig, client: ReadClient) -> Option<Self> {
        config.enabled.then(|| Self {
            config,
            client,
            admission: Arc::new(tokio::sync::Semaphore::new(64)),
            release_after_auth: false,
        })
    }

    /// [`Self::new`] plus the configured storage wait, which `new` consumes.
    pub(crate) fn with_wait(
        config: AdminHttpConfig,
        client: ReadClient,
    ) -> Option<(Self, Duration)> {
        let wait = config.wait;
        Self::new(config, client).map(|gate| (gate, wait))
    }

    pub(crate) fn authentication_only(mut self) -> Self {
        self.release_after_auth = true;
        self
    }

    /// Run `routes` behind this gate, with problem responses for refusals.
    pub(crate) fn guard<S: Clone + Send + Sync + 'static>(self, routes: Router<S>) -> Router<S> {
        routes
            .layer(middleware::from_fn_with_state(self, guard))
            .route_layer(middleware::map_response(crate::problem::layer))
    }
}

#[derive(Clone)]
struct App {
    files: AdminFiles,
}

/// Mounts `GET`/`HEAD` `/legal/{summary,license,attribution,third-party}` and
/// `/filaments/custom` at `/` and `/api/v1`, where Node registers its core
/// routes, when `PP_DESKTOP_REBUILD` was exactly `1` or `true`; otherwise
/// nothing. Every route first proves the credential with an empty
/// `ReadClient` batch, as Node's auth hook runs before each handler.
pub fn admin_router(config: AdminHttpConfig, client: ReadClient, files: AdminFiles) -> Router {
    let Some(gate) = Gate::new(config, client) else {
        return Router::new();
    };
    let app = App { files };
    let mut routes = Router::new().route("/filaments/custom", get(custom_filaments));
    for (path, text) in LEGAL {
        routes = routes.route(
            path,
            get(move || async move {
                response(StatusCode::OK, "text/plain; charset=utf-8", text.into())
            }),
        );
    }
    gate.guard(Router::new().merge(routes.clone()).nest("/api/v1", routes))
        .with_state(app)
}

/// A request that passes carries its `PresentedCredential` as an extension.
async fn guard(
    State(app): State<Gate>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
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
    if let Some(refused) = authenticate(&app, presented.clone()).await {
        return refused;
    }
    if app.release_after_auth {
        drop(_permit);
    }
    request.extensions_mut().insert(presented);
    next.run(request).await
}

/// An empty batch reads no Plan; it only resolves the credential's tenant.
/// Returns the refusal when the credential does not hold.
async fn authenticate(app: &Gate, presented: PresentedCredential) -> Option<Response> {
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
pub(crate) fn response(status: StatusCode, content_type: &str, body: String) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}
