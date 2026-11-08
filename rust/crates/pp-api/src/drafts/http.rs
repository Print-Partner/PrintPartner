use crate::loopback::{
    LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
    require_api_key_tenant,
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{DefaultBodyLimit, Extension, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
};
use pp_storage::{
    auth::{self, AuthFailure, Secret, rebuild::RebuildAuthClient},
    plan_publication::{Outcome as AcceptOutcome, preview::PreviewOutcome},
    required_units::{
        AdmissionFailure, DraftListResponse, Outcome as WorkspaceOutcome, ReconciliationCommand,
        ReconciliationRequest,
    },
    working_drafts::{
        AbandonOutcome, AbandonRequest, DraftIdentity, DraftList, EditRequest, Failure, Outcome,
        PositiveId, Request, WorkingDraftClient, edit_rejection,
    },
};
use serde_json::{Value, json};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

mod lifecycle;

const ADMISSION_PERMITS: usize = 64;
const READ_PERMITS: usize = 64;
const TARGET_LIMIT: usize = 16 * 1024;
const READ_BODY_LIMIT: usize = 1024 * 1024;
const BODY_TIMEOUT: Duration = Duration::from_secs(5);
const READ_DETAIL: &str = "Plan draft data is inconsistent";
const EDIT_DETAIL: &str = "Plan draft update failed";

#[derive(Clone)]
pub struct DraftHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    body_limit: usize,
    /// Storage wait; read and edit handlers consume it.
    wait: Duration,
}

impl DraftHttpConfig {
    pub fn new(origin: &str, api_key_tenant: &str) -> Result<Self> {
        Self::from_startup(
            origin,
            api_key_tenant,
            std::env::var("PP_DESKTOP_REBUILD").ok().as_deref(),
        )
    }

    pub fn from_startup(origin: &str, api_key_tenant: &str, flag: Option<&str>) -> Result<Self> {
        let listener = LoopbackOrigin::parse(origin)?;
        require_api_key_tenant(api_key_tenant)?;
        Ok(Self {
            origin: listener.origin,
            host: listener.host,
            api_key_tenant: api_key_tenant.into(),
            enabled: rebuild_flag_enabled(flag),
            body_limit: 8 * 1024 * 1024,
            wait: Duration::from_millis(100),
        })
    }

    /// Lowers the PATCH body limit and storage wait; raising either fails.
    pub fn with_limits(mut self, body_limit: usize, wait: Duration) -> Result<Self> {
        ensure!(
            (1..=self.body_limit).contains(&body_limit) && wait <= self.wait,
            "Draft limits can only be lowered"
        );
        self.body_limit = body_limit;
        self.wait = wait;
        Ok(self)
    }
}

#[derive(Clone)]
struct Guard {
    config: DraftHttpConfig,
    admission: Arc<Semaphore>,
}

/// Mounts `routes` at the root and `/api/v2` behind loopback admission when
/// `PP_DESKTOP_REBUILD` was exactly `1` or `true` at startup. Otherwise
/// nothing is mounted and every request falls through to 404.
///
/// The guard buffers each body before routing, so Axum's 2 MiB extractor
/// default is disabled and the guard's limits are authoritative.
pub fn admit_draft_routes(config: DraftHttpConfig, routes: Router) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let guard_state = Guard {
        config,
        admission: Arc::new(Semaphore::new(ADMISSION_PERMITS)),
    };
    Router::new()
        .merge(routes.clone())
        .nest("/api/v2", routes)
        .layer(DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(guard_state, guard))
        .route_layer(middleware::map_response(crate::problem::layer))
}

