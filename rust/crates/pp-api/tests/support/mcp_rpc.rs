//! Shared by the MCP HTTP tests: the auth policy, a loopback listener, the
//! raw reply and the JSON-RPC message in it.
use reqwest::RequestBuilder;
use serde_json::Value;

#[path = "auth_policy.rs"]
mod auth_policy;
pub use auth_policy::policy;

#[path = "listener.rs"]
mod listener;
pub use listener::listen;

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
