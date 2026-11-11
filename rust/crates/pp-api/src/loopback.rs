//! Shared loopback Host/Origin admission for rebuild HTTP surfaces.
use anyhow::{Result, ensure};
use axum::http::{HeaderMap, Method, StatusCode};
use pp_storage::{auth::Secret, read_model::Credential};
use std::net::IpAddr;

/// Parsed loopback listener origin (`http://127.0.0.1:port`) and its Host form.
#[derive(Clone, Debug)]
pub struct LoopbackOrigin {
    pub origin: String,
    pub host: String,
}

impl LoopbackOrigin {
    pub fn parse(origin: &str) -> Result<Self> {
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
        let origin = url.origin().ascii_serialization();
        let host = origin
            .split_once("://")
            .ok_or_else(|| anyhow::anyhow!("Listener must be an origin"))?
            .1
            .to_owned();
        Ok(Self { origin, host })
    }
}

pub fn rebuild_flag_enabled(flag: Option<&str>) -> bool {
    matches!(flag, Some("1" | "true"))
}

pub fn require_api_key_tenant(api_key_tenant: &str) -> Result<()> {
    ensure!(
        !api_key_tenant.trim().is_empty() && api_key_tenant.len() <= 512,
        "Invalid API key tenant"
    );
    Ok(())
}

/// The one credential a rebuild request presented. API keys route to the
/// configured tenant; request headers cannot select another tenant.
#[derive(Clone, Debug)]
pub enum PresentedCredential {
    Session(String),
    ApiKey { tenant: String, secret: String },
}

impl PresentedCredential {
    pub fn into_credential(self) -> Credential {
        match self {
            Self::Session(value) => Credential::Session(Secret::new(value)),
            Self::ApiKey { tenant, secret } => Credential::ApiKey {
                routed_tenant: tenant,
                secret: Secret::new(secret),
            },
        }
    }
}

pub fn principal(headers: &HeaderMap, tenant: &str) -> Option<PresentedCredential> {
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
    let authorization = headers.get("authorization");
    let custom = headers.get("x-print-partner-api-key");
    if headers.get_all("authorization").iter().count() > 1
        || headers.get_all("x-print-partner-api-key").iter().count() > 1
        || usize::from(session.is_some())
            + usize::from(authorization.is_some())
            + usize::from(custom.is_some())
            != 1
    {
        return None;
    }
    if let Some(value) = session {
        return (!value.is_empty() && value.len() <= 4096)
            .then(|| PresentedCredential::Session(value.into()));
    }
    let key = if let Some(value) = authorization {
        value.to_str().ok()?.strip_prefix("Bearer ")?
    } else {
        custom?.to_str().ok()?
    }
    .trim();
    (!key.is_empty() && key.len() <= 4096).then(|| PresentedCredential::ApiKey {
        tenant: tenant.into(),
        secret: key.into(),
    })
}

/// Loopback transport, Host, Origin, and cross-site checks shared by drafts and builds.
pub fn admit(
    origin: &str,
    host: &str,
    api_key_tenant: &str,
    request: &axum::extract::Request,
) -> std::result::Result<(), (StatusCode, &'static str)> {
    crate::NetworkAdmission::peer(request)?;
    let network = request.extensions().get::<crate::NetworkAdmission>();
    let origin = network.map_or(origin, |value| value.origin.as_str());
    let host = network.map_or(host, |value| value.host.as_str());
    let headers = request.headers();
    let authority = request.uri().authority().map(|value| value.as_str());
    let request_host = headers.get("host").and_then(|value| value.to_str().ok());
    if headers.get_all("host").iter().count() > 1
        || (headers.contains_key("host") && request_host != Some(host))
        || authority.or(request_host) != Some(host)
        || headers
            .keys()
            .any(|key| key == "forwarded" || key.as_str().starts_with("x-forwarded-"))
    {
        return Err((StatusCode::FORBIDDEN, "Invalid request host"));
    }
    let request_origin = headers.get("origin").and_then(|value| value.to_str().ok());
    if headers.get_all("origin").iter().count() > 1
        || (headers.contains_key("origin") && request_origin != Some(origin))
        || (!matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        ) && request_origin != Some(origin)
            && !matches!(
                principal(headers, api_key_tenant),
                Some(PresentedCredential::ApiKey { .. })
            ))
        || headers
            .get_all("sec-fetch-site")
            .iter()
            .any(|value| value == "cross-site")
    {
        return Err((StatusCode::FORBIDDEN, "Invalid request origin"));
    }
    Ok(())
}