async fn guard(
    State(Guard { config, admission }): State<Guard>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Ok(permit) = admission.try_acquire_owned() else {
        return crate::problem::overloaded("Draft service temporarily unavailable");
    };
    if request.uri().to_string().len() > TARGET_LIMIT {
        return failure(StatusCode::URI_TOO_LONG, "Request target too long");
    }
    if let Err((status, detail)) = admit_request(&config, &request) {
        return failure(status, detail);
    }
    let Some(presented) = principal(request.headers(), &config.api_key_tenant) else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    request.extensions_mut().insert(presented);
    // A keyed write without a usable key is refused before its body is read.
    if keyed_write(request.method(), request.uri().path())
        && idempotency_key(request.headers()).is_none()
    {
        return invalid_request();
    }
    let limit = match *request.method() {
        Method::PATCH | Method::PUT => config.body_limit,
        _ => config.body_limit.min(READ_BODY_LIMIT),
    };
    let (parts, body) = request.into_parts();
    let bytes = match tokio::time::timeout(BODY_TIMEOUT, to_bytes(body, limit)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return failure(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"),
        Err(_) => return failure(StatusCode::REQUEST_TIMEOUT, "Request body timed out"),
    };
    let response = next
        .run(axum::extract::Request::from_parts(parts, Body::from(bytes)))
        .await;
    response.map(|body| {
        Body::new(AdmittedBody {
            body,
            _permit: permit,
        })
    })
}

/// Holds an admission permit until the response body is consumed or dropped.
struct AdmittedBody {
    body: Body,
    _permit: OwnedSemaphorePermit,
}

impl http_body::Body for AdmittedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Bytes>, Self::Error>>> {
        Pin::new(&mut self.body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

fn admit_request(
    config: &DraftHttpConfig,
    request: &axum::extract::Request,
) -> std::result::Result<(), (StatusCode, &'static str)> {
    admit(
        &config.origin,
        &config.host,
        &config.api_key_tenant,
        request,
    )
}

fn failure(status: StatusCode, detail: &str) -> Response {
    crate::problem::response(status, detail)
}

#[cfg(test)]
type WorkerHold = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct App {
    client: WorkingDraftClient,
    auth: RebuildAuthClient,
    wait: Duration,
    waiters: Arc<Semaphore>,
    #[cfg(test)]
    hold: Option<WorkerHold>,
}

/// Result of a storage read that has already accepted the credential.
#[derive(Clone)]
struct Authorized {
    profile: PositiveId,
    outcome: Arc<Outcome>,
}

struct CancelOnDrop {
    flag: Arc<AtomicBool>,
    armed: bool,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::Release);
        }
    }
}

enum ReadError {
    Busy,
    Worker,
    Storage(anyhow::Error),
}

