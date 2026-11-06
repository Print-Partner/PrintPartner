//! Saves Plan choices atomically and preserves receipts if capture fails after commit.
//!
//! Compiled with `desktop_rebuild`; service callers must check `PP_DESKTOP_REBUILD`.
use super::*;
use crate::plan_publication::{
    self as publication, AuthenticatedPublicationContext, PublicationRequests,
    RequiredUnitTokenAllocator,
};
use pp_contracts::{
    autosave::{ApplyPlanDraftReceipt, PlanChoice, SavePlanChoicesRequest},
    publication::ApplyRequest,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub struct SaveCommand {
    pub(super) credential: Credential,
    pub(super) profile: PositiveId,
    pub(super) request: SavePlanChoicesRequest,
    pub(super) key: String,
}
impl SaveCommand {
    pub fn new(
        credential: Credential,
        profile: PositiveId,
        request: SavePlanChoicesRequest,
        key: String,
    ) -> Result<Self> {
        let key = JsText::scalar(&key)
            .trim_ecmascript()
            .as_scalar()
            .ok_or_else(|| anyhow!("Invalid Save idempotency key"))?;
        ensure!(
            !key.is_empty() && key.encode_utf16().count() <= 200,
            "Invalid Save idempotency key"
        );
        Ok(Self {
            credential,
            profile,
            request,
            key,
        })
    }
}
#[derive(Debug)]
pub struct SavedAuthority {
    pub snapshot: Box<crate::read_model::Snapshot>,
    pub context: crate::read_model::CapturedContext,
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Refusal {
    NotFound,
    BuildArchived,
    AcceptedBaselineRequired,
    BaseChanged,
    DraftChanged,
    InputsChanged,
    IdempotencyConflict,
    PartNotFound,
    PartAmbiguous,
    NoLayers,
    NoStls,
    Reconciliation {
        outcome: required_units::Outcome,
    },
    Publication {
        outcome: pp_contracts::publication::Outcome,
    },
}
#[derive(Debug)]
pub enum Outcome {
    Saved {
        receipt: ApplyPlanDraftReceipt,
        closed_draft_ids: Vec<PositiveId>,
        authority: Box<SavedAuthority>,
    },
    Refused {
        reason: Refusal,
    },
}
pub struct SaveOutcomeJsonBody(Vec<u8>);
impl SaveOutcomeJsonBody {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
impl Outcome {
    pub fn into_json_body(self) -> Result<SaveOutcomeJsonBody> {
        let bytes = match self {
            Self::Saved {
                receipt,
                closed_draft_ids,
                authority,
            } => {
                let mut output = b"{\"kind\":\"saved\",\"receipt\":".to_vec();
                output.extend_from_slice(&serde_json::to_vec(&receipt)?);
                output.extend_from_slice(b",\"closed_draft_ids\":");
                output.extend_from_slice(&serde_json::to_vec(&closed_draft_ids)?);
                output.extend_from_slice(b",\"authority\":{\"snapshot\":");
                output.extend_from_slice(&authority.snapshot.into_json_body()?.into_bytes());
                output.extend_from_slice(b",\"context\":");
                output.extend_from_slice(&serde_json::to_vec(&authority.context)?);
                output.extend_from_slice(b"}}");
                output
            }
            Self::Refused { reason } => {
                serde_json::to_vec(&serde_json::json!({"kind":"refused","reason":reason}))?
            }
        };
        Ok(SaveOutcomeJsonBody(bytes))
    }
}
#[derive(Debug)]
pub struct CommittedSaveCaptureFailure {
    pub receipt: ApplyPlanDraftReceipt,
    pub closed_draft_ids: Vec<PositiveId>,
    pub capture_failure: anyhow::Error,
}
impl std::fmt::Display for CommittedSaveCaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Save committed; accepted capture failed: {}",
            self.capture_failure
        )
    }
}
impl std::error::Error for CommittedSaveCaptureFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.capture_failure.as_ref())
    }
}
impl WorkingDraftClient {
    pub fn save(
        &self,
        input: SaveCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Outcome> {
        self.enqueue_observed(
            input.credential,
            input.profile,
            cancelled,
            wait,
            |observations, reply| Operation::Save {
                request: input.request,
                key: input.key,
                observations,
                reply,
            },
        )
    }
}
pub(super) struct Command {
    pub(super) input: SaveCommand,
    pub(super) policy: auth::AuthPolicy,
    pub(super) observations: Observations,
    pub(super) cancelled: Arc<AtomicBool>,
}
pub(super) fn copy_credential(credential: &Credential) -> Credential {
    match credential {
        Credential::Session(secret) => {
            Credential::Session(auth::Secret::new(secret.expose().to_owned()))
        }
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => Credential::ApiKey {
            routed_tenant: routed_tenant.clone(),
            secret: auth::Secret::new(secret.expose().to_owned()),
        },
    }
}
enum TransactionResult {
    Saved(ApplyPlanDraftReceipt),
    Refused(Refusal),
}
pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    tokens: &mut dyn RequiredUnitTokenAllocator,
    admission: writer_admission::Deadline,
) -> Result<Outcome> {
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    let mut tx = writer_admission::begin_immediate(connection, admission)?;
    let receipt = match transact(&mut tx, &command, tokens)? {
        TransactionResult::Saved(receipt) => receipt,
        TransactionResult::Refused(reason) => return Ok(Outcome::Refused { reason }),
    };
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    tx.commit()?;
    let closed_draft_ids = command
        .input
        .request
        .expected_draft()
        .map(|d| vec![d.draft_id()])
        .unwrap_or_default();
    let captured = (|| {
        let tx = connection.transaction()?;
        let (tenant, _) = auth::authority::queries::observe_reconciliation_actor(
            &tx,
            &command.input.credential,
            command.policy,
        )?;
        let (snapshot, context) = crate::read_model::capture_published(
            &tx,
            &tenant,
            command.input.profile.get() as i64,
            &command.observations.repos,
            receipt.plan_version().get(),
        )?;
        tx.commit()?;
        Ok::<_, anyhow::Error>(SavedAuthority { snapshot, context })
    })();
    match captured {
        Ok(authority) => Ok(Outcome::Saved {
            receipt,
            closed_draft_ids,
            authority: Box::new(authority),
        }),
        Err(capture_failure) => Err(CommittedSaveCaptureFailure {
            receipt,
            closed_draft_ids,
            capture_failure,
        }
        .into()),
    }
}
fn keys(c: &SaveCommand, actor: &str) -> (String, String) {
    use sha2::{Digest, Sha256};
    let key = format!(
        "autosave-v1:{}",
        hex::encode(Sha256::digest(c.key.as_bytes()))
    );
    let base = c.request.expected_base();
    let expected_base = match base.revision_id() {
        Some(id) => json!({"kind":"revision","revisionId":id,"planVersion":base.plan_version()}),
        None => json!({"kind":"empty","planVersion":base.plan_version()}),
    };
    let expected_draft = c.request.expected_draft().map(|d| json!({"id":d.draft_id(),"snapshotDigest":d.snapshot_digest(),"lifecycleVersion":d.lifecycle_version()}));
    let changes = changes(c.request.decisions());
    let payload = json!({"profileId":c.profile,"actorId":actor,"idempotencyKey":c.key,"expectedBase":expected_base,"expectedDraft":expected_draft,"remapCheckoffLinks":c.request.remap_checkoff_links(),"changes":changes});
    (
        key,
        format!(
            "autosave-payload-v1:{}",
            required_units::model::digest(&payload)
        ),
    )
}
/// The choices as a receipt payload names them.
pub(super) fn changes(decisions: &[PlanChoice]) -> Vec<Value> {
    decisions
        .iter()
        .map(|choice| {
            let t = choice.target();
            let (kind, value) = match choice {
                PlanChoice::SetIncluded { value, .. } => ("set_included", json!(value)),
                PlanChoice::SetQuantityOverride { value, .. } => {
                    ("set_quantity_override", json!(value))
                }
            };
            json!({"target":{"partKey":t.part_key(),"relativePath":t.relative_path(),"sourceLayer":t.source_layer()},"kind":kind,"value":value})
        })
        .collect()
}

