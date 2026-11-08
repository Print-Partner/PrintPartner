use crate::source_scan::node_json::RawObject;
use crate::{Envelope, SettingSnapshot, Shared, WriterOwner, auth, read_model::Credential};
use anyhow::{Result, anyhow, ensure};
use pp_domain::{
    PositiveId,
    features::{Effective, Feature, Source, Toggles, resolve},
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::{Map, Value, json};
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

/// Node's `featurePatchSchema`: for each named feature a choice, or `None` to
/// clear it. It applies in registry order, as Node walks `featureKeys`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeaturePatch(BTreeMap<Feature, Option<bool>>);

impl FeaturePatch {
    /// A JSON object whose keys are all features and whose values are all
    /// booleans or `null`; anything else is `None`.
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        let Value::Object(fields) = serde_json::from_slice(bytes).ok()? else {
            return None;
        };
        fields
            .into_iter()
            .map(|(key, value)| {
                let choice = match value {
                    Value::Bool(enabled) => Some(enabled),
                    Value::Null => None,
                    _ => return None,
                };
                Some((Feature::from_key(&key)?, choice))
            })
            .collect::<Option<_>>()
            .map(Self)
    }
}

/// Node's `GET` (no patch) or `PATCH /plans/{id}/features`.
#[derive(Debug, Clone)]
pub struct PlanFeatureRequest {
    pub plan: PositiveId,
    pub patch: Option<FeaturePatch>,
}

#[derive(Debug)]
pub enum PlanFeatureOutcome {
    /// Node's `{profile_id, overrides, effective}` body.
    View(Value),
    MissingPlan,
    /// The tenant or Plan layer is not an object whose features are booleans.
    InvalidStorage,
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
        let patch = FeaturePatch(BTreeMap::from([(feature, choice)]));
        match &mut layers.plan {
            Some((_, key, layer)) => {
                layer.apply(&patch);
                write(&tx, &tenant, key, &layer.raw.text())?;
            }
            None => {
                layers.user.apply(&patch);
                store_user(&tx, &tenant, &layers.user)?;
            }
        }
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
            ensure!(owned(tx, tenant, id)?, "Plan not found");
            let key = plan_key(id);
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

/// A send's features, as Node's `readFeatureAccess` reads them: a Plan that
/// no longer exists has no layer, so the user's choices apply.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn resolve_for_send(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: Option<PositiveId>,
) -> Result<View> {
    let plan = match plan {
        Some(id) => owned(tx, tenant, id)?.then_some(id),
        None => None,
    };
    resolve_for_tenant(tx, tenant, plan)
}

/// Node's `GET` and `PATCH /plans/{id}/features` in one transaction: the
/// Plan must be the tenant's, then both layers must parse. A patch writes the
/// Plan layer's text even when it is unchanged or empty, as Node's
/// compare-and-set does, and never deletes the row. `overrides` keeps Node's
/// key order: the choices the layer held, in registry order, then the ones the
/// patch added.
pub(crate) fn plan_features(
    tx: &Transaction<'_>,
    tenant: &str,
    request: &PlanFeatureRequest,
) -> Result<PlanFeatureOutcome> {
    if !owned(tx, tenant, request.plan)? {
        return Ok(PlanFeatureOutcome::MissingPlan);
    }
    let key = plan_key(request.plan);
    let (Some(user), Some(mut plan)) = (read(tx, tenant, USER_KEY)?, read(tx, tenant, &key)?)
    else {
        return Ok(PlanFeatureOutcome::InvalidStorage);
    };
    let kept: Vec<Feature> = Feature::ALL
        .into_iter()
        .filter(|feature| plan.toggles.get(*feature).is_some())
        .collect();
    if let Some(patch) = &request.patch {
        plan.apply(patch);
        write(tx, tenant, &key, &plan.raw.text())?;
    }
    let overrides: Map<String, Value> = kept
        .iter()
        .copied()
        .chain(
            Feature::ALL
                .into_iter()
                .filter(|feature| !kept.contains(feature)),
        )
        .filter_map(|feature| {
            let enabled = plan.toggles.get(feature)?;
            Some((feature.key().into(), Value::Bool(enabled)))
        })
        .collect();
    let effective: Map<String, Value> = resolve(&user.toggles, Some(&plan.toggles))
        .iter()
        .map(|resolved| {
            let source = match resolved.source {
                Source::Plan => "plan",
                Source::User => "user",
                Source::Default => "default",
            };
            (
                resolved.feature.key().into(),
                json!({"enabled": resolved.enabled, "source": source}),
            )
        })
        .collect();
    Ok(PlanFeatureOutcome::View(json!({
        "profile_id": request.plan.get(),
        "overrides": overrides,
        "effective": effective,
    })))
}

fn owned(tx: &Transaction<'_>, tenant: &str, plan: PositiveId) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM build_profiles WHERE id=?1 AND tenant_id=?2)",
        params![i64::try_from(plan.get())?, tenant],
        |row| row.get(0),
    )?)
}

fn plan_key(plan: PositiveId) -> String {
    format!("{USER_KEY}:plan:{}", plan.get())
}

/// A stored layer: its members as Node's `parseFeatureLayer` keeps them,
/// unknown keys and number text included, and its feature choices.
#[derive(Default)]
struct Layer {
    raw: RawObject,
    toggles: Toggles,
}

impl Layer {
    fn apply(&mut self, patch: &FeaturePatch) {
        for (&feature, &choice) in &patch.0 {
            self.toggles.set(feature, choice);
            match choice {
                Some(enabled) => self.raw.set(feature.key(), &enabled.to_string()),
                None => self.raw.remove(feature.key()),
            }
        }
    }
}

/// The layer stored at `key`, empty when there is none, or `None` when it is
/// not a JSON object whose feature keys all hold booleans.
fn read(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Option<Layer>> {
    let SettingSnapshot::Stored { value } = crate::snapshot(tx, tenant, key)? else {
        return Ok(Some(Layer::default()));
    };
    let Some(raw) = RawObject::parse(&value) else {
        return Ok(None);
    };
    let mut toggles = Toggles::default();
    for feature in Feature::ALL {
        match raw.get(feature.key()) {
            None => {}
            Some("true") => toggles.set(feature, Some(true)),
            Some("false") => toggles.set(feature, Some(false)),
            Some(_) => return Ok(None),
        }
    }
    Ok(Some(Layer { raw, toggles }))
}

fn load(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Layer> {
    read(tx, tenant, key)?
        .ok_or_else(|| anyhow!("Stored {key} is invalid: expected an object of feature booleans"))
}

/// The tenant layer keeps this client's own format: sorted keys, and no row
/// once it is empty. Node owns `PATCH /settings/features`.
fn store_user(tx: &Transaction<'_>, tenant: &str, layer: &Layer) -> Result<()> {
    let raw: BTreeMap<String, Value> = serde_json::from_str(&layer.raw.text())?;
    if raw.is_empty() {
        tx.execute(
            "DELETE FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, USER_KEY],
        )?;
        return Ok(());
    }
    write(tx, tenant, USER_KEY, &serde_json::to_string(&raw)?)
}

fn write(tx: &Transaction<'_>, tenant: &str, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        params![tenant, key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
