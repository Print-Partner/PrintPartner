//! Node's `rebasePlanDraft`, with the `mergeRebasedPlanDraft` it calls.
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceState {
    Open,
    #[default]
    Abandoned,
}

/// The JSON body of Node's draft rebase route.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RebaseRequest {
    #[serde(default)]
    expected_source_state: SourceState,
    expected_source_lifecycle_version: Version,
    expected_source_snapshot_digest: Digest,
}

/// Inputs for rebasing a source draft onto a fresh preparation.
pub struct RebaseArgs<'a> {
    pub draft_id: PositiveId,
    pub idempotency_key: &'a str,
    pub request: RebaseRequest,
}

/// Node's `rebasePlanDraft` result, after the service's Plan check.
#[derive(Debug)]
pub enum RebaseOutcome {
    Rebased { draft: DraftDocument },
    Existing { draft: DraftDocument },
    ProfileNotFound,
    NotFound,
    SourceConflict { draft: DraftDocument },
    NotAbandoned { state: DraftState },
    AcceptedBaselineRequired,
    BaseUnchanged,
    BaseChanged,
    InputsChanged,
    IdempotencyConflict,
    MergeConflicts { conflicts: Vec<Conflict> },
    NoLayers,
    NoStls,
    WouldWipe,
}

/// One of Node's `RebaseConflict`s. Target IDs number the fresh draft's Parts
/// from 1, as Node's unsaved draft does.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Conflict {
    SourceIdentity {
        source_part_id: i64,
        source_layer: String,
    },
    TargetAmbiguous {
        source_part_id: i64,
        target_part_ids: Vec<usize>,
    },
    TargetCollision {
        source_part_ids: Vec<i64>,
        target_part_id: usize,
    },
    ConcurrentDecision {
        source_part_id: i64,
        target_part_id: usize,
        field: Field,
    },
}
impl Conflict {
    fn order(&self) -> (i64, &'static str) {
        match self {
            Self::SourceIdentity { source_part_id, .. } => (*source_part_id, "source_identity"),
            Self::TargetAmbiguous { source_part_id, .. } => (*source_part_id, "target_ambiguous"),
            Self::TargetCollision {
                source_part_ids, ..
            } => (source_part_ids[0], "target_collision"),
            Self::ConcurrentDecision { source_part_id, .. } => {
                (*source_part_id, "concurrent_decision")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    Included,
    QuantityOverride,
}
impl Field {
    fn key(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::QuantityOverride => "quantityOverride",
        }
    }
}

impl RebaseOutcome {
    pub fn into_json_body(self) -> Result<DraftOutcomeJsonBody> {
        let (kind, field, body) = match self {
            Self::Rebased { draft } => ("rebased", Some("draft"), draft.into_json_body().0),
            Self::Existing { draft } => ("existing", Some("draft"), draft.into_json_body().0),
            Self::SourceConflict { draft } => {
                ("source_conflict", Some("draft"), draft.into_json_body().0)
            }
            Self::NotAbandoned { state } => {
                ("not_abandoned", Some("state"), serde_json::to_vec(&state)?)
            }
            Self::MergeConflicts { conflicts } => (
                "merge_conflicts",
                Some("conflicts"),
                serde_json::to_vec(&conflicts)?,
            ),
            Self::ProfileNotFound => ("profile_not_found", None, Vec::new()),
            Self::NotFound => ("not_found", None, Vec::new()),
            Self::AcceptedBaselineRequired => ("accepted_baseline_required", None, Vec::new()),
            Self::BaseUnchanged => ("base_unchanged", None, Vec::new()),
            Self::BaseChanged => ("base_changed", None, Vec::new()),
            Self::InputsChanged => ("inputs_changed", None, Vec::new()),
            Self::IdempotencyConflict => ("idempotency_conflict", None, Vec::new()),
            Self::NoLayers => ("no_layers", None, Vec::new()),
            Self::NoStls => ("no_stls", None, Vec::new()),
            Self::WouldWipe => ("would_wipe", None, Vec::new()),
        };
        Ok(outcome_json_body(kind, field, &body))
    }
}

/// What the request names: the source draft as the caller last saw it, and
/// the lifecycle version it has once abandoned.
pub(super) struct Source<'a> {
    pub(super) id: i64,
    state: SourceState,
    version: i64,
    pub(super) generation: i64,
    pub(super) digest: &'a str,
}
impl Source<'_> {
    fn origin(&self) -> Value {
        json!({"kind":"rebase", "sourceDraftId":self.id, "sourceLifecycleVersion":self.generation, "sourceSnapshotDigest":self.digest})
    }
    /// Node's checks of the current source draft against the request.
    fn check(&self, current: &Draft) -> Result<Option<RebaseOutcome>> {
        let identity = DraftIdentity::from_header(&current.header)?;
        let expected = match self.state {
            SourceState::Open => DraftState::Open,
            SourceState::Abandoned => DraftState::Abandoned,
        };
        if identity.state != expected {
            return Ok(Some(RebaseOutcome::NotAbandoned {
                state: identity.state,
            }));
        }
        if i64::from(identity.lifecycle_version) != self.version
            || identity.snapshot_digest.as_str() != self.digest
        {
            return Ok(Some(RebaseOutcome::SourceConflict {
                draft: snapshot(current)?,
            }));
        }
        Ok(None)
    }
}

