use super::{
    ProviderClient, ResetMailer,
    wire::{self, Failure},
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
#[cfg(feature = "desktop_rebuild")]
use pp_storage::auth::rebuild::RebuildAuthClient;
use pp_storage::auth::{AuthClient, AuthReply, Outcome, Provider, Request, Secret};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub enum CookieTransport {
    LoopbackHttp,
    Secure,
}
#[derive(Clone)]
pub struct AuthHttpConfig {
    pub(super) origin: String,
    host: String,
    pub(super) transport: CookieTransport,
    multi_user: bool,
    public_origin: Option<String>,
    dev_reset_exposure: bool,
    tenant_feature_toggles: bool,
    #[cfg(feature = "desktop_rebuild")]
    modules: Option<(pp_storage::SettingsClient, String)>,
}
impl AuthHttpConfig {
    /// List each Module's effective state for `tenant` in `/health`, read
    /// from storage on every request. Only the Rust-only runtime opts in;
    /// without it `/health` omits `modules`, as Node does.
    #[cfg(feature = "desktop_rebuild")]
    #[must_use]
    pub fn with_modules(mut self, settings: pp_storage::SettingsClient, tenant: &str) -> Self {
        self.modules = Some((settings, tenant.to_owned()));
        self
    }
    /// Advertise `tenant_feature_toggles` in `/health`, as Node does when
    /// `PP_DESKTOP_REBUILD` opts in. The UI keys its rebuild behavior on it.
    #[must_use]
    pub fn with_tenant_feature_toggles(mut self) -> Self {
        self.tenant_feature_toggles = true;
        self
    }
    pub fn new(
        origin: &str,
        transport: CookieTransport,
        multi_user: bool,
        public_origin: Option<&str>,
        dev_reset_exposure: bool,
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
        ensure!(ip.is_loopback(), "Listener must be loopback");
        ensure!(
            matches!(
                (url.scheme(), transport),
                ("http", CookieTransport::LoopbackHttp) | ("https", CookieTransport::Secure)
            ),
            "Cookie transport must match listener"
        );
        let origin = url.origin().ascii_serialization();
        let Some((_, host)) = origin.split_once("://") else {
            panic!("URL scheme");
        };
        let host = host.to_owned();
        let public_origin = public_origin
            .map(|value| -> Result<String> {
                let url = reqwest::Url::parse(value)?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "Invalid public reset origin"
                );
                Ok(url.as_str().trim_end_matches('/').to_owned())
            })
            .transpose()?;
        Ok(Self {
            origin,
            host,
            transport,
            multi_user,
            public_origin,
            dev_reset_exposure,
            tenant_feature_toggles: false,
            #[cfg(feature = "desktop_rebuild")]
            modules: None,
        })
    }
}
type RateSources = HashMap<(IpAddr, bool), (Instant, u32)>;

