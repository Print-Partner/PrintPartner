//! Binds each MCP session to the API key that opened it (GRE-453).
//!
//! rmcp's session manager knows session IDs only, so the guard records the
//! key ID each session was opened with. A request that names a session gets
//! the same `404` rmcp gives an unknown session unless its own key opened
//! it, on either alias, so another key cannot tell a foreign session from a
//! missing one. A rotated key is a new key ID and opens a fresh session. The
//! table lives in memory beside the sessions, so a restart forgets both.

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const SESSION_ID: &str = "mcp-session-id";

/// Session ID to the ID of the key that opened it.
pub(super) struct SessionKeys {
    sessions: Arc<LocalSessionManager>,
    owners: Mutex<HashMap<String, String>>,
}

impl SessionKeys {
    pub(super) fn new(sessions: Arc<LocalSessionManager>) -> Self {
        Self {
            sessions,
            owners: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `key_id` opened the session `id`. A session this table does
    /// not know, including one from before a restart, belongs to no key.
    pub(super) fn opened_by(&self, id: &str, key_id: &str) -> bool {
        self.owners().get(id).is_some_and(|owner| owner == key_id)
    }

    /// Binds the session `opened` to `key_id`. Bindings of sessions rmcp has
    /// since closed or timed out are dropped first, so the table holds no
    /// more entries than rmcp holds sessions.
    pub(super) async fn bind(&self, opened: String, key_id: String) {
        let live = self.sessions.sessions.read().await;
        let mut owners = self.owners();
        owners.retain(|session, _| live.contains_key(session.as_str()));
        owners.insert(opened, key_id);
    }

    /// Drops the binding of a session its key closed with DELETE.
    pub(super) fn forget(&self, id: &str) {
        self.owners().remove(id);
    }

    fn owners(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The session a request or response names. A value that is not text names a
/// session no key opened.
pub(super) fn named(headers: &HeaderMap) -> Option<String> {
    headers
        .get(SESSION_ID)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
}

/// rmcp's answer for an unknown session, byte for byte.
pub(super) fn not_found() -> Response {
    let mut response = Response::new(Body::from("Not Found: Session not found"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}