pub(super) struct Args<'a> {
    pub credential: &'a Credential,
    pub policy: auth::AuthPolicy,
    pub profile_id: PositiveId,
    pub draft_id: PositiveId,
    pub key: &'a str,
    pub request: &'a RebaseRequest,
    pub observations: &'a Observations,
    pub cancelled: &'a AtomicBool,
}

pub(super) fn rebase(connection: &mut Connection, args: Args<'_>) -> Result<RebaseOutcome> {
    let Args {
        credential,
        policy,
        profile_id,
        draft_id,
        key,
        request,
        observations,
        cancelled,
    } = args;
    ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
    let finished = |outcome| {
        ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
        Ok(outcome)
    };
    let version = i64::try_from(request.expected_source_lifecycle_version.get())?;
    let open = request.expected_source_state == SourceState::Open;
    ensure!(
        version <= 2_147_483_647 && !(open && version == 2_147_483_647),
        "Expected source Plan draft lifecycle version is invalid"
    );
    let source = Source {
        id: i64::try_from(draft_id.get())?,
        state: request.expected_source_state,
        version,
        generation: version + i64::from(open),
        digest: request.expected_source_snapshot_digest.as_str(),
    };
    let profile = i64::try_from(profile_id.get())?;
    let mut budget = observation::PreparationBudget::new(cancelled, observations.limits)?;
    let tx = connection.transaction()?;
    let (tenant, actor) =
        auth::authority::queries::observe_reconciliation_actor(&tx, credential, policy)?;
    let Some(profile_row) = one(
        &tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return finished(RebaseOutcome::ProfileNotFound);
    };
    if let Some(outcome) = stored(&tx, &tenant, &actor, profile, key, &source, &mut budget)? {
        return finished(outcome);
    }
    let Some(current) = load(&tx, &tenant, profile, source.id, &mut budget)? else {
        return finished(RebaseOutcome::NotFound);
    };
    if let Some(outcome) = source.check(&current)? {
        return finished(outcome);
    }
    if baseline_required(&tx, &tenant, &profile_row)? {
        return finished(RebaseOutcome::AcceptedBaselineRequired);
    }
    let base = json!({"baseRevisionId":profile_row["acceptedPlanRevisionId"], "basePlanVersion":profile_row["acceptedPlanVersion"]});
    let same_base = base["baseRevisionId"] == current.header["baseRevisionId"]
        && base["basePlanVersion"] == current.header["basePlanVersion"];
    let reads = observations.reads.as_ref();
    let inputs_current = |tx: &Transaction<'_>,
                          inputs: &[Value],
                          budget: &mut observation::PreparationBudget<'_>|
     -> Result<bool> {
        let captured = preparation::capture(tx, &tenant, profile, reads, budget)?;
        Ok(canonical_inputs(&captured.inputs) == canonical_inputs(inputs))
    };
    if same_base && inputs_current(&tx, &current.inputs, &mut budget)? {
        tx.commit()?;
        // Node's write transaction waits for the writer lock on its own.
        let admission = writer_admission::Deadline::at_dequeue();
        return transact(connection, admission, cancelled, |tx| {
            let checked = recheck(
                tx,
                (credential, policy),
                (&tenant, &actor),
                profile,
                key,
                &source,
                &mut budget,
            )?;
            let (current, profile_row) = match checked {
                Ok(checked) => checked,
                Err(outcome) => return Ok(outcome),
            };
            if profile_row["acceptedPlanRevisionId"] != current.header["baseRevisionId"]
                || profile_row["acceptedPlanVersion"] != current.header["basePlanVersion"]
            {
                return Ok(RebaseOutcome::BaseChanged);
            }
            Ok(if inputs_current(tx, &current.inputs, &mut budget)? {
                RebaseOutcome::BaseUnchanged
            } else {
                RebaseOutcome::InputsChanged
            })
        });
    }
    tx.commit()?;
    let options = preparation::PreparationOptions {
        apply_manifest: false,
        ..Default::default()
    };
    let prepared = preparation::prepare(
        connection,
        &tenant,
        profile,
        base.clone(),
        preparation::PreparationContext {
            options: &options,
            repos: &observations.repos,
        },
        reads,
        &mut budget,
    );
    let mut prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return match error.downcast_ref() {
                Some(preparation::Failure::NoLayers) => finished(RebaseOutcome::NoLayers),
                Some(preparation::Failure::NoStls) => finished(RebaseOutcome::NoStls),
                Some(preparation::Failure::WouldWipe) => finished(RebaseOutcome::WouldWipe),
                None => Err(error),
            };
        }
    };
    let tx = connection.transaction()?;
    let old = accepted_parts(
        &tx,
        &tenant,
        profile,
        &current.header["baseRevisionId"],
        &mut budget,
    )?;
    let now = accepted_parts(&tx, &tenant, profile, &base["baseRevisionId"], &mut budget)?;
    tx.commit()?;
    let conflicts = merge(&current, &old, &now, &mut prepared, &mut budget)?;
    if !conflicts.is_empty() {
        return finished(RebaseOutcome::MergeConflicts { conflicts });
    }
    let admission = writer_admission::Deadline::at_dequeue();
    transact(connection, admission, cancelled, |tx| {
        let checked = recheck(
            tx,
            (credential, policy),
            (&tenant, &actor),
            profile,
            key,
            &source,
            &mut budget,
        )?;
        let (current, profile_row) = match checked {
            Ok(checked) => checked,
            Err(outcome) => return Ok(outcome),
        };
        if profile_row["acceptedPlanRevisionId"] != prepared.base["baseRevisionId"]
            || profile_row["acceptedPlanVersion"] != prepared.base["basePlanVersion"]
        {
            return Ok(RebaseOutcome::BaseChanged);
        }
        let captured = preparation::capture(tx, &tenant, profile, reads, &mut budget)?;
        if captured.fingerprint != prepared.capture.fingerprint {
            return Ok(RebaseOutcome::InputsChanged);
        }
        if open
            && tx.execute(
                "UPDATE plan_drafts SET state='abandoned',lifecycle_version=? WHERE tenant_id=? AND profile_id=? AND id=? AND state='open' AND lifecycle_version=? AND snapshot_digest=?",
                params![source.generation, tenant, profile, source.id, source.version, source.digest],
            )? != 1
        {
            return Ok(RebaseOutcome::SourceConflict {
                draft: snapshot(&current)?,
            });
        }
        if open {
            crate::domain_events::draft_changed(tx, &tenant, profile, source.id)?;
        }
        let id = insert_snapshot(
            tx,
            &tenant,
            &actor,
            profile,
            key,
            prepared.rows(),
            Origin::Rebased(&source),
        )?;
        let rebased = load(tx, &tenant, profile, id, &mut budget)?
            .filter(|d| {
                d.header["snapshotDigest"] == prepared.digest.as_str()
                    && draft_origin(&d.header).ok() == Some(source.origin())
            })
            .ok_or_else(|| anyhow!("Rebased Plan draft could not be verified"))?;
        Ok(RebaseOutcome::Rebased {
            draft: snapshot(&rebased)?,
        })
    })
}

