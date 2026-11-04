//! Gated HTTP list, read-one, and job WebSocket admission (`GET`/`HEAD`).
//!
//! Two serializer permits bound JSON construction. A successful response keeps
//! its permit until the body is sent or the client drops the connection.
//! A read or encode failure drops the permit before the error response returns.
//! `/ws/jobs/{id}` admits the caller with `read_public` before `subscribe`.
//! A missing or foreign job upgrades and closes `1008` `Job not found`.
//! An admitted socket sends the public snapshot, then later snapshots, and
//! closes when the job is terminal or the stream is revoked.
//! `HEAD` on that route admits the same way and does not subscribe.
use crate::loopback::{
    LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
    require_api_key_tenant,
};
use anyhow::Result;
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{
        Path, State,
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
    },
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use pp_storage::{
    auth::{AuthFailure, Secret},
    jobs::{
        AtomicJobClient, Credential, FrameAdmission, JobAccessFailure, JobSnapshot,
        JobSubscription, LegacyListFilter, LegacyProfileFilter, LegacyStatusFilter,
        NonblockingSend, PreparedEvent, RetainedJobs, StreamClose,
    },
};
use serde_json::json;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::OwnedSemaphorePermit;

const SERIALIZERS: usize = 2;

#[derive(Clone)]
pub struct JobsHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    wait: Duration,
}

impl JobsHttpConfig {
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
            wait: Duration::from_millis(100),
        })
    }

    pub fn with_wait(mut self, wait: Duration) -> Result<Self> {
        anyhow::ensure!(wait <= self.wait, "Jobs HTTP wait may only be reduced");
        self.wait = wait;
        Ok(self)
    }
}

#[derive(Clone)]
struct App {
    config: JobsHttpConfig,
    client: AtomicJobClient,
    admission: Arc<tokio::sync::Semaphore>,
    serializers: Arc<tokio::sync::Semaphore>,
}

struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Mounts `GET`/`HEAD` `/api/v1/jobs`, read-one aliases `/jobs/{id}` and
/// `/api/v1/jobs/{id}`, and `/ws/jobs/{id}` when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`. Otherwise mounts nothing. Mutation routes stay
/// unregistered.
pub fn jobs_router(config: JobsHttpConfig, client: AtomicJobClient) -> Router {
    if !config.enabled {
        return Router::new();
    }
    let app = App {
        config,
        client,
        admission: Arc::new(tokio::sync::Semaphore::new(64)),
        serializers: Arc::new(tokio::sync::Semaphore::new(SERIALIZERS)),
    };
    Router::new()
        .route("/api/v1/jobs", get(list))
        .route("/jobs/{id}", get(detail))
        .route("/api/v1/jobs/{id}", get(detail))
        .route("/ws/jobs/{id}", get(stream).head(stream_head))
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
        return crate::problem::overloaded("Jobs service temporarily unavailable");
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
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let filter = legacy_filter(request.uri().query());
    let head = *request.method() == Method::HEAD;
    let Ok(permit) = app.serializers.clone().try_acquire_owned() else {
        return crate::problem::overloaded("Jobs service temporarily unavailable");
    };
    let client = app.client.clone();
    let wait = app.config.wait;
    let cancellation = Cancellation::new();
    let cancelled = cancellation.0.clone();
    finish_retained(
        tokio::task::spawn_blocking(move || {
            let jobs = client.list_retained(job_credential(presented), filter, &cancelled, wait)?;
            let body = serialize_retained_list(&jobs)?;
            Ok::<_, anyhow::Error>((body, permit))
        })
        .await,
        head,
    )
}

async fn detail(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let head = *request.method() == Method::HEAD;
    let Ok(permit) = app.serializers.clone().try_acquire_owned() else {
        return crate::problem::overloaded("Jobs service temporarily unavailable");
    };
    let client = app.client.clone();
    let wait = app.config.wait;
    let cancellation = Cancellation::new();
    let cancelled = cancellation.0.clone();
    finish_retained(
        tokio::task::spawn_blocking(move || {
            let job = client.read_public(job_credential(presented), id, &cancelled, wait)?;
            let body = serialize_retained(&job)?;
            Ok::<_, anyhow::Error>((body, permit))
        })
        .await,
        head,
    )
}

