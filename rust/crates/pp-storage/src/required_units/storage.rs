use super::model::{self, BasePart, FORMAT, Unit, digest};
use crate::manifest_text::{
    DraftPart, ManifestText, OptionGroupId, PartManifestMetadata, read_manifest_text,
};
use anyhow::{Result, anyhow, bail, ensure};
use pp_domain::PositiveId;
use rusqlite::{Transaction, types::ValueRef};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

mod json;

type Check<'a> = &'a dyn Fn() -> Result<()>;

pub(crate) const DRAFT_COLUMNS: &str = "id,tenant_id,profile_id,base_revision_id,base_plan_version,state,lifecycle_version,rebased_from_draft_id,rebased_from_lifecycle_version,rebased_from_snapshot_digest,digest_format,snapshot_digest,created_by,idempotency_key,created_at,current_required_unit_reconciliation_id,consumed_revision_id,consumed_at";
pub(crate) const INPUT_COLUMNS: &str = "id,tenant_id,draft_id,source_id,source_layer,layer_order,tracking_kind,source_revision_id,manifest_digest,effective_naming_digest";
pub(crate) const PART_COLUMNS: &str = "id,tenant_id,draft_id,base_revision_part_id,part_key,relative_path,filename,source_layer,status,role_inferred,role_override,filament_color_id,filament_custom_hex,spoolman_spool_id,quantity_inferred,quantity_override,quantity_effective,included,notes,github_blob_url,geometry_same,requirement,option_group_id,manifest_source,artifact_digest";
pub(crate) const RECONCILIATION_CHECK_COLUMNS: &str = "id";
pub(crate) const RECONCILIATION_COLUMNS: &str = "id,tenant_id,profile_id,draft_id,format,planning_digest,base_revision_id,base_mapping_digest,selection_basis_digest,selection_basis_json,decision_digest,result_kind,result_digest,result_json,reconciliation_digest,expected_assignment_count,actor_id,idempotency_key,payload_digest,created_at,finalized_at";
pub(crate) const DECISION_COLUMNS: &str = "kind,target_draft_part_id,predecessor_revision_part_id";
pub(crate) const ASSIGNMENT_COLUMNS: &str =
    "kind,target_draft_part_id,unit_index,required_unit_token";

pub(crate) const ROW_LIMIT: usize = 200_000;
/// Largest checked-in assignment list is 23 (`edit-selected`). Ten times that.
pub(crate) const ASSIGNMENT_CAP: i64 = 230;
const TEXT_LIMIT: usize = 65_536;
const TOTAL_TEXT_LIMIT: usize = 64 * 1024 * 1024;
const UNIT_LIMIT: u16 = 10_000;

fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn add_text(total: &mut usize, bytes: usize) -> Result<()> {
    *total = total
        .checked_add(bytes)
        .filter(|total| *total <= TOTAL_TEXT_LIMIT)
        .ok_or_else(|| anyhow!("Reconciliation text limit exceeded"))?;
    Ok(())
}

fn scalar(value: ValueRef<'_>) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => {
            ensure!(
                n.unsigned_abs() <= pp_domain::MAX_SAFE_INTEGER,
                "Invalid stored integer"
            );
            json!(n)
        }
        ValueRef::Text(t) => json!(std::str::from_utf8(t)?),
        ValueRef::Real(_) | ValueRef::Blob(_) => bail!("Invalid stored reconciliation value"),
    })
}

/// Reads rows as camel-case JSON objects. The two stored JSON documents may
/// reach the 64 MiB total; every other text column is capped at 64 KiB.
pub(crate) fn rows(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Value>> {
    rows_checked(tx, sql, args, &|| Ok(()))
}

pub(crate) fn rows_checked(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
    check: Check<'_>,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    each_row(tx, sql, args, check, |row| {
        out.push(row);
        Ok(())
    })?;
    Ok(out)
}

