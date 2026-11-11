use crate::loopback::{
    LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
    require_api_key_tenant,
};
use anyhow::Result;
use axum::{
    Extension, Router,
    body::to_bytes,
    extract::{Path, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use pp_storage::{
    auth::AuthFailure,
    build_graph::{
        BuildCommand, BuildGraphClient, BuildOutcome, Failure as BuildFailure, PositiveId,
        wire::{CreateBuildRequest, ProfileLayers, ProfileList, layer, profile_summary},
    },
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const DEFAULT_BODY_LIMIT: usize = 65_536;

#[derive(Clone)]
pub struct BuildHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    body_limit: usize,
    wait: Duration,
}

impl BuildHttpConfig {
    pub fn new(origin: &str, api_key_tenant: &str) -> Result<Self> {
        Self::from_startup(
            origin,
            api_key_tenant,
            std::env::var("PP_DESKTOP_REBUILD").ok().as_deref(),
        )
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
            body_limit: DEFAULT_BODY_LIMIT,
            wait: Duration::from_millis(100),
        })
    }

    pub fn with_wait(mut self, wait: Duration) -> Result<Self> {
        anyhow::ensure!(wait <= self.wait, "Build HTTP wait may only be reduced");
        self.wait = wait;
        Ok(self)
    }

    pub fn with_body_limit(mut self, body_limit: usize) -> Result<Self> {
        anyhow::ensure!(
            (1..=self.body_limit).contains(&body_limit),
            "Build HTTP body limit may only be reduced"
        );
        self.body_limit = body_limit;
        Ok(self)
    }
}

#[derive(Clone)]
pub(super) struct App {
    config: BuildHttpConfig,
    client: BuildGraphClient,
    admission: Arc<tokio::sync::Semaphore>,
    waiters: Arc<tokio::sync::Semaphore>,
}

impl App {
    pub(super) fn body_limit(&self) -> usize {
        self.config.body_limit
    }
}

struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Which Plan summary shape a mount answers with. Node's `/api/v1` keeps the
/// legacy `remaining_units`/`total_units` fields and refuses Plans whose
/// accepted progress is unavailable; `/` and `/api/v2` send `accepted_progress`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SummaryContract {
    Accepted,
    LegacyV1,
}

impl SummaryContract {
    pub(super) fn of(request: &axum::extract::Request) -> Self {
        request
            .extensions()
            .get::<Self>()
            .copied()
            .unwrap_or(Self::Accepted)
    }
}

/// Mounts Plan list/detail GET+HEAD, create/delete, layer list/mutate, touch,
/// kit-manifest ReadKit plus SaveKit (`PUT`), plan-manifest-builder ReadBuilder,
/// variant-dimensions GET+HEAD plus variant-selection `POST` (GRE-1221), and
/// Plan features GET+PATCH plus decision append `POST` (GRE-1243) at `/`, `/api/v1`, and `/api/v2` when `PP_DESKTOP_REBUILD` was exactly `1` or
/// `true`. Otherwise mounts nothing. Jobs stay unregistered. `/api/v1` answers
/// with the legacy Plan summary (see [`SummaryContract`]).
pub fn build_router(config: BuildHttpConfig, client: BuildGraphClient) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let app = App {
        config,
        client,
        admission: Arc::new(tokio::sync::Semaphore::new(64)),
        waiters: Arc::new(tokio::sync::Semaphore::new(64)),
    };
    let routes = Router::new()
        .route("/plans", get(list).post(create))
        .route(
            "/plans/{id}",
            get(detail).delete(remove).patch(super::header::update),
        )
        .route("/plans/{id}/duplicate", post(super::header::duplicate))
        .route("/plans/{id}/archive", post(super::header::archive))
        .route("/plans/{id}/layers", get(list_layers).post(attach_addon))
        .route("/plans/{id}/layers/base", put(set_base))
        .route(
            "/plans/{id}/layers/{layer_id}",
            put(replace_attachment).delete(detach),
        )
        .route("/plans/{id}/touch", post(touch))
        .route(
            "/plans/{id}/kit-manifest",
            get(super::manifest_options::read_kit).put(super::manifest_options::save_kit),
        )
        .route(
            "/plans/{id}/plan-manifest-builder",
            get(super::manifest_options::read_builder),
        )
        .route("/plans/{id}/variant-dimensions", get(super::variants::read))
        .route(
            "/plans/{id}/variant-selection",
            post(super::variants::select),
        )
        .route(
            "/plans/{id}/features",
            get(super::features::read).patch(super::features::patch),
        )
        .route("/plans/{id}/decisions", post(super::decisions::append));
    Router::new()
        .merge(routes.clone())
        .nest(
            "/api/v1",
            routes.clone().layer(Extension(SummaryContract::LegacyV1)),
        )
        .nest("/api/v2", routes)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .route_layer(middleware::map_response(crate::problem::layer))
        .with_state(app)
}

