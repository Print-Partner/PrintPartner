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

use modules::{Module, ModuleStatus};

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
    change: Change,
}

enum Change {
    /// Read, or patch one feature, in `scope`.
    Feature(Scope, Option<(Feature, Option<bool>)>),
    /// Turn a Module on or off for the user.
    Module(Module, bool),
}

/// What the writer answers: a feature view, or every Module's state after a
/// Module change.
pub(super) enum Reply {
    View(View),
    Modules(Vec<ModuleStatus>),
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
        view(self.submit(credential, Change::Feature(scope, None), cancelled, wait)?)
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
        let change = Change::Feature(scope, Some((feature, choice)));
        view(self.submit(credential, change, cancelled, wait)?)
    }

    /// Turns `module` on or off for the user, as the replacement Settings
    /// screen does (GRE-1341), and answers every Module's state read in the
    /// same transaction. A feature Module writes the user layer only: Plan
    /// overrides and unknown stored keys stay. Assembly tracking writes
    /// `build_tracking_assembly` as `"1"` or `"0"`, as Node's
    /// `PUT /settings/build-tracking` does. An unreadable user layer refuses a
    /// feature Module with [`InvalidLayer`] and writes nothing.
    pub fn set_module(
        &self,
        credential: Credential,
        module: Module,
        on: bool,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Vec<ModuleStatus>> {
        match self.submit(credential, Change::Module(module, on), cancelled, wait)? {
            Reply::Modules(modules) => Ok(modules),
            Reply::View(_) => Err(anyhow!("Module change answered a feature view")),
        }
    }

    fn submit(
        &self,
        credential: Credential,
        change: Change,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Reply> {
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
                change,
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

fn view(reply: Reply) -> Result<View> {
    match reply {
        Reply::View(view) => Ok(view),
        Reply::Modules(_) => Err(anyhow!("Feature request answered Module states")),
    }
}

pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Reply> {
    let tx = match command.change {
        Change::Feature(_, None) => {
            connection.transaction_with_behavior(TransactionBehavior::Deferred)?
        }
        _ => crate::writer_admission::begin_immediate(connection, admission)?,
    };
    let tenant = match command.credential {
        Credential::Session(secret) => auth::read_session_tenant(&tx, &secret, command.policy)?,
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => auth::read_key_tenant(&tx, &routed_tenant, &secret)?,
    };
    let (scope, patch) = match command.change {
        Change::Feature(scope, patch) => (scope, patch),
        Change::Module(module, on) => {
            match module.feature() {
                Some(feature) => {
                    let mut user = load(&tx, &tenant, USER_KEY)?;
                    user.apply(&FeaturePatch(BTreeMap::from([(feature, Some(on))])));
                    store_user(&tx, &tenant, &user)?;
                }
                None => write(
                    &tx,
                    &tenant,
                    modules::ASSEMBLY_TRACKING_KEY,
                    if on { "1" } else { "0" },
                )?,
            }
            let modules = modules::statuses_in(&tx, &tenant)?;
            tx.commit()?;
            return Ok(Reply::Modules(modules));
        }
    };
    let plan = match scope {
        Scope::User => None,
        Scope::Plan(id) => Some(id),
    };
    let mut layers = load_layers(&tx, &tenant, plan)?;
    if let Some((feature, choice)) = patch {
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
    Ok(Reply::View(layers.into_view()))
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

/// Whether the single-account tenant has Bambu enabled, from its
/// `feature_toggles` setting alone, as Node's tenant-global integration
/// admission reads it (Plan overrides do not apply). For routes the admin gate
/// has already authenticated. `Ok(None)` when the stored toggles are invalid.
pub fn bambu_enabled(client: &crate::SettingsClient, wait: Duration) -> Result<Option<bool>> {
    bambu_enabled_in(&client.reader(wait)?)
}

/// [`bambu_enabled`] on a reader the caller already holds.
pub(crate) fn bambu_enabled_in(reader: &crate::ReaderCheckout) -> Result<Option<bool>> {
    let toggles = match reader.snapshot("default", USER_KEY)? {
        SettingSnapshot::Missing => Toggles::default(),
        SettingSnapshot::Stored { value } => match serde_json::from_str(&value) {
            Ok(toggles) => toggles,
            Err(_) => return Ok(None),
        },
    };
    Ok(Some(resolve(&toggles, None).enabled(Feature::Bambu)))
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

pub(crate) fn owned(tx: &Transaction<'_>, tenant: &str, plan: PositiveId) -> Result<bool> {
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

/// A stored feature layer that is not an object whose feature keys all hold
/// booleans. Nothing is written while it stays, and `GET /health` reports its
/// Modules `setting_unreadable`.
#[derive(Debug)]
pub struct InvalidLayer {
    key: String,
}

impl std::fmt::Display for InvalidLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Stored {} is invalid: expected an object of feature booleans",
            self.key
        )
    }
}

impl std::error::Error for InvalidLayer {}

fn load(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Layer> {
    read(tx, tenant, key)?.ok_or_else(|| {
        InvalidLayer {
            key: key.to_owned(),
        }
        .into()
    })
}

/// The tenant layer keeps this client's own format: sorted keys, and no row
/// once it is empty. Node owns `PATCH /settings/features`; the Rust-only
/// runtime writes through `PUT /api/v2/modules/{module}` (GRE-1341).
fn store_user(tx: &Transaction<'_>, tenant: &str, layer: &Layer) -> Result<()> {
    // A member `RawObject` keeps but `serde_json` refuses (a lone surrogate
    // escape, a number past `f64`) is unreadable as `/health` reads it too.
    let raw: BTreeMap<String, Value> =
        serde_json::from_str(&layer.raw.text()).map_err(|_| InvalidLayer {
            key: USER_KEY.to_owned(),
        })?;
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