/// Upgrade `/ws/jobs/{id}` only after `authenticate` accepts the caller.
/// A missing or foreign job still upgrades, then closes `1008` `Job not found`
/// and does not subscribe. An accepted caller is subscribed, then the socket
/// receives public snapshots until the job ends or the stream closes.
async fn stream(
    State(app): State<App>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
    request: axum::extract::Request,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    match authenticate(&app, presented.clone(), id.clone()).await {
        Err(Admission::Rejected(response)) => return *response,
        Err(Admission::Missing) => return ws.on_upgrade(close_missing),
        Ok(()) => {}
    }
    let client = app.client.clone();
    let wait = app.config.wait;
    let cancellation = Cancellation::new();
    let cancelled = cancellation.0.clone();
    let subscription = match tokio::task::spawn_blocking(move || {
        client.subscribe(job_credential(presented), id, &cancelled, wait)
    })
    .await
    {
        Ok(Ok(subscription)) => subscription,
        Ok(Err(error)) => return domain_error(error),
        Err(_) => {
            return failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Jobs service unavailable",
            );
        }
    };
    ws.on_upgrade(move |socket| pump(socket, subscription, wait))
}

/// `HEAD /ws/jobs/{id}` runs the same admission as the upgrade and returns
/// an empty body. It does not call `subscribe`.
async fn stream_head(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    match authenticate(&app, presented, id).await {
        Ok(()) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, 0)
            .body(Body::empty())
            .unwrap_or_else(|_| {
                failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Jobs service unavailable",
                )
            }),
        Err(Admission::Missing) => failure(StatusCode::NOT_FOUND, "Job not found"),
        Err(Admission::Rejected(response)) => *response,
    }
}

enum Admission {
    Missing,
    Rejected(Box<Response>),
}

/// Confirms the presented credential can read this job. `read_public` resolves
/// the session or routed key, including expiry and tenant, and does not
/// register a stream. Callers subscribe only after this returns `Ok`.
async fn authenticate(
    app: &App,
    presented: PresentedCredential,
    id: String,
) -> Result<(), Admission> {
    let client = app.client.clone();
    let wait = app.config.wait;
    let cancellation = Cancellation::new();
    let cancelled = cancellation.0.clone();
    match tokio::task::spawn_blocking(move || {
        client.read_public(job_credential(presented), id, &cancelled, wait)
    })
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(classify(error)),
        Err(_) => Err(Admission::Rejected(Box::new(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Jobs service unavailable",
        )))),
    }
}

fn classify(error: anyhow::Error) -> Admission {
    if let Some(access) = error.downcast_ref::<JobAccessFailure>()
        && matches!(
            access,
            JobAccessFailure::NotFound | JobAccessFailure::InternalOnly
        )
    {
        return Admission::Missing;
    }
    Admission::Rejected(Box::new(domain_error(error)))
}

struct Outgoing {
    status: String,
    body: String,
}

fn encode_event(job: &JobSnapshot) -> Result<Outgoing> {
    Ok(Outgoing {
        status: job.status.to_owned(),
        body: String::from_utf8(serialize_retained(job)?)?,
    })
}

fn terminal(status: &str) -> bool {
    matches!(status, "done" | "error" | "cancelled")
}

fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from(reason),
    }))
}

async fn send_close(socket: &mut WebSocket, code: u16, reason: &str) {
    let _ = socket.send(close_frame(code, reason)).await;
}

async fn close_missing(mut socket: WebSocket) {
    send_close(&mut socket, 1008, "Job not found").await;
}

fn close_reason(reason: StreamClose) -> (u16, &'static str) {
    match reason {
        StreamClose::Finished => (1000, ""),
        StreamClose::Revoked => (1008, "Session revoked"),
        StreamClose::Expired => (1008, "Session expired"),
        StreamClose::Lagged | StreamClose::OwnerStopped => (1011, "Job stream unavailable"),
    }
}