async fn guard(
    State(app): State<App>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Ok(_permit) = app.admission.try_acquire() else {
        return crate::problem::overloaded("Build service temporarily unavailable");
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
    request.extensions_mut().insert(presented);
    next.run(request).await
}

async fn list(State(app): State<App>, request: axum::extract::Request) -> Response {
    execute(app, request, BuildCommand::List).await
}

async fn detail(
    State(app): State<App>,
    BuildId(build): BuildId,
    request: axum::extract::Request,
) -> Response {
    execute(app, request, BuildCommand::Read { build }).await
}

async fn create(
    State(app): State<App>,
    Received {
        presented,
        path,
        contract,
        bytes,
        ..
    }: Received,
) -> Response {
    let body: CreateBuildRequest = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) if !crate::problem::is_json_object(&bytes) => return unreadable(),
        Err(error) => {
            let message = error.to_string();
            // Node reads `String(body.name ?? "")`, so an object without a
            // name, or with a null one, is a blank name.
            let unnamed = matches!(
                serde_json::from_slice::<Value>(&bytes),
                Ok(Value::Object(fields))
                    if fields.get("name").is_none_or(serde_json::Value::is_null)
            );
            let detail = if unnamed {
                "Profile name is required"
            } else {
                ["Profile name is required", "Profile name is too long"]
                    .into_iter()
                    .find(|known| message.contains(known))
                    .unwrap_or("Request is invalid")
            };
            return failure(StatusCode::BAD_REQUEST, detail);
        }
    };
    run(
        app,
        presented,
        Method::POST,
        path,
        contract,
        BuildCommand::Create {
            name: body.name,
            base_source: body.base_project_id,
        },
    )
    .await
}

async fn remove(
    State(app): State<App>,
    BuildId(build): BuildId,
    request: axum::extract::Request,
) -> Response {
    execute(app, request, BuildCommand::Delete { build }).await
}

async fn list_layers(
    State(app): State<App>,
    BuildId(build): BuildId,
    request: axum::extract::Request,
) -> Response {
    execute(app, request, BuildCommand::ListLayers { build }).await
}

async fn touch(
    State(app): State<App>,
    BuildId(build): BuildId,
    request: axum::extract::Request,
) -> Response {
    execute(app, request, BuildCommand::Touch { build }).await
}

async fn set_base(
    State(app): State<App>,
    BuildId(build): BuildId,
    request: axum::extract::Request,
) -> Response {
    attachment(app, request, Method::PUT, build, None, |source| {
        BuildCommand::SetBase { build, source }
    })
    .await
}

async fn attach_addon(
    State(app): State<App>,
    BuildId(build): BuildId,
    request: axum::extract::Request,
) -> Response {
    attachment(app, request, Method::POST, build, None, |source| {
        BuildCommand::AttachAddon { build, source }
    })
    .await
}

