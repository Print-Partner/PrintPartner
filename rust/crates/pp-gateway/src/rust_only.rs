//! Startup for a Rust-only gateway, with an optional explicit network listener.
use super::{
    Gateway, GatewayState, MoonrakerHosts, MoonrakerStatus, OperationOwners, RustOnlyMode,
    RustStatus, ServedUi, SignOut,
};
use anyhow::Result;
use pp_compat::CompatHandle;
use std::{borrow::Cow, sync::Arc};
use tokio::net::TcpListener;

impl Gateway {
    /// Dispatch through the gateway's HTTP admission; transports supply `ConnectInfo`.
    pub async fn respond(
        &self,
        request: axum::http::Request<axum::body::Body>,
    ) -> axum::http::Response<axum::body::Body> {
        super::handle(axum::extract::State(self.state.clone()), request).await
    }

    /// Start a runtime with no Node child: `product` answers admitted
    /// requests, nothing is forwarded, and `/__runtime` reports `status`.
    /// Moonraker watchers poll the hosts `hosts` reads; `product` receives
    /// the status lookup so an authenticated route can answer from them.
    /// `ui` is the legacy build's root, or the replacement bytes release
    /// verification retained.
    pub fn start_rust_only(
        listener: TcpListener,
        ui: ServedUi,
        product: impl FnOnce(MoonrakerStatus) -> axum::Router + 'static,
        status: tokio::sync::watch::Receiver<RustStatus>,
        hosts: Arc<dyn MoonrakerHosts>,
        sign_out: SignOut,
    ) -> Result<Self> {
        Self::start_rust_only_with_network(
            listener,
            RustOnlyMode {
                network: false,
                sign_out,
            },
            ui,
            product,
            status,
            hosts,
        )
    }

    /// Explicit headless listener. Desktop callers use `start_rust_only`.
    pub fn start_rust_only_with_network(
        listener: TcpListener,
        mode: RustOnlyMode,
        ui: ServedUi,
        product: impl FnOnce(MoonrakerStatus) -> axum::Router + 'static,
        status: tokio::sync::watch::Receiver<RustStatus>,
        hosts: Arc<dyn MoonrakerHosts>,
    ) -> Result<Self> {
        Self::start_inner(
            listener,
            ui,
            CompatHandle::unpublished(),
            &OperationOwners::node_only(),
            Some(Box::new(product)),
            Some((status, hosts, mode)),
        )
    }
}

/// Desktop requests keep the configured authority. Explicit listener requests
/// use the validated IP authority visible through Docker's published port.
/// MCP clients send no Origin, so on the MCP paths an unsafe request may omit
/// it; the MCP key guard then admits it only with an API key.
pub(super) fn authority<'a, B>(
    state: &'a GatewayState,
    request: &axum::http::Request<B>,
) -> Result<(Cow<'a, str>, Cow<'a, str>)> {
    if state.network {
        let (origin, host) = if super::is_mcp(request.uri().path()) {
            pp_net::inbound::authority(request)?
        } else {
            pp_net::inbound::origin(request)?
        };
        Ok((Cow::Owned(origin), Cow::Owned(host)))
    } else {
        Ok((Cow::Borrowed(&state.origin), Cow::Borrowed(&state.host)))
    }
}
