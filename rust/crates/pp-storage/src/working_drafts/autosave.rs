//! Draft-only autosave (GRE-418). Plan choices land in the observed open
//! draft, or in a new final-state draft when none is open, with one
//! Required-unit reconciliation whose row is the durable receipt, in one
//! immediate transaction. Autosave never publishes and never closes a draft:
//! Accept (`POST .../drafts/:draftId/apply`) is the only publisher.
//!
//! Compiled with `desktop_rebuild`; service callers must check `PP_DESKTOP_REBUILD`.
use super::save::{
    Checked, Refusal, accepted_base, canonical_inputs, changes, choices, copy_credential,
    observed_draft, prepare_new,
};
use super::*;
pub use pp_contracts::autosave::SavePlanChoicesRequest;
use serde::Serialize;

const RECEIPT_PREFIX: &str = "autosave-draft-v1:";

pub struct AutosaveCommand {
    credential: Credential,
    profile: PositiveId,
    request: SavePlanChoicesRequest,
    key: String,
}
impl AutosaveCommand {
    /// The key is the `Idempotency-Key` header, trimmed as JavaScript trims,
    /// 1 to 160 UTF-16 units.
    pub fn new(
        credential: Credential,
        profile: PositiveId,
        request: SavePlanChoicesRequest,
        key: &str,
    ) -> Result<Self> {
        let key = JsText::scalar(key)
            .trim_ecmascript()
            .as_scalar()
            .ok_or_else(|| anyhow!("Invalid autosave idempotency key"))?;
        ensure!(
            !key.is_empty() && key.encode_utf16().count() <= 160,
            "Invalid autosave idempotency key"
        );
        Ok(Self {
            credential,
            profile,
            request,
            key,
        })
    }
}

/// What a committed autosave wrote. A replay returns the same receipt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AutosaveReceipt {
    pub profile_id: i64,
    pub draft_id: i64,
    pub reconciliation_id: i64,
    pub snapshot_digest: String,
    pub saved_at: String,
}

#[derive(Debug)]
pub enum AutosaveOutcome {
    /// The receipt and the draft's workspace, read after the commit.
    Saved {
        receipt: AutosaveReceipt,
        workspace: Box<required_units::Workspace>,
    },
    Refused {
        reason: Refusal,
    },
}

impl WorkingDraftClient {
    pub fn autosave(
        &self,
        input: AutosaveCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<AutosaveOutcome> {
        let observations = self
            .observations
            .clone()
            .ok_or_else(|| anyhow!("Working-draft observations are not configured"))?;
        self.enqueue(cancelled, wait, |reply, cancelled| {
            Envelope::WorkingDraftWrite {
                command: Write {
                    credential: input.credential,
                    profile: input.profile,
                    policy: self.policy,
                    cancelled,
                    operation: Operation::Autosave {
                        request: input.request,
                        key: input.key,
                        observations,
                        reply,
                    },
                },
            }
        })
    }
}

pub(super) struct Args<'a> {
    pub credential: &'a Credential,
    pub policy: auth::AuthPolicy,
    pub profile: PositiveId,
    pub request: &'a SavePlanChoicesRequest,
    pub key: &'a str,
    pub observations: &'a Observations,
    pub cancelled: &'a Arc<AtomicBool>,
}

/// Runs the transaction, then reads the saved draft's workspace.
pub(super) fn execute(
    connection: &mut Connection,
    args: Args<'_>,
    admission: writer_admission::Deadline,
) -> Result<AutosaveOutcome> {
    ensure!(!args.cancelled.load(Ordering::Acquire), Failure::Cancelled);
    let mut tx = writer_admission::begin_immediate(connection, admission)?;
    let receipt = match transact(&mut tx, &args)? {
        Ok(receipt) => receipt,
        Err(reason) => return Ok(AutosaveOutcome::Refused { reason }),
    };
    ensure!(!args.cancelled.load(Ordering::Acquire), Failure::Cancelled);
    tx.commit()?;
    let read = Command {
        credential: copy_credential(args.credential),
        profile: args.profile,
        request: Request::Workspace {
            draft_id: PositiveId::new(receipt.draft_id as u64).map_err(anyhow::Error::msg)?,
        },
        policy: args.policy,
        cancelled: args.cancelled.clone(),
    };
    match super::execute(connection, read)? {
        Outcome::Workspace { workspace } => Ok(AutosaveOutcome::Saved { receipt, workspace }),
        _ => Err(anyhow!("Autosaved Plan draft is missing")),
    }
}

