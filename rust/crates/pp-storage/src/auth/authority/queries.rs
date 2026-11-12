use super::super::*;

pub(crate) struct StreamAuthority {
    pub(crate) tenant: String,
    pub(crate) subject: String,
    pub(crate) identity: StreamAuthorityIdentity,
    pub(crate) expires_at: Option<i64>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum StreamAuthorityIdentity {
    Session(String),
    Key { tenant: String, key_id: String },
}
pub(crate) fn resolve_stream_authority(
    tx: &Transaction<'_>,
    credential: &crate::jobs::Credential,
    policy: AuthPolicy,
    touch_key: bool,
) -> Result<StreamAuthority> {
    match credential {
        crate::jobs::Credential::Session(secret) => {
            ensure!(
                !secret.expose().is_empty() && secret.expose().len() <= 4096,
                AuthFailure::SessionRequired
            );
            let digest = crypto::digest(secret.expose());
            let user = actor(tx, &digest)?;
            let tenant = policy::tenant_for_authenticated_actor(tx, &user, policy)?;
            let expires: String = tx.query_row(
                "SELECT expires_at FROM sessions WHERE id=?1 AND expires_at>?2",
                params![digest, timestamp(0)],
                |row| row.get(0),
            )?;
            let expires_at = time::OffsetDateTime::parse(
                &expires,
                &time::format_description::well_known::Rfc3339,
            )?
            .unix_timestamp();
            Ok(StreamAuthority {
                tenant,
                subject: format!("user:{}", user.user_id),
                identity: StreamAuthorityIdentity::Session(digest),
                expires_at: Some(expires_at),
            })
        }
        crate::jobs::Credential::RoutedKey { tenant, key } => {
            let authority = keys::stream_authority(tx, tenant, key, touch_key)?
                .ok_or(AuthFailure::SessionRequired)?;
            Ok(StreamAuthority {
                tenant: authority.principal.tenant_id.clone(),
                subject: format!("key:{}", authority.principal.key_id),
                identity: StreamAuthorityIdentity::Key {
                    tenant: authority.principal.tenant_id,
                    key_id: authority.key_id,
                },
                expires_at: authority.expires_at,
            })
        }
        crate::jobs::Credential::PhysicalOwner(_) => Err(anyhow!(AuthFailure::SessionRequired)),
    }
}
pub(crate) fn job_key_actor_ref(
    tx: &Transaction<'_>,
    tenant: &str,
    key: &Secret,
) -> Result<(String, String)> {
    match keys::resolve_ref(tx, tenant, key)? {
        Outcome::KeyResolved {
            principal: Some(principal),
            ..
        } => Ok((principal.tenant_id, format!("key:{}", principal.key_id))),
        _ => Err(AuthFailure::SessionRequired.into()),
    }
}

pub(crate) fn reconciliation_actor(
    tx: &Transaction<'_>,
    credential: crate::read_model::Credential,
    policy: AuthPolicy,
) -> Result<(String, String)> {
    reconciliation_actor_ref(tx, &credential, policy)
}
pub(crate) fn reconciliation_actor_ref(
    tx: &Transaction<'_>,
    credential: &crate::read_model::Credential,
    policy: AuthPolicy,
) -> Result<(String, String)> {
    match credential {
        crate::read_model::Credential::Session(secret) => {
            ensure!(
                !secret.expose().is_empty() && secret.expose().len() <= 4096,
                AuthFailure::SessionRequired
            );
            let user = actor(tx, &crypto::digest(secret.expose()))?;
            let tenant = policy::tenant_for_authenticated_actor(tx, &user, policy)?;
            Ok((tenant, user.user_id))
        }
        crate::read_model::Credential::ApiKey {
            routed_tenant,
            secret,
        } => match keys::resolve_ref(tx, routed_tenant, secret)? {
            Outcome::KeyResolved {
                principal: Some(principal),
                ..
            } => {
                let actor = format!("tenant:{}", principal.tenant_id);
                Ok((principal.tenant_id, actor))
            }
            _ => Err(AuthFailure::SessionRequired.into()),
        },
    }
}

pub(crate) fn observe_reconciliation_actor(
    tx: &Transaction<'_>,
    credential: &crate::read_model::Credential,
    policy: AuthPolicy,
) -> Result<(String, String)> {
    match credential {
        crate::read_model::Credential::Session(secret) => {
            ensure!(
                !secret.expose().is_empty() && secret.expose().len() <= 4096,
                AuthFailure::SessionRequired
            );
            let user = actor(tx, &crypto::digest(secret.expose()))?;
            let tenant = policy::tenant_for_authenticated_actor(tx, &user, policy)?;
            Ok((tenant, user.user_id))
        }
        crate::read_model::Credential::ApiKey {
            routed_tenant,
            secret,
        } => {
            let tenant = read_key_tenant(tx, routed_tenant, secret)?;
            Ok((tenant.clone(), format!("tenant:{tenant}")))
        }
    }
}
