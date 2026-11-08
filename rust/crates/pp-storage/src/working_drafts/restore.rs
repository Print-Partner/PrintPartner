//! Restore a Plan snapshot as a draft (GRE-1094):
//! `POST /plans/{id}/snapshots/{snapshot_id}/restore` in draft mode. The
//! snapshot's Accepted revision becomes a new open draft, which only Accept
//! publishes. Nothing outside the draft tables changes: no Accepted Plan row,
//! Checkoff record, send queue item or Plate.
//!
//! Only a snapshot that records the Accepted revision it captured can be
//! restored; Accept saves one before every publication. The draft's Parts and
//! pinned inputs are that revision's, and its header is the one the snapshot
//! saved. Each restored Part's predecessor is the current Accepted Part
//! holding its Required units; a Part the current Plan no longer has is new.
//! A restore whose Part quantities could not hold the current printed units
//! is refused. That is the only printed-unit check: when a restored Part's
//! artifact, Source or role differs from its predecessor's, the selection
//! reports `unsafe_predecessor`, and a `replace` decision at that gate mints
//! fresh tokens and retires the printed ones, as Accept's `printed_affected`
//! preview lists.
//!
//! Compiled with `desktop_rebuild`; service callers must check `PP_DESKTOP_REBUILD`.
use super::save::{
    Refusal, accepted_base, copy_credential, keyed_expectations, observed_draft, tenant_actor,
};
use super::*;
pub use pp_contracts::snapshot_restore::RestoreSnapshotRequest;
use required_units::storage::part_rows;

const KEY_PREFIX: &str = "snapshot-restore-v1:";

pub struct RestoreCommand {
    credential: Credential,
    profile: PositiveId,
    snapshot: PositiveId,
    request: RestoreSnapshotRequest,
    key: String,
}
impl RestoreCommand {
    /// The key is the `Idempotency-Key` header, trimmed as JavaScript trims,
    /// 1 to 160 UTF-16 units.
    pub fn new(
        credential: Credential,
        (profile, snapshot): (PositiveId, PositiveId),
        request: RestoreSnapshotRequest,
        key: &str,
    ) -> Result<Self> {
        let key = JsText::scalar(key)
            .trim_ecmascript()
            .as_scalar()
            .ok_or_else(|| anyhow!("Invalid snapshot restore idempotency key"))?;
        ensure!(
            !key.is_empty() && key.encode_utf16().count() <= 160,
            "Invalid snapshot restore idempotency key"
        );
        Ok(Self {
            credential,
            profile,
            snapshot,
            request,
            key,
        })
    }
}

#[derive(Debug)]
pub enum RestoreRefusal {
    Draft(Refusal),
    SnapshotNotFound,
    /// The snapshot records no Accepted revision of this Build.
    NotRestorable,
    /// The current Accepted revision has no Required-unit set, so no
    /// predecessor can be found.
    RequiredUnitSetUnavailable,
    /// The snapshot's header fails a header save's field rule; the field and
    /// the rule.
    HeaderInvalid {
        field: &'static str,
        detail: &'static str,
    },
    /// Printed Required units of a current Part that the restored quantities
    /// cannot hold. Passing this check does not keep printed tokens: a
    /// `replace` decision on an `unsafe_predecessor` Part still retires them.
    PrintedUnitsAffected {
        tokens: Vec<String>,
    },
}

#[derive(Debug)]
pub enum RestoreOutcome {
    /// The restored draft after its automatic Required-unit selection.
    Restored(WorkspaceOutcome),
    Refused {
        reason: RestoreRefusal,
    },
}

impl WorkingDraftClient {
    pub fn restore_snapshot(
        &self,
        input: RestoreCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<RestoreOutcome> {
        self.enqueue_observed(
            input.credential,
            input.profile,
            cancelled,
            wait,
            |observations, reply| Operation::Restore {
                snapshot: input.snapshot,
                request: input.request,
                key: input.key,
                observations,
                reply,
            },
        )
    }
}

pub(super) struct Args<'a> {
    pub credential: &'a Credential,
    pub policy: auth::AuthPolicy,
    pub profile: PositiveId,
    pub snapshot: PositiveId,
    pub request: &'a RestoreSnapshotRequest,
    pub key: &'a str,
    pub observations: &'a Observations,
    pub cancelled: &'a Arc<AtomicBool>,
}

