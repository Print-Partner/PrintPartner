#[cfg(feature = "desktop_rebuild")]
mod authority;
use super::{KeyInfo, KeyPrincipal, LocalCommit, Outcome, Secret, crypto, timestamp};
use anyhow::{Result, anyhow, ensure};
#[cfg(feature = "desktop_rebuild")]
pub(crate) use authority::{admit_reference, resolve_ref, resolve_reference, stream_authority};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub(crate) enum Action {
    List,
    Create,
    Revoke(String),
    Rotate(String),
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredKey {
    id: String,
    key_hash: String,
    created_at: String,
    last_used_at: Option<String>,
    expires_at: Option<String>,
    is_active: bool,
}
impl StoredKey {
    fn info(&self) -> KeyInfo {
        KeyInfo {
            id: self.id.clone(),
            created_at: self.created_at.clone(),
            last_used_at: self.last_used_at.clone(),
            expires_at: self.expires_at.clone(),
            is_active: self.is_active,
        }
    }
}
fn digest(raw: &str) -> Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"print-partner:settings-api-key:v1")
        .map_err(|_| anyhow!("Invalid API key hashing context"))?;
    mac.update(raw.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}
fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn migrate(value: &str) -> Result<String> {
    let decoded = STANDARD
        .decode(value)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    match decoded {
        Some(decoded) if STANDARD.encode(decoded.as_bytes()) == value => digest(&decoded),
        _ => digest(value),
    }
}
fn load(tx: &Transaction<'_>, tenant: &str) -> Result<(Vec<StoredKey>, bool)> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='api_keys_v1'",
            [tenant],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Ok((Vec::new(), false));
    };
    ensure!(raw.len() <= 1024 * 1024, "API key collection too large");
    let mut keys: Vec<StoredKey> =
        serde_json::from_str(&raw).map_err(|_| anyhow!("Corrupt API key collection"))?;
    ensure!(keys.len() <= 4096, "API key collection too large");
    let mut ids = std::collections::HashSet::new();
    let mut migrated = false;
    for key in &mut keys {
        ensure!(
            !key.id.is_empty()
                && key.id.len() <= 128
                && ids.insert(key.id.clone())
                && !key.key_hash.is_empty()
                && key.key_hash.len() <= 4096,
            "Corrupt API key collection"
        );
        if !is_digest(&key.key_hash) {
            key.key_hash = migrate(&key.key_hash)?;
            migrated = true;
        }
    }
    Ok((keys, migrated))
}
fn save(tx: &Transaction<'_>, tenant: &str, keys: &[StoredKey]) -> Result<()> {
    let value = serde_json::to_string(keys)?;
    ensure!(value.len() <= 1024 * 1024, "API key collection too large");
    tx.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,'api_keys_v1',?2) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", params![tenant,value])?;
    Ok(())
}
fn create(
    keys: &mut Vec<StoredKey>,
    #[cfg(feature = "desktop_rebuild")] lifetime: Option<i64>,
) -> Result<Outcome> {
    ensure!(keys.len() < 4096, "API key collection too large");
    let raw = Secret(format!("ppk_{}", hex::encode(crypto::random::<32>()?)));
    let key = StoredKey {
        id: format!("key_{}", hex::encode(crypto::random::<8>()?)),
        key_hash: digest(raw.expose())?,
        created_at: timestamp(0),
        last_used_at: None,
        #[cfg(not(feature = "desktop_rebuild"))]
        expires_at: None,
        #[cfg(feature = "desktop_rebuild")]
        expires_at: lifetime.map(timestamp),
        is_active: true,
    };
    let info = key.info();
    keys.push(key);
    Ok(Outcome::KeyCreated { info, key: raw })
}
pub(super) fn manage(
    tx: &Transaction<'_>,
    tenant: &str,
    action: Action,
    #[cfg(feature = "desktop_rebuild")] lifetime: Option<i64>,
) -> Result<Outcome> {
    let (mut keys, mut changed) = load(tx, tenant)?;
    let outcome = match action {
        Action::List => Outcome::Keys {
            keys: keys.iter().map(StoredKey::info).collect(),
            commit: if changed {
                LocalCommit::Committed
            } else {
                LocalCommit::ReadOnly
            },
        },
        Action::Create => {
            changed = true;
            create(
                &mut keys,
                #[cfg(feature = "desktop_rebuild")]
                lifetime,
            )?
        }
        Action::Revoke(id) => {
            let found = keys.iter_mut().find(|key| key.id == id);
            let exists = found.is_some();
            if let Some(key) = found {
                key.is_active = false;
                changed = true;
            }
            Outcome::KeyChanged {
                changed: exists,
                commit: if changed {
                    LocalCommit::Committed
                } else {
                    LocalCommit::ReadOnly
                },
            }
        }
        Action::Rotate(id) => {
            if let Some(key) = keys.iter_mut().find(|key| key.id == id) {
                key.is_active = false;
                changed = true;
                create(
                    &mut keys,
                    #[cfg(feature = "desktop_rebuild")]
                    lifetime,
                )?
            } else {
                Outcome::KeyChanged {
                    changed: false,
                    commit: if changed {
                        LocalCommit::Committed
                    } else {
                        LocalCommit::ReadOnly
                    },
                }
            }
        }
    };
    if changed {
        save(tx, tenant, &keys)?;
    }
    Ok(outcome)
}
fn unexpired(expires: &Option<String>) -> bool {
    match expires {
        None => true,
        Some(expires) => {
            time::OffsetDateTime::parse(expires, &time::format_description::well_known::Rfc3339)
                .is_ok_and(|expiry| expiry > time::OffsetDateTime::now_utc())
        }
    }
}
pub(super) fn resolve(tx: &Transaction<'_>, tenant: &str, raw: Secret) -> Result<Outcome> {
    ensure!(
        !tenant.is_empty() && tenant.len() <= 512 && raw.expose().len() <= 4096,
        "Invalid key request"
    );
    if raw.expose().is_empty() {
        return Ok(Outcome::KeyResolved {
            principal: None,
            commit: LocalCommit::ReadOnly,
        });
    }
    let (mut keys, mut changed) = load(tx, tenant)?;
    let hash = digest(raw.expose())?;
    let found = keys.iter_mut().find(|key| {
        key.is_active
            && unexpired(&key.expires_at)
            && bool::from(key.key_hash.as_bytes().ct_eq(hash.as_bytes()))
    });
    let principal = found.map(|key| {
        key.last_used_at = Some(timestamp(0));
        changed = true;
        KeyPrincipal {
            tenant_id: tenant.to_owned(),
            key_id: key.id.clone(),
        }
    });
    if changed {
        save(tx, tenant, &keys)?;
    }
    Ok(Outcome::KeyResolved {
        principal,
        commit: if changed {
            LocalCommit::Committed
        } else {
            LocalCommit::ReadOnly
        },
    })
}

/// Whether `secret` is an active, unexpired API key of `tenant`.
pub(super) fn proves(tx: &Transaction<'_>, tenant: &str, secret: &Secret) -> Result<bool> {
    let sized = !tenant.is_empty()
        && tenant.len() <= 512
        && !secret.expose().is_empty()
        && secret.expose().len() <= 4096;
    if !sized {
        return Ok(false);
    }
    let bytes: i64 = tx.query_row("SELECT coalesce(sum(length(cast(value AS blob))),0) FROM app_settings WHERE key='api_keys_v1' AND tenant_id=?1", [tenant], |r| r.get(0))?;
    ensure!(bytes <= 1024 * 1024, "API key collection too large");
    let (keys, _) = load(tx, tenant)?;
    let hash = digest(secret.expose())?;
    Ok(keys.iter().any(|key| {
        key.is_active
            && unexpired(&key.expires_at)
            && bool::from(key.key_hash.as_bytes().ct_eq(hash.as_bytes()))
    }))
}
