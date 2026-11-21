//! Shared by the MCP HTTP tests: the auth policy, a loopback listener, the
//! raw reply and the JSON-RPC message in it.

#[path = "auth_policy.rs"]
mod auth_policy;
#[path = "mcp_reply.rs"]
mod mcp_reply;
pub use auth_policy::policy;
pub use mcp_reply::{rpc, send};

/// A listener on a free loopback port, and its origin.
pub async fn listen() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    (listener, origin)
}