/// The checks Node repeats under the writer lock. On success, the source
/// draft and the Plan's row.
fn recheck(
    tx: &Transaction<'_>,
    (credential, policy): (&Credential, auth::AuthPolicy),
    (tenant, actor): (&str, &str),
    profile: i64,
    key: &str,
    source: &Source<'_>,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Result<(Draft, Value), RebaseOutcome>> {
    let principal = auth::authority::queries::reconciliation_actor_ref(tx, credential, policy)?;
    ensure!(
        principal == (tenant.to_owned(), actor.to_owned()),
        "Authentication principal changed"
    );
    if let Some(outcome) = stored(tx, tenant, actor, profile, key, source, budget)? {
        return Ok(Err(outcome));
    }
    let Some(current) = load(tx, tenant, profile, source.id, budget)? else {
        return Ok(Err(RebaseOutcome::NotFound));
    };
    if let Some(outcome) = source.check(&current)? {
        return Ok(Err(outcome));
    }
    let Some(profile_row) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(Err(RebaseOutcome::NotFound));
    };
    if baseline_required(tx, tenant, &profile_row)? {
        return Ok(Err(RebaseOutcome::AcceptedBaselineRequired));
    }
    Ok(Ok((current, profile_row)))
}

/// Node's `storedRebasePlanDraft`: the draft this key or this source
/// generation already created.
fn stored(
    tx: &Transaction<'_>,
    tenant: &str,
    actor: &str,
    profile: i64,
    key: &str,
    source: &Source<'_>,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Option<RebaseOutcome>> {
    if let Some(row) = one(
        tx,
        "SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND created_by=? AND idempotency_key=?",
        &[&tenant, &profile, &actor, &key],
    )? {
        let winner = load(tx, tenant, profile, num(&row, "id")?, budget)?
            .ok_or_else(|| anyhow!("Saved Plan draft is missing"))?;
        return Ok(Some(if draft_origin(&winner.header)? == source.origin() {
            RebaseOutcome::Existing {
                draft: snapshot(&winner)?,
            }
        } else {
            RebaseOutcome::IdempotencyConflict
        }));
    }
    let Some(row) = one(
        tx,
        "SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND rebased_from_draft_id=? AND rebased_from_lifecycle_version=?",
        &[&tenant, &profile, &source.id, &source.generation],
    )?
    else {
        return Ok(None);
    };
    let successor = load(tx, tenant, profile, num(&row, "id")?, budget)?
        .ok_or_else(|| anyhow!("Rebased Plan draft is missing"))?;
    if successor.header["rebasedFromSnapshotDigest"] == source.digest {
        return Ok(Some(RebaseOutcome::Existing {
            draft: snapshot(&successor)?,
        }));
    }
    Ok(Some(match load(tx, tenant, profile, source.id, budget)? {
        Some(current) => RebaseOutcome::SourceConflict {
            draft: snapshot(&current)?,
        },
        None => RebaseOutcome::NotFound,
    }))
}

