//! Startup for a Rust-only gateway, with an optional explicit network listener.
use super::{
    Gateway, GatewayState, MoonrakerHosts, MoonrakerStatus, OperationOwners, RustStatus, ServedUi,
};
use anyhow::Result;
use pp_compat::CompatHandle;
use std::{borrow::Cow, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;

impl Gateway {
    /// Start a runtime with no Node child: `product` answers admitted
    /// requests, nothing is forwarded, and `/__runtime` reports `status`.
    /// Moonraker watchers poll the hosts `hosts` reads; `product` receives
    /// the status lookup so an authenticated route can answer from them.
    /// `ui` names the build in `assets`; only [`ServedUi::Replacement`] gets
    /// the replacement Content-Security-Policy.
    pub fn start_rust_only(
        listener: TcpListener,
        assets: PathBuf,
        ui: ServedUi,
        product: impl FnOnce(MoonrakerStatus) -> axum::Router + 'static,
        status: tokio::sync::watch::Receiver<RustStatus>,
        hosts: Arc<dyn MoonrakerHosts>,
    ) -> Result<Self> {
        Self::start_rust_only_with_network(listener, false, assets, ui, product, status, hosts)
    }

    /// Explicit headless listener. Desktop callers use `start_rust_only`.
    pub fn start_rust_only_with_network(
        listener: TcpListener,
        network: bool,
        assets: PathBuf,
        ui: ServedUi,
        product: impl FnOnce(MoonrakerStatus) -> axum::Router + 'static,
        status: tokio::sync::watch::Receiver<RustStatus>,
        hosts: Arc<dyn MoonrakerHosts>,
    ) -> Result<Self> {
        Self::start_inner(
            listener,
            assets,
            ui,
            CompatHandle::unpublished(),
            &OperationOwners::node_only(),
            Some(Box::new(product)),
            Some((status, hosts, network)),
        )
    }
}

/// Desktop requests keep the configured authority. Explicit listener requests
/// use the validated IP authority visible through Docker's published port.
pub(super) fn authority<'a, B>(
    state: &'a GatewayState,
    request: &axum::http::Request<B>,
) -> Result<(Cow<'a, str>, Cow<'a, str>)> {
    if state.network {
        let (origin, host) = pp_net::inbound::origin(request)?;
        Ok((Cow::Owned(origin), Cow::Owned(host)))
    } else {
        Ok((Cow::Borrowed(&state.origin), Cow::Borrowed(&state.host)))
    }
}