/// Mounts list/workspace reads, Accept previews, part edits, Required-unit
/// reconciliation, abandon, recompute, rebase, Accept and snapshot restore when the startup gate is on. Storage accepts the presented credential before a handler runs.
/// API-key reads also resolve the key so `lastUsedAt` commits as Node does.
/// A disabled gate mounts nothing.
pub fn draft_router(
    config: DraftHttpConfig,
    client: WorkingDraftClient,
    auth: RebuildAuthClient,
) -> Router {
    mount(
        config,
        client,
        auth,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
fn draft_router_with_hold(
    config: DraftHttpConfig,
    client: WorkingDraftClient,
    auth: RebuildAuthClient,
    hold: WorkerHold,
) -> Router {
    mount(config, client, auth, Some(hold))
}

fn mount(
    config: DraftHttpConfig,
    client: WorkingDraftClient,
    auth: RebuildAuthClient,
    #[cfg(test)] hold: Option<WorkerHold>,
) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let app = App {
        client,
        auth,
        wait: config.wait,
        waiters: Arc::new(Semaphore::new(READ_PERMITS)),
        #[cfg(test)]
        hold,
    };
    let routes = Router::new()
        .route("/plans/{id}/drafts", get(list))
        .route("/plans/{id}/drafts/{draft_id}", get(workspace))
        .route(
            "/plans/{id}/drafts/{draft_id}/accept-preview",
            get(accept_preview),
        )
        .route("/plans/{id}/drafts/{draft_id}/header", get(header))
        .route("/plans/{id}/drafts/{draft_id}/parts", patch(edit))
        .route(
            "/plans/{id}/drafts/{draft_id}/reconciliation",
            put(reconcile),
        )
        .route("/plans/{id}/drafts/{draft_id}/abandon", post(abandon))
        .route("/plans/{id}/drafts/recompute", post(lifecycle::recompute))
        .route("/plans/{id}/drafts/save", post(lifecycle::autosave))
        .route(
            "/plans/{id}/drafts/header",
            get(build_header).put(lifecycle::save_header),
        )
        .route(
            "/plans/{id}/drafts/{draft_id}/rebase",
            post(lifecycle::rebase),
        )
        .route(
            "/plans/{id}/drafts/{draft_id}/apply",
            post(lifecycle::apply),
        )
        .route(
            "/plans/{id}/snapshots/{snapshot_id}/restore",
            post(lifecycle::restore),
        )
        .route_layer(middleware::from_fn_with_state(app, authorize));
    admit_draft_routes(config, routes)
}

async fn authorize(
    State(app): State<App>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(presented) = request.extensions_mut().remove::<PresentedCredential>() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        // Handlers must not take `State<App>` — that would type the router as
        // `Router<App>` and break `admit_draft_routes`'s `Router<()>`.
        request.extensions_mut().insert(app);
        request.extensions_mut().insert(presented);
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    let Some((profile, operation)) = read_target(&path) else {
        return invalid_request();
    };
    let (operation_name, draft_id) = match &operation {
        Request::List | Request::BuildHeader => ("list", None),
        Request::Read { draft_id }
        | Request::Diff { draft_id }
        | Request::Workspace { draft_id }
        | Request::Header { draft_id } => ("read", Some(*draft_id)),
        Request::AcceptPreview { draft_id } => ("accept_preview", Some(*draft_id)),
    };
    match load(&app, presented, profile, operation).await {
        Ok(outcome) => {
            request.extensions_mut().insert(Authorized {
                profile,
                outcome: Arc::new(outcome),
            });
            next.run(request).await
        }
        Err(error) => {
            let (class, response) = match error {
                ReadError::Worker => ("worker_failure", internal_error()),
                ReadError::Storage(error) => (error_class(&error), storage_failure(&error)),
                ReadError::Busy => (
                    "read_pool_exhausted",
                    crate::problem::overloaded("Draft service temporarily unavailable"),
                ),
            };
            if response.status().is_server_error() {
                log::error!(
                    target: "pp_api::drafts",
                    "Draft HTTP failed; operation={operation_name} profile_id={profile:?} draft_id={draft_id:?} error_class={class}"
                );
            }
            response
        }
    }
}

async fn load(
    app: &App,
    presented: PresentedCredential,
    profile: PositiveId,
    operation: Request,
) -> std::result::Result<Outcome, ReadError> {
    let Ok(permit) = app.waiters.clone().try_acquire_owned() else {
        return Err(ReadError::Busy);
    };
    let flag = Arc::new(AtomicBool::new(false));
    let mut cancel = CancelOnDrop {
        flag: Arc::clone(&flag),
        armed: true,
    };
    let client = app.client.clone();
    let auth = app.auth.clone();
    let wait = app.wait;
    #[cfg(test)]
    let hold = app.hold.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        #[cfg(test)]
        if let Some(hold) = hold {
            hold();
        }
        if flag.load(Ordering::Acquire) {
            return Err(anyhow::Error::new(Failure::Cancelled));
        }
        if let PresentedCredential::ApiKey { tenant, secret } = &presented {
            let resolved = auth
                .submit(
                    auth::Request::ResolveKey {
                        tenant_id: tenant.clone(),
                        key: Secret::new(secret.clone()),
                    },
                    Arc::clone(&flag),
                    wait,
                )?
                .recv()
                .map_err(|_| AuthFailure::CommitUnknown)??;
            match resolved {
                auth::Outcome::KeyResolved { principal, .. } => {
                    ensure!(
                        principal.is_some_and(|key| key.tenant_id == *tenant),
                        AuthFailure::InvalidCredentials
                    );
                }
                _ => return Err(AuthFailure::Storage.into()),
            }
        }
        client.execute(presented.into_credential(), profile, operation, flag, wait)
    })
    .await;
    cancel.armed = false;
    match joined {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => Err(ReadError::Storage(error)),
        Err(_) => Err(ReadError::Worker),
    }
}

fn read_target(path: &str) -> Option<(PositiveId, Request)> {
    let path = path.strip_prefix("/api/v2").unwrap_or(path);
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    if parts.next()? != "plans" {
        return None;
    }
    let profile = path_id(parts.next()?)?;
    if parts.next()? != "drafts" {
        return None;
    }
    match parts.next() {
        None => parts.next().is_none().then_some((profile, Request::List)),
        Some("header") => parts
            .next()
            .is_none()
            .then_some((profile, Request::BuildHeader)),
        Some(draft) => {
            let draft_id = path_id(draft)?;
            let request = match parts.next() {
                None => Request::Workspace { draft_id },
                Some("accept-preview") => Request::AcceptPreview { draft_id },
                Some("header") => Request::Header { draft_id },
                Some(_) => return None,
            };
            parts.next().is_none().then_some((profile, request))
        }
    }
}