fn each_row(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
    check: Check<'_>,
    mut visit: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    check()?;
    let mut statement = tx.prepare(sql)?;
    let columns: Vec<_> = statement.column_names().map(|n| camel(n)).collect();
    let mut query = statement.query(args)?;
    let mut count = 0;
    let mut total = 0;
    while let Some(row) = query.next()? {
        check()?;
        ensure!(count < ROW_LIMIT, "Reconciliation row limit exceeded");
        count += 1;
        let mut object = serde_json::Map::new();
        for (i, column) in columns.iter().enumerate() {
            let value = row.get_ref(i)?;
            if let ValueRef::Text(t) = value {
                let limit = match column.as_str() {
                    "selectionBasisJson" | "resultJson" => TOTAL_TEXT_LIMIT,
                    _ => TEXT_LIMIT,
                };
                ensure!(t.len() <= limit, "Reconciliation text limit exceeded");
                add_text(&mut total, t.len())?;
            }
            object.insert(column.clone(), scalar(value)?);
        }
        visit(Value::Object(object))?;
    }
    check()
}

/// Reads Part rows, keeping manifest text columns out of the scalar JSON.
pub(crate) fn part_rows(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<DraftPart>> {
    part_rows_checked(tx, sql, args, &|| Ok(()))
}

fn part_rows_checked(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
    check: Check<'_>,
) -> Result<Vec<DraftPart>> {
    check()?;
    let mut statement = tx.prepare(sql)?;
    let columns: Vec<_> = statement.column_names().map(|n| camel(n)).collect();
    let mut query = statement.query(args)?;
    let mut parts = Vec::new();
    let mut total = 0;
    while let Some(row) = query.next()? {
        check()?;
        ensure!(parts.len() < ROW_LIMIT, "Reconciliation row limit exceeded");
        let mut object = serde_json::Map::new();
        let mut manifest = PartManifestMetadata::default();
        for (i, column) in columns.iter().enumerate() {
            let value = row.get_ref(i)?;
            match value {
                ValueRef::Text(t) => {
                    ensure!(t.len() <= TEXT_LIMIT, "Reconciliation text limit exceeded");
                    add_text(&mut total, t.len())?;
                }
                ValueRef::Blob(b) if matches!(column.as_str(), "requirement" | "optionGroupId") => {
                    add_text(&mut total, b.len())?;
                }
                _ => {}
            }
            match column.as_str() {
                "requirement" => {
                    manifest.requirement = read_manifest_text(value)?.map(ManifestText)
                }
                "optionGroupId" => {
                    manifest.option_group_id = read_manifest_text(value)?.map(OptionGroupId)
                }
                "manifestSource" => {
                    manifest.manifest_source = match value {
                        ValueRef::Null => None,
                        ValueRef::Text(t) => Some(std::str::from_utf8(t)?.to_owned()),
                        _ => bail!("Invalid manifest source"),
                    }
                }
                _ => {
                    object.insert(column.clone(), scalar(value)?);
                }
            }
        }
        parts.push(DraftPart::new(Value::Object(object), manifest));
    }
    check()?;
    Ok(parts)
}

pub(crate) fn one(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Option<Value>> {
    one_checked(tx, sql, args, &|| Ok(()))
}

fn one_checked(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
    check: Check<'_>,
) -> Result<Option<Value>> {
    Ok(rows_checked(tx, sql, args, check)?.into_iter().next())
}

pub(crate) fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| anyhow!("Invalid stored {k}"))
}

pub(crate) fn num(v: &Value, k: &str) -> Result<i64> {
    v[k].as_i64().ok_or_else(|| anyhow!("Invalid stored {k}"))
}

pub(super) fn optional_num(v: &Value, k: &str) -> Result<Option<i64>> {
    if v[k].is_null() {
        return Ok(None);
    }
    num(v, k).map(Some)
}