async fn replace_attachment(
    State(app): State<App>,
    Path((id, layer_id)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    let Some(build) = parse_build_id(&id) else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    let Some(layer) = parse_build_id(&layer_id) else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    attachment(app, request, Method::PUT, build, Some(layer), |source| {
        BuildCommand::ReplaceAttachment {
            build,
            layer,
            source,
        }
    })
    .await
}

async fn detach(
    State(app): State<App>,
    Path((id, layer_id)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    let Some(build) = parse_build_id(&id) else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    let Some(layer) = parse_build_id(&layer_id) else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    execute(app, request, BuildCommand::Detach { build, layer }).await
}

/// Runs a layer mutation whose JSON object body names a Source. Node coerces
/// `project_id` with `Number()`, so a JSON number with an integral value
/// (`2.0`, `1e0`) names that Source, and an id that is missing, null or not a
/// positive integer finds no Source: after its Plan check (and, for a
/// replace, its layer check) Node answers 404 "Project not found".
async fn attachment(
    app: App,
    request: axum::extract::Request,
    method: Method,
    build: PositiveId,
    layer: Option<PositiveId>,
    command: impl FnOnce(PositiveId) -> BuildCommand,
) -> Response {
    let Received {
        presented,
        path,
        bytes,
        ..
    } = match receive(&app, request).await {
        Ok(received) => received,
        Err(response) => return *response,
    };
    let source = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(body)) => body
            .get("project_id")
            .and_then(source_id)
            .and_then(|id| PositiveId::new(id).ok()),
        _ => return unreadable(),
    };
    let contract = SummaryContract::Accepted;
    let Some(source) = source else {
        return missing_source(app, presented, path, build, layer).await;
    };
    run(app, presented, method, path, contract, command(source)).await
}

/// Node's answer to a layer mutation that names no Source: the Plan's own
/// refusal if it has one, "Layer not found" for a replace of a layer the Plan
/// does not have, and otherwise 404 "Project not found". Nothing is written.
async fn missing_source(
    app: App,
    presented: PresentedCredential,
    path: String,
    build: PositiveId,
    layer: Option<PositiveId>,
) -> Response {
    match outcome(app, presented, BuildCommand::ListLayers { build }).await {
        Ok(BuildOutcome::Layers { layers, .. })
            if layer.is_some_and(|layer| {
                !layers
                    .iter()
                    .any(|item| u64::try_from(item.id) == Ok(layer.get()))
            }) =>
        {
            failure(StatusCode::NOT_FOUND, "Layer not found")
        }
        Ok(BuildOutcome::Layers { .. }) => failure(StatusCode::NOT_FOUND, "Project not found"),
        Ok(other) => present(&Method::GET, &path, SummaryContract::Accepted, other),
        Err(response) => *response,
    }
}

/// Reads a JSON number the way Node's `Number()` does for an id: any whole
/// number, written as an integer or not. The float bound keeps the cast exact.
fn source_id(value: &Value) -> Option<u64> {
    const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
    value.as_u64().or_else(|| {
        let id = value.as_f64()?;
        (id.fract() == 0.0 && (0.0..=MAX_SAFE).contains(&id)).then_some(id as u64)
    })
}

/// A Plan id path segment that is a positive integer, else 400 "Request is
/// invalid".
struct BuildId(PositiveId);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for BuildId {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Response> {
        let Path(id) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        parse_build_id(&id)
            .map(Self)
            .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Request is invalid"))
    }
}

pub(super) fn parse_build_id(id: &str) -> Option<PositiveId> {
    id.parse::<u64>()
        .ok()
        .and_then(|value| PositiveId::new(value).ok())
}

/// Node reads the id with `Number(id)`, so `0x3`, `3e0` and `" 3 "` name Plan 3.
pub(super) fn number_build_id(id: &str) -> Option<PositiveId> {
    let id = pp_storage::plates::build_id(id)??;
    PositiveId::new(u64::try_from(id).ok()?).ok()
}

/// A Build request with its credential, mount and body read.
pub(super) struct Received {
    pub(super) presented: PresentedCredential,
    pub(super) path: String,
    pub(super) contract: SummaryContract,
    pub(super) json: bool,
    /// Fastify reads a `text/plain` body as a string, never as JSON.
    pub(super) text: bool,
    pub(super) bytes: axum::body::Bytes,
}