/// Writes the draft in one immediate transaction, then selects its Required
/// units as recompute does. The draft commits before the selection starts.
pub(super) fn execute(
    connection: &mut Connection,
    args: Args<'_>,
    admission: writer_admission::Deadline,
) -> Result<RestoreOutcome> {
    let written = super::transact(connection, admission, args.cancelled, |tx| {
        transact(tx, &args)
    })?;
    let draft_id = match written {
        Ok(id) => PositiveId::new(u64::try_from(id)?).map_err(anyhow::Error::msg)?,
        Err(reason) => return Ok(RestoreOutcome::Refused { reason }),
    };
    let read = Command {
        credential: copy_credential(args.credential),
        profile: args.profile,
        request: Request::Read { draft_id },
        policy: args.policy,
        cancelled: args.cancelled.clone(),
    };
    let Outcome::Read { draft } = super::execute(connection, read)? else {
        return Err(anyhow!("Restored Plan draft is missing"));
    };
    // A replay can find its draft consumed or abandoned before the selection
    // ran, after a crash between the two commits. Selecting a closed draft
    // fails, so answer with the draft as it stands.
    if draft.identity.state != DraftState::Open {
        return ready_workspace(
            connection,
            copy_credential(args.credential),
            args.policy,
            args.profile,
            args.cancelled,
            draft.identity.draft_id,
        )
        .map(RestoreOutcome::Restored);
    }
    select_draft(
        connection,
        copy_credential(args.credential),
        args.policy,
        args.profile,
        args.cancelled,
        draft,
    )
    .map(RestoreOutcome::Restored)
}

fn transact(
    tx: &Transaction<'_>,
    c: &Args<'_>,
) -> Result<std::result::Result<i64, RestoreRefusal>> {
    let refused = |reason| Ok(Err(RestoreRefusal::Draft(reason)));
    let profile = i64::try_from(c.profile.get())?;
    let snapshot = i64::try_from(c.snapshot.get())?;
    let (tenant, actor) = tenant_actor(tx, c.credential, c.policy)?;
    let tenant = tenant.as_str();
    let Some(p) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return refused(Refusal::NotFound);
    };
    let (prefix, key) = draft_key(c, profile, &actor);
    if let Some(answer) = prior_draft(tx, (tenant, &actor, profile), &prefix, &key)? {
        return Ok(answer);
    }
    let Some(saved) = one(
        tx,
        "SELECT payload_json FROM plan_snapshots WHERE tenant_id=? AND profile_id=? AND id=?",
        &[&tenant, &profile, &snapshot],
    )?
    else {
        return Ok(Err(RestoreRefusal::SnapshotNotFound));
    };
    let mut budget = observation::PreparationBudget::new(c.cancelled, c.observations.limits)?;
    let base = match observed_base(tx, c, (tenant, profile, &p), &mut budget)? {
        Ok(base) => base,
        Err(reason) => return refused(reason),
    };
    let Some(revision) = restorable_revision(tx, tenant, profile, text(&saved, "payloadJson")?)?
    else {
        return Ok(Err(RestoreRefusal::NotRestorable));
    };
    let Some(inputs) = restored_inputs(tx, tenant, profile, revision)? else {
        return refused(Refusal::InputsChanged);
    };
    let parts = match restored_parts(tx, tenant, profile, revision, &base)? {
        Ok(parts) => parts,
        Err(reason) => return Ok(Err(reason)),
    };
    let header = match header::snapshot(tx, tenant, snapshot)? {
        Ok(header) => header,
        Err((field, detail)) => return Ok(Err(RestoreRefusal::HeaderInvalid { field, detail })),
    };
    budget.check()?;
    abandon_open(tx, tenant, profile)?;
    let digest = model::planning(&base, &inputs, &parts)?;
    let restored = DraftRows {
        base: &base,
        inputs: &inputs,
        parts: &parts,
        digest: &digest,
    };
    let id = insert_snapshot(
        tx,
        tenant,
        &actor,
        profile,
        &key,
        restored,
        Origin::Snapshot(header.as_ref()),
    )?;
    Ok(Ok(id))
}