fn path_id(value: &str) -> Option<PositiveId> {
    PositiveId::new(value.parse().ok()?).ok()
}

async fn list(Extension(read): Extension<Authorized>) -> Response {
    match read.outcome.as_ref() {
        Outcome::Listed { drafts } => match listed(read.profile, drafts) {
            Ok(body) => json_response(StatusCode::OK, body),
            Err(_) => internal_error(),
        },
        Outcome::ProfileNotFound => profile_missing(),
        _ => internal_error(),
    }
}

async fn workspace(Extension(read): Extension<Authorized>) -> Response {
    match read.outcome.as_ref() {
        Outcome::Workspace { workspace } => json_response(StatusCode::OK, workspace),
        Outcome::ProfileNotFound => profile_missing(),
        Outcome::DraftNotFound => draft_missing(),
        _ => internal_error(),
    }
}

async fn header(Extension(read): Extension<Authorized>) -> Response {
    match read.outcome.as_ref() {
        Outcome::Header { header } => json_response(StatusCode::OK, header),
        Outcome::ProfileNotFound => profile_missing(),
        Outcome::DraftNotFound => draft_missing(),
        _ => internal_error(),
    }
}

async fn build_header(Extension(read): Extension<Authorized>) -> Response {
    match read.outcome.as_ref() {
        Outcome::BuildHeader { header } => json_response(StatusCode::OK, header),
        Outcome::ProfileNotFound => profile_missing(),
        _ => internal_error(),
    }
}

async fn accept_preview(Extension(read): Extension<Authorized>) -> Response {
    match read.outcome.as_ref() {
        Outcome::AcceptPreview { preview } => present_preview(preview),
        Outcome::ProfileNotFound => profile_missing(),
        Outcome::DraftNotFound => draft_missing(),
        _ => internal_error(),
    }
}

/// A preview is 200. A refusal is the one Accept would return: 404 for a
/// missing draft, otherwise a 409 problem with Accept's outcome kind as `code`
/// and its fields as extensions. Like `conflict`, it has no `detail`.
fn present_preview(preview: &PreviewOutcome) -> Response {
    match preview {
        PreviewOutcome::Ready(preview) => json_response(StatusCode::OK, preview),
        PreviewOutcome::Refused(AcceptOutcome::NotFound) => draft_missing(),
        PreviewOutcome::Refused(refused) => match serde_json::to_value(refused) {
            Ok(Value::Object(mut extensions)) => match extensions.remove("kind") {
                Some(Value::String(code)) => {
                    let problem = crate::problem::problem(StatusCode::CONFLICT, "", Some(&code));
                    crate::problem::build(problem.without_detail(), extensions)
                }
                _ => internal_error(),
            },
            _ => internal_error(),
        },
    }
}

async fn edit(
    Extension(app): Extension<App>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
    body: Bytes,
) -> Response {
    let Some((profile, draft_id)) = edit_target(uri.path()) else {
        return invalid_request();
    };
    let request = match crate::problem::parse_body::<EditRequest>(&body) {
        Ok(request) => request,
        // The untagged edit wire hides the failing member; recover it.
        Err(_) => {
            let rejection = edit_rejection(&body);
            let errors = rejection.map(|error| crate::problem::field_errors(&error));
            return invalid_body(&body, errors.unwrap_or_default());
        }
    };
    let credential = presented.into_credential();
    let work = move |client: &WorkingDraftClient, flag, wait| {
        client.edit(credential, profile, draft_id, request, flag, wait)
    };
    match run_write(&app, work).await {
        Ok(outcome) => present_edit(outcome),
        Err(error) => write_failure(error, "edit", profile, Some(draft_id)),
    }
}

/// Runs one draft write on a blocking worker, holding a read permit, and
/// cancels it if the handler is dropped first.
async fn run_write<T: Send + 'static>(
    app: &App,
    work: impl FnOnce(&WorkingDraftClient, Arc<AtomicBool>, Duration) -> Result<T> + Send + 'static,
) -> std::result::Result<T, ReadError> {
    let Ok(permit) = app.waiters.clone().try_acquire_owned() else {
        return Err(ReadError::Busy);
    };
    let flag = Arc::new(AtomicBool::new(false));
    let mut cancel = CancelOnDrop {
        flag: Arc::clone(&flag),
        armed: true,
    };
    let client = app.client.clone();
    let wait = app.wait;
    #[cfg(test)]
    let hold = app.hold.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        #[cfg(test)]
        if let Some(hold) = hold {
            hold();
        }
        if flag.load(Ordering::Acquire) {
            return Err(anyhow::Error::new(Failure::Cancelled));
        }
        work(&client, flag, wait)
    })
    .await;
    cancel.armed = false;
    match joined {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => Err(ReadError::Storage(error)),
        Err(_) => Err(ReadError::Worker),
    }
}

