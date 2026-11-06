mod model;
pub mod preview;
mod snapshot;
#[cfg(test)]
mod tests;

use crate::{
    Envelope, Shared, WriterOwner, auth,
    manifest_text::{DraftPart, JsText},
    read_model::Credential,
    required_units::{
        self as ru,
        storage::{normalize_parts, num, one, part_rows, rows, text},
    },
};
use anyhow::{Result, anyhow, ensure};
use model::{Assignment, Prepared};
pub use pp_contracts::publication::{ApplyRequest, Outcome};
use pp_contracts::{
    autosave::{ApplyPlanDraftReceipt, PositiveId},
    publication::{ReconciliationReason, UnmappableLink},
};
use rusqlite::{Connection, Transaction, params};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub(crate) fn validate_schema(connection: &Connection, version: u64) -> Result<()> {
    let objects = |connection: &Connection| -> Result<Vec<(String, Option<String>)>> {
        Ok(connection
            .prepare(
                "SELECT name,sql FROM sqlite_master WHERE name='plan_apply_admissions' OR name LIKE 'trg_plan_apply_admissions_%' ORDER BY name",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    };
    let actual = objects(connection)?;
    if version < 37 {
        ensure!(actual.is_empty(), "Unexpected Plan Apply admission schema");
        return Ok(());
    }
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(
        "CREATE TABLE build_profiles(id INTEGER PRIMARY KEY,tenant_id TEXT NOT NULL);
         CREATE TABLE plan_apply_requests(id INTEGER PRIMARY KEY,tenant_id TEXT NOT NULL,profile_id INTEGER NOT NULL);",
    )?;
    expected.execute_batch(include_str!("plan_publication/schema.sql"))?;
    ensure!(
        actual == objects(&expected)?,
        "Plan Apply admission schema mismatch"
    );
    let orphans: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM plan_apply_admissions admission LEFT JOIN plan_apply_requests request ON request.id=admission.apply_request_id WHERE request.id IS NULL)",
        [],
        |row| row.get(0),
    )?;
    ensure!(!orphans, "Orphan Plan Apply admission");
    Ok(())
}

struct RandomTokens;
impl RequiredUnitTokenAllocator for RandomTokens {
    fn allocate(&mut self) -> std::result::Result<[u8; 16], TokenAllocationFailure> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| TokenAllocationFailure)?;
        Ok(bytes)
    }
}
pub(crate) fn random_tokens() -> Box<dyn RequiredUnitTokenAllocator> {
    Box::new(RandomTokens)
}
#[derive(Clone)]
pub struct PublicationClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}
pub struct PublicationCommand {
    profile: PositiveId,
    draft: PositiveId,
    admitted_request: ApplyRequest,
    execution_request: ApplyRequest,
    key: String,
    credential: Credential,
}
impl PublicationCommand {
    pub fn new(
        profile: PositiveId,
        draft: PositiveId,
        request: ApplyRequest,
        key: String,
        credential: Credential,
    ) -> Result<Self> {
        let key = JsText::scalar(&key).trim_ecmascript();
        ensure!(
            !key.is_empty() && key.units().len() <= 160,
            "Request is invalid"
        );
        let key = key
            .as_scalar()
            .ok_or_else(|| anyhow!("Request is invalid"))?;
        Ok(Self {
            profile,
            draft,
            admitted_request: request.clone(),
            execution_request: request,
            key,
            credential,
        })
    }

    pub fn normalized_http(
        profile: PositiveId,
        draft: PositiveId,
        admitted_request: ApplyRequest,
        execution_request: ApplyRequest,
        key: String,
        credential: Credential,
    ) -> Result<Self> {
        let mut command = Self::new(profile, draft, execution_request, key, credential)?;
        command.admitted_request = admitted_request;
        Ok(command)
    }
}
pub(crate) struct Command {
    input: PublicationCommand,
    policy: auth::AuthPolicy,
}
impl WriterOwner {
    #[cfg(test)]
    pub(crate) fn publication(&self) -> PublicationClient {
        self.publication_with_policy(auth::AuthPolicy {
            registration: auth::RegistrationPolicy::Open,
            session_tenant: auth::SessionTenantPolicy::AccountTenant,
            first_user: auth::FirstUserTenant::NewUser,
        })
        .expect("Valid publication test policy")
    }
    /// The deployment must select its authentication policy explicitly.
    ///
    /// ```compile_fail,E0599
    /// fn client(owner: &pp_storage::WriterOwner) {
    ///     owner.publication();
    /// }
    /// ```
    pub fn publication_with_policy(&self, policy: auth::AuthPolicy) -> Result<PublicationClient> {
        auth::validate_policy(policy)?;
        Ok(PublicationClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}
impl PublicationClient {
    pub(crate) fn new(shared: Arc<Shared>, policy: auth::AuthPolicy) -> Self {
        Self { shared, policy }
    }

    pub fn apply(
        &self,
        input: PublicationCommand,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Outcome> {
        use ru::AdmissionFailure;
        let deadline = Instant::now() + wait;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!(AdmissionFailure::Stopped))?;
        loop {
            ensure!(!queue.closed, AdmissionFailure::Stopped);
            ensure!(
                !cancelled.load(Ordering::Acquire),
                AdmissionFailure::Cancelled
            );
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, AdmissionFailure::QueueFull);
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!(AdmissionFailure::Stopped))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::Publication {
            command: Command {
                input,
                policy: self.policy,
            },
            reply,
        });
        self.shared.changed.notify_all();
        drop(queue);
        receiver
            .recv()
            .map_err(|_| anyhow!(AdmissionFailure::OutcomeUnknown))?
            .map_err(|error| {
                if error.downcast_ref::<auth::AuthFailure>().is_some() {
                    error
                } else {
                    error.context("Plan draft update failed")
                }
            })
    }
}
fn projection(p: &DraftPart) -> Result<DraftPart> {
    let mut row = p.projected(ru::model::PART_FIELDS);
    let o = row.as_object_mut().ok_or_else(|| anyhow!("Invalid Part"))?;
    o.remove("artifactDigest");
    o.remove("roleInferred");
    o.remove("roleOverride");
    o.remove("partKey");
    o.remove("quantityInferred");
    row["matchKey"] = p["partKey"].clone();
    row["quantityAuto"] = p["quantityInferred"].clone();
    row["role"] = json!(ru::model::role(p));
    Ok(row)
}
/// Filament fields an accepted Part may change without a new revision. Accept
/// publishes the live accepted Part's values for them, not the draft's.
const LIVE_FILAMENT_FIELDS: [&str; 3] = ["filamentColorId", "filamentCustomHex", "spoolmanSpoolId"];

/// Gives each draft Part that has a base its accepted Part's live filament, as
/// Accept publishes it. `predecessors` come from [`validate_projection`]. A new
/// Part keeps its own values.
fn inherit_live_filament(parts: &mut [DraftPart], predecessors: &[DraftPart]) {
    for part in parts {
        if let Some(old) = predecessors
            .iter()
            .find(|row| row["id"] == part["baseRevisionPartId"])
        {
            for key in LIVE_FILAMENT_FIELDS {
                part[key] = old[key].clone();
            }
        }
    }
}