/// The stored receipt key: the request key's SHA-256 and the payload digest,
/// so a lookup by key finds the prior save and tells a changed payload apart.
fn receipt_key(c: &Args<'_>, actor: &str) -> (String, String) {
    use sha2::{Digest as _, Sha256};
    let prefix = format!(
        "{RECEIPT_PREFIX}{}:",
        hex::encode(Sha256::digest(c.key.as_bytes()))
    );
    let base = c.request.expected_base();
    let draft = c.request.expected_draft().map(|d| {
        json!({"id":d.draft_id(),"snapshotDigest":d.snapshot_digest(),"lifecycleVersion":d.lifecycle_version()})
    });
    let changes = changes(c.request.decisions());
    let payload = json!({"profileId":c.profile,"actorId":actor,"expectedBase":{"revisionId":base.revision_id(),"planVersion":base.plan_version()},"expectedDraft":draft,"remapCheckoffLinks":c.request.remap_checkoff_links(),"changes":changes});
    let full = format!("{prefix}{}", required_units::model::digest(&payload));
    (prefix, full)
}

fn receipt(row: &Value) -> Result<AutosaveReceipt> {
    Ok(AutosaveReceipt {
        profile_id: num(row, "profileId")?,
        draft_id: num(row, "draftId")?,
        reconciliation_id: num(row, "id")?,
        snapshot_digest: model::selection(
            text(row, "planningDigest")?,
            Some(text(row, "reconciliationDigest")?),
        ),
        saved_at: text(row, "createdAt")?.into(),
    })
}

fn transact(
    tx: &mut Transaction<'_>,
    c: &Args<'_>,
) -> Result<std::result::Result<AutosaveReceipt, Refusal>> {
    let (tenant, actor) =
        auth::authority::queries::observe_reconciliation_actor(tx, c.credential, c.policy)?;
    let tenant = tenant.as_str();
    let actor = JsText::scalar(&actor)
        .trim_ecmascript()
        .as_scalar()
        .ok_or_else(|| anyhow!("Invalid autosave actor"))?;
    let actor = actor.as_str();
    let profile = i64::try_from(c.profile.get())?;
    let request = c.request;
    let Some(p) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(Err(Refusal::NotFound));
    };
    let (prefix, key) = receipt_key(c, actor);
    // The receipt is read before any stale check, so a committed retry
    // replays even after the base or draft moved on.
    let prior = one(
        tx,
        "SELECT * FROM plan_draft_required_unit_reconciliations WHERE tenant_id=? AND profile_id=? AND actor_id=? AND substr(idempotency_key,1,?)=? ORDER BY id DESC LIMIT 1",
        &[&tenant, &profile, &actor, &(prefix.len() as i64), &prefix],
    )?;
    if let Some(prior) = prior {
        if prior["idempotencyKey"] != key.as_str() {
            return Ok(Err(Refusal::IdempotencyConflict));
        }
        return Ok(Ok(receipt(&prior)?));
    }
    let base = match accepted_base(tx, tenant, &p, request)? {
        Ok(base) => base,
        Err(reason) => return Ok(Err(reason)),
    };
    let mut budget = observation::PreparationBudget::new(c.cancelled, c.observations.limits)?;
    budget.check()?;
    let at = Target {
        tenant,
        actor,
        profile,
        base: &base,
    };
    let written = match observed_draft(tx, tenant, profile, &base, request, &mut budget)? {
        Ok(Some(d)) => write_observed(tx, c, &at, &d, &mut budget)?,
        Ok(None) => write_new(tx, c, &at, &key, &mut budget)?,
        Err(reason) => Err(reason),
    };
    let (draft_id, decisions) = match written {
        Ok(written) => written,
        Err(reason) => return Ok(Err(reason)),
    };
    let current = storage::draft_checked(tx, tenant, profile, draft_id, &|| Ok(budget.check()?))?
        .ok_or_else(|| anyhow!("Autosave draft is missing"))?;
    let reconcile = serde_json::from_value(
        json!({"expected_snapshot_digest":current.header["snapshotDigest"],"decisions":decisions}),
    )?;
    let selection = required_units::reconcile::run(
        tx,
        &ReconciliationCommand::new(
            c.profile,
            PositiveId::new(draft_id as u64).map_err(anyhow::Error::msg)?,
            reconcile,
            &key,
            copy_credential(c.credential),
        )?,
        c.policy,
        &mut || Ok(budget.check()?),
    )?;
    match selection {
        required_units::Outcome::Ready { .. } => {}
        required_units::Outcome::DraftChanged { .. } => return Ok(Err(Refusal::DraftChanged)),
        other => return Ok(Err(Refusal::Reconciliation { outcome: other })),
    }
    let saved = one(
        tx,
        "SELECT * FROM plan_draft_required_unit_reconciliations WHERE tenant_id=? AND profile_id=? AND actor_id=? AND draft_id=? AND idempotency_key=?",
        &[&tenant, &profile, &actor, &draft_id, &key],
    )?
    .ok_or_else(|| anyhow!("Autosave receipt is missing"))?;
    Ok(Ok(receipt(&saved)?))
}

