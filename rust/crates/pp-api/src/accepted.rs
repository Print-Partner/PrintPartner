//! Accepted Plan read routes (GRE-415): one consistent `ReadClient` snapshot
//! per request, projected by `read_model::views` as Node's `plans.ts` does.

use crate::loopback::{
    LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
    require_api_key_tenant,
};
use anyhow::Result;
use axum::{
    Router,
    extract::{Path, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use pp_storage::{
    auth::AuthFailure,
    read_model::{
        AcceptedRead, ReadClient,
        views::{self, CatalogOnly},
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

const MAX_SAFE_ID: u64 = 9_007_199_254_740_991;

#[derive(Clone)]
pub struct AcceptedHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    wait: Duration,
}

impl AcceptedHttpConfig {
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

/// Admission the read routers in this module and `history` share: the
/// rebuild flag, a 64-request bound, loopback Host/Origin checks and a
/// credential.
#[derive(Clone)]
pub(crate) struct Gate {
    config: AcceptedHttpConfig,
    admission: Arc<tokio::sync::Semaphore>,
}

impl Gate {
    /// `None` unless `PP_DESKTOP_REBUILD` was exactly `1` or `true`.
    pub(crate) fn open(config: AcceptedHttpConfig) -> Option<Self> {
        config.enabled.then(|| Self {
            config,
            admission: Arc::new(tokio::sync::Semaphore::new(64)),
        })
    }

    pub(crate) fn wait(&self) -> Duration {
        self.config.wait
    }

    /// Runs `guard` in front of every route in `routes`, and serves every
    /// error, the guard's included, as an RFC 9457 problem.
    pub(crate) fn guard<S: Clone + Send + Sync + 'static>(&self, routes: Router<S>) -> Router<S> {
        routes
            .layer(middleware::from_fn_with_state(self.clone(), guard))
            .route_layer(middleware::map_response(crate::problem::layer))
    }
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: ReadClient,
}

/// Mounts Accepted Plan Checkoff GET+HEAD at `/`, `/api/v1`, and `/api/v2`,
/// and Part assembled units at `/` and `/api/v1`, when `PP_DESKTOP_REBUILD`
/// was exactly `1` or `true`; otherwise nothing.
/// Filament labels come from the colour catalog only; Plans that name custom
/// or Spoolman filaments answer 503 until those lookups are ported.
pub fn accepted_router(config: AcceptedHttpConfig, client: ReadClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let plans = Router::new().route("/plans/{id}/checkoff", get(checkoff));
    // Node registers Part routes at `/` and `/api/v1` only.
    let parts = plans.clone().route("/parts/{id}/assembled", get(assembled));
    gate.guard(
        Router::new()
            .merge(parts.clone())
            .nest("/api/v1", parts)
            .nest("/api/v2", plans),
    )
    .with_state(App { gate, client })
}

async fn guard(
    State(gate): State<Gate>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Ok(_permit) = gate.admission.try_acquire() else {
        return crate::problem::overloaded("Service temporarily unavailable");
    };
    if request.uri().to_string().len() > 16_384 {
        return failure(StatusCode::URI_TOO_LONG, "Request target too long");
    }
    if let Err((status, detail)) = admit(
        &gate.config.origin,
        &gate.config.host,
        &gate.config.api_key_tenant,
        &request,
    ) {
        return failure(status, detail);
    }
    let Some(presented) = principal(request.headers(), &gate.config.api_key_tenant) else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    request.extensions_mut().insert(presented);
    next.run(request).await
}

/// Sets the flag when the request future is dropped, so a queued storage read
/// is skipped.
pub(crate) struct Cancellation(pub(crate) Arc<AtomicBool>);
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The accepted projection a route answers with.
#[derive(Clone, Copy)]
enum View {
    Checkoff,
    Assembled,
}

impl View {
    fn missing(self) -> &'static str {
        match self {
            Self::Checkoff => "Profile not found",
            Self::Assembled => "Part not found",
        }
    }
}

async fn checkoff(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    project(app, &id, request, View::Checkoff).await
}

async fn assembled(
    State(app): State<App>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Response {
    project(app, &id, request, View::Assembled).await
}

async fn project(app: App, id: &str, request: axum::extract::Request, view: View) -> Response {
    let Some(presented) = request.extensions().get::<PresentedCredential>().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let head = *request.method() == Method::HEAD;
    // An id that cannot name a row is "not found" once the credential is
    // proven (an empty batch).
    let target = node_id(id);
    let client = app.client.clone();
    let wait = app.gate.wait();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Cancellation(cancelled.clone());
    let read = tokio::task::spawn_blocking(move || {
        let credential = presented.into_credential();
        let batch = match (view, target) {
            (View::Assembled, Some(part)) => {
                client.read_part(credential, part, &cancelled, wait)?
            }
            _ => client.read(credential, target.as_slice(), &cancelled, wait)?,
        };
        let Some(build) = batch.builds.first() else {
            return Ok(None);
        };
        Ok::<_, anyhow::Error>(Some(match (view, target) {
            (View::Checkoff, _) if needs_filament_lookup(&build.accepted) => {
                json!({"kind":"filament_lookup_unavailable"})
            }
            (View::Checkoff, _) => {
                views::checkoff(build.profile_id, &build.accepted, &CatalogOnly)?
            }
            (View::Assembled, part) => views::assembled(part.unwrap_or_default(), &build.accepted),
        }))
    })
    .await;
    drop(flag);
    match read {
        Ok(Ok(Some(body))) => present(body, head, view.missing()),
        Ok(Ok(None)) => failure(StatusCode::NOT_FOUND, view.missing()),
        Ok(Err(error)) => read_error(&error),
        Err(_) => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}

/// Node reads the id with `Number()`: JS whitespace is trimmed, and decimal,
/// exponent and `0x`/`0o`/`0b` forms are numbers. Only a safe positive integer
/// can name a Plan or Part.
fn node_id(raw: &str) -> Option<i64> {
    let text = views::js_trim(raw);
    let radix = |digits: &str, radix: u32| {
        (!digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)))
            .then(|| u64::from_str_radix(digits, radix).ok())
            .flatten()
            .map(|value| value as f64)
    };
    let value = match text.get(..2) {
        Some("0x" | "0X") => radix(&text[2..], 16),
        Some("0o" | "0O") => radix(&text[2..], 8),
        Some("0b" | "0B") => radix(&text[2..], 2),
        // Rust also reads `inf` and `nan`; neither is a safe integer.
        _ => text.parse::<f64>().ok(),
    }?;
    (value.fract() == 0.0 && (1.0..=MAX_SAFE_ID as f64).contains(&value)).then_some(value as i64)
}

/// Node also resolves custom filaments from the data dir and Spoolman
/// filaments, which this server cannot read yet. A Plan whose included Parts
/// name any non-catalog filament is refused rather than answered with labels
/// that may differ from Node's.
fn needs_filament_lookup(read: &AcceptedRead) -> bool {
    let AcceptedRead::Ready { snapshot } = read else {
        return false;
    };
    snapshot
        .parts
        .iter()
        .filter(|part| part.included)
        .any(|part| {
            part.filament_color_id
                .as_deref()
                .is_some_and(|id| !views::js_trim(id).is_empty())
                && views::part_catalog_color(part.filament_color_id.as_deref()).is_none()
        })
}

/// Maps a `views` result to Node's response for the route, whose "not found"
/// detail is `missing`.
fn present(view: Value, head: bool, missing: &str) -> Response {
    match view["kind"].as_str() {
        Some("ready" | "empty") => json_response(StatusCode::OK, &view["body"], head),
        Some("not_found" | "part_not_found") => failure(StatusCode::NOT_FOUND, missing),
        Some("accepted_state_unavailable") => failure(
            StatusCode::CONFLICT,
            if view["reason"] == "compatibility_dirty" {
                "Accepted Plan requires compatibility repair"
            } else {
                "Accepted Plan operational state is not initialized"
            },
        ),
        Some("integrity_failure") => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Accepted Plan data is inconsistent",
        ),
        Some("filament_lookup_unavailable") => crate::problem::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "Custom and Spoolman filament details are not available on this server yet",
            "filament_lookup_unavailable",
        ),
        _ => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}

/// `ReadClient` refuses credentials with `AuthFailure` (sessions) or the plain
/// "Authentication required" message (API keys); admission refusals are plain
/// messages too. Anything else is Node's opaque 500.
const BUSY: &str = "Service temporarily unavailable";

pub(crate) fn read_error(error: &anyhow::Error) -> Response {
    match error.downcast_ref::<AuthFailure>() {
        Some(
            AuthFailure::SessionRequired
            | AuthFailure::InvalidCredentials
            | AuthFailure::CredentialChanged,
        ) => return failure(StatusCode::UNAUTHORIZED, "Authentication required"),
        Some(AuthFailure::OwnerMappingRequired) => {
            return failure(
                StatusCode::FORBIDDEN,
                "Explicit account owner mapping is required",
            );
        }
        Some(AuthFailure::QueueFull) => return crate::problem::overloaded(BUSY),
        Some(AuthFailure::CommitUnknown) => {
            return crate::problem::coded(StatusCode::SERVICE_UNAVAILABLE, BUSY, "outcome_unknown");
        }
        Some(AuthFailure::Stopped | AuthFailure::Storage) => {
            return failure(StatusCode::SERVICE_UNAVAILABLE, BUSY);
        }
        _ => {}
    }
    match error.to_string().as_str() {
        "Authentication required" => failure(StatusCode::UNAUTHORIZED, "Authentication required"),
        "Writer queue full" => crate::problem::overloaded(BUSY),
        "Storage stopped"
        | "Storage stopped without read result"
        | "Cancelled before admission" => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service temporarily unavailable",
        ),
        _ => failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
    }
}