/// The response for a failed draft write, logged when it is a server error.
fn write_failure(
    error: ReadError,
    operation: &str,
    profile: PositiveId,
    draft_id: Option<PositiveId>,
) -> Response {
    let (class, response) = match error {
        ReadError::Worker => ("worker_failure", edit_internal_error()),
        ReadError::Storage(error) => (
            error_class(&error),
            storage_failure_with(&error, EDIT_DETAIL),
        ),
        ReadError::Busy => (
            "read_pool_exhausted",
            crate::problem::overloaded("Draft service temporarily unavailable"),
        ),
    };
    if response.status().is_server_error() {
        let draft_id = draft_id.map_or_else(|| "none".to_owned(), |id| format!("{id:?}"));
        log::error!(
            target: "pp_api::drafts",
            "Draft HTTP failed; operation={operation} profile_id={profile:?} draft_id={draft_id} error_class={class}"
        );
    }
    response
}

fn edit_target(path: &str) -> Option<(PositiveId, PositiveId)> {
    write_target(path, "parts")
}

/// Node's `idempotencyKey`: the `Idempotency-Key` header trimmed, 1 to 160
/// characters. A header value is visible ASCII, spaces and tabs, so trimming
/// those two matches JavaScript's `trim`.
fn idempotency_key(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_matches([' ', '\t']))
        .filter(|key| (1..=160).contains(&key.len()))
}

/// The writes Node refuses without an `Idempotency-Key`: reconciliation,
/// recompute, rebase and Accept, and the Rust-only draft autosave, draft
/// header save and snapshot restore.
fn keyed_write(method: &Method, path: &str) -> bool {
    match *method {
        Method::PUT => path.ends_with("/reconciliation") || path.ends_with("/drafts/header"),
        Method::POST => [
            "/recompute",
            "/rebase",
            "/apply",
            "/drafts/save",
            "/restore",
        ]
        .iter()
        .any(|action| path.ends_with(action)),
        _ => false,
    }
}

fn reconcile_target(path: &str) -> Option<(PositiveId, PositiveId)> {
    write_target(path, "reconciliation")
}

fn abandon_target(path: &str) -> Option<(PositiveId, PositiveId)> {
    write_target(path, "abandon")
}

fn write_target(path: &str, action: &str) -> Option<(PositiveId, PositiveId)> {
    let path = path.strip_prefix("/api/v2").unwrap_or(path);
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    if parts.next()? != "plans" {
        return None;
    }
    let profile = path_id(parts.next()?)?;
    if parts.next()? != "drafts" {
        return None;
    }
    let draft_id = path_id(parts.next()?)?;
    if parts.next()? != action || parts.next().is_some() {
        return None;
    }
    Some((profile, draft_id))
}

async fn abandon(
    Extension(app): Extension<App>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
    body: Bytes,
) -> Response {
    let Some((profile, draft_id)) = abandon_target(uri.path()) else {
        return invalid_request();
    };
    let request = match crate::problem::parse_body::<AbandonRequest>(&body) {
        Ok(request) => request,
        Err(errors) => return invalid_body(&body, errors),
    };
    let credential = presented.into_credential();
    let work = move |client: &WorkingDraftClient, flag, wait| {
        client.abandon(credential, profile, draft_id, request, flag, wait)
    };
    match run_write(&app, work).await {
        Ok(outcome) => present_abandon(outcome),
        Err(error) => write_failure(error, "abandon", profile, Some(draft_id)),
    }
}

