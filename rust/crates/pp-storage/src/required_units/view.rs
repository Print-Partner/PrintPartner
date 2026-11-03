use super::{
    model,
    storage::{
        Draft, StoredAssignment, StoredConflict, StoredResult, normalize_parts, num, one,
        optional_num, part_rows, text,
    },
};
use crate::manifest_text::DraftPart;
use anyhow::{Result, anyhow, ensure};
use rusqlite::Transaction;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

/// The result of a reconcile request, serialized as Node's
/// `PlanDraftWorkspaceResult`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Ready {
        workspace: Box<Workspace>,
    },
    ProfileNotFound,
    DraftNotFound,
    DraftChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<Box<Workspace>>,
    },
    BaseChanged {
        workspace: Box<Workspace>,
    },
    NotOpen {
        workspace: Box<Workspace>,
    },
    AcceptedBaselineRequired,
    IdempotencyConflict,
    DomainError {
        code: DomainCode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainCode {
    RequiredUnitSetUnavailable,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Workspace {
    pub profile_id: i64,
    pub draft: DraftIdentity,
    pub parts: Vec<PartView>,
    pub diff: Diff,
    pub reconciliation: ReconciliationView,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DraftIdentity {
    pub draft_id: i64,
    pub state: String,
    pub lifecycle_version: i64,
    pub snapshot_digest: String,
    pub base: DraftBase,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DraftBase {
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub revision_id: Option<i64>,
    pub plan_version: i64,
}

/// `GET /plans/{id}/drafts` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DraftListResponse {
    pub profile_id: i64,
    pub drafts: Vec<DraftIdentity>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PartView {
    pub draft_part_id: i64,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub base_revision_part_id: Option<i64>,
    pub part_key: String,
    pub filename: String,
    pub relative_path: String,
    pub source_layer: String,
    pub role: String,
    pub quantity_inferred: i64,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub quantity_override: Option<i64>,
    pub quantity_effective: i64,
    pub included: bool,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AcceptedReference {
    pub revision_part_id: i64,
    pub filename: String,
    pub relative_path: String,
    pub source_layer: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    pub before: AcceptedReference,
    pub after: PartView,
    pub fields: Vec<&'static str>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Diff {
    pub base_is_current: bool,
    pub added: Vec<PartView>,
    pub removed: Vec<AcceptedReference>,
    pub changed: Vec<Change>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReconciliationView {
    Ready {
        reused_units: usize,
        new_units: usize,
        surplus_units: usize,
    },
    Unresolved {
        conflicts: Vec<ConflictView>,
    },
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConflictView {
    AmbiguousExactMatch {
        target_draft_part_id: u64,
        candidate_revision_part_ids: Vec<u64>,
    },
    UnsafePredecessor {
        target_draft_part_id: u64,
        predecessor_revision_part_id: u64,
    },
    PredecessorClaimed {
        target_draft_part_id: u64,
        predecessor_revision_part_id: u64,
    },
}

fn part_view(p: &DraftPart) -> Result<PartView> {
    Ok(PartView {
        draft_part_id: num(p, "id")?,
        base_revision_part_id: optional_num(p, "baseRevisionPartId")?,
        part_key: text(p, "partKey")?.into(),
        filename: text(p, "filename")?.into(),
        relative_path: text(p, "relativePath")?.into(),
        source_layer: text(p, "sourceLayer")?.into(),
        role: model::role(p).into(),
        quantity_inferred: num(p, "quantityInferred")?,
        quantity_override: optional_num(p, "quantityOverride")?,
        quantity_effective: num(p, "quantityEffective")?,
        included: p["included"]
            .as_bool()
            .ok_or_else(|| anyhow!("Invalid stored included"))?,
    })
}

fn reference(p: &DraftPart) -> Result<AcceptedReference> {
    Ok(AcceptedReference {
        revision_part_id: num(p, "id")?,
        filename: text(p, "filename")?.into(),
        relative_path: text(p, "relativePath")?.into(),
        source_layer: text(p, "sourceLayer")?.into(),
    })
}

/// Node sorts diff rows by their serialized JSON, which starts with the ID.
fn sort_by_id(parts: &mut [&DraftPart]) {
    parts.sort_by(|a, b| model::js_cmp(&a["id"], &b["id"]));
}

fn reconciliation(result: Option<&StoredResult>) -> ReconciliationView {
    match result {
        Some(StoredResult::Ready {
            assignments,
            surplus,
        }) => {
            let reused = assignments
                .iter()
                .filter(|a| matches!(a, StoredAssignment::Reuse { .. }))
                .count();
            ReconciliationView::Ready {
                reused_units: reused,
                new_units: assignments.len() - reused,
                surplus_units: surplus.len(),
            }
        }
        Some(StoredResult::Unresolved { conflicts }) => ReconciliationView::Unresolved {
            conflicts: conflicts
                .iter()
                .map(|conflict| match conflict {
                    StoredConflict::AmbiguousExactMatch {
                        target_draft_part_id,
                        candidate_revision_part_ids,
                    } => ConflictView::AmbiguousExactMatch {
                        target_draft_part_id: target_draft_part_id.get(),
                        candidate_revision_part_ids: candidate_revision_part_ids
                            .iter()
                            .map(|id| id.get())
                            .collect(),
                    },
                    StoredConflict::UnsafePredecessor {
                        target_draft_part_id,
                        predecessor_revision_part_id,
                    } => ConflictView::UnsafePredecessor {
                        target_draft_part_id: target_draft_part_id.get(),
                        predecessor_revision_part_id: predecessor_revision_part_id.get(),
                    },
                    StoredConflict::PredecessorClaimed {
                        target_draft_part_id,
                        predecessor_revision_part_id,
                    } => ConflictView::PredecessorClaimed {
                        target_draft_part_id: target_draft_part_id.get(),
                        predecessor_revision_part_id: predecessor_revision_part_id.get(),
                    },
                })
                .collect(),
        },
        None => ReconciliationView::Unresolved {
            conflicts: Vec::new(),
        },
    }
}

/// Links draft Parts to base Parts by `baseRevisionPartId`, as Node's
/// `diffPlanDraftSnapshot` does.
fn diff(previous: &[DraftPart], parts: &[DraftPart], base_is_current: bool) -> Result<Diff> {
    let mut linked = HashSet::new();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    for part in parts {
        let before = previous.iter().find(|b| {
            !part["baseRevisionPartId"].is_null() && b["id"] == part["baseRevisionPartId"]
        });
        let Some(before) = before else {
            added.push(part);
            continue;
        };
        linked.insert(num(before, "id")?);
        let fields: Vec<_> = model::PART_FIELDS
            .iter()
            .filter(|field| !before.same_field(part, field))
            .copied()
            .collect();
        if !fields.is_empty() {
            changed.push((part, before, fields));
        }
    }
    let mut removed = Vec::new();
    for part in previous {
        if !linked.contains(&num(part, "id")?) {
            removed.push(part);
        }
    }
    sort_by_id(&mut added);
    sort_by_id(&mut removed);
    changed.sort_by(|a, b| model::js_cmp(&a.0["id"], &b.0["id"]));
    Ok(Diff {
        base_is_current,
        added: added.into_iter().map(part_view).collect::<Result<_>>()?,
        removed: removed.into_iter().map(reference).collect::<Result<_>>()?,
        changed: changed
            .into_iter()
            .map(|(after, before, fields)| {
                Ok(Change {
                    before: reference(before)?,
                    after: part_view(after)?,
                    fields,
                })
            })
            .collect::<Result<_>>()?,
    })
}

/// Builds the draft workspace and its diff against the base revision, as
/// Node's `PlanDraftWorkspaceService.workspace` and `diffPlanDraftSnapshot` do.
pub(crate) fn workspace(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: &Value,
    d: &Draft,
) -> Result<Box<Workspace>> {
    let h = &d.header;
    let mut previous = match optional_num(h, "baseRevisionId")? {
        Some(id) => {
            ensure!(
                one(
                    tx,
                    "SELECT id FROM plan_revisions WHERE tenant_id=? AND profile_id=? AND id=?",
                    &[&tenant, &num(h, "profileId")?, &id]
                )?
                .is_some(),
                "Plan draft base revision is missing"
            );
            part_rows(
                tx,
                "SELECT * FROM plan_revision_parts WHERE tenant_id=? AND revision_id=?",
                &[&tenant, &id],
            )?
        }
        None => Vec::new(),
    };
    normalize_parts(&mut previous)?;
    Ok(Box::new(Workspace {
        profile_id: num(h, "profileId")?,
        draft: DraftIdentity {
            draft_id: num(h, "id")?,
            state: text(h, "state")?.into(),
            lifecycle_version: num(h, "lifecycleVersion")?,
            snapshot_digest: text(h, "snapshotDigest")?.into(),
            base: DraftBase {
                revision_id: optional_num(h, "baseRevisionId")?,
                plan_version: num(h, "basePlanVersion")?,
            },
        },
        parts: d.parts.iter().map(part_view).collect::<Result<_>>()?,
        diff: diff(
            &previous,
            &d.parts,
            profile["acceptedPlanRevisionId"] == h["baseRevisionId"]
                && profile["acceptedPlanVersion"] == h["basePlanVersion"],
        )?,
        reconciliation: reconciliation(d.selected.as_ref().map(|s| &s.result)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn part(id: i64, base: Option<i64>, quantity: i64) -> DraftPart {
        DraftPart::new(
            json!({
                "id":id,"baseRevisionPartId":base,"partKey":format!("{id}.stl"),
                "filename":format!("{id}.stl"),"relativePath":format!("{id}.stl"),
                "sourceLayer":"base:Source","roleInferred":"primary","roleOverride":null,
                "quantityInferred":1,"quantityOverride":null,"quantityEffective":quantity,
                "included":true
            }),
            crate::manifest_text::PartManifestMetadata::default(),
        )
    }

    #[test]
    fn diff_orders_rows_by_serialized_id_like_node() {
        let previous = [part(2, None, 1), part(11, None, 1), part(3, None, 1)];
        let parts = [
            part(9, None, 1),
            part(10, None, 1),
            part(30, Some(3), 1),
            part(4, Some(2), 2),
        ];
        let diff = serde_json::to_value(diff(&previous, &parts, true).unwrap()).unwrap();
        let rows = |key: &str, field: &str| {
            Value::from_iter(diff[key].as_array().unwrap().iter().map(|row| {
                field
                    .split('.')
                    .fold(row, |value, name| &value[name])
                    .clone()
            }))
        };
        assert_eq!(rows("added", "draft_part_id"), json!([10, 9]));
        assert_eq!(rows("removed", "revision_part_id"), json!([11]));
        assert_eq!(rows("changed", "after.draft_part_id"), json!([30, 4]));
        assert_eq!(rows("changed", "before.revision_part_id"), json!([3, 2]));
        assert_eq!(
            rows("changed", "fields"),
            json!([
                ["partKey", "relativePath", "filename"],
                ["partKey", "relativePath", "filename", "quantityEffective"]
            ])
        );
    }

    #[test]
    fn outcomes_serialize_with_node_kinds_and_optional_workspaces() {
        assert_eq!(
            serde_json::to_value(Outcome::DraftChanged { workspace: None }).unwrap(),
            json!({"kind":"draft_changed"})
        );
        assert_eq!(
            serde_json::to_value(Outcome::DomainError {
                code: DomainCode::RequiredUnitSetUnavailable
            })
            .unwrap(),
            json!({"kind":"domain_error","code":"required_unit_set_unavailable"})
        );
        assert_eq!(
            serde_json::to_value(Outcome::AcceptedBaselineRequired).unwrap(),
            json!({"kind":"accepted_baseline_required"})
        );
    }
}
