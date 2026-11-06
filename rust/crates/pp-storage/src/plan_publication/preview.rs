//! What accepting an open draft would change, read through Accept's own gate
//! ([`check_draft`] and [`check_selection`]) without writing.

use super::*;
use crate::required_units::DraftBase;
use crate::working_drafts::diff::{self, DraftDiffModel};
use serde::Serialize;

/// The preview's snapshot note when Accept will save one. Accept owns saving it.
const SNAPSHOT_DETAIL: &str =
    "Print Partner saves a snapshot of the accepted Plan before it accepts this draft.";
/// The note when `snapshot::replaced_layers` finds no Accepted Plan that a
/// restore could bring back.
const NO_SNAPSHOT_DETAIL: &str = "Print Partner has no accepted Plan it can restore from a snapshot, so it saves none before it accepts this draft.";

#[derive(Debug)]
pub enum PreviewOutcome {
    Ready(Box<AcceptPreview>),
    /// The refusal Accept itself would return for this draft.
    Refused(Outcome),
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptPreview {
    /// The draft equals the accepted Plan.
    NoChange {
        revision: PreviewRevision,
        snapshot: SnapshotNote,
    },
    Changed {
        revision: PreviewRevision,
        changes: Box<PreviewChanges>,
        snapshot: SnapshotNote,
    },
}

/// The draft revision the preview read. Accept refuses it once stale.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviewRevision {
    pub draft_id: i64,
    pub expected_snapshot_digest: String,
    pub expected_lifecycle_version: i64,
    pub expected_base: DraftBase,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SnapshotNote {
    pub saved_before_accept: bool,
    pub detail: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviewChanges {
    /// Changed Part fields, old against new.
    pub fields: Vec<FieldChange>,
    pub parts_added: Vec<PartRef>,
    pub parts_removed: Vec<PartRef>,
    /// Changed Source input fields, old against new. An added or removed
    /// input is one `source_layer` change with a `null` side.
    pub inputs: Vec<InputChange>,
    pub required_units: UnitChanges,
    pub plates: PlateChanges,
    pub stock: StockImpact,
    /// Build header fields the draft changes, old against new. Accept copies
    /// them to the Build.
    pub header: Vec<HeaderChange>,
    /// Other unarchived Plans with the order number this Plan has after
    /// Accept.
    pub duplicate_orders: Vec<DuplicateOrder>,
    /// Active checkoff links or queued sends Accept would remap.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<ProductionCheck>")
    )]
    pub production: Option<ProductionCheck>,
    /// Running operations that make Accept refuse until they finish.
    pub blocked_by: Vec<BlockingOperation>,
}

/// One Build header field (`name`, `order_number`, `customer` or
/// `due_date`) the draft changes. `null` is an empty field.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HeaderChange {
    pub field: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub old: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub new: Option<String>,
}

/// A Part or input field value, tagged so a client can tell `"3"` from `3`.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FieldValue {
    Text(String),
    Integer(i64),
    Flag(bool),
}