pub(crate) fn normalize_parts(parts: &mut [DraftPart]) -> Result<()> {
    for part in parts {
        for k in ["included", "geometrySame"] {
            if let Some(n) = optional_num(part, k)? {
                ensure!(n == 0 || n == 1, "Invalid stored Part boolean");
                part[k] = json!(n == 1);
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BasisRow {
    revision_part_id: PositiveId,
    token: String,
    prior_index: u16,
    created_at: String,
    completed: bool,
    assembled: bool,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum StoredResult {
    Ready {
        assignments: Vec<StoredAssignment>,
        surplus: Vec<String>,
    },
    Unresolved {
        conflicts: Vec<StoredConflict>,
    },
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(super) enum StoredAssignment {
    Reuse {
        draft_part_id: PositiveId,
        unit_index: u16,
        token: String,
    },
    Create {
        draft_part_id: PositiveId,
        unit_index: u16,
    },
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(super) enum StoredConflict {
    AmbiguousExactMatch {
        target_draft_part_id: PositiveId,
        candidate_revision_part_ids: Vec<PositiveId>,
    },
    UnsafePredecessor {
        target_draft_part_id: PositiveId,
        predecessor_revision_part_id: PositiveId,
    },
    PredecessorClaimed {
        target_draft_part_id: PositiveId,
        predecessor_revision_part_id: PositiveId,
    },
}

impl StoredConflict {
    fn target(&self) -> u64 {
        match self {
            Self::AmbiguousExactMatch {
                target_draft_part_id,
                ..
            }
            | Self::UnsafePredecessor {
                target_draft_part_id,
                ..
            }
            | Self::PredecessorClaimed {
                target_draft_part_id,
                ..
            } => target_draft_part_id.get(),
        }
    }
}

pub(crate) struct Draft {
    pub(crate) header: Value,
    pub(crate) inputs: Vec<Value>,
    pub(crate) parts: Vec<DraftPart>,
    pub(crate) planning: String,
    pub(crate) selected: Option<Saved>,
}

pub(crate) struct Saved {
    pub(crate) header: Value,
    pub(super) result: StoredResult,
}

pub(crate) fn selection_basis_digest(mapping: &Value, basis: &Value) -> String {
    digest(&json!({"format":FORMAT,"base_mapping_digest":mapping,"rows":basis}))
}

pub(super) fn reconciliation_digest(h: &Value) -> String {
    digest(&json!({
        "format":FORMAT,
        "base_revision_id":h["baseRevisionId"],
        "base_mapping_digest":h["baseMappingDigest"],
        "planning_digest":h["planningDigest"],
        "selection_basis_digest":h["selectionBasisDigest"],
        "decision_digest":h["decisionDigest"],
        "result_kind":h["resultKind"],
        "result_digest":h["resultDigest"]
    }))
}

/// Reads a finalized reconciliation and checks every stored digest, decision
/// and assignment row against it, as Node's `readSavedRequiredUnitReconciliation` does.
pub(super) fn saved(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft: i64,
    id: i64,
) -> Result<Saved> {
    saved_checked(tx, tenant, profile, draft, id, &|| Ok(()))
}

fn saved_checked(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft: i64,
    id: i64,
    check: Check<'_>,
) -> Result<Saved> {
    check()?;
    let header = one_checked(
        tx,
        &format!("SELECT {RECONCILIATION_COLUMNS} FROM plan_draft_required_unit_reconciliations WHERE tenant_id=? AND id=?"),
        &[&tenant, &id],
        check,
    )?
    .filter(|h| h["finalizedAt"].is_string())
    .ok_or_else(|| anyhow!("Required-unit reconciliation is not finalized"))?;
    ensure!(
        header["profileId"] == profile && header["draftId"] == draft,
        "Plan draft Required-unit selection is corrupt"
    );
    ensure!(
        header["format"] == FORMAT,
        "Required-unit reconciliation format is unsupported"
    );
    let basis = json::basis(text(&header, "selectionBasisJson")?, check)?;
    let result = json::result(text(&header, "resultJson")?, check)?;
    let kind = match &result {
        StoredResult::Ready { .. } => "ready",
        StoredResult::Unresolved { .. } => "unresolved",
    };
    ensure!(
        kind == header["resultKind"],
        "Required-unit reconciliation result kind mismatch"
    );
    #[derive(Serialize)]
    struct SelectionBasis<'a> {
        format: &'static str,
        base_mapping_digest: &'a Value,
        rows: &'a [BasisRow],
    }
    ensure!(
        json::digest(
            &SelectionBasis {
                format: FORMAT,
                base_mapping_digest: &header["baseMappingDigest"],
                rows: &basis
            },
            check
        )? == header["selectionBasisDigest"],
        "Required-unit reconciliation selection basis digest mismatch"
    );
    #[derive(Serialize)]
    struct FullResult<'a> {
        #[serde(flatten)]
        result: &'a StoredResult,
        #[serde(rename = "selectionBasis")]
        basis: &'a [BasisRow],
    }
    ensure!(
        json::digest(
            &FullResult {
                result: &result,
                basis: &basis
            },
            check
        )? == header["resultDigest"],
        "Required-unit reconciliation result digest mismatch"
    );
    let decisions = rows_checked(
        tx,
        &format!("SELECT {DECISION_COLUMNS} FROM plan_draft_required_unit_decisions WHERE tenant_id=? AND reconciliation_id=? ORDER BY target_draft_part_id"),
        &[&tenant, &id],
        check,
    )?
    .into_iter()
    .map(|row| {
        check()?;
        let mut decision = json!({"kind":row["kind"],"targetDraftPartId":row["targetDraftPartId"]});
        let predecessor = &row["predecessorRevisionPartId"];
        if row["kind"] == "replace" {
            ensure!(predecessor.is_null(), "Required-unit replacement decision is corrupt");
        } else {
            ensure!(!predecessor.is_null(), "Required-unit predecessor decision is corrupt");
            decision["predecessorRevisionPartId"] = predecessor.clone();
        }
        Ok(decision)
    })
    .collect::<Result<Vec<_>>>()?;
    ensure!(
        json::digest(&decisions, check)? == header["decisionDigest"],
        "Required-unit reconciliation decision digest mismatch"
    );
    let expected: &[StoredAssignment] = match &result {
        StoredResult::Ready { assignments, .. } => assignments,
        StoredResult::Unresolved { .. } => &[],
    };
    let mut count = 0;
    let mut assignments_match = true;
    each_row(
        tx,
        &format!(
            "SELECT {ASSIGNMENT_COLUMNS} FROM plan_draft_required_unit_assignments WHERE tenant_id=? AND reconciliation_id=? ORDER BY target_draft_part_id,unit_index"
        ),
        &[&tenant, &id],
        check,
        |row| {
            let token = &row["requiredUnitToken"];
            let slot = (&row["targetDraftPartId"], &row["unitIndex"]);
            let assignment = if row["kind"] == "create" {
                ensure!(
                    token.is_null(),
                    "Required-unit create assignment is corrupt"
                );
                json!({"kind":"create","draftPartId":slot.0,"unitIndex":slot.1})
            } else {
                ensure!(
                    !token.is_null(),
                    "Required-unit reuse assignment is corrupt"
                );
                json!({"kind":"reuse","draftPartId":slot.0,"unitIndex":slot.1,"token":token})
            };
            assignments_match &= expected
                .get(count)
                .map(serde_json::to_value)
                .transpose()?
                .as_ref()
                == Some(&assignment);
            count += 1;
            Ok(())
        },
    )?;
    match &result {
        StoredResult::Ready { .. } => ensure!(
            Some(count as i64) == header["expectedAssignmentCount"].as_i64(),
            "Required-unit reconciliation assignment count mismatch"
        ),
        StoredResult::Unresolved { .. } => ensure!(
            count == 0,
            "Required-unit reconciliation assignment count mismatch"
        ),
    }
    ensure!(
        count == expected.len() && assignments_match,
        "Required-unit reconciliation assignments do not match the saved result"
    );
    check()?;
    ensure!(
        reconciliation_digest(&header) == header["reconciliationDigest"],
        "Required-unit reconciliation digest mismatch"
    );
    check()?;
    Ok(Saved { header, result })
}

/// Loads a Plan draft and verifies its stored snapshot digest, as Node's
/// `getPlanDraft` does.
pub(crate) fn draft(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    id: i64,
) -> Result<Option<Draft>> {
    draft_checked(tx, tenant, profile, id, &|| Ok(()))
}

pub(crate) fn draft_checked(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    id: i64,
    check: Check<'_>,
) -> Result<Option<Draft>> {
    check()?;
    let Some(header) = one_checked(
        tx,
        &format!(
            "SELECT {DRAFT_COLUMNS} FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND id=?"
        ),
        &[&tenant, &profile, &id],
        check,
    )?
    else {
        return Ok(None);
    };
    let inputs = rows_checked(
        tx,
        &format!(
            "SELECT {INPUT_COLUMNS} FROM plan_draft_inputs WHERE tenant_id=? AND draft_id=? ORDER BY layer_order,source_id"
        ),
        &[&tenant, &id],
        check,
    )?;
    let mut parts = part_rows_checked(
        tx,
        &format!(
            "SELECT {PART_COLUMNS} FROM plan_draft_parts WHERE tenant_id=? AND draft_id=? ORDER BY id"
        ),
        &[&tenant, &id],
        check,
    )?;
    check()?;
    normalize_parts(&mut parts)?;
    let planning = model::planning(&header, &inputs, &parts)?;
    check()?;
    let selected = optional_num(&header, "currentRequiredUnitReconciliationId")?
        .map(|rid| {
            ensure!(
                one_checked(
                    tx,
                    &format!("SELECT {RECONCILIATION_CHECK_COLUMNS} FROM plan_draft_required_unit_reconciliations WHERE tenant_id=? AND id=? AND profile_id=? AND draft_id=? AND finalized_at IS NOT NULL"),
                    &[&tenant, &rid, &profile, &id],
                    check,
                )?
                .is_some(),
                "Plan draft Required-unit selection is corrupt"
            );
            saved_checked(tx, tenant, profile, id, rid, check)
        })
        .transpose()?;
    let expected = match text(&header, "digestFormat")? {
        "plan-draft-v1" => {
            ensure!(
                selected.is_none(),
                "Plan draft v1 cannot select a Required-unit reconciliation"
            );
            planning.clone()
        }
        "plan-draft-v2" => model::selection(
            &planning,
            selected
                .as_ref()
                .map(|s| text(&s.header, "reconciliationDigest"))
                .transpose()?,
        ),
        _ => bail!("Plan draft digest format is unsupported"),
    };
    ensure!(
        expected == header["snapshotDigest"],
        "Plan draft snapshot digest mismatch"
    );
    check()?;
    Ok(Some(Draft {
        header,
        inputs,
        parts,
        planning,
        selected,
    }))
}

fn valid_object_name(name: &str, token: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_ .()+-".contains(&b))
        && name
            .strip_suffix(token)
            .is_some_and(|rest| rest.ends_with("__"))
}

/// Reads the accepted revision's Required-unit set as predecessor Parts.
/// Returns `None` when the revision has no set, as Node's
/// `readRequiredUnitSetByRevision` reports `uninitialized`.
pub(crate) fn base(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    revision: Option<i64>,
) -> Result<Option<(Value, Vec<BasePart>)>> {
    let Some(revision) = revision else {
        return Ok(Some((Value::Null, Vec::new())));
    };
    let header = one(
        tx,
        "SELECT input_set_id FROM plan_revisions WHERE tenant_id=? AND profile_id=? AND id=?",
        &[&tenant, &profile, &revision],
    )?
    .ok_or_else(|| anyhow!("Plan draft base revision is missing"))?;
    let set = one(
        tx,
        "SELECT * FROM plan_revision_required_unit_sets WHERE tenant_id=? AND profile_id=? AND revision_id=?",
        &[&tenant, &profile, &revision],
    )?;
    let created = rows(
        tx,
        "SELECT token FROM required_units WHERE tenant_id=? AND profile_id=? AND created_in_revision_id=?",
        &[&tenant, &profile, &revision],
    )?;
    let Some(set) = set else {
        ensure!(
            created.is_empty()
                && one(
                    tx,
                    "SELECT required_unit_token FROM plan_revision_required_units WHERE tenant_id=? AND revision_id=? LIMIT 1",
                    &[&tenant, &revision],
                )?
                .is_none(),
            "Required-unit set is partial"
        );
        return Ok(None);
    };
    ensure!(
        set["format"] == "required-unit-map-v1",
        "Required-unit set format is corrupt"
    );
    let mut sources = HashMap::new();
    if let Some(input_set) = optional_num(&header, "inputSetId")? {
        for input in rows(
            tx,
            "SELECT source_id,source_layer FROM plan_revision_inputs WHERE tenant_id=? AND input_set_id=?",
            &[&tenant, &input_set],
        )? {
            let layer = text(&input, "sourceLayer")?.to_owned();
            let id = if sources.contains_key(&layer) {
                None
            } else {
                optional_num(&input, "sourceId")?
            };
            sources.insert(layer, id);
        }
    }
    let parts = rows(
        tx,
        "SELECT id,quantity_effective,projection_part_id,source_layer,artifact_digest,role_inferred,role_override FROM plan_revision_parts WHERE tenant_id=? AND revision_id=? ORDER BY id",
        &[&tenant, &revision],
    )?;
    let mut quantities = HashMap::new();
    let mut count = 0;
    for part in &parts {
        let id = num(part, "id")?;
        ensure!(
            optional_num(part, "projectionPartId")?.is_some(),
            "Accepted Plan revision Part has no compatibility projection ID"
        );
        let quantity = num(part, "quantityEffective")?;
        ensure!(
            (1..=10_000).contains(&quantity),
            "Accepted Plan revision Part {id} has invalid quantity"
        );
        quantities.insert(id, quantity);
        count += quantity;
    }
    let mappings = rows(
        tx,
        "SELECT m.revision_part_id,m.unit_index,u.token,u.object_name,u.created_at,COALESCE(p.completed,0) AS completed,COALESCE(p.assembled,0) AS assembled FROM plan_revision_required_units m JOIN required_units u ON u.token=m.required_unit_token AND u.tenant_id=?1 AND u.profile_id=?2 JOIN plan_revision_parts r ON r.id=m.revision_part_id AND r.revision_id=?3 AND r.tenant_id=?1 LEFT JOIN print_progress p ON p.tenant_id=?1 AND p.part_id=r.projection_part_id AND p.unit_index=m.unit_index WHERE m.tenant_id=?1 AND m.revision_id=?3 ORDER BY m.revision_part_id,m.unit_index",
        &[&tenant, &profile, &revision],
    )?;
    ensure!(
        num(&set, "expectedUnitCount")? == count && mappings.len() as i64 == count,
        "Required-unit set is incomplete"
    );
    let mut units: HashMap<i64, Vec<Unit>> = HashMap::new();
    let mut tokens = HashSet::new();
    let mut canonical = Vec::new();
    for mapping in &mappings {
        let part = num(mapping, "revisionPartId")?;
        let index = num(mapping, "unitIndex")?;
        let slots = units.entry(part).or_default();
        ensure!(
            quantities.get(&part).is_some_and(|q| index < *q) && index == slots.len() as i64,
            "Required-unit set is incomplete"
        );
        let token = text(mapping, "token")?;
        model::validate_token(token)?;
        let name = text(mapping, "objectName")?;
        ensure!(
            valid_object_name(name, token),
            "Required-unit Object name is invalid"
        );
        let completed = num(mapping, "completed")?;
        let assembled = num(mapping, "assembled")?;
        ensure!(
            (0..=1).contains(&completed) && (0..=1).contains(&assembled) && assembled <= completed,
            "Required-unit progress is corrupt"
        );
        let created_at = text(mapping, "createdAt")?;
        ensure!(
            !created_at.is_empty(),
            "Required-unit creation time is missing"
        );
        tokens.insert(token);
        canonical.push(
            json!({"revision_part_id":part,"unit_index":index,"token":token,"object_name":name}),
        );
        slots.push(Unit {
            token: token.into(),
            prior_index: index,
            created_at: created_at.into(),
            completed: completed == 1,
            assembled: assembled == 1,
        });
    }
    ensure!(
        created
            .iter()
            .all(|u| u["token"].as_str().is_some_and(|t| tokens.contains(t))),
        "Required-unit set contains an orphan unit"
    );
    let mapping = digest(&json!({
        "format":"required-unit-map-v1",
        "revision_id":revision,
        "expected_unit_count":count,
        "rows":canonical
    }));
    ensure!(
        mapping == set["mappingDigest"],
        "Required-unit set digest is corrupt"
    );
    let parts = parts
        .iter()
        .map(|part| {
            let id = num(part, "id")?;
            Ok(BasePart {
                id,
                source_id: sources.get(text(part, "sourceLayer")?).copied().flatten(),
                artifact: part["artifactDigest"].as_str().map(str::to_owned),
                role: model::role(part).into(),
                units: units.remove(&id).unwrap_or_default(),
            })
        })
        .collect::<Result<_>>()?;
    Ok(Some((json!(mapping), parts)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn refusal(tx: &Transaction<'_>, sql: &str, args: &[&dyn rusqlite::ToSql]) -> String {
        part_rows(tx, sql, args).err().unwrap().to_string()
    }

    #[test]
    fn manifest_columns_keep_the_per_field_text_limit() {
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        for column in ["requirement", "option_group_id", "manifest_source"] {
            let sql = format!("SELECT ?1 AS {column}");
            assert_eq!(
                part_rows(&tx, &sql, &[&"x".repeat(65_536)]).unwrap().len(),
                1
            );
            assert_eq!(
                refusal(&tx, &sql, &[&"x".repeat(65_537)]),
                "Reconciliation text limit exceeded"
            );
        }
    }

    #[test]
    fn manifest_text_and_ppjs_bytes_count_toward_the_total_limit() {
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let text = "x".repeat(65_536);
        let ppjs = b"PPJS\x01\0\0\0\x01\xd8\0".as_slice();
        let filled = "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1024)";
        for column in ["requirement", "option_group_id", "manifest_source"] {
            let full = format!("{filled} SELECT ?1 AS {column} FROM n");
            assert_eq!(part_rows(&tx, &full, &[&text]).unwrap().len(), 1024);
            assert_eq!(
                refusal(&tx, &format!("{full} UNION ALL SELECT 'x'"), &[&text]),
                "Reconciliation text limit exceeded"
            );
        }
        for column in ["requirement", "option_group_id"] {
            let single = part_rows(&tx, &format!("SELECT ?1 AS {column}"), &[&ppjs]).unwrap();
            let manifest = &single[0].manifest;
            let units = match column {
                "requirement" => manifest.requirement.as_ref().map(|t| t.0.units()),
                _ => manifest.option_group_id.as_ref().map(|t| t.0.units()),
            };
            assert_eq!(units, Some([0xd800].as_slice()));
            let sql = format!(
                "{filled} SELECT ?1 AS notes,NULL AS {column} FROM n UNION ALL SELECT NULL,?2"
            );
            assert_eq!(
                refusal(&tx, &sql, &[&text, &ppjs]),
                "Reconciliation text limit exceeded"
            );
        }
    }
}
