use super::CookieTransport;
use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use pp_storage::auth::{AuthFailure, Secret, User};
use serde_json::{Value, json};

/// Full admission, writer queue, or a stopping writer: retry later.
const BUSY: &str = "Authentication temporarily unavailable";
/// A write whose commit could not be confirmed: reload, do not retry blindly.
#[cfg(feature = "desktop_rebuild")]
const OUTCOME_UNKNOWN: &str = "Authentication outcome unknown; reload before retrying";

#[derive(Debug)]
pub(crate) struct Failure(pub StatusCode, pub &'static str);
impl IntoResponse for Failure {
    /// The rebuild serves problem documents.
    #[cfg(feature = "desktop_rebuild")]
    fn into_response(self) -> Response {
        use crate::problem::{ProblemType, coded, response, typed};
        match (self.0, self.1) {
            (StatusCode::UNAUTHORIZED, "Current password is incorrect") => {
                coded(self.0, self.1, "current_password_rejected")
            }
            (StatusCode::SERVICE_UNAVAILABLE, BUSY) => typed(ProblemType::Overloaded, self.0, BUSY),
            (_, OUTCOME_UNKNOWN) => coded(self.0, self.1, "outcome_unknown"),
            _ => response(self.0, self.1),
        }
    }
    /// Feature off keeps the Node-compatible bodies unchanged.
    #[cfg(not(feature = "desktop_rebuild"))]
    fn into_response(self) -> Response {
        let mut body = json!({"detail": self.1});
        if self.0 == StatusCode::NOT_FOUND && self.1 == "API key not found" {
            body["title"] = json!("Not Found");
            body["status"] = json!(404);
        }
        if self.0 == StatusCode::UNAUTHORIZED && self.1 == "Current password is incorrect" {
            body["code"] = json!("current_password_rejected");
        }
        (self.0, Json(body)).into_response()
    }
}
impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        use AuthFailure::*;
        let (status, detail) = match error.downcast_ref::<AuthFailure>() {
            Some(InvalidInput(pp_storage::auth::AuthInputFailure::PasswordTooShort)) => {
                (400, "Password must be at least 8 characters")
            }
            Some(InvalidInput(_)) => (400, "Invalid authentication input"),
            Some(InvalidCredentials) => (401, "Invalid email or password"),
            Some(SessionRequired) => (401, "Authentication required"),
            Some(CurrentPasswordIncorrect) => (401, "Current password is incorrect"),
            Some(OAuthOnlyAccount) => (400, "This account uses OAuth sign-in only"),
            Some(DuplicateEmail) => (409, "Email already registered"),
            Some(RegistrationClosed) => (403, "Registration is closed"),
            Some(SingleAccountExists) => (403, "The single-user administrator already exists"),
            Some(OwnerMappingRequired) => (403, "Explicit account owner mapping is required"),
            Some(CredentialChanged) => (401, "Credential changed; sign in again"),
            // Node sends one detail for both; both are retryable.
            Some(QueueFull | Stopped) => (503, BUSY),
            #[cfg(feature = "desktop_rebuild")]
            Some(CommitUnknown) => (503, OUTCOME_UNKNOWN),
            // Storage's plain admission messages are retryable too.
            #[cfg(feature = "desktop_rebuild")]
            None if crate::problem::writer_admission(&error).is_some() => (503, BUSY),
            _ => (500, "Authentication unavailable"),
        };
        let Ok(status) = StatusCode::from_u16(status) else {
            panic!("valid status");
        };
        Self(status, detail)
    }
}
pub(super) fn public_user(user: User) -> Value {
    json!({"user_id":user.user_id,"login":user.login,"display_name":user.display_name,"email":user.email,"provider":user.provider,"is_admin":user.is_admin,"hasPassword":user.has_password})
}
pub(super) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut result = None;
    for header in headers.get_all(header::COOKIE) {
        for item in header.to_str().ok()?.split(';') {
            let Some((key, value)) = item.trim().split_once('=') else {
                continue;
            };
            if key == name {
                if result.is_some() || value.len() > 4096 {
                    return None;
                }
                result = Some(value.to_owned());
            }
        }
    }
    result
}
pub(super) fn session(headers: &HeaderMap) -> Result<Secret, Failure> {
    cookie(headers, "pp_session")
        .filter(|v| !v.is_empty())
        .map(Secret::new)
        .ok_or(Failure(StatusCode::UNAUTHORIZED, "Authentication required"))
}
pub(super) fn set_cookie(
    response: &mut Response,
    name: &str,
    value: &str,
    age: u32,
    transport: CookieTransport,
) {
    let secure = if matches!(transport, CookieTransport::Secure) {
        "; Secure"
    } else {
        ""
    };
    let value = format!("{name}={value}; Max-Age={age}; Path=/; HttpOnly; SameSite=Lax{secure}");
    response.headers_mut().append(
        header::SET_COOKIE,
        match HeaderValue::from_str(&value) {
            Ok(header) => header,
            Err(_) => panic!("generated cookie"),
        },
    );
}
/// Schema-only marker for a member a response may omit but never sends as
/// null, unless `T` itself is nullable.
#[cfg(feature = "codegen")]
pub(crate) struct Omittable<T>(std::marker::PhantomData<T>);
#[cfg(feature = "codegen")]
impl<T: schemars::JsonSchema> schemars::JsonSchema for Omittable<T> {
    fn schema_name() -> String {
        format!("Omittable_{}", T::schema_name())
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        T::json_schema(generator)
    }
    fn _schemars_private_is_option() -> bool {
        true
    }
}

