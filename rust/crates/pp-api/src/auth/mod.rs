mod http;
mod mail;
mod provider;
mod wire;
pub use http::{AuthBackend, AuthHttpConfig, CookieTransport, auth_router};
pub use mail::{Delivery, ResetMailer, SmtpConfig, SmtpSecurity};
pub use provider::{OAuthCredentials, ProviderClient};
#[cfg(feature = "codegen")]
pub(crate) use wire::Health;

/// The provider and mail suites run on the pooled client by default and on
/// the rebuild client with `desktop_rebuild`, so each CI configuration covers one.
#[cfg(test)]
fn test_backend(
    owner: &pp_storage::WriterOwner,
    policy: pp_storage::auth::AuthPolicy,
) -> AuthBackend {
    #[cfg(feature = "desktop_rebuild")]
    let client = owner.rebuild_auth_with_policy(policy, Default::default());
    #[cfg(not(feature = "desktop_rebuild"))]
    let client = owner.auth_with_policy(policy);
    client.unwrap().into()
}
