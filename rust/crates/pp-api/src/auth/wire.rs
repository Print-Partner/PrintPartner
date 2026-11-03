use super::CookieTransport;
use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use pp_storage::auth::{AuthFailure, Secret, User};
use serde_json::{Value, json};

#[derive(Debug)]
pub(super) struct Failure(pub StatusCode, pub &'static str);
impl IntoResponse for Failure {
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
            Some(QueueFull | Stopped) => (503, "Authentication temporarily unavailable"),
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
/// `GET /health` body.
#[derive(Debug, serde::Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct Health {
    pub ok: bool,
    pub authentication_required: bool,
    pub authenticated: bool,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_storage::build_graph::wire::RequiredNullable<String>")
    )]
    pub degraded_detail: Option<&'static str>,
    pub capabilities: Vec<&'static str>,
    pub multi_user: bool,
    pub single_user_auth: bool,
    pub single_user_setup_required: bool,
    pub registration_open: bool,
    pub owner_mapping_required: bool,
    pub github_oauth_configured: bool,
    pub discord_oauth_configured: bool,
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

    #[tokio::test]
    async fn failure_response_contracts() {
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
            let response = Failure(status, detail).into_response();
            assert_eq!(response.status(), status, "{detail}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body, expected, "{status} {detail}");
        }
    }
}
