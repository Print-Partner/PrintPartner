//! Checked outbound HTTPS through `reqwest`, for the clients that are not
//! printer HTTP (GitHub reads, the filament provider).
//!
//! [`checked_client`] returns a builder that:
//! - resolves every connection's host through [`CheckedDns`], so the first
//!   request and each redirect hop connect only to addresses the [`Policy`]
//!   allows;
//! - uses rustls with the aws-lc-rs provider named here, never a process-wide
//!   default that another crate installed, and the platform's roots;
//! - verifies the certificate against the request URL's host, never the
//!   resolved address;
//! - ignores `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY`, and never retries.
//!
//! reqwest does not call the resolver for an IP-literal host. Callers check a
//! literal host with [`Policy::allows`] before the first request and follow
//! redirects only within that origin.

use crate::outbound::{Resolver, permitted};
use anyhow::Result;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use rustls::{ClientConfig, crypto::CryptoProvider, pki_types::CertificateDer};
use rustls_platform_verifier::Verifier;
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

/// The most addresses one DNS answer may carry.
pub const MAX_ADDRESSES: usize = 32;

/// Which resolved addresses a client may connect to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// What [`permitted`] allows: LAN and loopback hosts, as a filament
    /// provider on the user's network can be.
    Lan,
    /// [`Policy::Lan`] without loopback, RFC 1918 private or unique-local
    /// addresses: a public service such as GitHub.
    Public,
}

impl Policy {
    pub fn allows(self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        };
        permitted(ip)
            && match (self, ip) {
                (Policy::Lan, _) => true,
                (Policy::Public, IpAddr::V4(v4)) => !v4.is_loopback() && !v4.is_private(),
                (Policy::Public, IpAddr::V6(v6)) => {
                    !v6.is_loopback() && v6.segments()[0] & 0xfe00 != 0xfc00
                }
            }
    }
}

/// The DNS step of a checked client: one lookup per connection, refused with
/// `refusal` as the error text when the answer is empty, longer than
/// [`MAX_ADDRESSES`], or holds any address the policy refuses.
pub struct CheckedDns {
    resolver: Arc<dyn Resolver>,
    policy: Policy,
    refusal: &'static str,
}

impl CheckedDns {
    pub fn new(resolver: Arc<dyn Resolver>, policy: Policy, refusal: &'static str) -> Self {
        Self {
            resolver,
            policy,
            refusal,
        }
    }
}

impl Resolve for CheckedDns {
    fn resolve(&self, name: Name) -> Resolving {
        let answer = self.resolver.resolve(name.as_str().into());
        let (policy, refusal) = (self.policy, self.refusal);
        Box::pin(async move {
            let addresses = answer.await?;
            if addresses.is_empty()
                || addresses.len() > MAX_ADDRESSES
                || !addresses.iter().all(|ip| policy.allows(*ip))
            {
                return Err(refusal.into());
            }
            Ok(Box::new(addresses.into_iter().map(|ip| SocketAddr::new(ip, 0))) as Addrs)
        })
    }
}

/// The provider every checked client uses.
pub fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// The rustls setup behind [`checked_client`]: aws-lc-rs, TLS 1.2 and 1.3,
/// HTTP/1.1 over ALPN, and the platform verifier with `extra_roots` added.
pub fn tls_config(extra_roots: Vec<CertificateDer<'static>>) -> Result<ClientConfig> {
    let provider = crypto_provider();
    let verifier = if extra_roots.is_empty() {
        Verifier::new(provider.clone())?
    } else {
        Verifier::new_with_extra_roots(extra_roots, provider.clone())?
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// A `reqwest` builder with checked DNS, the explicit TLS setup, no proxy and
/// no retries. Callers add their redirect policy and deadlines.
pub fn checked_client(dns: CheckedDns) -> Result<reqwest::ClientBuilder> {
    checked_client_trusting(dns, Vec::new())
}

/// [`checked_client`] that also trusts `extra_roots`, for a local TLS fixture.
pub fn checked_client_trusting(
    dns: CheckedDns,
    extra_roots: Vec<CertificateDer<'static>>,
) -> Result<reqwest::ClientBuilder> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .dns_resolver(Arc::new(dns))
        .tls_backend_preconfigured(tls_config(extra_roots)?))
}
