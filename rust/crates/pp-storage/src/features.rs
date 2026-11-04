use crate::{Envelope, SettingSnapshot, Shared, WriterOwner, auth, read_model::Credential};
use anyhow::{Result, anyhow, ensure};
use pp_domain::{
    PositiveId,
    features::{Effective, Feature, Toggles, resolve},
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub mod modules;

const USER_KEY: &str = "feature_toggles";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Plan(PositiveId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    user: Toggles,
    plan: Option<(PositiveId, Toggles)>,
    effective: Effective,
}

impl View {
    pub fn user(&self) -> &Toggles {
        &self.user
    }
    pub fn plan(&self) -> Option<(PositiveId, &Toggles)> {
        self.plan.as_ref().map(|(id, toggles)| (*id, toggles))
    }
    pub fn effective(&self) -> &Effective {
        &self.effective
    }
}

#[derive(Clone)]
pub struct FeatureClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}
pub(super) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    scope: Scope,
    patch: Option<(Feature, Option<bool>)>,
}

impl WriterOwner {
    pub fn features_with_policy(&self, policy: auth::AuthPolicy) -> Result<FeatureClient> {
        auth::validate_policy(policy)?;
        Ok(FeatureClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}
impl FeatureClient {
    pub fn read(
        &self,
        credential: Credential,
        plan: Option<PositiveId>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<View> {
        let scope = plan.map_or(Scope::User, Scope::Plan);
        self.submit(credential, scope, None, cancelled, wait)
    }
    pub fn set(
        &self,
        credential: Credential,
        scope: Scope,
        feature: Feature,
        choice: Option<bool>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<View> {
        self.submit(credential, scope, Some((feature, choice)), cancelled, wait)
    }
    fn submit(
        &self,
        credential: Credential,
        scope: Scope,
        patch: Option<(Feature, Option<bool>)>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<View> {
        let deadline = Instant::now() + wait;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!("Writer admission poisoned"))?;
        loop {
            ensure!(!queue.closed, "Storage stopped");
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Cancelled before admission"
            );
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, "Writer queue full");
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!("Writer admission poisoned"))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::Features {
            command: Command {
                credential,
                policy: self.policy,
                scope,
                patch,
            },
            reply,
        });
        self.shared.changed.notify_all();
        drop(queue);
        receiver
            .recv()
            .map_err(|_| anyhow!("Storage stopped without feature toggles result"))?
    }
}

pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<View> {
    let tx = match command.patch {
        Some(_) => crate::writer_admission::begin_immediate(connection, admission)?,
        None => connection.transaction_with_behavior(TransactionBehavior::Deferred)?,
    };
    let tenant = match command.credential {
        Credential::Session(secret) => auth::read_session_tenant(&tx, &secret, command.policy)?,
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => auth::read_key_tenant(&tx, &routed_tenant, &secret)?,
    };
    let plan = match command.scope {
        Scope::User => None,
        Scope::Plan(id) => Some(id),
    };
    let mut layers = load_layers(&tx, &tenant, plan)?;
    if let Some((feature, choice)) = command.patch {
        let (key, layer) = match &mut layers.plan {
            Some((_, key, toggles)) => (key.as_str(), toggles),
            None => (USER_KEY, &mut layers.user),
        };
        layer.toggles.set(feature, choice);
        match choice {
            Some(enabled) => layer.raw.insert(feature.key().into(), enabled.into()),
            None => layer.raw.remove(feature.key()),
        };
        store(&tx, &tenant, key, layer)?;
    }
    tx.commit()?;
    Ok(layers.into_view())
}

struct Layers {
    user: Layer,
    plan: Option<(PositiveId, String, Layer)>,
}
impl Layers {
    fn into_view(self) -> View {
        let user = self.user.toggles;
        let plan = self.plan.map(|(id, _, layer)| (id, layer.toggles));
        View {
            effective: resolve(&user, plan.as_ref().map(|(_, toggles)| toggles)),
            user,
            plan,
        }
    }
}

fn load_layers(tx: &Transaction<'_>, tenant: &str, plan: Option<PositiveId>) -> Result<Layers> {
    let plan = match plan {
        None => None,
        Some(id) => {
            let owned: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM build_profiles WHERE id=?1 AND tenant_id=?2)",
                params![i64::try_from(id.get())?, tenant],
                |row| row.get(0),
            )?;
            ensure!(owned, "Plan not found");
            let key = format!("{USER_KEY}:plan:{}", id.get());
            let toggles = load(tx, tenant, &key)?;
            Some((id, key, toggles))
        }
    };
    let user = load(tx, tenant, USER_KEY)?;
    Ok(Layers { user, plan })
}

pub(crate) fn resolve_for_tenant(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: Option<PositiveId>,
) -> Result<View> {
    Ok(load_layers(tx, tenant, plan)?.into_view())
}

#[derive(Default)]
struct Layer {
    raw: BTreeMap<String, serde_json::Value>,
    toggles: Toggles,
}

fn load(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Layer> {
    match crate::snapshot(tx, tenant, key)? {
        SettingSnapshot::Missing => Ok(Layer::default()),
        SettingSnapshot::Stored { value } => {
            let toggles = serde_json::from_str(&value)
                .map_err(|error| anyhow!("Stored {key} is invalid: {error}"))?;
            let raw = serde_json::from_str(&value)
                .map_err(|error| anyhow!("Stored {key} is invalid: {error}"))?;
            Ok(Layer { raw, toggles })
        }
    }
}

fn store(tx: &Transaction<'_>, tenant: &str, key: &str, layer: &Layer) -> Result<()> {
    if layer.raw.is_empty() {
        tx.execute(
            "DELETE FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key],
        )?;
    } else {
        tx.execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
            params![tenant, key, serde_json::to_string(&layer.raw)?],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
