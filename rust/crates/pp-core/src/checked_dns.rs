//! The DNS step of a checked outbound `reqwest` client: resolve once, and
//! refuse the answer unless every address passes the caller's check.

use pp_net::outbound::Resolver;
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

/// The most addresses one answer may carry.
const MAX_ADDRESSES: usize = 32;

/// Refuses an empty answer, one longer than [`MAX_ADDRESSES`], or one with
/// any address `allowed` rejects, with `refusal` as the error text.
pub(crate) struct CheckedDns {
    resolver: Arc<dyn Resolver>,
    allowed: fn(IpAddr) -> bool,
    refusal: &'static str,
}

impl CheckedDns {
    pub(crate) fn new(
        resolver: Arc<dyn Resolver>,
        allowed: fn(IpAddr) -> bool,
        refusal: &'static str,
    ) -> Self {
        Self {
            resolver,
            allowed,
            refusal,
        }
    }
}

impl reqwest::dns::Resolve for CheckedDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let future = self.resolver.resolve(name.as_str().into());
        let (allowed, refusal) = (self.allowed, self.refusal);
        Box::pin(async move {
            let addresses = future.await?;
            if addresses.is_empty()
                || addresses.len() > MAX_ADDRESSES
                || !addresses.iter().all(|ip| allowed(*ip))
            {
                return Err(refusal.into());
            }
            Ok(
                Box::new(addresses.into_iter().map(|ip| SocketAddr::new(ip, 0)))
                    as reqwest::dns::Addrs,
            )
        })
    }
}