/// Where an autosave writes: the caller's tenant, actor, Plan and its base.
struct Target<'a> {
    tenant: &'a str,
    actor: &'a str,
    profile: i64,
    base: &'a Value,
}

type Written = Checked<(i64, Vec<Value>)>;

/// Applies the choices to the open draft the caller observed and carries its
/// selection's decisions into the next reconciliation.
fn write_observed(
    tx: &Transaction<'_>,
    c: &Args<'_>,
    at: &Target<'_>,
    d: &Draft,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Written> {
    let current = preparation::capture(
        tx,
        at.tenant,
        at.profile,
        c.observations.reads.as_ref(),
        budget,
    )?;
    if canonical_inputs(&current.inputs) != canonical_inputs(&d.inputs) {
        return Ok(Err(Refusal::InputsChanged));
    }
    let mut parts = d.parts.clone();
    if let Some(reason) = choices(&mut parts, c.request.decisions(), budget)? {
        return Ok(Err(reason));
    }
    if parts != d.parts {
        update_parts(tx, at.tenant, at.profile, d, &parts)?;
    }
    let decisions = match &d.selected {
        Some(selected) => carried_decisions(tx, at.tenant, selected, budget)?,
        None => Vec::new(),
    };
    Ok(Ok((num(&d.header, "id")?, decisions)))
}

/// Prepares a final-state draft when the caller observed none and none is open.
fn write_new(
    tx: &mut Transaction<'_>,
    c: &Args<'_>,
    at: &Target<'_>,
    key: &str,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Written> {
    let mut prepared =
        match prepare_new(tx, at.tenant, at.profile, at.base, c.observations, budget)? {
            Ok(prepared) => prepared,
            Err(reason) => return Ok(Err(reason)),
        };
    if let Some(reason) = choices(&mut prepared.parts, c.request.decisions(), budget)? {
        return Ok(Err(reason));
    }
    prepared.digest = model::planning(at.base, &prepared.capture.inputs, &prepared.parts)?;
    budget.check()?;
    let id = insert_snapshot(tx, at.tenant, at.actor, at.profile, key, &prepared, None)?;
    Ok(Ok((id, Vec::new())))
}

/// Writes the chosen Parts into the open draft as an edit does: the
/// selection is cleared and the snapshot digest follows the new Parts.
fn update_parts(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    d: &Draft,
    parts: &[crate::manifest_text::DraftPart],
) -> Result<()> {
    let id = num(&d.header, "id")?;
    let planning = model::planning(&d.header, &d.inputs, parts)?;
    let next = if d.header["digestFormat"] == "plan-draft-v2" {
        model::selection(&planning, None)
    } else {
        planning
    };
    ensure!(
        tx.execute(
            "UPDATE plan_drafts SET current_required_unit_reconciliation_id=NULL,snapshot_digest=? WHERE tenant_id=? AND profile_id=? AND id=? AND state='open' AND snapshot_digest=?",
            params![next, tenant, profile, id, text(&d.header, "snapshotDigest")?],
        )? == 1,
        "Autosave lost its draft"
    );
    for (before, after) in d.parts.iter().zip(parts) {
        if before == after {
            continue;
        }
        ensure!(
            tx.execute(
                "UPDATE plan_draft_parts SET included=?,quantity_override=?,quantity_effective=? WHERE tenant_id=? AND draft_id=? AND id=?",
                params![
                    after["included"].as_bool(),
                    after["quantityOverride"].as_i64(),
                    num(after, "quantityEffective")?,
                    tenant,
                    id,
                    num(after, "id")?
                ],
            )? == 1,
            "Autosave did not update a chosen Part"
        );
    }
    crate::domain_events::draft_changed(tx, tenant, profile, id)?;
    Ok(())
}

/// The prior selection's decisions, which name the same draft Parts.
fn carried_decisions(
    tx: &Transaction<'_>,
    tenant: &str,
    selected: &required_units::storage::Saved,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Vec<Value>> {
    let mut decisions = Vec::new();
    for row in rows(
        tx,
        "SELECT * FROM plan_draft_required_unit_decisions WHERE tenant_id=? AND reconciliation_id=? ORDER BY target_draft_part_id",
        &[&tenant, &num(&selected.header, "id")?],
    )? {
        budget.check()?;
        let mut decision =
            json!({"kind":row["kind"],"target_draft_part_id":row["targetDraftPartId"]});
        if row["kind"] != "replace" {
            decision["predecessor_revision_part_id"] = row["predecessorRevisionPartId"].clone();
        }
        decisions.push(decision);
    }
    Ok(decisions)
}