pub(super) async fn receive(
    app: &App,
    request: axum::extract::Request,
) -> Result<Received, Box<Response>> {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return Err(Box::new(failure(
            StatusCode::UNAUTHORIZED,
            "Authentication required",
        )));
    };
    let path = request.uri().path().to_owned();
    let contract = SummaryContract::of(&request);
    let media = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    let json = media.is_some_and(|media| media.eq_ignore_ascii_case("application/json"));
    let text = media.is_some_and(|media| media.eq_ignore_ascii_case("text/plain"));
    let Ok(bytes) = to_bytes(request.into_body(), app.body_limit()).await else {
        return Err(Box::new(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request body too large",
        )));
    };
    Ok(Received {
        presented,
        path,
        contract,
        json,
        text,
        bytes,
    })
}

impl axum::extract::FromRequest<App> for Received {
    type Rejection = Response;

    async fn from_request(request: axum::extract::Request, app: &App) -> Result<Self, Response> {
        receive(app, request).await.map_err(|response| *response)
    }
}

/// Node reads a Plan id with `Number()`, so an id that cannot name a Plan
/// (`0`, `abc`) is "Profile not found" once the caller is authenticated. The
/// List command only proves the credential; its Plans are not used.
pub(super) async fn unknown_build(app: App, request: axum::extract::Request) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    missing_build(app, presented).await
}

/// [`unknown_build`] once the request is read.
pub(super) async fn missing_build(app: App, presented: PresentedCredential) -> Response {
    refuse_after_auth(
        app,
        presented,
        failure(StatusCode::NOT_FOUND, "Profile not found"),
    )
    .await
}

/// `refusal` once the credential is proven, as Node's auth hook answers
/// before its route reads the request.
pub(super) async fn refuse_after_auth(
    app: App,
    presented: PresentedCredential,
    refusal: Response,
) -> Response {
    match outcome(app, presented, BuildCommand::List).await {
        Ok(_) => refusal,
        Err(response) => *response,
    }
}

pub(super) async fn execute(
    app: App,
    request: axum::extract::Request,
    command: BuildCommand,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let contract = SummaryContract::of(&request);
    run(app, presented, method, path, contract, command).await
}

pub(super) async fn run(
    app: App,
    presented: PresentedCredential,
    method: Method,
    path: String,
    contract: SummaryContract,
    command: BuildCommand,
) -> Response {
    match outcome(app, presented, command).await {
        Ok(outcome) => present(&method, &path, contract, outcome),
        Err(response) => *response,
    }
}

/// Executes `command` on a blocking worker; a refusal is already a response.
async fn outcome(
    app: App,
    presented: PresentedCredential,
    command: BuildCommand,
) -> Result<BuildOutcome, Box<Response>> {
    let Ok(permit) = app.waiters.clone().try_acquire_owned() else {
        return Err(Box::new(temporarily_unavailable("writer_queue_full")));
    };
    let client = app.client.clone();
    let wait = app.config.wait;
    let cancellation = Cancellation::new();
    let cancelled = cancellation.flag();
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        client.execute(presented.into_credential(), command, cancelled, wait)
    })
    .await
    {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => Err(Box::new(domain_error(error))),
        Err(_) => Err(Box::new(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Build service unavailable",
        ))),
    }
}