/// The answer to a committed retry: the draft this actor created under the
/// key when the request is the same, even after the Plan moved on, or an
/// idempotency conflict.
fn prior_draft(
    tx: &Transaction<'_>,
    (tenant, actor, profile): (&str, &str, i64),
    prefix: &str,
    key: &str,
) -> Result<Option<std::result::Result<i64, RestoreRefusal>>> {
    let Some(prior) = one(
        tx,
        "SELECT id,idempotency_key FROM plan_drafts WHERE tenant_id=? AND created_by=? AND profile_id=? AND substr(idempotency_key,1,?)=?",
        &[&tenant, &actor, &profile, &(prefix.len() as i64), &prefix],
    )?
    else {
        return Ok(None);
    };
    Ok(Some(if text(&prior, "idempotencyKey")? == key {
        Ok(num(&prior, "id")?)
    } else {
        Err(RestoreRefusal::Draft(Refusal::IdempotencyConflict))
    }))
}

/// The Accepted base, after the base and open-draft checks a draft header
/// save makes.
fn observed_base(
    tx: &Transaction<'_>,
    c: &Args<'_>,
    (tenant, profile, p): (&str, i64, &Value),
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<std::result::Result<Value, Refusal>> {
    let base = match accepted_base(tx, tenant, p, &c.request.expected_base)? {
        Ok(base) => base,
        Err(reason) => return Ok(Err(reason)),
    };
    let expected = c.request.expected_draft.as_ref();
    Ok(observed_draft(tx, tenant, profile, &base, expected, budget)?.map(|_| base))
}

/// The draft key prefix for the request key, and the whole stored key: the
/// prefix and the digest of what the restore was asked to do, so a retry
/// finds its draft and a changed payload is told apart.
fn draft_key(c: &Args<'_>, profile: i64, actor: &str) -> (String, String) {
    let request = c.request;
    let (key, base, draft) = keyed_expectations(
        c.key,
        &request.expected_base,
        request.expected_draft.as_ref(),
    );
    let prefix = format!("{KEY_PREFIX}{key}:");
    let payload = json!({"profileId":profile,"actorId":actor,"snapshotId":c.snapshot,"expectedBase":base,"expectedDraft":draft});
    let key = format!("{prefix}{}", model::digest(&payload));
    (prefix, key)
}

/// The Accepted revision a snapshot's `payload_json` records, if any. Build
/// history reads it too, so History offers restore only where this route can.
pub(crate) fn snapshot_revision(payload: &str) -> Option<i64> {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|payload| payload["accepted_plan_revision_id"].as_i64())
}

/// The Accepted revision of Build `profile` the snapshot payload records, if
/// it records one. Node's own snapshots record Source layers and kit choices
/// only, which a draft cannot hold.
fn restorable_revision(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    payload: &str,
) -> Result<Option<i64>> {
    let Some(revision) = snapshot_revision(payload) else {
        return Ok(None);
    };
    Ok(one(
        tx,
        "SELECT id FROM plan_revisions WHERE tenant_id=? AND profile_id=? AND id=?",
        &[&tenant, &profile, &revision],
    )?
    .map(|_| revision))
}