/// Saves and selects a Required-unit reconciliation as Node's
/// `PUT /plans/:id/drafts/:draftId/reconciliation` does. The idempotency key
/// is the `Idempotency-Key` header; a missing, blank or over-long key is 400
/// before the body is read. Outcomes map as an edit's do.
async fn reconcile(
    Extension(app): Extension<App>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((profile, draft_id)) = reconcile_target(uri.path()) else {
        return invalid_request();
    };
    let Some(key) = idempotency_key(&headers) else {
        return invalid_request();
    };
    let request = match crate::problem::parse_body::<ReconciliationRequest>(&body) {
        Ok(request) => request,
        Err(errors) => return invalid_body(&body, errors),
    };
    let Ok(command) =
        ReconciliationCommand::new(profile, draft_id, request, key, presented.into_credential())
    else {
        return invalid_request();
    };
    let work = move |client: &WorkingDraftClient, flag, wait| {
        client.reconciliation().reconcile(command, flag, wait)
    };
    match run_write(&app, work).await {
        Ok(outcome) => present_edit(outcome),
        Err(error) => write_failure(error, "reconcile", profile, Some(draft_id)),
    }
}

/// Maps abandon outcomes to Node's `sendFailure` shapes in `plan-drafts.ts`.
/// Ready returns the draft identity. 409 is `{code}` plus `workspace` with no
/// synthetic conflict `detail`.
fn present_abandon(outcome: AbandonOutcome) -> Response {
    match outcome {
        AbandonOutcome::Ready { draft } => json_response(StatusCode::OK, draft),
        AbandonOutcome::ProfileNotFound => profile_missing(),
        AbandonOutcome::DraftNotFound => draft_missing(),
        AbandonOutcome::DraftChanged { workspace } => {
            conflict("draft_changed", Some(json!(workspace)))
        }
        AbandonOutcome::NotOpen { workspace } => conflict("not_open", Some(json!(workspace))),
    }
}

/// Maps edit outcomes to Node's `sendFailure` shapes in `plan-drafts.ts`.
/// 409 is `{code}` plus `workspace` only when one exists; 422 domain_error is
/// `{code}` only. 404/503 keep their Node `detail`.
fn present_edit(outcome: WorkspaceOutcome) -> Response {
    match outcome {
        WorkspaceOutcome::Ready { workspace } => json_response(StatusCode::OK, workspace),
        WorkspaceOutcome::ProfileNotFound => profile_missing(),
        WorkspaceOutcome::DraftNotFound => draft_missing(),
        WorkspaceOutcome::DraftChanged { workspace } => {
            conflict("draft_changed", workspace.map(|workspace| json!(workspace)))
        }
        WorkspaceOutcome::BaseChanged { workspace } => {
            conflict("base_changed", Some(json!(workspace)))
        }
        WorkspaceOutcome::NotOpen { workspace } => conflict("not_open", Some(json!(workspace))),
        WorkspaceOutcome::AcceptedBaselineRequired => conflict("accepted_baseline_required", None),
        WorkspaceOutcome::IdempotencyConflict => conflict("idempotency_conflict", None),
        WorkspaceOutcome::DomainError { code } => {
            let code = serde_json::to_value(code).ok();
            domain_error(
                code.as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or("domain_error"),
            )
        }
    }
}

/// Node sends no `detail` here, so the problem has none either.
fn conflict(code: &str, workspace: Option<Value>) -> Response {
    let workspace = workspace.map(|workspace| ("workspace", workspace));
    refusal(StatusCode::CONFLICT, code, workspace)
}

/// Node's 422 `{ code }` for a `domain_error`.
fn domain_error(code: &str) -> Response {
    refusal(StatusCode::UNPROCESSABLE_ENTITY, code, [])
}

/// A problem with no `detail`, `code`, and Node's other fields as extension
/// members.
fn refusal<'a>(
    status: StatusCode,
    code: &str,
    fields: impl IntoIterator<Item = (&'a str, Value)>,
) -> Response {
    let extensions = fields
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    let problem = crate::problem::problem(status, "", Some(code));
    crate::problem::build(problem.without_detail(), extensions)
}

fn listed(profile: PositiveId, drafts: &DraftList) -> Result<DraftListResponse> {
    Ok(DraftListResponse {
        profile_id: i64::try_from(profile.get())?,
        drafts: drafts
            .iter()
            .map(DraftIdentity::workspace_identity)
            .collect::<Result<_>>()?,
    })
}

fn error_class(error: &anyhow::Error) -> &'static str {
    if matches!(
        error.downcast_ref::<Failure>(),
        Some(Failure::CorruptOrigin)
    ) {
        "draft_integrity"
    } else if error.is::<AuthFailure>() {
        "auth_storage"
    } else {
        "unexpected_storage"
    }
}

fn storage_failure(error: &anyhow::Error) -> Response {
    storage_failure_with(error, READ_DETAIL)
}