fn present(
    method: &Method,
    path: &str,
    contract: SummaryContract,
    outcome: BuildOutcome,
) -> Response {
    let head = *method == Method::HEAD;
    let summary = |value| summary_body(contract, value);
    match outcome {
        BuildOutcome::Listed { profiles } => {
            // Like Node, an integrity failure on any Plan outranks the other
            // refusals, wherever it appears in the list.
            let (profiles, refusals): (Vec<_>, Vec<_>) =
                profiles.into_iter().map(summary).partition(Result::is_ok);
            let refusals: Vec<Refusal> = refusals.into_iter().filter_map(Result::err).collect();
            if refusals.contains(&Refusal::Encode) {
                return encode_unavailable();
            }
            if refusals.contains(&Refusal::Integrity) {
                return Refusal::Integrity.response();
            }
            if !refusals.is_empty() {
                return failure(
                    StatusCode::CONFLICT,
                    "Accepted Plan progress is unavailable for one or more Plans",
                );
            }
            let profiles: Vec<_> = profiles.into_iter().filter_map(Result::ok).collect();
            json_response(StatusCode::OK, &json!(ProfileList { profiles }), head)
        }
        BuildOutcome::Duplicated(copy) => json_response(StatusCode::CREATED, &copy, head),
        BuildOutcome::HasExperiments { builds } => super::header::has_experiments(builds),
        BuildOutcome::RenameTaken { name } => crate::problem::duplicate_build_name(&name),
        BuildOutcome::NameRequired => failure(StatusCode::BAD_REQUEST, "Profile name is required"),
        BuildOutcome::FeatureDisabled => crate::problem::coded(
            StatusCode::FORBIDDEN,
            "Feature orders is disabled.",
            "FEATURE_DISABLED",
        ),
        BuildOutcome::Uncoercible { archive: true } => {
            failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
        BuildOutcome::Uncoercible { archive: false } => failure(
            StatusCode::BAD_REQUEST,
            "Cannot convert object to primitive value",
        ),
        BuildOutcome::FeatureTogglesInvalid => crate::problem::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored feature toggles are invalid.",
            "FEATURE_TOGGLES_INVALID",
        ),
        BuildOutcome::ArchiveRefused(refusal) => super::header::archive_refused(refusal),
        BuildOutcome::Read { profile: value } | BuildOutcome::Touched { profile: value } => {
            match summary(*value) {
                Ok(body) => json_response(StatusCode::OK, &body, head),
                Err(refusal) => refusal.response(),
            }
        }
        BuildOutcome::Created {
            profile: value,
            layers,
        } => {
            let mut body = match summary(*value) {
                Ok(body) => body,
                Err(refusal) => return refusal.response(),
            };
            body["layers"] = json!(layers.into_iter().map(layer).collect::<Vec<_>>());
            json_response(StatusCode::OK, &body, false)
        }
        BuildOutcome::Layers { profile_id, layers } => json_response(
            StatusCode::OK,
            &json!(ProfileLayers {
                profile_id,
                layers: layers.into_iter().map(layer).collect(),
            }),
            head,
        ),
        BuildOutcome::Deleted if *method == Method::DELETE => {
            StatusCode::NO_CONTENT.into_response()
        }
        BuildOutcome::Manifest(outcome) => super::manifest_options::present(method, outcome),
        BuildOutcome::Variants(outcome) => super::variants::present(method, outcome),
        BuildOutcome::PlanFeatures(outcome) => super::features::present(outcome),
        BuildOutcome::Decision(outcome) => super::decisions::present(outcome),
        BuildOutcome::MissingBuild => failure(StatusCode::NOT_FOUND, "Profile not found"),
        BuildOutcome::InvalidBaseSource => failure(StatusCode::BAD_REQUEST, "Project not found"),
        BuildOutcome::MissingSource => failure(StatusCode::NOT_FOUND, "Project not found"),
        BuildOutcome::MissingLayer => failure(StatusCode::NOT_FOUND, "Layer not found"),
        BuildOutcome::DuplicateName { name } => failure(
            StatusCode::BAD_REQUEST,
            &format!("Profile already exists: {name}"),
        ),
        BuildOutcome::DuplicateAttachment { source_name } => {
            let detail = format!("Source \"{source_name}\" is already attached to this build");
            let status = if path.ends_with("/layers") && *method == Method::POST {
                StatusCode::CONFLICT
            } else if path.ends_with("/layers/base") || *method == Method::PUT {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            failure(status, &detail)
        }
        _ => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Build service unavailable",
        ),
    }
}

/// Why the legacy v1 contract cannot present a Plan whose accepted progress
/// is unavailable.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    Integrity,
    CompatibilityDirty,
    Uninitialized,
    ConcurrentUpdate,
    /// The summary did not serialize; not a progress state.
    Encode,
}