impl FieldValue {
    fn from_json(value: &Value) -> Result<Option<Self>> {
        Ok(Some(match value {
            Value::Null => return Ok(None),
            Value::String(text) => Self::Text(text.clone()),
            Value::Bool(flag) => Self::Flag(*flag),
            Value::Number(number) => Self::Integer(
                number
                    .as_i64()
                    .ok_or_else(|| anyhow!("Invalid preview integer"))?,
            ),
            _ => return Err(anyhow!("Invalid preview field")),
        }))
    }
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    pub draft_part_id: i64,
    pub filename: String,
    pub field: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<FieldValue>")
    )]
    pub old: Option<FieldValue>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<FieldValue>")
    )]
    pub new: Option<FieldValue>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PartRef {
    /// The draft Part ID when added; the accepted revision Part ID when removed.
    pub part_id: i64,
    pub filename: String,
    pub quantity_effective: i64,
    pub included: bool,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputChange {
    pub source_id: i64,
    pub field: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<FieldValue>")
    )]
    pub old: Option<FieldValue>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<FieldValue>")
    )]
    pub new: Option<FieldValue>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnitChanges {
    pub added: Vec<UnitRef>,
    pub removed: Vec<UnitRef>,
    /// Printed units that Accept removes or whose Part changes.
    pub printed_affected: Vec<UnitRef>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnitRef {
    /// `null` for a unit Accept would create.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub token: Option<String>,
    pub filename: String,
    pub unit_index: i64,
    pub printed: bool,
    pub assembled: bool,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlateChanges {
    /// Accept clears the accepted Plate layout; these Plates go.
    pub cleared: Vec<PlateRef>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlateRef {
    pub plate_id: String,
    pub ordinal: i64,
    pub printer_name: String,
    pub unit_count: i64,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StockImpact {
    /// Spoolman is off for this Plan.
    Off,
    /// Spools whose unprinted unit count changes.
    On { spools: Vec<SpoolImpact> },
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpoolImpact {
    pub spool_id: String,
    pub unprinted_before: i64,
    pub unprinted_after: i64,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DuplicateOrder {
    pub profile_id: i64,
    pub name: String,
    pub order_number: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProductionCheck {
    pub checkoff_link_count: i64,
    pub send_queue_item_count: i64,
    /// Links Accept could not remap; empty when `remap_checkoff_links` works.
    pub unmappable: Vec<Unmappable>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unmappable {
    pub link_id: String,
    pub filename: String,
    pub reason: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockingOperation {
    pub operation_id: String,
    pub kind: String,
    pub state: String,
}

/// Runs Accept's gate for the draft's current revision and reports what
/// Accept would change. Reads only.
pub(crate) fn preview(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft_id: i64,
) -> Result<PreviewOutcome> {
    // Accept's idempotency-key lookup comes first there; a preview has no key.
    if let Some(applied) = already_applied(tx, tenant, profile, draft_id)? {
        return Ok(PreviewOutcome::Refused(applied));
    }
    let h = match open_draft(tx, tenant, profile, draft_id)? {
        Ok((_, h)) => h,
        Err(refused) => return Ok(PreviewOutcome::Refused(refused)),
    };
    let revision = PreviewRevision {
        draft_id,
        expected_snapshot_digest: text(&h, "snapshotDigest")?.into(),
        expected_lifecycle_version: num(&h, "lifecycleVersion")?,
        expected_base: DraftBase {
            revision_id: h["baseRevisionId"].as_i64(),
            plan_version: num(&h, "basePlanVersion")?,
        },
    };
    let request = serde_json::from_value::<ApplyRequest>(json!({
        "expected_snapshot_digest": revision.expected_snapshot_digest,
        "expected_lifecycle_version": revision.expected_lifecycle_version,
        "expected_base": {
            "revision_id": revision.expected_base.revision_id,
            "plan_version": revision.expected_base.plan_version,
        },
    }));
    // No Accept request can name this revision, so every Accept gets
    // `draft_changed`.
    let Ok(request) = request else {
        return Ok(PreviewOutcome::Refused(Outcome::DraftChanged));
    };
    let mut gate = match check_draft(tx, tenant, profile, draft_id, &request)? {
        Ok(gate) => gate,
        Err(refused) => return Ok(PreviewOutcome::Refused(refused)),
    };
    let draft_header = match super::draft_header(tx, tenant, profile, draft_id)? {
        Ok(header) => header,
        Err(refused) => return Ok(PreviewOutcome::Refused(refused)),
    };
    let snapshot = snapshot_note(
        super::snapshot::replaced_layers(tx, tenant, profile, revision.expected_base.revision_id)?
            .is_some(),
    );
    // Diff the Parts Accept would publish: filament comes from the live Part.
    inherit_live_filament(&mut gate.draft.parts, &gate.predecessors);
    let diff = diff::read(tx, tenant, &gate.profile_row, &gate.draft)?.0;
    let checked = match check_selection(tx, tenant, profile, gate)? {
        Ok(checked) => checked,
        Err(refused) => return Ok(PreviewOutcome::Refused(refused)),
    };
    let header = header_changes(tx, tenant, profile, draft_header.as_ref())?;
    if diff.is_empty() && header.is_empty() {
        // `no_change` only when Accept, sent without `remap_checkoff_links`,
        // would pass too. A changed preview reports these as checks instead.
        if let Err(refused) = checked.refusals(tx, tenant, profile, false)? {
            return Ok(PreviewOutcome::Refused(refused));
        }
        return Ok(PreviewOutcome::Ready(Box::new(AcceptPreview::NoChange {
            revision,
            snapshot,
        })));
    }
    let (fields, parts_added, parts_removed, inputs) = changes(&diff)?;
    let production = checked
        .production(profile)?
        .map(|(links, queue, unmappable)| ProductionCheck {
            checkoff_link_count: links as i64,
            send_queue_item_count: queue as i64,
            unmappable: unmappable
                .into_iter()
                .map(|link| Unmappable {
                    link_id: link.link_id,
                    filename: link.filename,
                    reason: link.reason,
                })
                .collect(),
        });
    Ok(PreviewOutcome::Ready(Box::new(AcceptPreview::Changed {
        revision,
        changes: Box::new(PreviewChanges {
            fields,
            parts_added,
            parts_removed,
            inputs,
            required_units: units(&checked, &diff)?,
            plates: plates(tx, tenant, profile)?,
            stock: stock(tx, tenant, profile, &checked)?,
            header,
            duplicate_orders: duplicate_orders(
                tx,
                tenant,
                profile,
                &checked,
                draft_header.as_ref(),
            )?,
            production,
            blocked_by: blocked_by(tx, tenant, profile)?,
        }),
        snapshot,
    })))
}

fn field(part: &DraftPart, name: &str) -> Result<Option<FieldValue>> {
    let object: Value = serde_json::from_slice(&part.json_fields(&[name], false))?;
    FieldValue::from_json(&object[name])
}

fn part_ref(part: &DraftPart) -> Result<PartRef> {
    Ok(PartRef {
        part_id: num(part, "id")?,
        filename: text(part, "filename")?.into(),
        quantity_effective: num(part, "quantityEffective")?,
        included: part["included"] == true,
    })
}

type Changes = (
    Vec<FieldChange>,
    Vec<PartRef>,
    Vec<PartRef>,
    Vec<InputChange>,
);

fn changes(diff: &DraftDiffModel) -> Result<Changes> {
    let mut fields = Vec::new();
    for part in &diff.changed {
        for name in &part.fields {
            fields.push(FieldChange {
                draft_part_id: num(&part.after, "id")?,
                filename: text(&part.after, "filename")?.into(),
                field: model::snake(name),
                old: field(&part.before, name)?,
                new: field(&part.after, name)?,
            });
        }
    }
    let inputs = &diff.ordinary["inputs"];
    let mut changed_inputs = Vec::new();
    for (key, added) in [("added", true), ("removed", false)] {
        for row in inputs[key].as_array().into_iter().flatten() {
            let layer = FieldValue::from_json(&row["sourceLayer"])?;
            changed_inputs.push(InputChange {
                source_id: num(row, "sourceId")?,
                field: "source_layer".into(),
                old: if added { None } else { layer.clone() },
                new: if added { layer } else { None },
            });
        }
    }
    for row in inputs["changed"].as_array().into_iter().flatten() {
        let (before, after) = (&row["before"], &row["after"]);
        for name in ru::model::INPUT_FIELDS {
            if before[*name] != after[*name] {
                changed_inputs.push(InputChange {
                    source_id: num(after, "sourceId")?,
                    field: model::snake(name),
                    old: FieldValue::from_json(&before[*name])?,
                    new: FieldValue::from_json(&after[*name])?,
                });
            }
        }
    }
    let added = diff.added.iter().map(part_ref).collect::<Result<_>>()?;
    let removed = diff.removed.iter().map(part_ref).collect::<Result<_>>()?;
    Ok((fields, added, removed, changed_inputs))
}

fn units(checked: &Checked, diff: &DraftDiffModel) -> Result<UnitChanges> {
    let prepared = &checked.prepared;
    let filename = |id: i64, parts: &[DraftPart]| -> Result<String> {
        let part = parts
            .iter()
            .find(|part| part["id"] == id)
            .ok_or_else(|| anyhow!("Missing preview Part"))?;
        Ok(text(part, "filename")?.into())
    };
    let changed: HashSet<i64> = diff
        .changed
        .iter()
        .filter_map(|part| part.before["id"].as_i64())
        .collect();
    let mut kept = HashSet::new();
    let mut result = UnitChanges {
        added: Vec::new(),
        removed: Vec::new(),
        printed_affected: Vec::new(),
    };
    for assignment in &prepared.assignments {
        match assignment {
            Assignment::Create { part, index } => result.added.push(UnitRef {
                token: None,
                filename: filename(*part, &prepared.parts)?,
                unit_index: *index,
                printed: false,
                assembled: false,
            }),
            Assignment::Reuse { token, .. } => {
                kept.insert(token.as_str());
            }
        }
    }
    for part in &checked.base_parts {
        for unit in &part.units {
            let reference = UnitRef {
                token: Some(unit.token.clone()),
                filename: filename(part.id, &checked.predecessors)?,
                unit_index: unit.prior_index,
                printed: unit.completed,
                assembled: unit.assembled,
            };
            let removed = !kept.contains(unit.token.as_str());
            if unit.completed && (removed || changed.contains(&part.id)) {
                result.printed_affected.push(reference.clone());
            }
            if removed {
                result.removed.push(reference);
            }
        }
    }
    Ok(result)
}

fn plates(tx: &Transaction<'_>, tenant: &str, profile: i64) -> Result<PlateChanges> {
    let cleared = rows(
        tx,
        "SELECT p.plate_id,p.ordinal,p.printer_name,(SELECT COUNT(*) FROM accepted_plate_units u WHERE u.tenant_id=p.tenant_id AND u.revision_id=p.revision_id AND u.plate_id=p.plate_id) AS unit_count FROM accepted_plate_heads h JOIN accepted_plates p ON p.tenant_id=h.tenant_id AND p.revision_id=h.current_revision_id WHERE h.tenant_id=? AND h.profile_id=? ORDER BY p.ordinal",
        &[&tenant, &profile],
    )?
    .iter()
    .map(|row| {
        Ok(PlateRef {
            plate_id: text(row, "plateId")?.into(),
            ordinal: num(row, "ordinal")?,
            printer_name: text(row, "printerName")?.into(),
            unit_count: num(row, "unitCount")?,
        })
    })
    .collect::<Result<_>>()?;
    Ok(PlateChanges { cleared })
}

fn stock(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    checked: &Checked,
) -> Result<StockImpact> {
    use pp_domain::features::Feature;
    let plan = pp_domain::PositiveId::new(u64::try_from(profile)?).map_err(anyhow::Error::msg)?;
    let view = crate::features::resolve_for_tenant(tx, tenant, Some(plan))?;
    if !view.effective().enabled(Feature::Spoolman) {
        return Ok(StockImpact::Off);
    }
    // Stored as text; Node writes whatever Spoolman ID the user picked.
    let spool = |part: &DraftPart| match &part["spoolmanSpoolId"] {
        Value::String(id) if part["included"] == true && !id.is_empty() => Some(id.clone()),
        Value::Number(id) if part["included"] == true => Some(id.to_string()),
        _ => None,
    };
    let mut counts: std::collections::BTreeMap<String, (i64, i64)> = Default::default();
    for part in &checked.base_parts {
        let accepted = checked.predecessors.iter().find(|row| row["id"] == part.id);
        if let Some(id) = accepted.and_then(spool) {
            let unprinted = part.units.iter().filter(|unit| !unit.completed).count();
            counts.entry(id).or_default().0 += unprinted as i64;
        }
    }
    let prepared = &checked.prepared;
    for (assignment, (completed, _)) in prepared.assignments.iter().zip(&prepared.progress) {
        let (id, _) = assignment.slot();
        let part = prepared.parts.iter().find(|part| part["id"] == id);
        if let Some(id) = part.and_then(spool).filter(|_| !completed) {
            counts.entry(id).or_default().1 += 1;
        }
    }
    let spools = counts
        .into_iter()
        .filter(|(_, (before, after))| before != after)
        .map(
            |(spool_id, (unprinted_before, unprinted_after))| SpoolImpact {
                spool_id,
                unprinted_before,
                unprinted_after,
            },
        )
        .collect();
    Ok(StockImpact::On { spools })
}

/// The header fields the draft's header row changes against Build
/// `profile`'s current header. A draft with no header row changes none.
fn header_changes(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft: Option<&crate::working_drafts::header::DraftHeaderState>,
) -> Result<Vec<HeaderChange>> {
    let Some(draft) = draft else {
        return Ok(Vec::new());
    };
    let build = crate::working_drafts::header::build_header(tx, tenant, profile)?;
    let new = draft.header.clone();
    Ok([
        ("name", Some(build.name), Some(new.name)),
        ("order_number", build.order_number, new.order_number),
        ("customer", build.customer, new.customer),
        ("due_date", build.due_date, new.due_date),
    ]
    .into_iter()
    .filter(|(_, old, new)| old != new)
    .map(|(field, old, new)| HeaderChange {
        field: field.into(),
        old,
        new,
    })
    .collect())
}

fn snapshot_note(saved_before_accept: bool) -> SnapshotNote {
    SnapshotNote {
        saved_before_accept,
        detail: if saved_before_accept {
            SNAPSHOT_DETAIL
        } else {
            NO_SNAPSHOT_DETAIL
        }
        .into(),
    }
}

fn blocked_by(tx: &Transaction<'_>, tenant: &str, profile: i64) -> Result<Vec<BlockingOperation>> {
    Ok(crate::jobs::publication_conflicts(tx, tenant, profile)?
        .into_iter()
        .map(|operation| BlockingOperation {
            operation_id: operation.operation_id,
            kind: operation.kind,
            state: operation.state,
        })
        .collect())
}

/// Other unarchived Plans with the order number Build `profile` has after
/// Accept: the draft header's when it has one, else the current one.
fn duplicate_orders(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    checked: &Checked,
    draft: Option<&crate::working_drafts::header::DraftHeaderState>,
) -> Result<Vec<DuplicateOrder>> {
    let order = draft.map_or_else(
        || checked.profile_row["orderNumber"].as_str(),
        |state| state.header.order_number.as_deref(),
    );
    let Some(order) = order.filter(|order| !order.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    rows(
        tx,
        "SELECT id,name,order_number FROM build_profiles WHERE tenant_id=? AND id<>? AND archived_at IS NULL AND order_number=? ORDER BY id",
        &[&tenant, &profile, &order],
    )?
    .iter()
    .map(|row| {
        Ok(DuplicateOrder {
            profile_id: num(row, "id")?,
            name: text(row, "name")?.into(),
            order_number: text(row, "orderNumber")?.into(),
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture's accepted revision marks its excluded Part `base`; its open
    /// draft marks it `excluded`. Marking the revision and its projection
    /// `excluded` too makes the draft equal to the accepted Plan. The
    /// immutability triggers are dropped on this disposable copy only. The
    /// pp-api preview tests seed the same revision digest.
    const ALIGNED_REVISION_DIGEST: &str =
        "19ccb8c4f3f9e8d9c4da6083b64bd8fe25a2bfce6545a94782da40b99ca82b63";
    /// The aligned revision with accepted Part 1 on Spoolman spool 7. Seeded by
    /// the pp-api test where only the live Part's spool differs from the draft.
    const LIVE_SPOOL_REVISION_DIGEST: &str =
        "44de35eded4fab323f4df212ba4634a51912d1af49829c5672d86ebd7e552f7c";

    /// A disposable copy of the fixture after `sql`. The immutability and
    /// invalidation triggers are dropped on the copy first.
    fn fixture_after(sql: &str) -> (tempfile::TempDir, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("print-partner.db");
        std::fs::write(
            &path,
            include_bytes!("../../tests/fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "DROP TRIGGER trg_plan_revisions_immutable_update;
                 DROP TRIGGER trg_plan_revision_parts_immutable_update;
                 DROP TRIGGER trg_parts_invalidate_accepted_revision_update;
                 {sql}"
            ))
            .unwrap();
        connection
            .execute_batch(include_str!("../working_drafts/header_schema.sql"))
            .unwrap();
        (directory, connection)
    }

    fn revision_1_digest(tx: &Transaction<'_>) -> String {
        let mut parts = part_rows(
            tx,
            "SELECT * FROM plan_revision_parts WHERE tenant_id=? AND revision_id=? ORDER BY id",
            &[&"default", &1],
        )
        .unwrap();
        normalize_parts(&mut parts).unwrap();
        model::revision_digest(&parts).unwrap()
    }

    fn revision_digest_after(sql: &str) -> String {
        let (_directory, mut connection) = fixture_after(sql);
        revision_1_digest(&connection.transaction().unwrap())
    }

    #[test]
    fn accept_preview_test_revision_digests_are_pinned() {
        let aligned = "UPDATE plan_revision_parts SET status='excluded' WHERE id=2;";
        assert_eq!(revision_digest_after(aligned), ALIGNED_REVISION_DIGEST);
        let spool =
            format!("{aligned} UPDATE plan_revision_parts SET spoolman_spool_id='7' WHERE id=1;");
        assert_eq!(revision_digest_after(&spool), LIVE_SPOOL_REVISION_DIGEST);
    }

    /// Accept and the preview give a Part with a base its live filament; a new
    /// Part keeps its own.
    #[test]
    fn accept_preview_parts_take_live_filament_only_with_a_base() {
        let part = |value: Value| DraftPart::new(value, Default::default());
        let live = [part(
            json!({"id":1,"filamentColorId":null,"filamentCustomHex":"#112233","spoolmanSpoolId":"7"}),
        )];
        let mut parts = [
            part(
                json!({"id":4,"baseRevisionPartId":1,"filamentColorId":5,"filamentCustomHex":null,"spoolmanSpoolId":"9"}),
            ),
            part(
                json!({"id":5,"baseRevisionPartId":null,"filamentColorId":6,"filamentCustomHex":null,"spoolmanSpoolId":"8"}),
            ),
        ];
        inherit_live_filament(&mut parts, &live);
        let filament = |part: &DraftPart| LIVE_FILAMENT_FIELDS.map(|key| part[key].clone());
        assert_eq!(filament(&parts[0]), filament(&live[0]));
        assert_eq!(filament(&parts[1]), [json!(6), Value::Null, json!("8")]);
    }

    /// With no Required-unit selection, Accept refuses even an unchanged
    /// draft, so the preview does too instead of answering `no_change`.
    #[test]
    fn accept_preview_of_an_unchanged_draft_without_a_selection_is_refused() {
        let (_directory, mut connection) = fixture_after(
            "UPDATE plan_revision_parts SET status='excluded' WHERE id=2;
             UPDATE parts SET status='excluded' WHERE id=2;",
        );
        let tx = connection.transaction().unwrap();
        let digest = revision_1_digest(&tx);
        assert_eq!(digest, ALIGNED_REVISION_DIGEST);
        tx.execute(
            "UPDATE plan_revisions SET snapshot_digest=? WHERE id=1",
            [digest],
        )
        .unwrap();
        let PreviewOutcome::Refused(refused) = preview(&tx, "default", 1, 2).unwrap() else {
            panic!("preview answered without a selection")
        };
        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            json!({"kind":"reconciliation_required","reason":"missing"})
        );
    }
}