fn validate_projection(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    revision: i64,
) -> Result<Vec<DraftPart>> {
    let header = one(
        tx,
        "SELECT * FROM plan_revisions WHERE tenant_id=? AND profile_id=? AND id=?",
        &[&tenant, &profile, &revision],
    )?
    .ok_or_else(|| anyhow!("Missing accepted revision"))?;
    let mut immutable = part_rows(
        tx,
        "SELECT * FROM plan_revision_parts WHERE tenant_id=? AND revision_id=? ORDER BY id",
        &[&tenant, &revision],
    )?;
    normalize_parts(&mut immutable)?;
    ensure!(
        header["digestFormat"] == model::REVISION_FORMAT
            && model::revision_digest(&immutable)? == header["snapshotDigest"],
        "Accepted revision digest mismatch"
    );
    let mut current = part_rows(
        tx,
        "SELECT * FROM parts WHERE tenant_id=? AND profile_id=? ORDER BY id",
        &[&tenant, &profile],
    )?;
    normalize_parts(&mut current)?;
    ensure!(
        immutable.len() == current.len(),
        "Accepted projection count mismatch"
    );
    for part in &mut immutable {
        let actual = current
            .iter()
            .find(|p| p["id"] == part["projectionPartId"])
            .ok_or_else(|| anyhow!("Missing accepted projection"))?;
        for (key, value) in projection(part)?
            .as_object()
            .ok_or_else(|| anyhow!("Invalid Part"))?
        {
            if !LIVE_FILAMENT_FIELDS.contains(&key.as_str()) {
                ensure!(actual[key] == *value, "Accepted projection mismatch {key}");
            }
        }
        ensure!(
            actual.manifest == part.manifest,
            "Accepted manifest projection mismatch"
        );
        let progress = rows(
            tx,
            "SELECT unit_index,completed,assembled FROM print_progress WHERE tenant_id=? AND part_id=? ORDER BY unit_index",
            &[&tenant, &num(actual, "id")?],
        )?;
        let quantity = num(part, "quantityEffective")?;
        for index in 0..quantity {
            let u = progress
                .iter()
                .find(|u| u["unitIndex"] == index)
                .ok_or_else(|| anyhow!("Incomplete accepted progress"))?;
            let c = num(u, "completed")?;
            let a = num(u, "assembled")?;
            ensure!(
                (0..=1).contains(&c) && (0..=c).contains(&a),
                "Invalid accepted progress"
            );
        }
        for field in LIVE_FILAMENT_FIELDS {
            part[field] = actual[field].clone();
        }
    }
    Ok(immutable)
}
struct Production {
    links: Option<Vec<Value>>,
    queue: Option<Vec<Value>>,
    /// The queue came from `printer_send_queue`, not the legacy setting.
    queue_in_table: bool,
    link_count: usize,
    queue_count: usize,
    owners: HashMap<i64, i64>,
    match_keys: HashMap<i64, String>,
}
fn setting(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Option<Vec<Value>>> {
    let Some(row) = one(
        tx,
        "SELECT value FROM app_settings WHERE tenant_id=? AND key=?",
        &[&tenant, &key],
    )?
    else {
        return Ok(None);
    };
    let raw = text(&row, "value")?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str(raw)? {
        Value::Array(value) => Ok(Some(value)),
        _ => Err(anyhow!("Invalid production setting")),
    }
}
fn coordinate(unit: &Value) -> Result<(i64, i64)> {
    let part = num(unit, "part_id")?;
    let index = num(unit, "unit_index")?;
    ensure!(part > 0 && index >= 0, "Invalid production coordinate");
    Ok((part, index))
}
fn production(tx: &Transaction<'_>, tenant: &str, profile: i64) -> Result<Production> {
    let links = setting(tx, tenant, "printer.checkoff_links")?;
    let table = crate::send_queue::rows(tx, tenant)?;
    let queue_in_table = table.is_some();
    let queue = match table {
        Some(items) => Some(items),
        None => setting(tx, tenant, crate::send_queue::LEGACY_KEY)?,
    };
    let parts = rows(
        tx,
        "SELECT id,profile_id,match_key FROM parts WHERE tenant_id=?",
        &[&tenant],
    )?;
    let match_keys = parts
        .iter()
        .filter(|p| p["profileId"] == profile)
        .map(|p| Ok((num(p, "id")?, text(p, "matchKey")?.to_owned())))
        .collect::<Result<_>>()?;
    let owners: HashMap<_, _> = parts
        .iter()
        .map(|p| Ok((num(p, "id")?, num(p, "profileId")?)))
        .collect::<Result<_>>()?;
    let mut link_count = 0;
    let mut queue_count = 0;
    if let Some(v) = &links {
        for row in v {
            let state = text(row, "state")?;
            ensure!(
                [
                    "watching",
                    "awaiting_verify",
                    "host_failed",
                    "dismissed",
                    "verified",
                    "applied"
                ]
                .contains(&state)
                    && num(row, "profile_id")? > 0,
                "Invalid Checkoff link"
            );
            for unit in row["units"]
                .as_array()
                .ok_or_else(|| anyhow!("Invalid Checkoff units"))?
            {
                coordinate(unit)?;
            }
            if row["profile_id"] == profile && ["watching", "awaiting_verify"].contains(&state) {
                link_count += 1;
            }
        }
    }
    if let Some(v) = &queue {
        for row in v {
            let state = text(row, "state")?;
            ensure!(
                ["queued", "sending", "done", "error", "cancelled"].contains(&state),
                "Invalid queue state"
            );
            let explicit = row["profile_id"].as_i64();
            ensure!(
                row["profile_id"].is_null() || explicit.is_some_and(|n| n > 0),
                "Invalid queue owner"
            );
            let mut unit_owners = HashSet::new();
            if !row["checkoff_units"].is_null() {
                for u in row["checkoff_units"]
                    .as_array()
                    .ok_or_else(|| anyhow!("Invalid queue units"))?
                {
                    let (p, _) = coordinate(u)?;
                    if let Some(owner) = owners.get(&p) {
                        unit_owners.insert(*owner);
                    }
                }
            }
            ensure!(
                unit_owners.len() <= 1
                    && explicit.is_none_or(|p| unit_owners.iter().all(|o| *o == p)),
                "Queue owner mismatch"
            );
            let owner = explicit.or_else(|| unit_owners.into_iter().next());
            if owner == Some(profile) && ["queued", "sending", "error"].contains(&state) {
                queue_count += 1;
            }
        }
    }
    Ok(Production {
        links,
        queue,
        queue_in_table,
        link_count,
        queue_count,
        owners,
        match_keys,
    })
}
enum Remap {
    Mapped(HashMap<(i64, i64), (i64, i64)>),
    Unmappable(Vec<UnmappableLink>),
}
fn remap(
    production: &Production,
    profile: i64,
    old_tokens: &HashMap<(i64, i64), String>,
    prepared: &Prepared,
) -> Result<Remap> {
    let mut targets = HashMap::new();
    for a in &prepared.assignments {
        if let Assignment::Reuse { token, .. } = a {
            let (part, index) = a.slot();
            let p = prepared
                .parts
                .iter()
                .find(|p| p["id"] == part)
                .ok_or_else(|| anyhow!("Invalid assignment coordinate"))?;
            if p["included"] == true {
                targets.insert(token.clone(), (part, index));
            }
        }
    }
    let mut mapped = HashMap::new();
    let mut missing = Vec::new();
    let mut seen = HashSet::new();
    let mut gate_missing = Vec::new();
    let mut gate_seen = HashSet::new();
    let mut resolve = |row: &Value, units: &Value, node_gate: bool| -> Result<()> {
        if units.is_null() {
            return Ok(());
        }
        for u in units
            .as_array()
            .ok_or_else(|| anyhow!("Invalid production coordinates"))?
        {
            let slot = coordinate(u)?;
            let link_id = row["id"].as_str().unwrap_or("unknown");
            let filename = row["filename"].as_str().unwrap_or("unknown file");
            if node_gate && gate_seen.insert(slot) {
                let reason = match production
                    .match_keys
                    .get(&slot.0)
                    .filter(|key| !key.is_empty())
                {
                    None => Some(format!(
                        "Checked-off part id {} no longer exists in this Plan's parts",
                        slot.0
                    )),
                    Some(key) => match prepared.parts.iter().rfind(|p| p["partKey"] == *key) {
                        None => Some(format!(
                            "STL for \"{filename}\" (match_key {key}) is no longer part of the reconciled Plan"
                        )),
                        Some(p) => {
                            let quantity = num(p, "quantityEffective")?;
                            (slot.1 >= quantity).then(|| format!("Checked-off unit index {} exceeds the new quantity ({quantity}) for \"{filename}\"", slot.1))
                        }
                    },
                };
                if let Some(reason) = reason {
                    gate_missing.push(UnmappableLink {
                        link_id: link_id.into(),
                        filename: filename.into(),
                        reason,
                    });
                }
            }
            if !seen.insert(slot) {
                continue;
            }
            if let Some(target) = old_tokens.get(&slot).and_then(|t| targets.get(t)) {
                mapped.insert(slot, *target);
            } else {
                missing.push(UnmappableLink{link_id:row["id"].as_str().unwrap_or("unknown").into(),filename:row["filename"].as_str().unwrap_or("unknown file").into(),reason:format!("Required unit at Part {} index {} is absent or excluded in the selected Plan",slot.0,slot.1)});
            }
        }
        Ok(())
    };
    if let Some(v) = &production.links {
        for row in v {
            if row["profile_id"] == profile {
                resolve(row, &row["units"], true)?;
                resolve(row, &row["resolved_units"], false)?;
            }
        }
    }
    if let Some(v) = &production.queue {
        for row in v {
            let units = row["checkoff_units"].as_array();
            let related = row["profile_id"] == profile
                || (row["profile_id"].is_null()
                    && units.is_some_and(|u| {
                        u.iter().any(|u| {
                            u["part_id"]
                                .as_i64()
                                .and_then(|id| production.owners.get(&id))
                                .copied()
                                == Some(profile)
                        }) || !u.iter().any(|u| {
                            u["part_id"]
                                .as_i64()
                                .is_some_and(|id| production.owners.contains_key(&id))
                        })
                    }));
            if related {
                resolve(row, &row["checkoff_units"], true)?;
            }
        }
    }
    Ok(if !gate_missing.is_empty() {
        Remap::Unmappable(gate_missing)
    } else if missing.is_empty() {
        Remap::Mapped(mapped)
    } else {
        Remap::Unmappable(missing)
    })
}
fn write_remap(
    tx: &Transaction<'_>,
    tenant: &str,
    production: Production,
    mapping: &HashMap<(i64, i64), (i64, i64)>,
    projection: &HashMap<i64, i64>,
) -> Result<()> {
    if mapping.is_empty() {
        return Ok(());
    }
    for (key, value, fields, in_table) in [
        (
            "printer.checkoff_links",
            production.links,
            vec!["units", "resolved_units"],
            false,
        ),
        (
            crate::send_queue::LEGACY_KEY,
            production.queue,
            vec!["checkoff_units"],
            production.queue_in_table,
        ),
    ] {
        if let Some(mut value) = value {
            for row in &mut value {
                for field in &fields {
                    if let Some(units) = row.get_mut(*field).and_then(Value::as_array_mut) {
                        for unit in units {
                            let slot = coordinate(unit)?;
                            if let Some((part, index)) = mapping.get(&slot) {
                                unit["part_id"] = json!(
                                    projection
                                        .get(part)
                                        .ok_or_else(|| anyhow!("Missing remap projection"))?
                                );
                                unit["unit_index"] = json!(index);
                            }
                        }
                    }
                }
            }
            if in_table {
                crate::send_queue::write_units(tx, tenant, &value)?;
            } else {
                tx.execute(
                    "UPDATE app_settings SET value=? WHERE tenant_id=? AND key=?",
                    params![serde_json::to_string(&value)?, tenant, key],
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) struct AuthenticatedPublicationContext {
    tenant: String,
    actor: String,
}
pub(crate) struct PublicationRequests<'a> {
    admitted: &'a ApplyRequest,
    execution: &'a ApplyRequest,
}
impl<'a> PublicationRequests<'a> {
    pub(crate) fn new(admitted: &'a ApplyRequest, execution: &'a ApplyRequest) -> Self {
        Self {
            admitted,
            execution,
        }
    }
    #[cfg(test)]
    pub(crate) fn direct(request: &'a ApplyRequest) -> Self {
        Self::new(request, request)
    }
}
pub trait RequiredUnitTokenAllocator: Send {
    fn allocate(&mut self) -> std::result::Result<[u8; 16], TokenAllocationFailure>;
}
#[derive(Debug)]
pub struct TokenAllocationFailure;

impl AuthenticatedPublicationContext {
    pub(crate) fn resolve(
        tx: &Transaction<'_>,
        credential: Credential,
        policy: auth::AuthPolicy,
    ) -> Result<Self> {
        let (tenant, actor) =
            auth::authority::queries::reconciliation_actor(tx, credential, policy)?;
        Ok(Self { tenant, actor })
    }
}
pub(crate) fn execute(
    connection: &mut Connection,
    command: Command,
    tokens: &mut dyn RequiredUnitTokenAllocator,
    admission: crate::writer_admission::Deadline,
) -> Result<Outcome> {
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    let c = command.input;
    let context = AuthenticatedPublicationContext::resolve(&tx, c.credential, command.policy)?;
    let result = publish_in_transaction(
        &tx,
        &context,
        c.profile,
        c.draft,
        PublicationRequests::new(&c.admitted_request, &c.execution_request),
        &c.key,
        tokens,
    )?;
    if matches!(
        result,
        Outcome::Applied { .. } | Outcome::Existing { .. } | Outcome::AlreadyApplied { .. }
    ) {
        tx.commit()?
    }
    Ok(result)
}
fn request_digest(profile: i64, draft: i64, request: &ApplyRequest) -> String {
    ru::model::digest(
        &json!({"format":model::REQUEST_FORMAT,"profile_id":profile,"draft_id":draft,"expected_snapshot_digest":request.expected_snapshot_digest,"expected_lifecycle_version":request.expected_lifecycle_version,"expected_base_revision_id":request.expected_base.revision_id(),"expected_base_plan_version":request.expected_base.plan_version()}),
    )
}

fn stored_admission(
    tx: &Transaction<'_>,
    parent: &Value,
) -> Result<Option<(ApplyRequest, String)>> {
    let Some(row) = one(
        tx,
        "SELECT * FROM plan_apply_admissions WHERE apply_request_id=?",
        &[&num(parent, "id")?],
    )?
    else {
        return Ok(None);
    };
    let profile = num(parent, "profileId")?;
    let draft = num(parent, "draftId")?;
    let request: ApplyRequest = serde_json::from_value(json!({
        "expected_snapshot_digest":row["expectedSnapshotDigest"],
        "expected_lifecycle_version":row["expectedLifecycleVersion"],
        "expected_base":{
            "revision_id":row["expectedBaseRevisionId"],
            "plan_version":row["expectedBasePlanVersion"]
        }
    }))?;
    let digest = request_digest(profile, draft, &request);
    ensure!(
        row["requestFormat"] == model::REQUEST_FORMAT && row["requestDigest"] == digest,
        "Invalid publication admission"
    );
    Ok(Some((request, digest)))
}

fn admission_matches(tx: &Transaction<'_>, parent: &Value, request: &ApplyRequest) -> Result<bool> {
    let profile = num(parent, "profileId")?;
    let draft = num(parent, "draftId")?;
    let digest = request_digest(profile, draft, request);
    Ok(match stored_admission(tx, parent)? {
        Some((_, admitted_digest)) => admitted_digest == digest,
        None => parent["requestDigest"] == digest,
    })
}
fn sql_value(value: &Value) -> Result<rusqlite::types::Value> {
    Ok(match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(v) => rusqlite::types::Value::Integer(i64::from(*v)),
        Value::Number(n) => rusqlite::types::Value::Integer(
            n.as_i64()
                .ok_or_else(|| anyhow!("Invalid publication integer"))?,
        ),
        Value::String(s) => rusqlite::types::Value::Text(s.clone()),
        _ => return Err(anyhow!("Invalid publication field")),
    })
}
fn insert(tx: &Transaction<'_>, table: &str, row: &Value, ignore: bool) -> Result<i64> {
    let fields: &[&str] = match table {
        "plan_revision_input_sets" => &["tenantId", "inputSetDigest", "recordedAt", "recordedAt"],
        "plan_revision_inputs" => &[
            "tenant_id",
            "source_layer",
            "tracking_kind",
            "manifest_digest",
            "effective_naming_digest",
        ],
        "plan_revisions" => &[
            "tenantId",
            "provenanceKind",
            "digestFormat",
            "snapshotDigest",
            "createdBy",
            "acceptedBy",
            "createdAt",
            "acceptedAt",
        ],
        "required_units" => &["tenantId", "token", "objectName", "createdAt"],
        "plan_revision_required_unit_sets" => &["tenantId", "format", "mappingDigest", "createdAt"],
        "plan_revision_required_units"
        | "print_progress"
        | "plan_apply_requests"
        | "plan_apply_admissions" => &[],
        _ => return Err(anyhow!("Unsupported publication table")),
    };
    model::validate_named_text(row, fields, 0)?;
    let fields = row.as_object().ok_or_else(|| anyhow!("Invalid row"))?;
    let names: Vec<_> = fields.keys().map(|k| model::snake(k)).collect();
    let values = fields.values().map(sql_value).collect::<Result<Vec<_>>>()?;
    let sql = format!(
        "INSERT INTO {}({}) VALUES({}){}",
        table,
        names.join(","),
        vec!["?"; names.len()].join(","),
        if ignore {
            " ON CONFLICT DO NOTHING"
        } else {
            ""
        }
    );
    tx.prepare_cached(&sql)?
        .execute(rusqlite::params_from_iter(values))?;
    Ok(tx.last_insert_rowid())
}
fn insert_part(tx: &Transaction<'_>, table: &str, part: &DraftPart) -> Result<i64> {
    model::validate_part_text(part, text(part, "tenantId")?.len())?;
    let mut fields = part
        .scalar
        .as_object()
        .ok_or_else(|| anyhow!("Invalid part insert"))?
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    fields.extend(["requirement", "optionGroupId", "manifestSource"]);
    fields.sort_unstable();
    let values = part.sql_fields(&fields)?;
    let columns = fields
        .iter()
        .map(|field| model::snake(field))
        .collect::<Vec<_>>();
    let sql = format!(
        "INSERT INTO {table}({}) VALUES({})",
        columns.join(","),
        vec!["?"; fields.len()].join(",")
    );
    tx.prepare_cached(&sql)?
        .execute(rusqlite::params_from_iter(
            values.into_iter().map(|(_, value)| value),
        ))?;
    Ok(tx.last_insert_rowid())
}
fn historical_mapping(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    revision: i64,
) -> Result<String> {
    let set=one(tx,"SELECT * FROM plan_revision_required_unit_sets WHERE tenant_id=? AND profile_id=? AND revision_id=?",&[&tenant,&profile,&revision])?.ok_or_else(||anyhow!("Missing historical mapping"))?;
    let parts = rows(
        tx,
        "SELECT id,quantity_effective FROM plan_revision_parts WHERE tenant_id=? AND revision_id=? ORDER BY id",
        &[&tenant, &revision],
    )?;
    let mapped = rows(
        tx,
        "SELECT m.revision_part_id,m.unit_index,u.token,u.object_name FROM plan_revision_required_units m JOIN required_units u ON u.token=m.required_unit_token AND u.tenant_id=?1 AND u.profile_id=?2 WHERE m.tenant_id=?1 AND m.revision_id=?3 ORDER BY m.revision_part_id,m.unit_index",
        &[&tenant, &profile, &revision],
    )?;
    let mut expected = 0;
    let mut tokens = HashSet::new();
    for p in &parts {
        let q = num(p, "quantityEffective")?;
        ensure!((1..=10000).contains(&q), "Invalid historical quantity");
        let units: Vec<_> = mapped
            .iter()
            .filter(|u| u["revisionPartId"] == p["id"])
            .collect();
        ensure!(units.len() as i64 == q, "Incomplete historical mapping");
        for (i, u) in units.iter().enumerate() {
            ensure!(
                num(u, "unitIndex")? == i as i64 && tokens.insert(text(u, "token")?),
                "Invalid historical coordinate"
            );
            ru::model::validate_token(text(u, "token")?)?;
        }
        expected += q;
    }
    ensure!(
        set["format"] == "required-unit-map-v1"
            && num(&set, "expectedUnitCount")? == expected
            && mapped.len() as i64 == expected,
        "Invalid historical set"
    );
    let mapped: Vec<_> = mapped
        .iter()
        .map(|r| model::canonical(r, &["revisionPartId", "unitIndex", "token", "objectName"]))
        .collect();
    let digest = ru::model::digest(
        &json!({"format":"required-unit-map-v1","revision_id":revision,"expected_unit_count":expected,"rows":mapped}),
    );
    ensure!(
        set["mappingDigest"] == digest,
        "Historical mapping digest mismatch"
    );
    Ok(digest)
}
pub(crate) fn receipt(
    tx: &Transaction<'_>,
    tenant: &str,
    row: &Value,
) -> Result<ApplyPlanDraftReceipt> {
    let profile = num(row, "profileId")?;
    let draft = num(row, "draftId")?;
    let revision = num(row, "revisionId")?;
    let request: ApplyRequest = serde_json::from_value(
        json!({"expected_snapshot_digest":row["expectedSnapshotDigest"],"expected_lifecycle_version":row["expectedLifecycleVersion"],"expected_base":{"revision_id":row["expectedBaseRevisionId"],"plan_version":row["expectedBasePlanVersion"]}}),
    )?;
    ensure!(
        row["requestFormat"] == model::REQUEST_FORMAT
            && row["requestDigest"] == request_digest(profile, draft, &request)
            && num(row, "planVersion")? == num(row, "expectedBasePlanVersion")? + 1
            && num(row, "draftLifecycleVersion")? == num(row, "expectedLifecycleVersion")? + 1,
        "Invalid publication receipt"
    );
    stored_admission(tx, row)?;
    let d = ru::storage::draft(tx, tenant, profile, draft)?
        .ok_or_else(|| anyhow!("Missing receipt draft"))?;
    let selected = d
        .selected
        .as_ref()
        .ok_or_else(|| anyhow!("Missing receipt reconciliation"))?;
    let rev = one(
        tx,
        "SELECT * FROM plan_revisions WHERE tenant_id=? AND profile_id=? AND id=?",
        &[&tenant, &profile, &revision],
    )?
    .ok_or_else(|| anyhow!("Missing receipt revision"))?;
    ensure!(
        d.header["state"] == "consumed"
            && d.header["lifecycleVersion"] == row["draftLifecycleVersion"]
            && d.header["snapshotDigest"] == row["expectedSnapshotDigest"]
            && d.header["baseRevisionId"] == row["expectedBaseRevisionId"]
            && d.header["basePlanVersion"] == row["expectedBasePlanVersion"]
            && d.header["consumedRevisionId"] == revision
            && d.header["consumedAt"] == row["appliedAt"]
            && selected.header["id"] == row["reconciliationId"]
            && selected.header["reconciliationDigest"] == row["reconciliationDigest"]
            && rev["parentRevisionId"] == row["expectedBaseRevisionId"]
            && rev["snapshotDigest"] == row["revisionDigest"]
            && historical_mapping(tx, tenant, profile, revision)?
                == row["requiredUnitMappingDigest"],
        "Publication receipt linkage mismatch"
    );
    serde_json::from_value(json!({"profile_id":profile,"draft_id":draft,"revision_id":revision,"plan_version":row["planVersion"],"draft_lifecycle_version":row["draftLifecycleVersion"],"revision_digest":row["revisionDigest"],"required_unit_mapping_digest":row["requiredUnitMappingDigest"],"applied_at":row["appliedAt"]})).map_err(Into::into)
}
fn validate_inputs(tx: &Transaction<'_>, tenant: &str, inputs: &[Value]) -> Result<()> {
    let mut seen = HashSet::new();
    for i in inputs {
        model::validate_named_text(
            i,
            &[
                "sourceLayer",
                "trackingKind",
                "manifestDigest",
                "effectiveNamingDigest",
            ],
            tenant.len(),
        )?;
        let source = num(i, "sourceId")?;
        ensure!(seen.insert(source), "Duplicate input Source");
        ensure!(
            one(
                tx,
                "SELECT id FROM projects WHERE tenant_id=? AND id=?",
                &[&tenant, &source]
            )?
            .is_some(),
            "Missing input Source"
        );
        pp_contracts::autosave::Digest::new(text(i, "effectiveNamingDigest")?.into())
            .map_err(anyhow::Error::msg)?;
        if i["trackingKind"] == "revision" {
            let revision = num(i, "sourceRevisionId")?;
            let stored = one(
                tx,
                "SELECT * FROM source_revisions WHERE tenant_id=? AND project_id=? AND id=?",
                &[&tenant, &source, &revision],
            )?
            .ok_or_else(|| anyhow!("Missing pinned Source revision"))?;
            model::validate_named_text(
                &stored,
                &[
                    "upstreamRevisionKey",
                    "manifestDigest",
                    "snapshotLocator",
                    "syncedAt",
                    "completeness",
                ],
                tenant.len(),
            )?;
            ensure!(
                stored["manifestDigest"] == i["manifestDigest"],
                "Pinned manifest mismatch"
            );
        } else {
            ensure!(
                i["trackingKind"] == "untracked"
                    && i["sourceRevisionId"].is_null()
                    && i["manifestDigest"].is_null(),
                "Invalid untracked input"
            );
        }
    }
    Ok(())
}
fn publish_inputs(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    inputs: &[Value],
    at: &str,
) -> Result<i64> {
    let canonical = model::inputs(inputs);
    let digest = ru::model::digest(&json!({"version":2,"inputs":canonical}));
    insert(
        tx,
        "plan_revision_input_sets",
        &json!({"tenantId":tenant,"profileId":profile,"inputSetDigest":digest,"expectedInputCount":inputs.len(),"formatVersion":2,"recordedAt":at}),
        true,
    )?;
    let set=one(tx,"SELECT * FROM plan_revision_input_sets WHERE tenant_id=? AND profile_id=? AND input_set_digest=?",&[&tenant,&profile,&digest])?.ok_or_else(||anyhow!("Missing input set"))?;
    let id = num(&set, "id")?;
    ensure!(
        num(&set, "expectedInputCount")? == inputs.len() as i64 && set["formatVersion"] == 2,
        "Input set content conflict"
    );
    for input in &canonical {
        let mut row = input.clone();
        row["tenant_id"] = json!(tenant);
        row["input_set_id"] = json!(id);
        insert(tx, "plan_revision_inputs", &row, true)?;
    }
    let stored = rows(
        tx,
        "SELECT * FROM plan_revision_inputs WHERE tenant_id=? AND input_set_id=? ORDER BY source_id",
        &[&tenant, &id],
    )?;
    ensure!(model::inputs(&stored) == canonical, "Stored input mismatch");
    tx.execute("UPDATE plan_revision_input_sets SET published_at=? WHERE tenant_id=? AND id=? AND published_at IS NULL",params![at,tenant,id])?;
    Ok(id)
}

/// The draft half of Accept's gate. Accept and its preview both run it, then
/// [`check_selection`]; neither writes.
struct DraftGate {
    profile_row: Value,
    draft: ru::storage::Draft,
    previous: Option<i64>,
    version: i64,
    predecessors: Vec<DraftPart>,
}

/// Old checkoff coordinate (Part, unit index) to its new one.
type CheckoffRemap = HashMap<(i64, i64), (i64, i64)>;

/// What Accept would publish, after every read-only check passed.
struct Checked {
    profile_row: Value,
    inputs: Vec<Value>,
    previous: Option<i64>,
    version: i64,
    predecessors: Vec<DraftPart>,
    /// The selected reconciliation's header.
    selected: Value,
    base_parts: Vec<ru::model::BasePart>,
    /// Base Required units by token: stored row, completed, assembled.
    base: HashMap<String, (Value, bool, bool)>,
    old_tokens: HashMap<(i64, i64), String>,
    prepared: Prepared,
    production: Production,
}

impl Checked {
    /// Accept's last refusals, after [`check_selection`]: active production
    /// without the remap opt-in or with an unsafe remap, then running
    /// operations. Returns the checkoff remap Accept writes.
    fn refusals(
        &self,
        tx: &Transaction<'_>,
        tenant: &str,
        profile: i64,
        remap_checkoff_links: bool,
    ) -> Result<std::result::Result<CheckoffRemap, Outcome>> {
        let production = &self.production;
        let remapping = if production.link_count > 0 || production.queue_count > 0 {
            if !remap_checkoff_links {
                return Ok(Err(Outcome::ProductionActive {
                    checkoff_link_count: production.link_count,
                    send_queue_item_count: production.queue_count,
                }));
            }
            match remap(production, profile, &self.old_tokens, &self.prepared)? {
                Remap::Mapped(mapped) => mapped,
                Remap::Unmappable(unmappable) => {
                    return Ok(Err(Outcome::CheckoffRemapUnsafe { unmappable }));
                }
            }
        } else {
            HashMap::new()
        };
        let conflicts = crate::jobs::publication_conflicts(tx, tenant, profile)?;
        if !conflicts.is_empty() {
            return Ok(Err(Outcome::ExecutionConflict {
                operations: conflicts,
            }));
        }
        Ok(Ok(remapping))
    }

    /// Checkoff links and queued sends that Accept would have to remap, and
    /// whether the remap is safe.
    fn production(&self, profile: i64) -> Result<Option<(usize, usize, Vec<UnmappableLink>)>> {
        let production = &self.production;
        if production.link_count == 0 && production.queue_count == 0 {
            return Ok(None);
        }
        let unmappable = match remap(production, profile, &self.old_tokens, &self.prepared)? {
            Remap::Mapped(_) => Vec::new(),
            Remap::Unmappable(unmappable) => unmappable,
        };
        Ok(Some((
            production.link_count,
            production.queue_count,
            unmappable,
        )))
    }
}

/// The Plan and draft rows, after Accept's first draft checks in its order:
/// the Plan exists and is not archived, then the draft exists and is open.
fn open_draft(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft_id: i64,
) -> Result<std::result::Result<(Value, Value), Outcome>> {
    let Some(profile_row) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(Err(Outcome::NotFound));
    };
    if !profile_row["archivedAt"].is_null() {
        return Ok(Err(Outcome::BuildArchived));
    }
    model::validate_named_text(
        &profile_row,
        &[
            "tenantId",
            "name",
            "orderNumber",
            "specialRequest",
            "archivedAt",
        ],
        0,
    )?;
    let Some(h) = one(
        tx,
        "SELECT * FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND id=?",
        &[&tenant, &profile, &draft_id],
    )?
    else {
        return Ok(Err(Outcome::NotFound));
    };
    if h["state"] != "open" {
        return Ok(Err(Outcome::NotOpen {
            state: serde_json::from_value(h["state"].clone())?,
        }));
    }
    Ok(Ok((profile_row, h)))
}

/// Checks the Plan, the draft's lifecycle and base, and its inputs against
/// `request`, in Accept's order.
fn check_draft(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft_id: i64,
    request: &ApplyRequest,
) -> Result<std::result::Result<DraftGate, Outcome>> {
    let (profile_row, h) = match open_draft(tx, tenant, profile, draft_id)? {
        Ok(rows) => rows,
        Err(refused) => return Ok(Err(refused)),
    };
    if h["lifecycleVersion"].as_u64() != Some(request.expected_lifecycle_version.get())
        || h["snapshotDigest"] != request.expected_snapshot_digest.as_str()
        || !["plan-draft-v1", "plan-draft-v2"].contains(&text(&h, "digestFormat")?)
    {
        return Ok(Err(Outcome::DraftChanged));
    }
    let previous = request
        .expected_base
        .revision_id()
        .map(|id| id.get() as i64);
    let version = request.expected_base.plan_version().get() as i64;
    let accepted_version = num(&profile_row, "acceptedPlanVersion")?;
    let needs_baseline = if profile_row["acceptedPlanRevisionId"].is_null() {
        accepted_version != 0
            || one(
                tx,
                "SELECT id FROM parts WHERE tenant_id=? AND profile_id=? LIMIT 1",
                &[&tenant, &profile],
            )?
            .is_some()
    } else {
        accepted_version <= 0
    };
    if needs_baseline {
        return Ok(Err(Outcome::AcceptedBaselineRequired));
    }
    if profile_row["acceptedPlanRevisionId"] != json!(previous)
        || profile_row["acceptedPlanVersion"] != version
        || h["baseRevisionId"] != json!(previous)
        || h["basePlanVersion"] != version
    {
        return Ok(Err(Outcome::BaseChanged));
    }
    let d = ru::storage::draft(tx, tenant, profile, draft_id)?
        .ok_or_else(|| anyhow!("Missing draft"))?;
    let attached = rows(
        tx,
        "SELECT p.id AS source_id,l.layer_type||':'||p.name AS source_layer,l.layer_order FROM profile_layers l JOIN projects p ON p.id=l.project_id AND p.tenant_id=l.tenant_id WHERE l.tenant_id=? AND l.profile_id=? AND l.project_id IS NOT NULL ORDER BY l.layer_order,p.id",
        &[&tenant, &profile],
    )?;
    let identities: Vec<_> = d
        .inputs
        .iter()
        .map(|r| ru::model::project(r, &["sourceId", "sourceLayer", "layerOrder"]))
        .collect();
    if attached != identities {
        return Ok(Err(Outcome::InputsChanged));
    }
    validate_inputs(tx, tenant, &d.inputs)?;
    let predecessors = previous
        .map(|id| validate_projection(tx, tenant, profile, id))
        .transpose()?
        .unwrap_or_default();
    Ok(Ok(DraftGate {
        profile_row,
        draft: d,
        previous,
        version,
        predecessors,
    }))
}

/// Whether the selection's basis still matches the base units' current
/// progress. A changed unit identity is corruption, not staleness.
fn basis_is_current(
    selected: &Value,
    mapping: &Value,
    base_identity: &HashMap<String, (i64, &ru::model::Unit)>,
) -> Result<bool> {
    let mut live_basis: Value = serde_json::from_str(text(selected, "selectionBasisJson")?)?;
    for row in live_basis
        .as_array_mut()
        .ok_or_else(|| anyhow!("Invalid basis"))?
    {
        let (id, u) = base_identity
            .get(text(row, "token")?)
            .ok_or_else(|| anyhow!("Missing basis token"))?;
        ensure!(
            row["revisionPartId"] == *id
                && row["priorIndex"] == u.prior_index
                && row["createdAt"] == u.created_at,
            "Changed basis identity"
        );
        row["completed"] = json!(u.completed);
        row["assembled"] = json!(u.assembled);
    }
    Ok(ru::storage::selection_basis_digest(mapping, &live_basis)
        == selected["selectionBasisDigest"])
}

/// Checks the selected Required-unit reconciliation against the accepted base
/// and prepares the Parts and units Accept would publish.
fn check_selection(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    gate: DraftGate,
) -> Result<std::result::Result<Checked, Outcome>> {
    let DraftGate {
        profile_row,
        draft: mut d,
        previous,
        version,
        predecessors,
    } = gate;
    let Some(selected) = &d.selected else {
        return Ok(Err(Outcome::ReconciliationRequired {
            reason: ReconciliationReason::Missing,
        }));
    };
    if selected.header["resultKind"] != "ready" {
        return Ok(Err(Outcome::ReconciliationRequired {
            reason: ReconciliationReason::Unresolved,
        }));
    }
    ensure!(
        selected.header["planningDigest"] == d.planning
            && selected.header["baseRevisionId"] == json!(previous),
        "Invalid selected reconciliation"
    );
    let Some((mapping, base_parts)) = ru::storage::base(tx, tenant, profile, previous)? else {
        return Ok(Err(Outcome::ReconciliationRequired {
            reason: ReconciliationReason::Stale,
        }));
    };
    if mapping != selected.header["baseMappingDigest"] {
        return Ok(Err(Outcome::ReconciliationRequired {
            reason: ReconciliationReason::Stale,
        }));
    }
    let mut base = HashMap::new();
    let mut base_identity = HashMap::new();
    let mut old_tokens = HashMap::new();
    for p in &base_parts {
        let projection = predecessors
            .iter()
            .find(|r| r["id"] == p.id)
            .ok_or_else(|| anyhow!("Missing predecessor"))?;
        for u in &p.units {
            let stored = one(
                tx,
                "SELECT * FROM required_units WHERE tenant_id=? AND profile_id=? AND token=?",
                &[&tenant, &profile, &u.token],
            )?
            .ok_or_else(|| anyhow!("Missing Required unit"))?;
            ensure!(
                stored["createdAt"] == u.created_at && num(&stored, "createdInRevisionId")? > 0,
                "Invalid Required-unit origin"
            );
            model::validate_named_text(
                &stored,
                &["token", "tenantId", "objectName", "createdAt"],
                0,
            )?;
            base.insert(u.token.clone(), (stored, u.completed, u.assembled));
            base_identity.insert(u.token.clone(), (p.id, u));
            old_tokens.insert(
                (num(projection, "projectionPartId")?, u.prior_index),
                u.token.clone(),
            );
        }
    }
    if !basis_is_current(&selected.header, &mapping, &base_identity)? {
        return Ok(Err(Outcome::ReconciliationRequired {
            reason: ReconciliationReason::Stale,
        }));
    }
    inherit_live_filament(&mut d.parts, &predecessors);
    let production = production(tx, tenant, profile)?;
    let selected_result: Value = serde_json::from_str(text(&selected.header, "resultJson")?)?;
    let prepared = model::prepare(d.parts, &selected_result["assignments"], &base)?;
    Ok(Ok(Checked {
        profile_row,
        inputs: d.inputs,
        previous,
        version,
        predecessors,
        selected: selected.header.clone(),
        base_parts,
        base,
        old_tokens,
        prepared,
        production,
    }))
}

/// `AlreadyApplied` with its receipt when Accept already consumed the draft.
/// Accept checks it after its idempotency-key lookup and before
/// [`check_draft`]; the preview has no key, so it checks this first.
fn already_applied(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft_id: i64,
) -> Result<Option<Outcome>> {
    one(
        tx,
        "SELECT * FROM plan_apply_requests WHERE tenant_id=? AND profile_id=? AND draft_id=?",
        &[&tenant, &profile, &draft_id],
    )?
    .map(|row| {
        Ok(Outcome::AlreadyApplied {
            receipt: receipt(tx, tenant, &row)?,
        })
    })
    .transpose()
}

/// Mirrors Node AppRepository.applyPlanChanges inside the caller-owned transaction.
pub(crate) fn publish_in_transaction(
    tx: &Transaction<'_>,
    context: &AuthenticatedPublicationContext,
    profile: PositiveId,
    draft_id: PositiveId,
    requests: PublicationRequests<'_>,
    key: &str,
    tokens: &mut dyn RequiredUnitTokenAllocator,
) -> Result<Outcome> {
    let tenant = context.tenant.as_str();
    let actor = context.actor.as_str();
    let profile = profile.get() as i64;
    let draft_id = draft_id.get() as i64;
    let admitted_hash = request_digest(profile, draft_id, requests.admitted);
    let request_hash = request_digest(profile, draft_id, requests.execution);
    if let Some(row) = one(
        tx,
        "SELECT * FROM plan_apply_requests WHERE tenant_id=? AND actor_id=? AND profile_id=? AND idempotency_key=?",
        &[&tenant, &actor, &profile, &key],
    )? {
        return Ok(if admission_matches(tx, &row, requests.admitted)? {
            Outcome::Existing {
                receipt: receipt(tx, tenant, &row)?,
            }
        } else {
            Outcome::IdempotencyConflict
        });
    }
    if let Some(applied) = already_applied(tx, tenant, profile, draft_id)? {
        return Ok(applied);
    }
    let request = requests.execution;
    let gate = match check_draft(tx, tenant, profile, draft_id, request)? {
        Ok(gate) => gate,
        Err(refused) => return Ok(refused),
    };
    let checked = match check_selection(tx, tenant, profile, gate)? {
        Ok(checked) => checked,
        Err(refused) => return Ok(refused),
    };
    let remapping = match checked.refusals(tx, tenant, profile, request.remap_checkoff_links)? {
        Ok(remapping) => remapping,
        Err(refused) => return Ok(refused),
    };
    let Checked {
        inputs,
        previous,
        version,
        selected,
        base,
        prepared,
        production,
        ..
    } = checked;
    let existing = rows(tx, "SELECT token,object_name FROM required_units", &[])?;
    let mut existing_tokens: HashSet<String> = existing
        .iter()
        .map(|u| text(u, "token").map(str::to_owned))
        .collect::<Result<_>>()?;
    let mut names: HashSet<String> = existing
        .iter()
        .map(|u| text(u, "objectName").map(str::to_lowercase))
        .collect::<Result<_>>()?;
    let mut allocated = HashMap::new();
    for a in &prepared.assignments {
        if let Assignment::Create { part, index } = a {
            let p = prepared
                .parts
                .iter()
                .find(|p| p["id"] == *part)
                .ok_or_else(|| anyhow!("Missing assignment Part"))?;
            let mut chosen = None;
            for _ in 0..32 {
                if let Ok(bytes) = tokens.allocate() {
                    let token = format!("ppu_{}", hex::encode(bytes));
                    let name = model::object_name(text(p, "filename")?, &token);
                    if !existing_tokens.contains(&token) && !names.contains(&name.to_lowercase()) {
                        existing_tokens.insert(token.clone());
                        names.insert(name.to_lowercase());
                        chosen = Some((token, name));
                        break;
                    }
                }
            }
            let Some(chosen) = chosen else {
                return Ok(Outcome::TokenAllocationFailed);
            };
            allocated.insert((*part, *index), chosen);
        }
    }
    let now = time::OffsetDateTime::now_utc();
    let at = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    );
    snapshot::save_before_accept(tx, tenant, profile, draft_id, (previous, version), &at)?;
    tx.execute(
        "DELETE FROM accepted_plate_heads WHERE tenant_id=? AND profile_id=?",
        params![tenant, profile],
    )?;
    ensure!(tx.execute("UPDATE build_profiles SET accepted_plan_revision_id=NULL WHERE tenant_id=? AND id=? AND accepted_plan_revision_id IS ? AND accepted_plan_version=?",params![tenant,profile,previous,version])?==1,"Accepted pointer detach failed");
    let input_set = publish_inputs(tx, tenant, profile, &inputs, &at)?;
    let number:i64=tx.query_row("SELECT COALESCE(MAX(revision_number),0)+1 FROM plan_revisions WHERE tenant_id=? AND profile_id=?",params![tenant,profile],|r|r.get(0))?;
    let revision = insert(
        tx,
        "plan_revisions",
        &json!({"tenantId":tenant,"profileId":profile,"revisionNumber":number,"parentRevisionId":previous,"inputSetId":input_set,"provenanceKind":"tracked","digestFormat":model::REVISION_FORMAT,"snapshotDigest":prepared.digest,"createdBy":actor,"acceptedBy":actor,"createdAt":at,"acceptedAt":at}),
        false,
    )?;
    let old_ids: HashSet<i64> = rows(
        tx,
        "SELECT id FROM parts WHERE tenant_id=? AND profile_id=?",
        &[&tenant, &profile],
    )?
    .iter()
    .map(|p| num(p, "id"))
    .collect::<Result<_>>()?;
    tx.execute(
        "DELETE FROM parts WHERE tenant_id=? AND profile_id=?",
        params![tenant, profile],
    )?;
    let mut projection_ids = HashMap::new();
    let mut revision_ids = HashMap::new();
    for p in &prepared.parts {
        let id = num(p, "id")?;
        let mut projected = projection(p)?;
        projected["tenantId"] = json!(tenant);
        projected["profileId"] = json!(profile);
        let new_id = insert_part(tx, "parts", &projected)?;
        ensure!(!old_ids.contains(&new_id), "Projection identifier reused");
        projection_ids.insert(id, new_id);
        let mut immutable = p.projected(ru::model::PART_FIELDS);
        immutable["tenantId"] = json!(tenant);
        immutable["revisionId"] = json!(revision);
        immutable["projectionPartId"] = json!(new_id);
        revision_ids.insert(id, insert_part(tx, "plan_revision_parts", &immutable)?);
    }
    write_remap(tx, tenant, production, &remapping, &projection_ids)?;
    for a in &prepared.assignments {
        if let Assignment::Create { .. } = a {
            let (token, name) = allocated
                .get(&a.slot())
                .ok_or_else(|| anyhow!("Missing allocated unit"))?;
            insert(
                tx,
                "required_units",
                &json!({"tenantId":tenant,"profileId":profile,"createdInRevisionId":revision,"createdAt":at,"token":token,"objectName":name}),
                false,
            )?;
        }
    }
    let mut mappings = Vec::new();
    let mut expected_progress = Vec::new();
    for (i, a) in prepared.assignments.iter().enumerate() {
        let (part, index) = a.slot();
        let (token, name) = match a {
            Assignment::Create { .. } => {
                let (t, n) = allocated
                    .get(&a.slot())
                    .ok_or_else(|| anyhow!("Missing allocated unit"))?;
                (t.as_str(), n.as_str())
            }
            Assignment::Reuse { token, .. } => (
                token.as_str(),
                text(
                    &base
                        .get(token)
                        .ok_or_else(|| anyhow!("Missing reused unit"))?
                        .0,
                    "objectName",
                )?,
            ),
        };
        let revision_part = revision_ids[&part];
        insert(
            tx,
            "plan_revision_required_units",
            &json!({"tenantId":tenant,"revisionId":revision,"revisionPartId":revision_part,"unitIndex":index,"requiredUnitToken":token}),
            false,
        )?;
        mappings.push(json!({"revision_part_id":revision_part,"unit_index":index,"token":token,"object_name":name}));
        let (completed, assembled) = prepared.progress[i];
        expected_progress.push(json!({"partId":projection_ids[&part],"unitIndex":index,"completed":i64::from(completed),"assembled":i64::from(assembled)}));
    }
    let mapping_digest = ru::model::digest(
        &json!({"format":"required-unit-map-v1","revision_id":revision,"expected_unit_count":mappings.len(),"rows":mappings}),
    );
    insert(
        tx,
        "plan_revision_required_unit_sets",
        &json!({"revisionId":revision,"tenantId":tenant,"profileId":profile,"format":"required-unit-map-v1","expectedUnitCount":mappings.len(),"mappingDigest":mapping_digest,"createdAt":at}),
        false,
    )?;
    for p in &expected_progress {
        let mut row = p.clone();
        row["tenantId"] = json!(tenant);
        insert(tx, "print_progress", &row, false)?;
    }
    tx.execute("INSERT INTO plan_accepted_input_sets(tenant_id,profile_id,input_set_id,accepted_at) VALUES(?,?,?,?) ON CONFLICT(profile_id) DO UPDATE SET input_set_id=excluded.input_set_id,accepted_at=excluded.accepted_at",params![tenant,profile,input_set,at])?;
    ensure!(tx.execute("UPDATE build_profiles SET accepted_plan_revision_id=?,accepted_plan_version=?,last_recomputed_at=? WHERE tenant_id=? AND id=? AND accepted_plan_revision_id IS NULL AND accepted_plan_version=?",params![revision,version+1,at,tenant,profile,version])?==1,"Accepted pointer publication failed");
    let life = request.expected_lifecycle_version.get() as i64;
    let reconciliation = num(&selected, "id")?;
    ensure!(tx.execute("UPDATE plan_drafts SET state='consumed',lifecycle_version=?,consumed_revision_id=?,consumed_at=? WHERE tenant_id=? AND profile_id=? AND id=? AND state='open' AND lifecycle_version=? AND snapshot_digest=? AND current_required_unit_reconciliation_id=?",params![life+1,revision,at,tenant,profile,draft_id,life,request.expected_snapshot_digest.as_str(),reconciliation])?==1,"Draft consumption failed");
    let receipt_row = json!({"tenantId":tenant,"profileId":profile,"draftId":draft_id,"actorId":actor,"idempotencyKey":key,"requestFormat":model::REQUEST_FORMAT,"requestDigest":request_hash,"expectedSnapshotDigest":request.expected_snapshot_digest,"expectedLifecycleVersion":life,"expectedBaseRevisionId":previous,"expectedBasePlanVersion":version,"reconciliationId":reconciliation,"reconciliationDigest":selected["reconciliationDigest"],"revisionId":revision,"planVersion":version+1,"revisionDigest":prepared.digest,"requiredUnitMappingDigest":mapping_digest,"draftLifecycleVersion":life+1,"appliedAt":at});
    let apply_request_id = insert(tx, "plan_apply_requests", &receipt_row, false)?;
    insert(
        tx,
        "plan_apply_admissions",
        &json!({
            "applyRequestId":apply_request_id,
            "requestFormat":model::REQUEST_FORMAT,
            "requestDigest":admitted_hash,
            "expectedSnapshotDigest":requests.admitted.expected_snapshot_digest,
            "expectedLifecycleVersion":requests.admitted.expected_lifecycle_version,
            "expectedBaseRevisionId":requests.admitted.expected_base.revision_id(),
            "expectedBasePlanVersion":requests.admitted.expected_base.plan_version()
        }),
        false,
    )?;
    let stored = one(
        tx,
        "SELECT * FROM plan_apply_requests WHERE tenant_id=? AND profile_id=? AND draft_id=?",
        &[&tenant, &profile, &draft_id],
    )?
    .ok_or_else(|| anyhow!("Missing published receipt"))?;
    let receipt = receipt(tx, tenant, &stored)?;
    let accepted = validate_projection(tx, tenant, profile, revision)?;
    ensure!(
        accepted.len() == prepared.parts.len(),
        "Published Part count mismatch"
    );
    for p in &prepared.parts {
        let id = num(p, "id")?;
        let actual = accepted
            .iter()
            .find(|r| r["id"] == revision_ids[&id])
            .ok_or_else(|| anyhow!("Missing published Part"))?;
        ensure!(
            actual["projectionPartId"] == projection_ids[&id]
                && ru::model::project(actual, ru::model::PART_FIELDS)
                    == ru::model::project(p, ru::model::PART_FIELDS),
            "Published Part mismatch"
        );
    }
    let actual_progress = rows(
        tx,
        "SELECT p.part_id,p.unit_index,p.completed,p.assembled FROM print_progress p JOIN parts r ON r.id=p.part_id AND r.tenant_id=p.tenant_id WHERE r.tenant_id=? AND r.profile_id=? ORDER BY p.part_id,p.unit_index",
        &[&tenant, &profile],
    )?;
    ensure!(
        actual_progress == expected_progress,
        "Published progress mismatch"
    );
    let accepted_input=one(tx,"SELECT input_set_id,accepted_at FROM plan_accepted_input_sets WHERE tenant_id=? AND profile_id=?",&[&tenant,&profile])?.ok_or_else(||anyhow!("Missing accepted input"))?;
    ensure!(
        accepted_input["inputSetId"] == input_set && accepted_input["acceptedAt"] == at,
        "Accepted input mismatch"
    );
    let final_profile=one(tx,"SELECT accepted_plan_revision_id,accepted_plan_version,last_recomputed_at FROM build_profiles WHERE tenant_id=? AND id=?",&[&tenant,&profile])?.ok_or_else(||anyhow!("Missing accepted Build"))?;
    ensure!(
        final_profile["acceptedPlanRevisionId"] == revision
            && final_profile["acceptedPlanVersion"] == version + 1
            && final_profile["lastRecomputedAt"] == at,
        "Accepted Build mismatch"
    );
    crate::domain_events::append(
        tx,
        tenant,
        crate::domain_events::Change {
            kind: crate::domain_events::REVISION_ACCEPTED,
            aggregate_type: "build",
            aggregate_id: profile.to_string(),
            aggregate_version: Some(version + 1),
            payload: json!({ "revision_id": revision }),
        },
    )?;
    Ok(Outcome::Applied { receipt })
}