impl Refusal {
    pub(super) fn response(self) -> Response {
        match self {
            Self::Integrity => failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Accepted Plan data is inconsistent",
            ),
            Self::CompatibilityDirty => failure(
                StatusCode::CONFLICT,
                "Accepted Plan requires compatibility repair",
            ),
            Self::Uninitialized => failure(
                StatusCode::CONFLICT,
                "Accepted Plan operational state is not initialized",
            ),
            Self::ConcurrentUpdate => failure(
                StatusCode::CONFLICT,
                "Accepted Plan changed; reload and retry",
            ),
            Self::Encode => encode_unavailable(),
        }
    }
}

/// One Plan summary in `contract`'s shape, as Node's `presentProfile` builds it.
fn summary_body(
    contract: SummaryContract,
    value: pp_storage::build_graph::ProfileSummary,
) -> Result<serde_json::Value, Refusal> {
    use pp_storage::build_graph::wire::{
        AcceptedProgressSummary, AcceptedProgressUnavailableReason,
    };
    let summary = profile_summary(value);
    let units = match (&contract, &summary.accepted_progress) {
        (SummaryContract::Accepted, _) => None,
        (
            SummaryContract::LegacyV1,
            AcceptedProgressSummary::Ready {
                total_units,
                remaining_units,
            },
        ) => Some((*remaining_units, *total_units)),
        (SummaryContract::LegacyV1, AcceptedProgressSummary::Empty) => Some((0, 0)),
        (SummaryContract::LegacyV1, AcceptedProgressSummary::Unavailable { reason }) => {
            return Err(match reason {
                AcceptedProgressUnavailableReason::Integrity => Refusal::Integrity,
                AcceptedProgressUnavailableReason::CompatibilityDirty => {
                    Refusal::CompatibilityDirty
                }
                AcceptedProgressUnavailableReason::Uninitialized => Refusal::Uninitialized,
                AcceptedProgressUnavailableReason::ConcurrentUpdate => Refusal::ConcurrentUpdate,
            });
        }
    };
    let mut body = serde_json::to_value(summary).map_err(|_| Refusal::Encode)?;
    if let (Some((remaining, total)), Some(fields)) = (units, body.as_object_mut()) {
        fields.remove("accepted_progress");
        fields.insert("remaining_units".into(), json!(remaining));
        fields.insert("total_units".into(), json!(total));
    }
    Ok(body)
}

pub(crate) fn domain_error(error: anyhow::Error) -> Response {
    if let Some(refused) =
        crate::problem::admission_response(&error, "Build service temporarily unavailable")
    {
        return refused;
    }
    if let Some(auth) = error.downcast_ref::<AuthFailure>() {
        return match auth {
            AuthFailure::SessionRequired
            | AuthFailure::InvalidCredentials
            | AuthFailure::CredentialChanged => {
                failure(StatusCode::UNAUTHORIZED, "Authentication required")
            }
            AuthFailure::OwnerMappingRequired => failure(
                StatusCode::FORBIDDEN,
                "Explicit account owner mapping is required",
            ),
            AuthFailure::QueueFull => temporarily_unavailable("writer_queue_full"),
            AuthFailure::Stopped => temporarily_unavailable("writer_stopped"),
            AuthFailure::CommitUnknown => temporarily_unavailable("outcome_unknown"),
            _ => failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Build service unavailable",
            ),
        };
    }
    if let Some(kind) = error.downcast_ref::<BuildFailure>() {
        let code = match kind {
            BuildFailure::Cancelled => "request_cancelled",
            BuildFailure::QueueFull => "writer_queue_full",
            BuildFailure::Stopped => "writer_stopped",
            BuildFailure::OutcomeUnknown => "outcome_unknown",
            _ => {
                return failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Build service unavailable",
                );
            }
        };
        return temporarily_unavailable(code);
    }
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Build service unavailable",
    )
}

fn json_response(status: StatusCode, value: &impl serde::Serialize, head: bool) -> Response {
    match encode_json_response(status, value, head) {
        Ok(response) => response,
        Err(()) => encode_unavailable(),
    }
}

/// Lossless JSON bytes from storage (`into_success_body`), including UTF-16 ids.
pub(super) fn bytes_response(status: StatusCode, body: Vec<u8>, head: bool) -> Response {
    let length = body.len();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, length)
        .body(axum::body::Body::from(if head { Vec::new() } else { body }))
        .unwrap_or_else(|_| encode_unavailable())
}