pub(crate) fn json_response(status: StatusCode, value: &Value, head: bool) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, body.len())
        .body(axum::body::Body::from(if head { Vec::new() } else { body }))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}

impl Gate {
    /// Runs `read` on a blocking thread with the request's credential and the
    /// gate's queue wait. Dropping the request cancels a read still queued.
    /// `None` when the blocking task did not finish.
    pub(crate) async fn read<T: Send + 'static>(
        &self,
        presented: PresentedCredential,
        read: impl FnOnce(
            pp_storage::read_model::Credential,
            Arc<AtomicBool>,
            Duration,
        ) -> anyhow::Result<T>
        + Send
        + 'static,
    ) -> Option<anyhow::Result<T>> {
        let stop = Cancellation(Arc::new(AtomicBool::new(false)));
        let (cancelled, wait) = (stop.0.clone(), self.wait());
        let result =
            tokio::task::spawn_blocking(move || read(presented.into_credential(), cancelled, wait))
                .await
                .ok();
        drop(stop);
        result
    }
}

/// 200 with `value` as JSON.
pub(crate) fn ok_json(value: &impl serde::Serialize) -> Response {
    match serde_json::to_value(value) {
        Ok(body) => json_response(StatusCode::OK, &body, false),
        Err(_) => internal(),
    }
}