/// Sends the queued public snapshot, then waits for the next one. A terminal
/// snapshot is followed by a normal close. Revocation and expiry close `1008`
/// and send nothing further. `admit` accepts a frame before the socket write,
/// so a snapshot admitted before logout can still be written after logout
/// returns. The next close is `1008`. No frame admitted after revocation is sent.
async fn pump(mut socket: WebSocket, subscription: JobSubscription, wait: Duration) {
    loop {
        let prepare_on = subscription.clone();
        let prepared = tokio::task::spawn_blocking(move || prepare_on.prepare(wait, encode_event));
        tokio::pin!(prepared);
        let event = loop {
            tokio::select! {
                biased;
                incoming = socket.recv() => {
                    match incoming {
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                        Some(Ok(_)) => {}
                    }
                }
                joined = &mut prepared => {
                    break match joined {
                        Ok(Ok(event)) => event,
                        _ => {
                            send_close(&mut socket, 1011, "Job stream unavailable").await;
                            return;
                        }
                    };
                }
            }
        };
        match event {
            PreparedEvent::Pending => {}
            PreparedEvent::Closed(reason) => {
                let (code, text) = close_reason(reason);
                send_close(&mut socket, code, text).await;
                return;
            }
            PreparedEvent::Frame(frame) => {
                let mut outgoing = None;
                let admission = subscription.admit(frame, |event| {
                    outgoing = Some(event);
                    NonblockingSend::Accepted
                });
                match admission {
                    FrameAdmission::Admitted => {
                        let Some(event) = outgoing else {
                            return;
                        };
                        if socket
                            .send(Message::Text(Utf8Bytes::from(event.body)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if terminal(&event.status) {
                            send_close(&mut socket, 1000, "").await;
                            return;
                        }
                    }
                    FrameAdmission::Closed(reason) => {
                        let (code, text) = close_reason(reason);
                        send_close(&mut socket, code, text).await;
                        return;
                    }
                    FrameAdmission::Disconnected | FrameAdmission::Superseded => return,
                }
            }
        }
    }
}

/// A joined read error already dropped its permit inside the blocking task.
/// A successful body keeps the permit until that body is dropped.
fn finish_retained(
    joined: std::result::Result<
        Result<(Vec<u8>, OwnedSemaphorePermit), anyhow::Error>,
        tokio::task::JoinError,
    >,
    head: bool,
) -> Response {
    match joined {
        Ok(Ok((body, permit))) => retained_response(StatusCode::OK, body, head, permit),
        Ok(Err(error)) => domain_error(error),
        Err(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Jobs service unavailable",
        ),
    }
}

fn serialize_retained_list(jobs: &RetainedJobs) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({ "jobs": &**jobs }))?)
}

/// Node's read-one returns the stored public snapshot. The list keeps
/// `updated_at`; this read-one encoding removes it.
fn serialize_retained(job: &JobSnapshot) -> Result<Vec<u8>> {
    let mut body = serde_json::to_value(job)?;
    if let Some(fields) = body.as_object_mut() {
        fields.remove("updated_at");
    }
    Ok(serde_json::to_vec(&body)?)
}

/// Holds a serializer permit until hyper finishes the body or the client drops it.
struct RetainedBody {
    inner: Body,
    _permit: OwnedSemaphorePermit,
}

impl http_body::Body for RetainedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Bytes>, Self::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

fn retained_response(
    status: StatusCode,
    body: Vec<u8>,
    head: bool,
    permit: OwnedSemaphorePermit,
) -> Response {
    let length = body.len();
    let bytes = if head { Vec::new() } else { body };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, length)
        .body(Body::new(RetainedBody {
            inner: Body::from(bytes),
            _permit: permit,
        }))
        .unwrap_or_else(|_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Jobs service unavailable",
            )
        })
}

fn job_credential(presented: PresentedCredential) -> Credential {
    match presented {
        PresentedCredential::Session(value) => Credential::Session(Secret::new(value)),
        PresentedCredential::ApiKey { tenant, secret } => Credential::RoutedKey {
            tenant,
            key: Secret::new(secret),
        },
    }
}

