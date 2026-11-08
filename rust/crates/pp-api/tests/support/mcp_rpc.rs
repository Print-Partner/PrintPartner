//! Shared by the MCP HTTP tests: the auth policy, a loopback listener, the
//! raw reply and the JSON-RPC message in it.
use pp_storage::auth;
use reqwest::RequestBuilder;
use serde_json::Value;

pub fn policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
        first_user: auth::FirstUserTenant::NewUser,
    }
}

/// A listener on a free loopback port, and its origin.
pub async fn listen() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    (listener, origin)
}

/// Sends `request`; returns status, body text and any MCP session id.
pub async fn send(request: RequestBuilder) -> (u16, String, Option<String>) {
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let session = response
        .headers()
        .get("mcp-session-id")
        .map(|value| value.to_str().unwrap().to_owned());
    (status, response.text().await.unwrap(), session)
}

/// The first JSON-RPC message in an SSE or JSON response body.
pub fn rpc(body: &str) -> Value {
    // Skip the empty SSE priming event that precedes the response.
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .unwrap_or(body);
    serde_json::from_str(data).unwrap_or_else(|_| panic!("JSON-RPC body: {body}"))
}