pub(crate) fn internal() -> Response {
    failure(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
}

/// The Build a path segment names. Only a decimal positive safe integer names
/// one; any other id still authenticates in storage, then answers not found.
pub(crate) fn build_id(id: &str) -> Option<pp_storage::build_graph::PositiveId> {
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .then(|| id.parse().ok())
        .flatten()
        .and_then(|id| pp_storage::build_graph::PositiveId::new(id).ok())
}

pub(crate) fn failure(status: StatusCode, detail: &str) -> Response {
    crate::problem::response(status, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body(response: Response) -> (u16, Value) {
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes).unwrap();
        (status, crate::problem::without_envelope(body))
    }

    #[tokio::test]
    async fn ticket_t_61_read_model_http_checkoff_maps_every_view_kind() {
        for (view, status, expected) in [
            (
                json!({"kind":"ready","body":{"profile_id":1,"parts":[]}}),
                200,
                json!({"profile_id":1,"parts":[]}),
            ),
            (
                json!({"kind":"empty","body":{"profile_id":2,"parts":[]}}),
                200,
                json!({"profile_id":2,"parts":[]}),
            ),
            (
                json!({"kind":"not_found"}),
                404,
                json!({"detail":"Profile not found"}),
            ),
            (
                json!({"kind":"accepted_state_unavailable","reason":"compatibility_dirty"}),
                409,
                json!({"detail":"Accepted Plan requires compatibility repair"}),
            ),
            (
                json!({"kind":"accepted_state_unavailable","reason":"uninitialized"}),
                409,
                json!({"detail":"Accepted Plan operational state is not initialized"}),
            ),
            (
                json!({"kind":"integrity_failure","code":"pointer","message":"private"}),
                500,
                json!({"detail":"Accepted Plan data is inconsistent"}),
            ),
            (
                json!({"kind":"filament_lookup_unavailable"}),
                503,
                json!({
                    "detail": "Custom and Spoolman filament details are not available on this server yet",
                    "code": "filament_lookup_unavailable",
                }),
            ),
        ] {
            assert_eq!(
                body(present(view, false, "Profile not found")).await,
                (status, expected)
            );
        }
    }

    #[tokio::test]
    async fn ticket_t_61_read_model_http_read_errors_hide_details() {
        for (error, status, detail) in [
            (
                anyhow::anyhow!(AuthFailure::SessionRequired),
                401,
                "Authentication required",
            ),
            (
                anyhow::anyhow!("Authentication required"),
                401,
                "Authentication required",
            ),
            (
                anyhow::anyhow!(AuthFailure::OwnerMappingRequired),
                403,
                "Explicit account owner mapping is required",
            ),
            (
                anyhow::anyhow!("Writer queue full"),
                503,
                "Service temporarily unavailable",
            ),
            (
                anyhow::anyhow!("Storage stopped without read result"),
                503,
                "Service temporarily unavailable",
            ),
            (
                anyhow::anyhow!(AuthFailure::QueueFull),
                503,
                "Service temporarily unavailable",
            ),
            (
                anyhow::anyhow!(AuthFailure::Stopped),
                503,
                "Service temporarily unavailable",
            ),
            (
                anyhow::anyhow!(AuthFailure::Storage),
                503,
                "Service temporarily unavailable",
            ),
            (
                anyhow::anyhow!("private failure detail /tmp/secret"),
                500,
                "Internal Server Error",
            ),
        ] {
            assert_eq!(
                body(read_error(&error)).await,
                (status, json!({ "detail": detail }))
            );
        }
    }

    /// The Node-produced fixture's Ready snapshot: Part 2 is included, the
    /// other Part is excluded.
    fn ready() -> AcceptedRead {
        use pp_storage::{Limits, WriterOwner, auth::Secret, read_model::Credential};
        let directory = crate::scratch::Scratch::new("pp-accepted-unit-");
        std::fs::create_dir_all(directory.join("repos/1/revisions/accepted")).unwrap();
        std::fs::write(
            directory.join("print-partner.db"),
            include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        let mut batch = owner
            .accepted_reads()
            .unwrap()
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                &[1],
                &AtomicBool::new(false),
                Duration::from_secs(1),
            )
            .unwrap();
        owner.shutdown().unwrap();
        batch.builds.remove(0).accepted
    }

    fn with_filament(color: Option<&str>, hex: Option<&str>, included: bool) -> AcceptedRead {
        let mut read = ready();
        let AcceptedRead::Ready { snapshot } = &mut read else {
            panic!("fixture is ready")
        };
        let part = snapshot
            .parts
            .iter_mut()
            .find(|part| part.included == included)
            .unwrap();
        part.filament_color_id = color.map(Into::into);
        part.filament_custom_hex = hex.map(Into::into);
        read
    }

    #[test]
    fn ticket_t_61_read_model_http_refuses_filaments_it_cannot_resolve() {
        const CATALOG: &str = "abs-matte::black-pantone-7c";
        for (color, included, refused) in [
            (None, true, false),
            (Some(""), true, false),
            (Some(" \u{a0}\t"), true, false),
            (Some(CATALOG), true, false),
            (Some("\u{feff} abs-matte::black-pantone-7c \n"), true, false),
            (Some("custom-ocean-blue"), true, true),
            (Some("spoolman:shop:filament:3"), true, true),
            (Some("custom-ocean-blue"), false, false),
            (Some("spoolman:shop:filament:3"), false, false),
        ] {
            assert_eq!(
                needs_filament_lookup(&with_filament(color, None, included)),
                refused,
                "{color:?} included={included}"
            );
        }
        for read in [AcceptedRead::Missing, AcceptedRead::CompatibilityDirty] {
            assert!(!needs_filament_lookup(&read));
        }
    }

    #[test]
    fn ticket_t_61_read_model_http_catalog_hex_wins_like_node() {
        // Node's `filamentContext.resolve` trims the id and returns the
        // catalog colour, so `resolved.hex` wins over the Part's custom hex.
        let read = with_filament(Some(" abs-matte::black-pantone-7c "), Some("#123456"), true);
        let part = &views::checkoff(1, &read, &CatalogOnly).unwrap()["body"]["parts"][0];
        assert_eq!(part["filament_display"], "ABS Matte · Black (Pantone 7C)");
        assert_eq!(part["filament_hex"], "#c0afa1");
        // Without a filament id, Node's `resolvePartFilamentHex` uses the
        // custom hex and the label stays empty.
        let read = with_filament(None, Some("#123456"), true);
        let part = &views::checkoff(1, &read, &CatalogOnly).unwrap()["body"]["parts"][0];
        assert_eq!(part["filament_display"], "");
        assert_eq!(part["filament_hex"], "#123456");
    }
}