fn encode_json_response(
    status: StatusCode,
    value: &impl serde::Serialize,
    head: bool,
) -> std::result::Result<Response, ()> {
    let body = serde_json::to_vec(value).map_err(|_| ())?;
    let length = body.len();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, length)
        .body(axum::body::Body::from(if head { Vec::new() } else { body }))
        .map_err(|_| ())
}

/// 400 for a body that is not a JSON object: malformed, not invalid.
pub(super) fn unreadable() -> Response {
    crate::problem::typed(
        crate::problem::ProblemType::MalformedRequest,
        StatusCode::BAD_REQUEST,
        "Request is invalid",
    )
}

/// 503 whose `code` gives the cause: overloaded, stopped or outcome unknown.
fn temporarily_unavailable(code: &str) -> Response {
    crate::problem::coded(
        StatusCode::SERVICE_UNAVAILABLE,
        "Build service temporarily unavailable",
        code,
    )
}

fn encode_unavailable() -> Response {
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Build service unavailable",
    )
}

pub(super) fn failure(status: StatusCode, detail: &str) -> Response {
    crate::problem::response(status, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pp_storage::build_graph::{
        AcceptedProgress, AcceptedProgressUnavailable, PlanFreshness as DomainFreshness,
        PlanStaleReason as DomainStaleReason, PlanUntrackedReason as DomainUntrackedReason,
        ProfileSummary as DomainSummary, wire::AcceptedProgressSummary,
        wire::AcceptedProgressUnavailableReason,
    };

    #[test]
    fn unavailable_freshness_preserves_integrity_code() {
        let summary = profile_summary(DomainSummary {
            id: 7,
            name: "Broken".into(),
            order_number: None,
            special_request: None,
            part_count: 0,
            accepted_progress: AcceptedProgress::Unavailable(
                AcceptedProgressUnavailable::Integrity,
            ),
            build_stale: false,
            freshness: DomainFreshness::Unavailable {
                code: "accepted_inputs".into(),
            },
            archived_at: None,
            last_used_at: None,
            customer: None,
            due_date: None,
        });
        assert_eq!(summary.id, 7);
        assert!(matches!(
            summary.accepted_progress,
            AcceptedProgressSummary::Unavailable {
                reason: AcceptedProgressUnavailableReason::Integrity
            }
        ));
        let wire = serde_json::to_value(&summary.freshness).unwrap();
        assert_eq!(
            wire,
            json!({"status":"unavailable","code":"accepted_inputs"})
        );
    }

    #[test]
    fn touch_presenter_preserves_stale_and_untracked_reasons() {
        let wire = serde_json::to_value(profile_summary(DomainSummary {
            id: 3,
            name: "Stale".into(),
            order_number: None,
            special_request: None,
            part_count: 0,
            accepted_progress: AcceptedProgress::Empty,
            build_stale: true,
            freshness: DomainFreshness::Stale {
                accepted_input_set_id: 11,
                accepted_at: "accepted".into(),
                reasons: vec![
                    DomainStaleReason::PlanConfigurationChanged,
                    DomainStaleReason::SourceRevisionChanged {
                        source_id: 1,
                        source_name: "Source".into(),
                        accepted_revision_id: 2,
                        current_revision_id: 3,
                    },
                ],
                untracked_sources: vec![DomainUntrackedReason::SourceRevisionUntracked {
                    source_id: 7,
                    source_name: "Untracked".into(),
                }],
            },
            archived_at: None,
            last_used_at: Some("used".into()),
            customer: Some("Acme".into()),
            due_date: Some("2026-12-01".into()),
        }))
        .unwrap();
        assert_eq!(wire["build_stale"], true);
        assert_eq!(
            wire["freshness"],
            json!({
                "status": "stale",
                "accepted_input_set_id": 11,
                "accepted_at": "accepted",
                "reasons": [
                    {"kind": "plan_configuration_changed"},
                    {
                        "kind": "source_revision_changed",
                        "source_id": 1,
                        "source_name": "Source",
                        "accepted_revision_id": 2,
                        "current_revision_id": 3
                    }
                ],
                "untracked_sources": [{
                    "kind": "source_revision_untracked",
                    "source_id": 7,
                    "source_name": "Untracked"
                }]
            })
        );
        assert!(wire.get("layers").is_none());
    }

    fn plan(id: i64, progress: AcceptedProgress) -> DomainSummary {
        DomainSummary {
            id,
            name: format!("Plan {id}"),
            order_number: None,
            special_request: None,
            part_count: 0,
            accepted_progress: progress,
            build_stale: false,
            freshness: DomainFreshness::Unavailable {
                code: "accepted_inputs".into(),
            },
            archived_at: None,
            last_used_at: None,
            customer: None,
            due_date: None,
        }
    }

    async fn answer(contract: SummaryContract, outcome: BuildOutcome) -> (u16, serde_json::Value) {
        let response = present(&Method::GET, "/plans", contract, outcome);
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            crate::problem::without_envelope(serde_json::from_slice(&bytes).unwrap()),
        )
    }

    /// Node's legacy v1 projection (`toLegacyProfileSummary` and
    /// `sendLegacyFailure` in routes/plans.ts) for each accepted progress state.
    #[tokio::test]
    async fn legacy_v1_summaries_match_node_projection_and_refusals() {
        use AcceptedProgressUnavailable::{CompatibilityDirty, Integrity, Uninitialized};
        let ready = || {
            plan(
                1,
                AcceptedProgress::Ready {
                    total_units: 5,
                    remaining_units: 2,
                },
            )
        };
        let read = |value| BuildOutcome::Read {
            profile: Box::new(value),
        };
        let (status, body) = answer(SummaryContract::LegacyV1, read(ready())).await;
        assert_eq!(status, 200);
        assert_eq!(
            (&body["remaining_units"], &body["total_units"]),
            (&json!(2), &json!(5))
        );
        assert!(body.get("accepted_progress").is_none());
        let (_, accepted) = answer(SummaryContract::Accepted, read(ready())).await;
        assert_eq!(
            accepted["accepted_progress"],
            json!({"kind": "ready", "total_units": 5, "remaining_units": 2})
        );

        for (reason, status, detail) in [
            (Integrity, 500, "Accepted Plan data is inconsistent"),
            (
                CompatibilityDirty,
                409,
                "Accepted Plan requires compatibility repair",
            ),
            (
                Uninitialized,
                409,
                "Accepted Plan operational state is not initialized",
            ),
        ] {
            let value = plan(2, AcceptedProgress::Unavailable(reason));
            assert_eq!(
                answer(SummaryContract::LegacyV1, read(value)).await,
                (status, json!({"detail": detail}))
            );
        }

        // An integrity failure anywhere in the list outranks an earlier
        // unavailable Plan, as in Node's preflight.
        let listed = |progress: Vec<AcceptedProgress>| BuildOutcome::Listed {
            profiles: progress
                .into_iter()
                .enumerate()
                .map(|(index, progress)| plan(index as i64 + 1, progress))
                .collect(),
        };
        assert_eq!(
            answer(
                SummaryContract::LegacyV1,
                listed(vec![
                    AcceptedProgress::Empty,
                    AcceptedProgress::Unavailable(CompatibilityDirty),
                    AcceptedProgress::Unavailable(Integrity),
                ])
            )
            .await,
            (500, json!({"detail": "Accepted Plan data is inconsistent"}))
        );
        assert_eq!(
            answer(
                SummaryContract::LegacyV1,
                listed(vec![
                    AcceptedProgress::Empty,
                    AcceptedProgress::Unavailable(Uninitialized),
                ])
            )
            .await,
            (
                409,
                json!({"detail": "Accepted Plan progress is unavailable for one or more Plans"})
            )
        );
        let (status, body) = answer(
            SummaryContract::Accepted,
            listed(vec![AcceptedProgress::Unavailable(Integrity)]),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(
            body["profiles"][0]["accepted_progress"],
            json!({"kind": "unavailable", "reason": "integrity"})
        );
    }
}