pub(super) type Checked<T> = std::result::Result<T, Refusal>;

/// Refuses an archived Plan, a missing baseline or a stale base, and
/// otherwise returns the Accepted base.
pub(super) fn accepted_base(
    tx: &Transaction<'_>,
    tenant: &str,
    p: &Value,
    request: &SavePlanChoicesRequest,
) -> Result<Checked<Value>> {
    if !p["archivedAt"].is_null() {
        return Ok(Err(Refusal::BuildArchived));
    }
    if baseline_required(tx, tenant, p)? {
        return Ok(Err(Refusal::AcceptedBaselineRequired));
    }
    let base = json!({"baseRevisionId":p["acceptedPlanRevisionId"],"basePlanVersion":p["acceptedPlanVersion"]});
    if base["baseRevisionId"] != json!(request.expected_base().revision_id())
        || base["basePlanVersion"] != json!(request.expected_base().plan_version())
    {
        return Ok(Err(Refusal::BaseChanged));
    }
    Ok(Ok(base))
}

/// Loads the open draft the caller observed and checks it is unchanged, or
/// checks that no draft is open when the caller observed none.
pub(super) fn observed_draft(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    base: &Value,
    request: &SavePlanChoicesRequest,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Checked<Option<Draft>>> {
    let Some(expected) = request.expected_draft() else {
        let open = one(
            tx,
            "SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND state='open' LIMIT 1",
            &[&tenant, &profile],
        )?;
        return Ok(if open.is_some() {
            Err(Refusal::DraftChanged)
        } else {
            Ok(None)
        });
    };
    let id = i64::try_from(expected.draft_id().get())?;
    let Some(d) = load(tx, tenant, profile, id, budget)? else {
        return Ok(Err(Refusal::DraftChanged));
    };
    if d.header["state"] != "open"
        || d.header["snapshotDigest"] != expected.snapshot_digest().as_str()
        || d.header["lifecycleVersion"] != expected.lifecycle_version().get()
    {
        return Ok(Err(Refusal::DraftChanged));
    }
    if d.header["baseRevisionId"] != base["baseRevisionId"]
        || d.header["basePlanVersion"] != base["basePlanVersion"]
    {
        return Ok(Err(Refusal::BaseChanged));
    }
    Ok(Ok(Some(d)))
}

/// Prepares a new draft from the Plan's sources against the Accepted base.
pub(super) fn prepare_new(
    tx: &mut Transaction<'_>,
    tenant: &str,
    profile: i64,
    base: &Value,
    observations: &Observations,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Checked<preparation::PreparedDraftSnapshot>> {
    let options = preparation::PreparationOptions {
        apply_manifest: false,
        prefer_accepted: true,
        ..Default::default()
    };
    match preparation::prepare(
        tx,
        tenant,
        profile,
        base.clone(),
        preparation::PreparationContext {
            options: &options,
            repos: &observations.repos,
        },
        observations.reads.as_ref(),
        budget,
    ) {
        Ok(prepared) => Ok(Ok(prepared)),
        Err(error) => match error.downcast_ref::<preparation::Failure>() {
            Some(preparation::Failure::NoLayers) => Ok(Err(Refusal::NoLayers)),
            Some(preparation::Failure::NoStls) => Ok(Err(Refusal::NoStls)),
            _ => Err(error),
        },
    }
}

fn normalized(value: &str) -> String {
    value
        .replace('\\', "/")
        .to_lowercase()
        .trim_matches('/')
        .into()
}
pub(super) fn choices(
    parts: &mut [crate::manifest_text::DraftPart],
    changes: &[PlanChoice],
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Option<Refusal>> {
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_layer: HashMap<(Option<String>, String), Vec<usize>> = HashMap::new();
    let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_normalized: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in parts.iter().enumerate() {
        budget.comparison(p.json_fields(model::PART_FIELDS, false).len())?;
        let key = text(p, "partKey")?;
        let path = normalized(text(p, "relativePath")?);
        by_key.entry(key.into()).or_default().push(i);
        by_layer
            .entry((p["sourceLayer"].as_str().map(str::to_owned), path.clone()))
            .or_default()
            .push(i);
        by_path.entry(path).or_default().push(i);
        by_normalized.entry(normalized(key)).or_default().push(i);
    }
    let mut touched = HashSet::new();
    for change in changes {
        budget.comparison(
            parts.len() + change.target().part_key().len() + change.target().relative_path().len(),
        )?;
        let t = change.target();
        let exact = by_key.get(t.part_key()).cloned().unwrap_or_default();
        let exact_set: HashSet<_> = exact.iter().copied().collect();
        let path = normalized(t.relative_path());
        let restrict = |matches: Option<&Vec<usize>>| {
            matches
                .into_iter()
                .flatten()
                .copied()
                .filter(|i| exact.is_empty() || exact_set.contains(i))
                .collect::<Vec<_>>()
        };
        let candidates = [
            exact.clone(),
            restrict(by_layer.get(&(Some(t.source_layer().unwrap_or("").into()), path.clone()))),
            restrict(by_path.get(&path)),
            restrict(by_normalized.get(&normalized(t.part_key()))),
        ];
        let Some(position) = candidates.iter().find(|v| v.len() == 1).map(|v| v[0]) else {
            return Ok(Some(if exact.len() > 1 {
                Refusal::PartAmbiguous
            } else {
                Refusal::PartNotFound
            }));
        };
        let included = matches!(change, PlanChoice::SetIncluded { .. });
        if !touched.insert((position, included)) {
            return Ok(Some(Refusal::PartAmbiguous));
        }
        match change {
            PlanChoice::SetIncluded { value, .. } => parts[position]["included"] = json!(value),
            PlanChoice::SetQuantityOverride { value, .. } => {
                parts[position]["quantityOverride"] = json!(value);
                parts[position]["quantityEffective"] = value
                    .map(|v| json!(v))
                    .unwrap_or_else(|| parts[position]["quantityInferred"].clone());
            }
        }
    }
    Ok(None)
}
fn transact(
    tx: &mut Transaction<'_>,
    c: &Command,
    tokens: &mut dyn RequiredUnitTokenAllocator,
) -> Result<TransactionResult> {
    let context = AuthenticatedPublicationContext::resolve(
        tx,
        copy_credential(&c.input.credential),
        c.policy,
    )?;
    let (tenant, actor) =
        auth::authority::queries::observe_reconciliation_actor(tx, &c.input.credential, c.policy)?;
    let tenant = tenant.as_str();
    let actor = JsText::scalar(&actor)
        .trim_ecmascript()
        .as_scalar()
        .ok_or_else(|| anyhow!("Invalid Save actor"))?;
    let actor = actor.as_str();
    ensure!(
        !actor.is_empty() && actor.encode_utf16().count() <= 200,
        "Invalid Save actor"
    );
    let profile = c.input.profile.get() as i64;
    let request = &c.input.request;
    let Some(p) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(TransactionResult::Refused(Refusal::NotFound));
    };
    let (key, payload) = keys(&c.input, actor);
    if let Some(prior) = one(
        tx,
        "SELECT * FROM plan_apply_requests WHERE tenant_id=? AND profile_id=? AND actor_id=? AND idempotency_key=?",
        &[&tenant, &profile, &actor, &key],
    )? {
        let draft = one(
            tx,
            "SELECT idempotency_key FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND id=?",
            &[&tenant, &profile, &num(&prior, "draftId")?],
        )?;
        if draft
            .as_ref()
            .is_none_or(|d| d["idempotencyKey"] != payload)
        {
            return Ok(TransactionResult::Refused(Refusal::IdempotencyConflict));
        }
        return Ok(TransactionResult::Saved(publication::receipt(
            tx, tenant, &prior,
        )?));
    }
    let base = match accepted_base(tx, tenant, &p, request)? {
        Ok(base) => base,
        Err(reason) => return Ok(TransactionResult::Refused(reason)),
    };
    let mut budget = observation::PreparationBudget::new(&c.cancelled, c.observations.limits)?;
    budget.check()?;
    let source = match observed_draft(tx, tenant, profile, &base, request, &mut budget)? {
        Ok(source) => source,
        Err(reason) => return Ok(TransactionResult::Refused(reason)),
    };
    let (mut capture, mut parts) = if let Some(d) = &source {
        let current = preparation::capture(
            tx,
            tenant,
            profile,
            c.observations.reads.as_ref(),
            &mut budget,
        )?;
        if canonical_inputs(&current.inputs) != canonical_inputs(&d.inputs) {
            return Ok(TransactionResult::Refused(Refusal::InputsChanged));
        }
        (current, d.parts.clone())
    } else {
        match prepare_new(tx, tenant, profile, &base, &c.observations, &mut budget)? {
            Ok(prepared) => (prepared.capture, prepared.parts),
            Err(reason) => return Ok(TransactionResult::Refused(reason)),
        }
    };
    if let Some(reason) = choices(&mut parts, request.decisions(), &mut budget)? {
        return Ok(TransactionResult::Refused(reason));
    }
    if let Some(d) = &source {
        capture.inputs = d.inputs.clone();
    }
    let digest = required_units::model::planning(&base, &capture.inputs, &parts)?;
    budget.check()?;
    let id = insert_snapshot(
        tx,
        tenant,
        actor,
        profile,
        &payload,
        &preparation::PreparedDraftSnapshot {
            base,
            capture,
            parts,
            digest: digest.clone(),
        },
        None,
    )?;
    let inserted = storage::draft_checked(tx, tenant, profile, id, &|| Ok(budget.check()?))?
        .ok_or_else(|| anyhow!("Inserted Save draft missing"))?;
    let mut decisions = Vec::new();
    if let Some(d) = &source
        && let Some(selected) = &d.selected
    {
        ensure!(
            d.parts.len() == inserted.parts.len(),
            "Copied Save Parts differ"
        );
        let ids: HashMap<_, _> = d
            .parts
            .iter()
            .zip(&inserted.parts)
            .map(|(a, b)| Ok((num(a, "id")?, num(b, "id")?)))
            .collect::<Result<_>>()?;
        budget.check()?;
        for row in rows(
            tx,
            "SELECT * FROM plan_draft_required_unit_decisions WHERE tenant_id=? AND reconciliation_id=? ORDER BY target_draft_part_id",
            &[&tenant, &num(&selected.header, "id")?],
        )? {
            budget.check()?;
            let target = *ids
                .get(&num(&row, "targetDraftPartId")?)
                .ok_or_else(|| anyhow!("Copied decision target missing"))?;
            let mut decision = json!({"kind":row["kind"],"target_draft_part_id":target});
            if row["kind"] != "replace" {
                decision["predecessor_revision_part_id"] = row["predecessorRevisionPartId"].clone();
            }
            decisions.push(decision);
        }
    }
    let reconcile =
        serde_json::from_value(json!({"expected_snapshot_digest":digest,"decisions":decisions}))?;
    let draft_id = PositiveId::new(id as u64).map_err(anyhow::Error::msg)?;
    let selection = required_units::reconcile::run(
        tx,
        &ReconciliationCommand::new(
            c.input.profile,
            draft_id,
            reconcile,
            &key,
            copy_credential(&c.input.credential),
        )?,
        c.policy,
        &mut || Ok(budget.check()?),
    )?;
    let required_units::Outcome::Ready { workspace } = selection else {
        return Ok(TransactionResult::Refused(match selection {
            required_units::Outcome::DraftChanged { .. } => Refusal::DraftChanged,
            other => Refusal::Reconciliation { outcome: other },
        }));
    };
    let apply = ApplyRequest {
        expected_snapshot_digest: Digest::new(workspace.draft.snapshot_digest.clone())
            .map_err(anyhow::Error::msg)?,
        expected_lifecycle_version: pp_contracts::autosave::WireInteger::new(
            workspace.draft.lifecycle_version as u64,
        )
        .map_err(anyhow::Error::msg)?,
        expected_base: request.expected_base().clone(),
        remap_checkoff_links: request.remap_checkoff_links(),
    };
    let applied = publication::publish_in_transaction(
        tx,
        &context,
        c.input.profile,
        draft_id,
        PublicationRequests::new(&apply, &apply),
        &key,
        tokens,
    )?;
    let receipt = match applied {
        pp_contracts::publication::Outcome::Applied { receipt }
        | pp_contracts::publication::Outcome::Existing { receipt }
        | pp_contracts::publication::Outcome::AlreadyApplied { receipt } => receipt,
        other => {
            return Ok(TransactionResult::Refused(Refusal::Publication {
                outcome: other,
            }));
        }
    };
    if let Some(d) = source {
        ensure!(
            num(&d.header, "lifecycleVersion")? < i64::from(i32::MAX),
            "Draft lifecycle exhausted"
        );
        let changed=tx.execute("UPDATE plan_drafts SET state='abandoned',lifecycle_version=lifecycle_version+1 WHERE tenant_id=? AND profile_id=? AND id=? AND state='open' AND lifecycle_version=? AND snapshot_digest=?",params![tenant,profile,num(&d.header,"id")?,num(&d.header,"lifecycleVersion")?,text(&d.header,"snapshotDigest")?])?;
        if changed != 1 {
            return Ok(TransactionResult::Refused(Refusal::DraftChanged));
        }
        crate::domain_events::draft_changed(tx, tenant, profile, num(&d.header, "id")?)?;
        let after = storage::draft_checked(tx, tenant, profile, num(&d.header, "id")?, &|| {
            Ok(budget.check()?)
        })?
        .ok_or_else(|| anyhow!("Closed Save draft missing"))?;
        ensure!(
            after.header["snapshotDigest"] == d.header["snapshotDigest"]
                && after.header["lifecycleVersion"].as_i64()
                    == Some(num(&d.header, "lifecycleVersion")? + 1)
                && after.header["state"] == "abandoned",
            "Observed Save closure mismatch"
        );
    }
    Ok(TransactionResult::Saved(receipt))
}

pub(super) fn canonical_inputs(inputs: &[Value]) -> Vec<Value> {
    let mut inputs: Vec<_> = inputs
        .iter()
        .map(|input| model::project(input, model::INPUT_FIELDS))
        .collect();
    inputs.sort_by_key(|input| input["sourceId"].as_i64());
    inputs
}

pub(super) fn load(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    id: i64,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Option<Draft>> {
    budget.check()?;
    if one(
        tx,
        "SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND id=?",
        &[&tenant, &profile, &id],
    )?
    .is_none()
    {
        return Ok(None);
    }
    charge_open_draft(tx, tenant, id, budget)?;
    storage::draft_checked(tx, tenant, profile, id, &|| Ok(budget.check()?))
}
