//! What `POST /auth/logout` does to a runtime once Rust has revoked the
//! account session.
use super::GatewayState;

/// Chosen at startup: the desktop app ends with its sign-out; the headless
/// server keeps serving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignOut {
    /// The desktop app: clear every desktop session and stop the runtime.
    EndsRuntime,
    /// The headless server (`pp-server`): keep serving, so the browser returns
    /// to the sign-in screen without a restart or a new bootstrap URL.
    KeepsServing,
}

impl SignOut {
    pub(super) fn finish(self, state: &GatewayState) {
        if self == Self::KeepsServing {
            return;
        }
        state
            .access
            .lock()
            .expect("Session mutex poisoned")
            .sessions
            .clear();
        state.watcher_cancelled.cancel();
        state.cancelled.cancel();
        state.shutdown_requested.cancel();
    }
}