/// Node's `/api/v1/jobs` query: `status`, `since` (RFC3339 → millis), `profile_id`.
fn legacy_filter(query: Option<&str>) -> LegacyListFilter {
    let mut values: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(query) = query
        && let Ok(url) = reqwest::Url::parse(&format!("http://localhost/?{query}"))
    {
        for (key, value) in url.query_pairs() {
            values
                .entry(key.into_owned())
                .or_default()
                .push(value.into_owned());
        }
    }
    let status = match values.get("status").map(Vec::as_slice) {
        None | Some([]) => LegacyStatusFilter::Any,
        Some([value]) if value.is_empty() => LegacyStatusFilter::Any,
        Some([value])
            if ["pending", "running", "done", "error", "cancelled"].contains(&value.as_str()) =>
        {
            LegacyStatusFilter::Exact(value.clone())
        }
        _ => LegacyStatusFilter::NoMatch,
    };
    let profile = match values.get("profile_id").map(Vec::as_slice) {
        None | Some([]) => LegacyProfileFilter::Any,
        Some([value]) if value.is_empty() => LegacyProfileFilter::Any,
        Some([value]) => parse_node_profile_id(value)
            .map(LegacyProfileFilter::Exact)
            .unwrap_or(LegacyProfileFilter::NoMatch),
        _ => LegacyProfileFilter::NoMatch,
    };
    let since_millis = match values.get("since").map(Vec::as_slice) {
        Some([value]) => {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                .ok()
                .and_then(|value| i64::try_from(value.unix_timestamp_nanos() / 1_000_000).ok())
        }
        _ => None,
    };
    LegacyListFilter {
        status,
        since_millis,
        profile,
    }
}

/// Node `Number(profile_id)` for the literal query fixtures (positive safe ints).
fn parse_node_profile_id(value: &str) -> Option<u64> {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    let parsed = value.trim().parse::<f64>().ok()?;
    (parsed.is_finite()
        && parsed > 0.0
        && parsed.fract() == 0.0
        && parsed <= MAX_SAFE_INTEGER as f64)
        .then_some(parsed as u64)
}

pub(crate) fn domain_error(error: anyhow::Error) -> Response {
    if let Some(access) = error.downcast_ref::<JobAccessFailure>() {
        return match access {
            JobAccessFailure::NotFound | JobAccessFailure::InternalOnly => {
                failure(StatusCode::NOT_FOUND, "Job not found")
            }
            JobAccessFailure::Capacity => {
                crate::problem::overloaded("Jobs service temporarily unavailable")
            }
            JobAccessFailure::Stopped => failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Jobs service temporarily unavailable",
            ),
        };
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
            AuthFailure::QueueFull => {
                crate::problem::overloaded("Jobs service temporarily unavailable")
            }
            AuthFailure::CommitUnknown => crate::problem::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "Jobs service temporarily unavailable",
                "outcome_unknown",
            ),
            AuthFailure::Stopped => failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Jobs service temporarily unavailable",
            ),
            _ => failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Jobs service unavailable",
            ),
        };
    }
    failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Jobs service unavailable",
    )
}

fn failure(status: StatusCode, detail: &str) -> Response {
    crate::problem::response(status, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_body_drop_releases_permit() {
        let serializers = Arc::new(tokio::sync::Semaphore::new(1));
        let Some(permit) = serializers.clone().try_acquire_owned().ok() else {
            panic!("semaphore refused the only permit");
        };
        assert!(serializers.try_acquire().is_err());
        let body = RetainedBody {
            inner: Body::from(Vec::<u8>::new()),
            _permit: permit,
        };
        drop(body);
        assert!(serializers.try_acquire().is_ok());
    }

    #[test]
    fn retained_response_holds_permit_until_drop() {
        let serializers = Arc::new(tokio::sync::Semaphore::new(SERIALIZERS));
        let Some(first) = serializers.clone().try_acquire_owned().ok() else {
            panic!("first serializer permit was refused");
        };
        let Some(second) = serializers.clone().try_acquire_owned().ok() else {
            panic!("second serializer permit was refused");
        };
        let response_a =
            retained_response(StatusCode::OK, br#"{"ok":true}"#.to_vec(), false, first);
        let response_b =
            retained_response(StatusCode::OK, br#"{"ok":true}"#.to_vec(), false, second);
        assert!(serializers.try_acquire().is_err());
        drop(response_a);
        assert!(serializers.try_acquire().is_ok());
        drop(response_b);
    }
}