/// `GET /health` body.
#[derive(Debug, serde::Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct Health {
    pub ok: bool,
    pub authentication_required: bool,
    pub authenticated: bool,
    /// The six members below are Rust's own: Node's `/health` omits them, so
    /// the generated schema marks them optional and a client decodes either body.
    #[cfg_attr(feature = "codegen", schemars(with = "Omittable<Option<String>>"))]
    pub degraded_detail: Option<&'static str>,
    pub capabilities: Vec<&'static str>,
    pub multi_user: bool,
    #[cfg_attr(feature = "codegen", schemars(with = "Omittable<bool>"))]
    pub single_user_auth: bool,
    #[cfg_attr(feature = "codegen", schemars(with = "Omittable<bool>"))]
    pub single_user_setup_required: bool,
    pub registration_open: bool,
    #[cfg_attr(feature = "codegen", schemars(with = "Omittable<bool>"))]
    pub owner_mapping_required: bool,
    #[cfg_attr(feature = "codegen", schemars(with = "Omittable<bool>"))]
    pub github_oauth_configured: bool,
    #[cfg_attr(feature = "codegen", schemars(with = "Omittable<bool>"))]
    pub discord_oauth_configured: bool,
    /// Each Module's effective state. The Rust-only runtime sends it, every
    /// Module `setting_unreadable` when storage cannot be read in time. Node
    /// and the other Rust auth routers omit it, so the schema marks it optional.
    #[cfg(feature = "desktop_rebuild")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "Omittable<Vec<pp_storage::features::modules::ModuleStatus>>")
    )]
    pub modules: Option<Vec<pp_storage::features::modules::ModuleStatus>>,
}
pub(super) fn json_response(value: Value) -> Response {
    Json(value).into_response()
}
pub(super) fn redirect(target: &str) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        match HeaderValue::from_str(target) {
            Ok(header) => header,
            Err(_) => panic!("validated redirect"),
        },
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    /// Status, media type and parsed body of a rendered failure.
    async fn render(failure: Failure) -> (StatusCode, String, Value) {
        let response = failure.into_response();
        let status = response.status();
        let media = response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, media, serde_json::from_slice(&body).unwrap())
    }

    #[cfg(feature = "desktop_rebuild")]
    #[tokio::test]
    async fn problem_response_auth_failures() {
        #[rustfmt::skip]
        let cases = [
            (StatusCode::NOT_FOUND, "API key not found", "not-found", "Not Found", None),
            (StatusCode::BAD_REQUEST, "API key not found", "validation", "Validation Failed", None),
            (StatusCode::UNAUTHORIZED, "Current password is incorrect", "unauthenticated",
                "Authentication Required", Some("current_password_rejected")),
            (StatusCode::BAD_REQUEST, "Invalid JSON body", "malformed-request", "Malformed Request", None),
        ];
        for (status, detail, kind, title, code) in cases {
            let mut expected = json!({"type": format!("urn:print-partner:problem:{kind}"),
                "title": title, "status": status.as_u16(), "detail": detail});
            if let Some(code) = code {
                expected["code"] = json!(code);
            }
            let rendered = render(Failure(status, detail)).await;
            assert_eq!(
                rendered,
                (status, "application/problem+json".into(), expected)
            );
        }
    }

    /// Typed storage failures keep their cause: retry after overload, reload
    /// after an unknown outcome.
    #[cfg(feature = "desktop_rebuild")]
    #[tokio::test]
    async fn problem_response_auth_typed_failures() {
        use AuthFailure::*;
        for (failure, status, kind, code) in [
            (QueueFull, 503, "overloaded", None),
            (Stopped, 503, "overloaded", None),
            (
                CommitUnknown,
                503,
                "outcome-unknown",
                Some("outcome_unknown"),
            ),
            (DuplicateEmail, 409, "conflict", None),
            (SessionRequired, 401, "unauthenticated", None),
            (Storage, 500, "internal", None),
        ] {
            let (got, _, body) = render(Failure::from(anyhow::Error::new(failure))).await;
            assert_eq!(got.as_u16(), status, "{kind}");
            assert_eq!(body["type"], format!("urn:print-partner:problem:{kind}"));
            assert_eq!(body.get("code").and_then(Value::as_str), code, "{kind}");
        }
        let timeout = Failure(StatusCode::GATEWAY_TIMEOUT, "OAuth provider timed out");
        let (_, _, body) = render(timeout).await;
        assert_eq!(body["type"], "urn:print-partner:problem:timeout");
    }

    #[cfg(not(feature = "desktop_rebuild"))]
    #[tokio::test]
    async fn feature_off_failures_keep_the_node_body() {
        for (status, detail, expected) in [
            (
                StatusCode::NOT_FOUND,
                "API key not found",
                json!({"detail": "API key not found", "title": "Not Found", "status": 404}),
            ),
            (
                StatusCode::NOT_FOUND,
                "User not found",
                json!({"detail": "User not found"}),
            ),
            (
                StatusCode::BAD_REQUEST,
                "API key not found",
                json!({"detail": "API key not found"}),
            ),
            (
                StatusCode::UNAUTHORIZED,
                "Current password is incorrect",
                json!({"detail": "Current password is incorrect", "code": "current_password_rejected"}),
            ),
            (
                StatusCode::UNAUTHORIZED,
                "Authentication required",
                json!({"detail": "Authentication required"}),
            ),
            (
                StatusCode::BAD_REQUEST,
                "Current password is incorrect",
                json!({"detail": "Current password is incorrect"}),
            ),
        ] {
            let rendered = render(Failure(status, detail)).await;
            assert_eq!(rendered, (status, "application/json".into(), expected));
        }
    }
}