/// `draft`, charging what it reads to the budget and checking cancellation
/// between rows, as recompute's carry does. Another Plan's draft is not
/// charged.
fn load(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    id: i64,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Option<Draft>> {
    let scoped = "SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND id=?";
    if one(tx, scoped, &[&tenant, &profile, &id])?.is_none() {
        return Ok(None);
    }
    charge_open_draft(tx, tenant, id, budget)?;
    storage::draft_checked(tx, tenant, profile, id, &|| Ok(budget.check()?))
}

/// Node's `digestPlanInputs` comparison: the input fields, stably sorted by
/// Source.
fn canonical_inputs(inputs: &[Value]) -> Vec<Value> {
    let mut inputs: Vec<_> = inputs
        .iter()
        .map(|input| model::project(input, model::INPUT_FIELDS))
        .collect();
    inputs.sort_by_key(|input| input["sourceId"].as_i64());
    inputs
}

/// An accepted revision Part, as far as rebase reads it.
struct Accepted {
    id: i64,
    projection: Option<i64>,
    included: Value,
    quantity_override: Value,
}
impl Accepted {
    fn decided(&self, field: Field) -> &Value {
        match field {
            Field::Included => &self.included,
            Field::QuantityOverride => &self.quantity_override,
        }
    }
}

/// Node's `rebaseAcceptedParts`.
fn accepted_parts(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    revision: &Value,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Vec<Accepted>> {
    let Some(revision) = revision.as_i64() else {
        return Ok(Vec::new());
    };
    ensure!(
        one(
            tx,
            "SELECT id FROM plan_revisions WHERE tenant_id=? AND profile_id=? AND id=?",
            &[&tenant, &profile, &revision],
        )?
        .is_some(),
        "Plan draft base revision is missing"
    );
    let mut statement = tx.prepare(
        "SELECT id,projection_part_id,included,quantity_override FROM plan_revision_parts WHERE tenant_id=? AND revision_id=? ORDER BY id",
    )?;
    let mut query = statement.query(params![tenant, revision])?;
    let mut parts = Vec::new();
    while let Some(row) = query.next()? {
        budget.visit(0)?;
        parts.push(Accepted {
            id: row.get(0)?,
            projection: row.get(1)?,
            included: json!(row.get::<_, i64>(2)? != 0),
            quantity_override: json!(row.get::<_, Option<i64>>(3)?),
        });
    }
    Ok(parts)
}

/// Where a draft's Parts come from: each layer's Source, or `None` when two
/// inputs share the layer, and the Parts by Source and key or artifact.
struct Evidence {
    sources: HashMap<String, Option<i64>>,
    tracked: HashSet<i64>,
    by_key: HashMap<(i64, String), Vec<usize>>,
    by_artifact: HashMap<(i64, String), Vec<usize>>,
}
impl Evidence {
    fn new(inputs: &[Value], parts: &[DraftPart]) -> Result<Self> {
        let mut sources = HashMap::new();
        let mut tracked = HashSet::new();
        for input in inputs {
            let source = num(input, "sourceId")?;
            sources
                .entry(text(input, "sourceLayer")?.to_owned())
                .and_modify(|shared| *shared = None)
                .or_insert(Some(source));
            if input["trackingKind"] == "revision" {
                tracked.insert(source);
            }
        }
        let mut evidence = Self {
            sources,
            tracked,
            by_key: HashMap::new(),
            by_artifact: HashMap::new(),
        };
        for (index, part) in parts.iter().enumerate() {
            let Some(source) = evidence.source(part)? else {
                continue;
            };
            let key = (source, text(part, "partKey")?.to_owned());
            evidence.by_key.entry(key).or_default().push(index);
            if let Some(artifact) = part["artifactDigest"].as_str() {
                let key = (source, artifact.to_owned());
                evidence.by_artifact.entry(key).or_default().push(index);
            }
        }
        Ok(evidence)
    }
    fn source(&self, part: &DraftPart) -> Result<Option<i64>> {
        Ok(self
            .sources
            .get(text(part, "sourceLayer")?)
            .copied()
            .flatten())
    }
    fn keyed(&self, source: i64, key: &str) -> &[usize] {
        self.by_key
            .get(&(source, key.to_owned()))
            .map_or(&[], Vec::as_slice)
    }
    fn artifacts(&self, source: i64, artifact: &str) -> &[usize] {
        self.by_artifact
            .get(&(source, artifact.to_owned()))
            .map_or(&[], Vec::as_slice)
    }
}

/// Node's `mergeRebasedPlanDraft`: replays the source draft's inclusion and
/// quantity decisions onto the fresh preparation and refreshes its digest.
/// Returns the conflicts instead when any decision has no single target.
fn merge(
    source: &Draft,
    old: &[Accepted],
    now: &[Accepted],
    fresh: &mut preparation::PreparedDraftSnapshot,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<Vec<Conflict>> {
    let old_by_id: HashMap<_, _> = old.iter().map(|part| (part.id, part)).collect();
    let baseline = |part: &DraftPart, field: Field| -> Result<Value> {
        Ok(match part["baseRevisionPartId"].as_i64() {
            None if field == Field::Included => json!(true),
            None => Value::Null,
            Some(id) => old_by_id
                .get(&id)
                .ok_or_else(|| anyhow!("Plan draft rebase predecessor is missing"))?
                .decided(field)
                .clone(),
        })
    };
    let mut order: Vec<_> = (0..source.parts.len()).collect();
    order.sort_by_key(|index| source.parts[*index]["id"].as_i64());
    let mut decisions = Vec::new();
    for &index in &order {
        budget.check()?;
        let part = &source.parts[index];
        for field in [Field::Included, Field::QuantityOverride] {
            let new_included = field == Field::Included && part["baseRevisionPartId"].is_null();
            if new_included || part[field.key()] != baseline(part, field)? {
                decisions.push((index, field));
            }
        }
    }
    if decisions.is_empty() {
        return Ok(Vec::new());
    }
    let source_evidence = Evidence::new(&source.inputs, &source.parts)?;
    let fresh_evidence = Evidence::new(&fresh.capture.inputs, &fresh.parts)?;
    let projections = |parts: &[Accepted]| {
        let mut counts = HashMap::<i64, usize>::new();
        for projection in parts.iter().filter_map(|part| part.projection) {
            *counts.entry(projection).or_default() += 1;
        }
        counts
    };
    let (old_projections, now_projections) = (projections(old), projections(now));
    let now_by_id: HashMap<_, _> = now.iter().map(|part| (part.id, part)).collect();
    let mut fresh_by_projection = HashMap::<i64, Vec<usize>>::new();
    for (index, part) in fresh.parts.iter().enumerate() {
        let projection = part["baseRevisionPartId"]
            .as_i64()
            .and_then(|id| now_by_id.get(&id)?.projection);
        if let Some(projection) = projection {
            fresh_by_projection
                .entry(projection)
                .or_default()
                .push(index);
        }
    }
    let mut decided: Vec<_> = decisions.iter().map(|(index, _)| *index).collect();
    decided.dedup();
    let mut targets = Vec::<(usize, usize)>::new();
    let mut conflicts = Vec::new();
    for index in decided {
        budget.check()?;
        let part = &source.parts[index];
        let source_part_id = num(part, "id")?;
        let Some(source_id) = source_evidence.source(part)? else {
            conflicts.push(Conflict::SourceIdentity {
                source_part_id,
                source_layer: text(part, "sourceLayer")?.to_owned(),
            });
            continue;
        };
        let projection = part["baseRevisionPartId"]
            .as_i64()
            .and_then(|id| old_by_id.get(&id)?.projection)
            .filter(|projection| {
                old_projections.get(projection) == Some(&1)
                    && now_projections.get(projection) == Some(&1)
            });
        let mut candidates = projection
            .and_then(|projection| fresh_by_projection.get(&projection))
            .cloned()
            .unwrap_or_default();
        let mut ambiguous = false;
        if candidates.is_empty() {
            let key = text(part, "partKey")?;
            let found = fresh_evidence.keyed(source_id, key);
            ambiguous = !found.is_empty()
                && !(source_evidence.keyed(source_id, key).len() == 1 && found.len() == 1);
            candidates = found.to_vec();
        }
        if let Some(artifact) = part["artifactDigest"].as_str()
            && !ambiguous
            && candidates.is_empty()
        {
            let found = if fresh_evidence.tracked.contains(&source_id) {
                fresh_evidence.artifacts(source_id, artifact)
            } else {
                &[]
            };
            ambiguous = !found.is_empty()
                && !(source_evidence.tracked.contains(&source_id)
                    && source_evidence.artifacts(source_id, artifact).len() == 1
                    && found.len() == 1);
            candidates = found.to_vec();
        }
        match candidates.as_slice() {
            [] => {}
            [target] if !ambiguous => targets.push((index, *target)),
            _ => {
                budget.comparison(candidates.len())?;
                conflicts.push(Conflict::TargetAmbiguous {
                    source_part_id,
                    target_part_ids: candidates.iter().map(|target| target + 1).collect(),
                });
            }
        }
    }
    let mut claims = std::collections::BTreeMap::<usize, Vec<i64>>::new();
    for (index, target) in &targets {
        claims
            .entry(*target)
            .or_default()
            .push(num(&source.parts[*index], "id")?);
    }
    for (target, source_part_ids) in claims {
        if source_part_ids.len() > 1 {
            conflicts.push(Conflict::TargetCollision {
                source_part_ids,
                target_part_id: target + 1,
            });
        }
    }
    if conflicts.is_empty() {
        let targets: HashMap<_, _> = targets.into_iter().collect();
        let original = fresh.parts.clone();
        for (index, field) in decisions {
            budget.check()?;
            // A decision without a target was on a Part the Source dropped.
            let Some(&target) = targets.get(&index) else {
                continue;
            };
            let part = &source.parts[index];
            let value = &part[field.key()];
            let current = &original[target][field.key()];
            let independent = field == Field::Included
                && part["baseRevisionPartId"].is_null()
                && original[target]["baseRevisionPartId"].is_null();
            if !independent && *current != baseline(part, field)? && current != value {
                conflicts.push(Conflict::ConcurrentDecision {
                    source_part_id: num(part, "id")?,
                    target_part_id: target + 1,
                    field,
                });
                continue;
            }
            let next = &mut fresh.parts[target];
            next[field.key()] = value.clone();
            if field == Field::QuantityOverride {
                next["quantityEffective"] = if value.is_null() {
                    next["quantityInferred"].clone()
                } else {
                    value.clone()
                };
            }
        }
    }
    if conflicts.is_empty() {
        fresh.digest = model::planning(&fresh.base, &fresh.capture.inputs, &fresh.parts)?;
    }
    conflicts.sort_by(|left, right| left.order().cmp(&right.order()));
    Ok(conflicts)
}