/// Revision `revision`'s inputs, or `None` when they do not name Build
/// `profile`'s current Source layers: Accept refuses such a draft, so it could
/// never be published.
fn restored_inputs(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    revision: i64,
) -> Result<Option<Vec<Value>>> {
    let inputs = rows(
        tx,
        "SELECT i.source_id,i.source_layer,i.layer_order,i.tracking_kind,i.source_revision_id,i.manifest_digest,i.effective_naming_digest FROM plan_revisions r JOIN plan_revision_inputs i ON i.tenant_id=r.tenant_id AND i.input_set_id=r.input_set_id WHERE r.tenant_id=? AND r.id=? ORDER BY i.layer_order,i.source_id",
        &[&tenant, &revision],
    )?;
    let attached = rows(
        tx,
        "SELECT p.id AS source_id,l.layer_type||':'||p.name AS source_layer,l.layer_order FROM profile_layers l JOIN projects p ON p.id=l.project_id AND p.tenant_id=l.tenant_id WHERE l.tenant_id=? AND l.profile_id=? AND l.project_id IS NOT NULL ORDER BY l.layer_order,p.id",
        &[&tenant, &profile],
    )?;
    let identities: Vec<_> = inputs
        .iter()
        .map(|input| model::project(input, &["sourceId", "sourceLayer", "layerOrder"]))
        .collect();
    Ok((attached == identities).then_some(inputs))
}

/// Revision `revision`'s Parts as draft Parts. Each Part's predecessor is
/// the base Part holding its Required-unit tokens, or none. Refuses when the
/// base has no Required-unit set, or when a base Part's printed units
/// outnumber the quantity of the Part restored over it.
fn restored_parts(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    revision: i64,
    base: &Value,
) -> Result<std::result::Result<Vec<DraftPart>, RestoreRefusal>> {
    let Some((_, current)) =
        required_units::storage::base(tx, tenant, profile, base["baseRevisionId"].as_i64())?
    else {
        return Ok(Err(RestoreRefusal::RequiredUnitSetUnavailable));
    };
    let holder: HashMap<&str, i64> = current
        .iter()
        .flat_map(|part| part.units.iter().map(|unit| (unit.token.as_str(), part.id)))
        .collect();
    let mut tokens: HashMap<i64, Vec<String>> = HashMap::new();
    for row in rows(
        tx,
        "SELECT revision_part_id,required_unit_token FROM plan_revision_required_units WHERE tenant_id=? AND revision_id=?",
        &[&tenant, &revision],
    )? {
        tokens
            .entry(num(&row, "revisionPartId")?)
            .or_default()
            .push(text(&row, "requiredUnitToken")?.to_owned());
    }
    let mut stored = part_rows(
        tx,
        "SELECT * FROM plan_revision_parts WHERE tenant_id=? AND revision_id=? ORDER BY id",
        &[&tenant, &revision],
    )?;
    storage::normalize_parts(&mut stored)?;
    let mut claimed = HashMap::new();
    let mut parts = Vec::new();
    for stored in stored {
        let held: HashSet<i64> = tokens
            .get(&num(&stored, "id")?)
            .into_iter()
            .flatten()
            .filter_map(|token| holder.get(token.as_str()).copied())
            .collect();
        ensure!(
            held.len() <= 1,
            "Restored Part spans several Accepted Parts"
        );
        let predecessor = held.into_iter().next();
        let mut part = stored.projected(model::PART_FIELDS);
        part["baseRevisionPartId"] = json!(predecessor);
        if let Some(predecessor) = predecessor {
            ensure!(
                claimed
                    .insert(predecessor, num(&part, "quantityEffective")?)
                    .is_none(),
                "Accepted Part is the predecessor of several restored Parts"
            );
        }
        parts.push(part);
    }
    let mut printed = Vec::new();
    for part in &current {
        let completed: Vec<_> = part.units.iter().filter(|unit| unit.completed).collect();
        let kept = claimed.get(&part.id).copied().unwrap_or(0);
        if completed.len() as i64 > kept {
            printed.extend(completed.into_iter().map(|unit| unit.token.clone()));
        }
    }
    if !printed.is_empty() {
        printed.sort();
        return Ok(Err(RestoreRefusal::PrintedUnitsAffected {
            tokens: printed,
        }));
    }
    Ok(Ok(parts))
}