#[derive(Clone)]
pub enum AuthBackend {
    Pool(AuthClient),
    #[cfg(feature = "desktop_rebuild")]
    Rebuild(RebuildAuthClient),
}
impl AuthBackend {
    fn submit(
        &self,
        request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<AuthReply> {
        match self {
            Self::Pool(client) => client.submit(request, cancelled, wait),
            #[cfg(feature = "desktop_rebuild")]
            Self::Rebuild(client) => client.submit(request, cancelled, wait),
        }
    }
}
impl From<AuthClient> for AuthBackend {
    fn from(client: AuthClient) -> Self {
        Self::Pool(client)
    }
}
#[cfg(feature = "desktop_rebuild")]
impl From<RebuildAuthClient> for AuthBackend {
    fn from(client: RebuildAuthClient) -> Self {
        Self::Rebuild(client)
    }
}

#[derive(Clone)]
pub(super) struct App {
    pub(super) config: AuthHttpConfig,
    pub(super) auth: AuthBackend,
    pub(super) providers: ProviderClient,
    mail: ResetMailer,
    rates: Arc<Mutex<RateSources>>,
    admission: Arc<tokio::sync::Semaphore>,
    reset_delivery: Arc<tokio::sync::Semaphore>,
}
pub fn auth_router(
    config: AuthHttpConfig,
    auth: impl Into<AuthBackend>,
    providers: ProviderClient,
    mail: ResetMailer,
) -> Router {
    let discord = providers.configured(Provider::Discord);
    let app = App {
        config,
        auth: auth.into(),
        providers,
        mail,
        rates: Arc::default(),
        admission: Arc::new(tokio::sync::Semaphore::new(64)),
        reset_delivery: Arc::new(tokio::sync::Semaphore::new(16)),
    };
    let mut router = Router::new()
        .route("/auth/me", get(me))
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/forgot-password", post(forgot))
        .route("/auth/reset-password", post(reset))
        .route("/auth/change-password", post(change))
        .route("/auth/github", get(super::provider::github_start))
        .route("/auth/callback", get(super::provider::github_callback))
        .route("/auth/discord", get(super::provider::discord_start))
        .route("/settings/api-keys", get(keys).post(create_key))
        .route("/settings/api-keys/{id}", delete(revoke_key))
        .route("/settings/api-keys/{id}/regenerate", post(rotate_key))
        .route("/health", get(health));
    if discord {
        router = router.route(
            "/auth/discord/callback",
            get(super::provider::discord_callback),
        );
    }
    let router = router.layer(DefaultBodyLimit::max(16384));
    #[cfg(feature = "desktop_rebuild")]
    let router = router.route_layer(middleware::map_response(crate::problem::layer));
    router
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
async fn guard(State(app): State<App>, request: axum::extract::Request, next: Next) -> Response {
    let Ok(_permit) = app.admission.try_acquire() else {
        return Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication temporarily unavailable",
        )
        .into_response();
    };
    if request.uri().to_string().len() > 16384 {
        return Failure(StatusCode::URI_TOO_LONG, "Request target too long").into_response();
    }
    let peer = match crate::NetworkAdmission::peer(&request) {
        Ok(peer) => peer,
        Err((status, detail)) => return Failure(status, detail).into_response(),
    };
    let (expected_origin, expected_host) =
        crate::NetworkAdmission::authority(&request, &app.config.origin, &app.config.host);
    let peer_ip = peer.ip();
    let headers = request.headers();
    if headers.get_all("host").iter().count() != 1
        || headers.get("host").and_then(|v| v.to_str().ok()) != Some(expected_host)
        || headers
            .keys()
            .any(|k| k.as_str() == "forwarded" || k.as_str().starts_with("x-forwarded-"))
    {
        return Failure(StatusCode::FORBIDDEN, "Invalid request host").into_response();
    }
    let callback = matches!(
        request.uri().path(),
        "/auth/callback" | "/auth/discord/callback"
    ) && matches!(*request.method(), Method::GET | Method::HEAD);
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let unsafe_method = !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if headers.get_all("origin").iter().count() > 1
        || (headers.contains_key("origin") && origin != Some(expected_origin))
        || (unsafe_method && origin != Some(expected_origin))
        || (!callback
            && headers
                .get("sec-fetch-site")
                .is_some_and(|v| v == "cross-site"))
    {
        return Failure(StatusCode::FORBIDDEN, "Invalid request origin").into_response();
    }
    let credential = unsafe_method
        && matches!(
            request.uri().path(),
            "/auth/register"
                | "/auth/login"
                | "/auth/forgot-password"
                | "/auth/reset-password"
                | "/auth/change-password"
        );
    let key_create =
        request.method() == Method::POST && request.uri().path() == "/settings/api-keys";
    if credential || key_create {
        let now = Instant::now();
        let mut rates = match app.rates.lock() {
            Ok(rates) => rates,
            Err(_) => panic!("rate lock"),
        };
        rates.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        let key = (peer_ip, key_create);
        if !rates.contains_key(&key) && rates.len() >= 4096 {
            return Failure(StatusCode::SERVICE_UNAVAILABLE, "Rate registry full").into_response();
        }
        let (_, count) = rates.entry(key).or_insert((now, 0));
        *count += 1;
        if *count > if key_create { 10 } else { 20 } {
            return Failure(StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded").into_response();
        }
    }
    next.run(request).await
}
pub(super) async fn invoke(auth: &AuthBackend, request: Request) -> Result<Outcome, Failure> {
    static WAITERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let permit = WAITERS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(64)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            Failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Authentication temporarily unavailable",
            )
        })?;
    struct Cancellation(Arc<AtomicBool>);
    impl Drop for Cancellation {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancellation = Cancellation(cancelled.clone());
    let auth = auth.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let reply = auth.submit(request, cancelled, Duration::from_millis(100))?;
        reply
            .recv()
            .map_err(|_| anyhow::anyhow!(pp_storage::auth::AuthFailure::CommitUnknown))?
    })
    .await
    .map_err(|_| {
        Failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Authentication unavailable",
        )
    })?
    .map_err(Failure::from)
}
fn body(input: Result<Json<Value>, JsonRejection>) -> Result<Value, Failure> {
    let Json(value) = input.map_err(|_| Failure(StatusCode::BAD_REQUEST, "Invalid JSON body"))?;
    if !value.is_object() {
        return Err(Failure(StatusCode::BAD_REQUEST, "Invalid JSON body"));
    }
    Ok(value)
}
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_owned()
}
fn trim(value: &str) -> &str {
    value.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'))
}
fn email(value: &Value) -> String {
    trim(&text(value, "email")).to_lowercase()
}
fn valid_email(value: &str) -> Result<(), Failure> {
    if value.is_empty() || !value.contains('@') {
        return Err(Failure(StatusCode::BAD_REQUEST, "Valid email is required"));
    }
    Ok(())
}
pub(super) fn session_response(
    app: &App,
    outcome: Outcome,
    include_ok: bool,
    only_ok: bool,
) -> Result<Response, Failure> {
    let Outcome::Session { user, token } = outcome else {
        return Err(Failure(
            StatusCode::BAD_REQUEST,
            "Invalid or expired reset link",
        ));
    };
    let mut value = if only_ok {
        json!({})
    } else {
        json!({"user":wire::public_user(user)})
    };
    if include_ok {
        value["ok"] = json!(true);
    }
    let mut response = wire::json_response(value);
    wire::set_cookie(
        &mut response,
        "pp_session",
        token.expose(),
        1209600,
        app.config.transport,
    );
    Ok(response)
}
async fn me(State(app): State<App>, headers: HeaderMap) -> Result<Response, Failure> {
    let session = wire::session(&headers)
        .map_err(|_| Failure(StatusCode::UNAUTHORIZED, "Not authenticated"))?;
    match invoke(&app.auth, Request::ResolveSession { token: session }).await? {
        Outcome::User(Some(user)) => Ok(wire::json_response(
            json!({"user":wire::public_user(user),"multi_user":app.config.multi_user}),
        )),
        _ => Err(Failure(StatusCode::UNAUTHORIZED, "Not authenticated")),
    }
}
async fn register(
    State(app): State<App>,
    input: Result<Json<Value>, JsonRejection>,
) -> Result<Response, Failure> {
    let value = body(input)?;
    let email = email(&value);
    valid_email(&email)?;
    let display = text(&value, "display_name");
    let display_name = if trim(&display).is_empty() {
        email.split('@').next().unwrap_or("User").to_owned()
    } else {
        trim(&display).to_owned()
    };
    let outcome = invoke(
        &app.auth,
        Request::Register {
            email,
            display_name,
            password: Secret::new(text(&value, "password")),
        },
    )
    .await?;
    session_response(&app, outcome, false, false)
}
async fn login(
    State(app): State<App>,
    input: Result<Json<Value>, JsonRejection>,
) -> Result<Response, Failure> {
    let value = body(input)?;
    let email = email(&value);
    let password = text(&value, "password");
    if email.is_empty() || password.is_empty() {
        return Err(Failure(
            StatusCode::BAD_REQUEST,
            "Email and password are required",
        ));
    }
    let outcome = invoke(
        &app.auth,
        Request::Login {
            email,
            password: Secret::new(password),
        },
    )
    .await?;
    session_response(&app, outcome, false, false)
}
async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<Response, Failure> {
    if let Some(token) = wire::cookie(&headers, "pp_session") {
        invoke(
            &app.auth,
            Request::Logout {
                token: Secret::new(token),
            },
        )
        .await?;
    }
    let mut response = wire::json_response(json!({"ok":true}));
    wire::set_cookie(&mut response, "pp_session", "", 0, app.config.transport);
    Ok(response)
}
async fn forgot(
    State(app): State<App>,
    input: Result<Json<Value>, JsonRejection>,
) -> Result<Response, Failure> {
    let value = body(input)?;
    let email = email(&value);
    valid_email(&email)?;
    let mut result =
        json!({"ok":true,"message":"If an account exists for that email, a reset link was sent."});
    let origin = app.config.public_origin.as_deref().or_else(|| {
        (app.config.dev_reset_exposure && !app.mail.configured())
            .then_some(app.config.origin.as_str())
    });
    // Reserve the same fixed delivery window for every address. Neither account
    // existence nor SMTP completion may change the capacity visible to callers.
    let delivery_permit = if origin.is_some() && app.mail.configured() {
        Some(
            app.reset_delivery
                .clone()
                .try_acquire_owned()
                .map_err(|_| {
                    Failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Authentication temporarily unavailable",
                    )
                })?,
        )
    } else {
        None
    };
    let deadline = tokio::time::Instant::now() + super::mail::DELIVERY_TIMEOUT;
    let reset_url = if let Some(origin) = origin
        && let Outcome::ResetToken(Some(token)) = invoke(
            &app.auth,
            Request::RequestReset {
                email: email.clone(),
            },
        )
        .await?
    {
        let mut url = match reqwest::Url::parse(&format!("{origin}/reset-password")) {
            Ok(url) => url,
            Err(_) => panic!("validated origin"),
        };
        url.query_pairs_mut().append_pair("token", token.expose());
        Some(url.to_string())
    } else {
        None
    };
    if let Some(permit) = delivery_permit {
        app.mail.schedule_reset(email, reset_url, deadline, permit);
    } else if app.config.dev_reset_exposure
        && let Some(url) = reset_url
    {
        result["dev_reset_url"] = json!(url);
    }
    Ok(wire::json_response(result))
}
async fn reset(
    State(app): State<App>,
    input: Result<Json<Value>, JsonRejection>,
) -> Result<Response, Failure> {
    let value = body(input)?;
    let token = text(&value, "token").trim().to_owned();
    if token.is_empty() {
        return Err(Failure(StatusCode::BAD_REQUEST, "Reset token is required"));
    }
    let outcome = invoke(
        &app.auth,
        Request::ResetPassword {
            token: Secret::new(token),
            replacement: Secret::new(text(&value, "password")),
        },
    )
    .await?;
    session_response(&app, outcome, true, false)
}
async fn change(
    State(app): State<App>,
    headers: HeaderMap,
    input: Result<Json<Value>, JsonRejection>,
) -> Result<Response, Failure> {
    // Node resolves the cookie in onRequest before field checks. Match that so a
    // nonempty invalid, expired, or revoked token refuses with 401 before 400.
    let raw = wire::cookie(&headers, "pp_session")
        .filter(|value| !value.is_empty())
        .ok_or(Failure(StatusCode::UNAUTHORIZED, "Authentication required"))?;
    match invoke(
        &app.auth,
        Request::ResolveSession {
            token: Secret::new(raw.clone()),
        },
    )
    .await?
    {
        Outcome::User(Some(_)) => {}
        _ => {
            return Err(Failure(StatusCode::UNAUTHORIZED, "Authentication required"));
        }
    }
    let value = body(input)?;
    let current = text(&value, "current_password");
    let replacement = text(&value, "new_password");
    if current.is_empty() || replacement.is_empty() {
        return Err(Failure(
            StatusCode::BAD_REQUEST,
            "Current and new passwords are required",
        ));
    }
    let outcome = invoke(
        &app.auth,
        Request::ChangePassword {
            session: Secret::new(raw),
            current: Secret::new(current),
            replacement: Secret::new(replacement),
        },
    )
    .await?;
    session_response(&app, outcome, true, true)
}
fn health_auth(outcome: Result<Outcome, Failure>) -> Result<(bool, Option<&'static str>), Failure> {
    match outcome {
        Ok(Outcome::User(user)) => Ok((user.is_some(), None)),
        // Authentication/policy rejection is degraded auth, not failed liveness.
        Err(Failure(status, detail)) if status.is_client_error() => Ok((false, Some(detail))),
        Err(error) => Err(error),
        Ok(_) => unreachable!(),
    }
}
async fn health(State(app): State<App>, headers: HeaderMap) -> Result<Response, Failure> {
    let Outcome::Status(status) = invoke(&app.auth, Request::Status).await? else {
        unreachable!()
    };
    let (authenticated, degraded_detail) = if let Some(token) = wire::cookie(&headers, "pp_session")
    {
        health_auth(
            invoke(
                &app.auth,
                Request::ResolveSession {
                    token: Secret::new(token),
                },
            )
            .await,
        )?
    } else {
        (false, None)
    };
    let mut capabilities = Vec::new();
    if app.providers.configured(Provider::Github) {
        capabilities.push("github_oauth");
    }
    if app.providers.configured(Provider::Discord) {
        capabilities.push("discord_oauth");
    }
    if app.config.tenant_feature_toggles {
        capabilities.push("tenant_feature_toggles");
    }
    #[cfg(feature = "desktop_rebuild")]
    let modules = match app.config.modules.clone() {
        Some((settings, tenant)) => Some(
            tokio::task::spawn_blocking(move || {
                settings.modules(&tenant, Duration::from_millis(100))
            })
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_else(pp_storage::features::modules::unreadable),
        ),
        None => None,
    };
    Ok(wire::json_response(json!(wire::Health {
        ok: true,
        authentication_required: true,
        authenticated,
        degraded_detail,
        capabilities,
        multi_user: app.config.multi_user,
        single_user_auth: status.single_user_auth,
        single_user_setup_required: status.single_user_setup_required,
        registration_open: status.registration_open,
        owner_mapping_required: status.owner_mapping_required,
        github_oauth_configured: app.providers.configured(Provider::Github),
        discord_oauth_configured: app.providers.configured(Provider::Discord),
        #[cfg(feature = "desktop_rebuild")]
        modules,
    })))
}
async fn keys(State(app): State<App>, headers: HeaderMap) -> Result<Response, Failure> {
    let Outcome::Keys { keys, .. } = invoke(
        &app.auth,
        Request::ListKeys {
            session: wire::session(&headers)?,
        },
    )
    .await?
    else {
        unreachable!()
    };
    Ok(wire::json_response(json!({"total":keys.len(),"keys":keys})))
}
fn created(outcome: Outcome) -> Result<Response, Failure> {
    let Outcome::KeyCreated { info, key } = outcome else {
        return Err(Failure(StatusCode::NOT_FOUND, "API key not found"));
    };
    let mut value = match serde_json::to_value(info) {
        Ok(value) => value,
        Err(_) => panic!("key info"),
    };
    value["key"] = json!(key.expose());
    Ok((StatusCode::CREATED, Json(value)).into_response())
}
async fn create_key(State(app): State<App>, headers: HeaderMap) -> Result<Response, Failure> {
    created(
        invoke(
            &app.auth,
            Request::CreateKey {
                session: wire::session(&headers)?,
            },
        )
        .await?,
    )
}
async fn rotate_key(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    match invoke(
        &app.auth,
        Request::RotateKey {
            session: wire::session(&headers)?,
            key_id: id,
        },
    )
    .await?
    {
        outcome @ Outcome::KeyCreated { .. } => created(outcome),
        _ => Err(Failure(StatusCode::NOT_FOUND, "API key not found")),
    }
}
async fn revoke_key(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    match invoke(
        &app.auth,
        Request::RevokeKey {
            session: wire::session(&headers)?,
            key_id: id,
        },
    )
    .await?
    {
        Outcome::KeyChanged { changed: true, .. } => {
            Ok(wire::json_response(json!({"success":true})))
        }
        _ => Err(Failure(StatusCode::NOT_FOUND, "API key not found")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pp_storage::auth::{AuthFailure, AuthInputFailure};

    #[test]
    fn health_keeps_policy_rejections_degraded_and_runtime_failures_fatal() {
        for error in [
            AuthFailure::RegistrationClosed,
            AuthFailure::SingleAccountExists,
            AuthFailure::OwnerMappingRequired,
            AuthFailure::InvalidInput(AuthInputFailure::Policy),
            AuthFailure::InvalidCredentials,
            AuthFailure::SessionRequired,
            AuthFailure::CurrentPasswordIncorrect,
            AuthFailure::OAuthOnlyAccount,
            AuthFailure::DuplicateEmail,
            AuthFailure::CredentialChanged,
        ] {
            let (authenticated, detail) =
                health_auth(Err(Failure::from(anyhow::Error::new(error)))).unwrap();
            assert!(!authenticated);
            assert!(detail.is_some());
        }
        for error in [
            AuthFailure::Storage,
            AuthFailure::Stopped,
            AuthFailure::QueueFull,
            AuthFailure::CommitUnknown,
        ] {
            assert!(health_auth(Err(Failure::from(anyhow::Error::new(error)))).is_err());
        }
    }
}