pub(crate) fn storage_failure_with(error: &anyhow::Error, detail: &str) -> Response {
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
            AuthFailure::QueueFull => unavailable("writer_queue_full"),
            AuthFailure::Stopped => unavailable("writer_stopped"),
            AuthFailure::CommitUnknown => unavailable("outcome_unknown"),
            _ => coded(StatusCode::INTERNAL_SERVER_ERROR, detail, "internal_error"),
        };
    }
    if let Some(draft) = error.downcast_ref::<Failure>() {
        return match draft {
            Failure::Cancelled => unavailable("request_cancelled"),
            Failure::QueueFull => unavailable("writer_queue_full"),
            Failure::Stopped => unavailable("writer_stopped"),
            Failure::OutcomeUnknown => unavailable("outcome_unknown"),
            Failure::CorruptOrigin => {
                coded(StatusCode::INTERNAL_SERVER_ERROR, detail, "internal_error")
            }
        };
    }
    if let Some(admission) = error.downcast_ref::<AdmissionFailure>() {
        return unavailable(match admission {
            AdmissionFailure::Cancelled => "request_cancelled",
            AdmissionFailure::QueueFull => "writer_queue_full",
            AdmissionFailure::Stopped => "writer_stopped",
            AdmissionFailure::OutcomeUnknown => "outcome_unknown",
        });
    }
    // Clients may wrap a storage error with their own detail; classify by the
    // first message in the chain that names a known condition.
    let known = error.chain().map(ToString::to_string).find(|message| {
        matches!(
            message.as_str(),
            "Authentication required"
                | "Cancelled before admission"
                | "Writer queue full"
                | "Storage stopped"
                | "Storage stopped without read result"
                | "Writer admission poisoned"
                | "Writer transaction admission deadline expired"
                | "database is locked"
        )
    });
    match known.as_deref().unwrap_or_default() {
        "Authentication required" => failure(StatusCode::UNAUTHORIZED, "Authentication required"),
        "Cancelled before admission" => unavailable("request_cancelled"),
        "Writer queue full" => unavailable("writer_queue_full"),
        "Storage stopped" | "Storage stopped without read result" | "Writer admission poisoned" => {
            unavailable("writer_stopped")
        }
        "Writer transaction admission deadline expired" | "database is locked" => coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "Plan draft update is unavailable",
            "transaction_unavailable",
        ),
        _ => coded(StatusCode::INTERNAL_SERVER_ERROR, detail, "internal_error"),
    }
}

fn internal_error() -> Response {
    coded(
        StatusCode::INTERNAL_SERVER_ERROR,
        READ_DETAIL,
        "internal_error",
    )
}
fn edit_internal_error() -> Response {
    coded(
        StatusCode::INTERNAL_SERVER_ERROR,
        EDIT_DETAIL,
        "internal_error",
    )
}
fn profile_missing() -> Response {
    coded(StatusCode::NOT_FOUND, "Plan not found", "profile_not_found")
}
fn draft_missing() -> Response {
    coded(
        StatusCode::NOT_FOUND,
        "Plan draft not found",
        "draft_not_found",
    )
}
fn unavailable(code: &str) -> Response {
    coded(
        StatusCode::SERVICE_UNAVAILABLE,
        "Draft service temporarily unavailable",
        code,
    )
}
fn invalid_request() -> Response {
    coded(
        StatusCode::BAD_REQUEST,
        "Request is invalid",
        "invalid_request",
    )
}
/// 400 for a body that did not parse: malformed unless it is a JSON object,
/// with a field error when one field is to blame.
fn invalid_body(body: &[u8], errors: Vec<crate::problem::FieldError>) -> Response {
    use crate::problem::{Problem, ProblemType};
    let kind = if crate::problem::is_json_object(body) || !errors.is_empty() {
        ProblemType::Validation
    } else {
        ProblemType::MalformedRequest
    };
    let problem = Problem::new(kind, 400, "Request is invalid").with_code("invalid_request");
    crate::problem::build(problem.with_errors(errors), serde_json::Map::new())
}
fn coded(status: StatusCode, detail: &str, code: &str) -> Response {
    crate::problem::coded(status, detail, code)
}
fn json_response(status: StatusCode, value: impl serde::Serialize) -> Response {
    (status, Json(value)).into_response()
}

#[cfg(test)]
#[path = "http/tests.rs"]
mod tests;
