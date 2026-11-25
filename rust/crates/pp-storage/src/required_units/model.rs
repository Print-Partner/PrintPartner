use super::Decision;
use crate::manifest_text::{DraftPart, digest_json_bytes};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[derive(Debug, thiserror::Error)]
#[error("Required-unit assignment limit of {limit} exceeded")]
pub(crate) struct AssignmentLimitExceeded {
    pub(crate) limit: i64,
}

pub(super) const FORMAT: &str = "required-unit-reconciliation-v1";
pub(crate) const PART_FIELDS: &[&str] = &[
    "partKey",
    "relativePath",
    "filename",
    "sourceLayer",
    "status",
    "roleInferred",
    "roleOverride",
    "filamentColorId",
    "filamentCustomHex",
    "spoolmanSpoolId",
    "quantityInferred",
    "quantityOverride",
    "quantityEffective",
    "included",
    "notes",
    "githubBlobUrl",
    "geometrySame",
    "requirement",
    "optionGroupId",
    "manifestSource",
    "artifactDigest",
];
pub(crate) const INPUT_FIELDS: &[&str] = &[
    "sourceId",
    "sourceLayer",
    "layerOrder",
    "trackingKind",
    "sourceRevisionId",
    "manifestDigest",
    "effectiveNamingDigest",
];
pub(crate) fn digest(value: &Value) -> String {
    hex::encode(Sha256::digest(value.to_string()))
}
pub(crate) fn js_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    a.to_string()
        .encode_utf16()
        .cmp(b.to_string().encode_utf16())
}
pub(crate) fn project(row: &Value, fields: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for field in fields {
        out.insert((*field).into(), row[*field].clone());
    }
    Value::Object(out)
}
pub(crate) fn planning(draft: &Value, inputs: &[Value], parts: &[DraftPart]) -> Result<String> {
    let mut inputs: Vec<_> = inputs.iter().map(|r| project(r, INPUT_FIELDS)).collect();
    inputs.sort_by(|a, b| {
        a["layerOrder"]
            .as_i64()
            .cmp(&b["layerOrder"].as_i64())
            .then(a["sourceId"].as_i64().cmp(&b["sourceId"].as_i64()))
            .then_with(|| js_cmp(a, b))
    });
    let fields = [vec!["baseRevisionPartId"], PART_FIELDS.to_vec()].concat();
    let mut parts = parts
        .iter()
        .map(|part| {
            let bytes = part.json_fields(&fields, false);
            let key = std::str::from_utf8(&bytes)?
                .encode_utf16()
                .collect::<Vec<_>>();
            Ok((key, bytes))
        })
        .collect::<Result<Vec<_>>>()?;
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut bytes = b"{\"format\":\"plan-draft-v1\",\"base_revision_id\":".to_vec();
    bytes.extend_from_slice(draft["baseRevisionId"].to_string().as_bytes());
    bytes.extend_from_slice(b",\"base_plan_version\":");
    bytes.extend_from_slice(draft["basePlanVersion"].to_string().as_bytes());
    bytes.extend_from_slice(b",\"inputs\":");
    bytes.extend_from_slice(&serde_json::to_vec(&inputs)?);
    bytes.extend_from_slice(b",\"parts\":[");
    for (i, (_, p)) in parts.iter().enumerate() {
        if i > 0 {
            bytes.push(b',');
        }
        bytes.extend_from_slice(p);
    }
    bytes.extend_from_slice(b"]}");
    Ok(digest_json_bytes(&bytes))
}
pub(crate) fn selection(planning: &str, reconciliation: Option<&str>) -> String {
    digest(
        &json!({"format":"plan-draft-v2","planning_digest":planning,"required_unit_reconciliation":reconciliation.map(|d| json!({"format":FORMAT,"digest":d}))}),
    )
}
pub(super) fn decisions(input: &[Decision]) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    let mut predecessors = HashSet::new();
    let mut targets = HashSet::new();
    for d in input {
        let target = d.target().get();
        ensure!(
            targets.insert(target),
            "Required-unit reconciliation decision target is invalid"
        );
        result.push(match d {
            Decision::Replace { .. } => json!({"kind":"replace","targetDraftPartId":target}),
            Decision::SelectExactPredecessor {
                predecessor_revision_part_id,
                ..
            }
            | Decision::AcceptPriorCompletion {
                predecessor_revision_part_id,
                ..
            } => {
                ensure!(
                    predecessors.insert(predecessor_revision_part_id.get()),
                    "Required-unit reconciliation predecessor is already selected"
                );
                let kind = match d {
                    Decision::SelectExactPredecessor { .. } => "select_exact_predecessor",
                    _ => "accept_prior_completion",
                };
                json!({
                    "kind":kind,
                    "targetDraftPartId":target,
                    "predecessorRevisionPartId":predecessor_revision_part_id.get()
                })
            }
        });
    }
    result.sort_by_key(|d| d["targetDraftPartId"].as_i64());
    Ok(result)
}
#[derive(Clone)]
pub(crate) struct Unit {
    pub token: String,
    pub prior_index: i64,
    pub created_at: String,
    pub completed: bool,
    pub assembled: bool,
}
#[derive(Clone)]
pub(crate) struct BasePart {
    pub id: i64,
    pub source_id: Option<i64>,
    pub artifact: Option<String>,
    pub role: String,
    pub units: Vec<Unit>,
}
pub(crate) struct Reconciled {
    pub result: Value,
    pub basis: Value,
}
impl Reconciled {
    pub fn full(&self) -> Value {
        let mut result = self.result.clone();
        result["selectionBasis"] = self.basis.clone();
        result
    }
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    id: i64,
    base_revision_part_id: Option<i64>,
    source_layer: String,
    artifact_digest: Option<String>,
    role_inferred: String,
    role_override: Option<String>,
    quantity_effective: i64,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    source_layer: String,
    source_id: pp_domain::PositiveId,
}
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Assignment {
    Reuse {
        #[serde(rename = "draftPartId")]
        draft_part_id: i64,
        #[serde(rename = "unitIndex")]
        unit_index: usize,
        token: String,
    },
    Create {
        #[serde(rename = "draftPartId")]
        draft_part_id: i64,
        #[serde(rename = "unitIndex")]
        unit_index: usize,
    },
}
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Conflict {
    UnsafePredecessor {
        #[serde(rename = "targetDraftPartId")]
        target_draft_part_id: i64,
        #[serde(rename = "predecessorRevisionPartId")]
        predecessor_revision_part_id: i64,
    },
    AmbiguousExactMatch {
        #[serde(rename = "targetDraftPartId")]
        target_draft_part_id: i64,
        #[serde(rename = "candidateRevisionPartIds")]
        candidate_revision_part_ids: Vec<i64>,
    },
    PredecessorClaimed {
        #[serde(rename = "targetDraftPartId")]
        target_draft_part_id: i64,
        #[serde(rename = "predecessorRevisionPartId")]
        predecessor_revision_part_id: i64,
    },
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BasisRow {
    revision_part_id: i64,
    token: String,
    prior_index: i64,
    created_at: String,
    completed: bool,
    assembled: bool,
}
enum Selection {
    Normal(usize),
    PriorCompletion(usize),
    Create,
}
fn exact(target: &Target, source: Option<i64>, base: &BasePart) -> bool {
    source.is_some()
        && source == base.source_id
        && target.artifact_digest.is_some()
        && target.artifact_digest == base.artifact
        && target
            .role_override
            .as_ref()
            .unwrap_or(&target.role_inferred)
            == &base.role
}
pub(crate) fn role(part: &Value) -> &str {
    part["roleOverride"]
        .as_str()
        .or_else(|| part["roleInferred"].as_str())
        .unwrap_or("")
}
fn basis(part: &BasePart) -> Vec<BasisRow> {
    part.units
        .iter()
        .map(|u| BasisRow {
            revision_part_id: part.id,
            token: u.token.clone(),
            prior_index: u.prior_index,
            created_at: u.created_at.clone(),
            completed: u.completed,
            assembled: u.assembled,
        })
        .collect()
}
#[cfg(test)]
pub(super) fn reconcile(
    parts: &[DraftPart],
    inputs: &[Value],
    base: &[BasePart],
    decisions: &[Decision],
) -> Result<Reconciled> {
    reconcile_checked(parts, inputs, base, decisions, &mut || Ok(()))
}

pub(super) fn reconcile_checked(
    parts: &[DraftPart],
    inputs: &[Value],
    base: &[BasePart],
    decisions: &[Decision],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Reconciled> {
    let mut targets = parts
        .iter()
        .map(|part| part.scalar.clone())
        .map(serde_json::from_value::<Target>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    targets.sort_by_key(|p| p.id);
    let mut base = base.to_vec();
    for part in &mut base {
        part.units.sort_by_key(|unit| unit.prior_index);
    }
    base.sort_by_key(|part| part.id);
    let inputs = inputs
        .iter()
        .cloned()
        .map(serde_json::from_value::<Input>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let mut sources = HashMap::new();
    for i in inputs {
        let id = if sources.contains_key(&i.source_layer) {
            None
        } else {
            Some(i.source_id.get() as i64)
        };
        sources.insert(i.source_layer, id);
    }
    let mut ids = HashSet::new();
    for p in &targets {
        ensure!(
            (1..=pp_domain::MAX_SAFE_INTEGER as i64).contains(&p.id) && ids.insert(p.id),
            "Required-unit target Part ID is invalid"
        );
        ensure!(
            (1..=10_000).contains(&p.quantity_effective),
            "Required-unit target quantity is invalid"
        );
    }
    let mut assignments_needed = 0_i64;
    for target in &targets {
        match assignments_needed.checked_add(target.quantity_effective) {
            Some(next) if next <= super::storage::ASSIGNMENT_CAP => assignments_needed = next,
            _ => {
                return Err(anyhow::Error::new(AssignmentLimitExceeded {
                    limit: super::storage::ASSIGNMENT_CAP,
                }));
            }
        }
    }
    let mut base_ids = HashSet::new();
    let mut tokens = HashSet::new();
    for p in &base {
        ensure!(
            (1..=pp_domain::MAX_SAFE_INTEGER as i64).contains(&p.id) && base_ids.insert(p.id),
            "Required-unit predecessor Part ID is invalid"
        );
        for (i, u) in p.units.iter().enumerate() {
            validate_token(&u.token)?;
            ensure!(
                u.prior_index >= 0,
                "Required-unit predecessor index is invalid"
            );
            ensure!(
                !(u.assembled && !u.completed),
                "Required-unit predecessor progress is corrupt"
            );
            ensure!(
                u.prior_index == i as i64 && tokens.insert(&u.token),
                "Required-unit predecessor mapping is invalid"
            );
        }
    }
    self::decisions(decisions)?;
    for d in decisions {
        ensure!(
            ids.contains(&(d.target().get() as i64)),
            "Required-unit reconciliation decision target is missing"
        );
    }
    let candidates: Vec<Vec<usize>> = targets
        .iter()
        .map(|p| {
            base.iter()
                .enumerate()
                .filter_map(|(i, b)| {
                    exact(p, sources.get(&p.source_layer).copied().flatten(), b).then_some(i)
                })
                .collect()
        })
        .collect();
    let mut counts = HashMap::new();
    for candidates in &candidates {
        for i in candidates {
            *counts.entry(*i).or_insert(0) += 1;
        }
    }
    let mut claims = HashSet::new();
    let mut choices = Vec::new();
    let mut conflicts = Vec::new();
    for (target, candidates) in targets.iter().zip(&candidates) {
        let decision = decisions
            .iter()
            .find(|d| d.target().get() as i64 == target.id);
        let known = base
            .iter()
            .position(|b| Some(b.id) == target.base_revision_part_id);
        let mode = if let Some(known) = known {
            if candidates.contains(&known) {
                ensure!(
                    decision.is_none(),
                    "Required-unit reconciliation decision is not applicable"
                );
                Selection::Normal(known)
            } else {
                match decision {
                    None => {
                        conflicts.push(Conflict::UnsafePredecessor {
                            target_draft_part_id: target.id,
                            predecessor_revision_part_id: base[known].id,
                        });
                        continue;
                    }
                    Some(Decision::AcceptPriorCompletion {
                        predecessor_revision_part_id,
                        ..
                    }) => {
                        ensure!(
                            predecessor_revision_part_id.get() as i64 == base[known].id,
                            "Accepted prior completion predecessor is not the known predecessor"
                        );
                        Selection::PriorCompletion(known)
                    }
                    Some(Decision::Replace { .. }) => Selection::Create,
                    _ => bail!(
                        "Exact predecessor selection is not applicable to an unsafe predecessor"
                    ),
                }
            }
        } else if candidates.len() == 1 && counts[&candidates[0]] == 1 {
            ensure!(
                decision.is_none(),
                "Required-unit reconciliation decision is not applicable"
            );
            Selection::Normal(candidates[0])
        } else if !candidates.is_empty() {
            match decision {
                None => {
                    let mut ids: Vec<_> = candidates.iter().map(|i| base[*i].id).collect();
                    ids.sort();
                    conflicts.push(Conflict::AmbiguousExactMatch {
                        target_draft_part_id: target.id,
                        candidate_revision_part_ids: ids,
                    });
                    continue;
                }
                Some(Decision::SelectExactPredecessor {
                    predecessor_revision_part_id,
                    ..
                }) => {
                    let selected = candidates
                        .iter()
                        .find(|i| base[**i].id == predecessor_revision_part_id.get() as i64)
                        .ok_or_else(|| {
                            anyhow::anyhow!("Selected predecessor is not an exact-match candidate")
                        })?;
                    Selection::Normal(*selected)
                }
                Some(Decision::Replace { .. }) => Selection::Create,
                _ => bail!("Prior completion requires a known unsafe predecessor"),
            }
        } else {
            ensure!(
                decision.is_none(),
                "Required-unit reconciliation decision is not applicable"
            );
            Selection::Create
        };
        if let Selection::Normal(i) | Selection::PriorCompletion(i) = mode
            && !claims.insert(i)
        {
            conflicts.push(Conflict::PredecessorClaimed {
                target_draft_part_id: target.id,
                predecessor_revision_part_id: base[i].id,
            });
            continue;
        }
        choices.push((target, mode));
    }
    if !conflicts.is_empty() {
        return Ok(Reconciled {
            result: json!({"kind":"unresolved","conflicts":conflicts}),
            basis: json!([]),
        });
    }
    let mut assignments = Vec::new();
    let mut rows = Vec::new();
    let mut selected = HashSet::new();
    for (target, mode) in choices {
        check()?;
        let count = target.quantity_effective as usize;
        let mut units = match mode {
            Selection::Create => Vec::new(),
            Selection::Normal(i) | Selection::PriorCompletion(i) => {
                let prior_completion = matches!(mode, Selection::PriorCompletion(_));
                let predecessor = &base[i];
                let mut units: Vec<_> = predecessor
                    .units
                    .iter()
                    .filter(|u| !prior_completion || u.completed)
                    .collect();
                if prior_completion || count < predecessor.units.len() {
                    rows.extend(basis(predecessor));
                    units.sort_by(|a, b| {
                        b.completed
                            .cmp(&a.completed)
                            .then_with(|| {
                                crate::read_model::views::folder_compare(
                                    &a.created_at,
                                    &b.created_at,
                                )
                            })
                            .then_with(|| {
                                crate::read_model::views::folder_compare(&a.token, &b.token)
                            })
                    });
                    units.truncate(count);
                }
                units.sort_by_key(|u| u.prior_index);
                units
            }
        };
        let reused = units.len();
        for (unit_index, u) in units.drain(..).enumerate() {
            check()?;
            selected.insert(u.token.clone());
            assignments.push(Assignment::Reuse {
                draft_part_id: target.id,
                unit_index,
                token: u.token.clone(),
            });
        }
        for unit_index in reused..count {
            check()?;
            assignments.push(Assignment::Create {
                draft_part_id: target.id,
                unit_index,
            });
        }
    }
    let mut surplus: Vec<_> = base
        .iter()
        .flat_map(|b| b.units.iter())
        .filter(|u| !selected.contains(&u.token))
        .map(|u| u.token.clone())
        .collect();
    surplus.sort();
    rows.sort_by_key(|r| (r.revision_part_id, r.prior_index));
    Ok(Reconciled {
        result: json!({"kind":"ready","assignments":assignments,"surplus":surplus}),
        basis: json!(rows),
    })
}
pub(crate) fn validate_token(t: &str) -> Result<()> {
    ensure!(
        t.len() == 36
            && t.starts_with("ppu_")
            && t.bytes()
                .skip(4)
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Required-unit token is invalid"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest_text::{JsText, ManifestText, OptionGroupId, PartManifestMetadata};

    fn part(id: i64, predecessor: Option<i64>, quantity: i64) -> DraftPart {
        DraftPart::new(
            json!({
                "id":id,"draftId":7,"baseRevisionPartId":predecessor,
                "partKey":format!("part-{id}"),"relativePath":"",
                "filename":format!("part-{id}.stl"),"sourceLayer":"source","status":"base",
                "roleInferred":"primary","roleOverride":null,"filamentColorId":null,
                "filamentCustomHex":null,"spoolmanSpoolId":null,"quantityInferred":1,
                "quantityOverride":null,"quantityEffective":quantity,"included":true,"notes":"",
                "githubBlobUrl":null,"geometrySame":null,"artifactDigest":"a".repeat(64)
            }),
            PartManifestMetadata::default(),
        )
    }

    fn input() -> Value {
        json!({"sourceId":5,"sourceLayer":"source","layerOrder":0,"trackingKind":"revision",
            "sourceRevisionId":9,"manifestDigest":"b".repeat(64),"effectiveNamingDigest":"c".repeat(64)})
    }

    fn unit(token: u64, prior_index: i64, completed: bool) -> Unit {
        Unit {
            token: format!("ppu_{token:032x}"),
            prior_index,
            created_at: "2026-08-20T09:00:00.000Z".into(),
            completed,
            assembled: false,
        }
    }

    fn base(id: i64) -> BasePart {
        BasePart {
            id,
            source_id: Some(5),
            artifact: Some("a".repeat(64)),
            role: "primary".into(),
            units: vec![unit(id as u64, 0, false)],
        }
    }

    fn decision(value: Value) -> Decision {
        serde_json::from_value(value).unwrap()
    }

    fn run(parts: &[DraftPart], base: &[BasePart], decisions: &[Decision]) -> Value {
        reconcile(parts, &[input()], base, decisions)
            .unwrap()
            .full()
    }

    #[test]
    fn empty_and_new_plans_create_slots_in_target_order() {
        assert_eq!(
            run(&[], &[], &[]),
            json!({"kind":"ready","assignments":[],"surplus":[],"selectionBasis":[]})
        );
        assert_eq!(
            run(&[part(22, None, 2), part(21, None, 1)], &[], &[]),
            json!({
                "kind":"ready","assignments":[
                    {"kind":"create","draftPartId":21,"unitIndex":0},
                    {"kind":"create","draftPartId":22,"unitIndex":0},
                    {"kind":"create","draftPartId":22,"unitIndex":1}
                ],"surplus":[],"selectionBasis":[]
            })
        );
    }

    #[test]
    fn exact_predecessor_reuses_tokens_and_creates_growth_slots() {
        assert_eq!(
            run(&[part(21, Some(10), 2)], &[base(10)], &[]),
            json!({
                "kind":"ready","assignments":[
                    {"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_0000000000000000000000000000000a"},
                    {"kind":"create","draftPartId":21,"unitIndex":1}
                ],"surplus":[],"selectionBasis":[]
            })
        );
        assert_eq!(
            run(&[part(21, None, 1)], &[base(10)], &[]),
            json!({
                "kind":"ready","assignments":[
                    {"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_0000000000000000000000000000000a"}
                ],"surplus":[],"selectionBasis":[]
            })
        );
    }

    #[test]
    fn shrink_prefers_completion_then_creation_time_then_token() {
        let mut prior = base(10);
        let mut later = unit(2, 1, true);
        later.created_at = "2026-08-20T09:02:00.000Z".into();
        prior.units = vec![unit(1, 0, false), later, unit(3, 2, true), unit(4, 3, true)];
        assert_eq!(
            run(&[part(21, Some(10), 2)], &[prior], &[]),
            json!({
                "kind":"ready","assignments":[
                    {"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_00000000000000000000000000000003"},
                    {"kind":"reuse","draftPartId":21,"unitIndex":1,"token":"ppu_00000000000000000000000000000004"}
                ],"surplus":["ppu_00000000000000000000000000000001","ppu_00000000000000000000000000000002"],
                "selectionBasis":[
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000001","priorIndex":0,"createdAt":"2026-08-20T09:00:00.000Z","completed":false,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000002","priorIndex":1,"createdAt":"2026-08-20T09:02:00.000Z","completed":true,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000003","priorIndex":2,"createdAt":"2026-08-20T09:00:00.000Z","completed":true,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000004","priorIndex":3,"createdAt":"2026-08-20T09:00:00.000Z","completed":true,"assembled":false}
                ]
            })
        );
    }

    #[test]
    fn shrink_breaks_token_ties_and_restores_prior_index_order() {
        let mut prior = base(10);
        prior.units = vec![unit(4, 0, true), unit(3, 1, true)];
        let result = run(&[part(21, Some(10), 1)], &[prior], &[]);
        assert_eq!(
            result["assignments"],
            json!([
                {"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_00000000000000000000000000000003"}
            ])
        );
        assert_eq!(
            result["surplus"],
            json!(["ppu_00000000000000000000000000000004"])
        );
        assert_eq!(
            result["selectionBasis"],
            json!([{"revisionPartId":10,"token":"ppu_00000000000000000000000000000004","priorIndex":0,"createdAt":"2026-08-20T09:00:00.000Z","completed":true,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000003","priorIndex":1,"createdAt":"2026-08-20T09:00:00.000Z","completed":true,"assembled":false}])
        );
        let mut earlier = unit(3, 2, true);
        earlier.created_at = "2026-08-20T08:00:00.000Z".into();
        let mut prior = base(10);
        prior.units = vec![unit(4, 0, true), unit(1, 1, false), earlier];
        let result = run(&[part(21, Some(10), 2)], &[prior], &[]);
        assert_eq!(
            result["assignments"],
            json!([
                {"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_00000000000000000000000000000004"},
                {"kind":"reuse","draftPartId":21,"unitIndex":1,"token":"ppu_00000000000000000000000000000003"}
            ])
        );
        assert_eq!(
            result["surplus"],
            json!(["ppu_00000000000000000000000000000001"])
        );
        assert_eq!(
            result["selectionBasis"],
            json!([{"revisionPartId":10,"token":"ppu_00000000000000000000000000000004","priorIndex":0,"createdAt":"2026-08-20T09:00:00.000Z","completed":true,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000001","priorIndex":1,"createdAt":"2026-08-20T09:00:00.000Z","completed":false,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000003","priorIndex":2,"createdAt":"2026-08-20T08:00:00.000Z","completed":true,"assembled":false}])
        );
    }

    // Expected results are captured by tests/fixtures/required-units/gaps-node.mjs.
    #[test]
    fn surplus_tokens_are_sorted_independently_of_prior_indices() {
        let mut prior = base(10);
        prior.units = vec![unit(4, 0, false), unit(1, 1, false)];
        assert_eq!(
            run(&[], &[prior], &[]),
            json!({"kind":"ready","assignments":[],"surplus":["ppu_00000000000000000000000000000001","ppu_00000000000000000000000000000004"],"selectionBasis":[]})
        );
    }

    #[test]
    fn selection_basis_is_sorted_across_shrinking_predecessors() {
        let mut first = base(10);
        first.units = vec![unit(1, 0, false), unit(2, 1, false)];
        let mut second = base(11);
        second.units = vec![unit(3, 0, false), unit(4, 1, false)];
        assert_eq!(
            run(
                &[part(21, Some(11), 1), part(22, Some(10), 1)],
                &[first, second],
                &[]
            ),
            json!({"kind":"ready","assignments":[{"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_00000000000000000000000000000003"},
                    {"kind":"reuse","draftPartId":22,"unitIndex":0,"token":"ppu_00000000000000000000000000000001"}],"surplus":["ppu_00000000000000000000000000000002","ppu_00000000000000000000000000000004"],"selectionBasis":[{"revisionPartId":10,"token":"ppu_00000000000000000000000000000001","priorIndex":0,"createdAt":"2026-08-20T09:00:00.000Z","completed":false,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000002","priorIndex":1,"createdAt":"2026-08-20T09:00:00.000Z","completed":false,"assembled":false},
                    {"revisionPartId":11,"token":"ppu_00000000000000000000000000000003","priorIndex":0,"createdAt":"2026-08-20T09:00:00.000Z","completed":false,"assembled":false},
                    {"revisionPartId":11,"token":"ppu_00000000000000000000000000000004","priorIndex":1,"createdAt":"2026-08-20T09:00:00.000Z","completed":false,"assembled":false}]})
        );
    }

    #[test]
    fn null_source_on_both_sides_does_not_make_an_exact_match() {
        let mut prior = base(10);
        prior.source_id = None;
        assert_eq!(
            reconcile(&[part(21, None, 1)], &[input(), input()], &[prior], &[])
                .unwrap()
                .full(),
            json!({"kind":"ready","assignments":[{"kind":"create","draftPartId":21,"unitIndex":0}],"surplus":["ppu_0000000000000000000000000000000a"],"selectionBasis":[]})
        );
    }

    #[test]
    fn null_artifact_on_both_sides_does_not_make_an_exact_match() {
        let mut prior = base(10);
        prior.artifact = None;
        let mut target = part(21, None, 1);
        target["artifactDigest"] = Value::Null;
        assert_eq!(
            run(&[target], &[prior], &[]),
            json!({"kind":"ready","assignments":[{"kind":"create","draftPartId":21,"unitIndex":0}],"surplus":["ppu_0000000000000000000000000000000a"],"selectionBasis":[]})
        );
    }

    #[test]
    fn prior_completion_reuses_only_completed_tokens_and_records_basis() {
        let mut target = part(21, Some(10), 2);
        target["artifactDigest"] = json!("b".repeat(64));
        let mut prior = base(10);
        prior.units = vec![unit(1, 0, false), unit(2, 1, true)];
        let accept = decision(
            json!({"kind":"accept_prior_completion","target_draft_part_id":21,"predecessor_revision_part_id":10}),
        );
        assert_eq!(
            run(&[target], &[prior], &[accept]),
            json!({
                "kind":"ready","assignments":[
                    {"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_00000000000000000000000000000002"},
                    {"kind":"create","draftPartId":21,"unitIndex":1}
                ],"surplus":["ppu_00000000000000000000000000000001"],"selectionBasis":[
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000001","priorIndex":0,"createdAt":"2026-08-20T09:00:00.000Z","completed":false,"assembled":false},
                    {"revisionPartId":10,"token":"ppu_00000000000000000000000000000002","priorIndex":1,"createdAt":"2026-08-20T09:00:00.000Z","completed":true,"assembled":false}
                ]
            })
        );
    }

    #[test]
    fn unsafe_predecessor_requires_a_decision_and_replace_creates_fresh_slots() {
        let mut target = part(21, Some(10), 1);
        target["artifactDigest"] = json!("b".repeat(64));
        assert_eq!(
            run(&[target.clone()], &[base(10)], &[]),
            json!({
                "kind":"unresolved","conflicts":[{"kind":"unsafe_predecessor","targetDraftPartId":21,"predecessorRevisionPartId":10}],"selectionBasis":[]
            })
        );
        let replace = decision(json!({"kind":"replace","target_draft_part_id":21}));
        assert_eq!(
            run(&[target], &[base(10)], &[replace]),
            json!({
                "kind":"ready","assignments":[{"kind":"create","draftPartId":21,"unitIndex":0}],
                "surplus":["ppu_0000000000000000000000000000000a"],"selectionBasis":[]
            })
        );
    }

    #[test]
    fn ambiguous_predecessors_are_sorted_and_explicit_selection_reuses_the_chosen_token() {
        assert_eq!(
            run(&[part(21, None, 1)], &[base(11), base(10)], &[]),
            json!({
                "kind":"unresolved","conflicts":[{"kind":"ambiguous_exact_match","targetDraftPartId":21,"candidateRevisionPartIds":[10,11]}],"selectionBasis":[]
            })
        );
        let select = decision(
            json!({"kind":"select_exact_predecessor","target_draft_part_id":21,"predecessor_revision_part_id":11}),
        );
        assert_eq!(
            run(&[part(21, None, 1)], &[base(11), base(10)], &[select]),
            json!({
                "kind":"ready","assignments":[{"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_0000000000000000000000000000000b"}],
                "surplus":["ppu_0000000000000000000000000000000a"],"selectionBasis":[]
            })
        );
    }

    #[test]
    fn competing_targets_require_decisions_and_cannot_claim_the_same_predecessor() {
        assert_eq!(
            run(&[part(22, None, 1), part(21, None, 1)], &[base(10)], &[]),
            json!({
                "kind":"unresolved","conflicts":[
                    {"kind":"ambiguous_exact_match","targetDraftPartId":21,"candidateRevisionPartIds":[10]},
                    {"kind":"ambiguous_exact_match","targetDraftPartId":22,"candidateRevisionPartIds":[10]}
                ],"selectionBasis":[]
            })
        );
        assert_eq!(
            run(
                &[part(22, Some(10), 1), part(21, Some(10), 1)],
                &[base(10)],
                &[]
            ),
            json!({
                "kind":"unresolved","conflicts":[{"kind":"predecessor_claimed","targetDraftPartId":22,"predecessorRevisionPartId":10}],"selectionBasis":[]
            })
        );
    }

    #[test]
    fn duplicate_source_layers_missing_artifacts_and_role_changes_do_not_match() {
        let expected = json!({"kind":"ready","assignments":[{"kind":"create","draftPartId":21,"unitIndex":0}],"surplus":["ppu_0000000000000000000000000000000a"],"selectionBasis":[]});
        for inputs in [
            vec![],
            vec![input(), input()],
            vec![input(), input(), input()],
        ] {
            assert_eq!(
                reconcile(&[part(21, None, 1)], &inputs, &[base(10)], &[])
                    .unwrap()
                    .full(),
                expected
            );
        }
        for (field, value) in [
            ("artifactDigest", Value::Null),
            ("roleOverride", json!("accent")),
            ("sourceLayer", json!("unknown")),
        ] {
            let mut target = part(21, None, 1);
            target[field] = value;
            assert_eq!(run(&[target], &[base(10)], &[]), expected);
        }
        let mut target = part(21, None, 1);
        target["roleInferred"] = json!("accent");
        target["roleOverride"] = json!("primary");
        assert_eq!(role(&target.scalar), "primary");
        assert_eq!(role(&json!({"roleInferred":"accent"})), "accent");
        assert_eq!(
            run(&[target], &[base(10)], &[]),
            json!({"kind":"ready","assignments":[{"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_0000000000000000000000000000000a"}],"surplus":[],"selectionBasis":[]})
        );
    }

    #[test]
    fn unordered_predecessor_units_are_normalized_to_prior_indices() {
        let mut prior = base(10);
        prior.units = vec![unit(2, 1, false), unit(1, 0, false)];
        assert_eq!(
            run(&[part(21, Some(10), 2)], &[prior], &[]),
            json!({
                "kind":"ready","assignments":[
                    {"kind":"reuse","draftPartId":21,"unitIndex":0,"token":"ppu_00000000000000000000000000000001"},
                    {"kind":"reuse","draftPartId":21,"unitIndex":1,"token":"ppu_00000000000000000000000000000002"}
                ],"surplus":[],"selectionBasis":[]
            })
        );
    }

    #[test]
    fn decisions_use_canonical_camel_case_order_and_node_digest() {
        let parsed = [
            decision(json!({"kind":"replace","target_draft_part_id":22})),
            decision(
                json!({"kind":"select_exact_predecessor","target_draft_part_id":21,"predecessor_revision_part_id":10}),
            ),
        ];
        let canonical = decisions(&parsed).unwrap();
        assert_eq!(
            canonical,
            vec![
                json!({"kind":"select_exact_predecessor","targetDraftPartId":21,"predecessorRevisionPartId":10}),
                json!({"kind":"replace","targetDraftPartId":22}),
            ]
        );
        assert_eq!(
            digest(&json!(canonical)),
            "c06295cd9c4b4bba94e0b9c7e863cea130ffbc648093719d1e390648de77ef4b"
        );
        let accept = decision(
            json!({"kind":"accept_prior_completion","target_draft_part_id":21,"predecessor_revision_part_id":10}),
        );
        assert_eq!(
            decisions(&[accept]).unwrap(),
            vec![
                json!({"kind":"accept_prior_completion","targetDraftPartId":21,"predecessorRevisionPartId":10})
            ]
        );
    }

    #[test]
    fn planning_and_selection_digests_match_node_literals() {
        assert_eq!(
            planning(
                &json!({"baseRevisionId":11,"basePlanVersion":2}),
                &[input()],
                &[part(21, Some(10), 2)]
            )
            .unwrap(),
            "34d08359307d43a212182fab31b345ffe18752c94b0045a6b7c17fed5bdf88cb"
        );
        assert_eq!(
            planning(
                &json!({"baseRevisionId":null,"basePlanVersion":0}),
                &[],
                &[]
            )
            .unwrap(),
            "3fa8b0076adaf112304fbe2e7370eeecbb2b4989ff28cc3ed5a4b43f7c7bc3b3"
        );
        assert_eq!(
            selection("planning", None),
            "9d4aaeb35a8641a0326c8b2b9669594302e42b13d45b71d64a8a66ead3900114"
        );
        assert_eq!(
            selection("planning", Some("recon")),
            "a8adf07025602638eaa5d77e31a76594f5849f65aef9345b59f35a456c255690"
        );
    }

    #[test]
    fn planning_uses_utf16_sorting_and_preserves_manifest_surrogates() {
        let mut astral = part(21, None, 1);
        astral["partKey"] = json!("\u{10000}");
        astral.manifest = PartManifestMetadata {
            requirement: Some(ManifestText(JsText::scalar("optional"))),
            option_group_id: Some(OptionGroupId(JsText::from_units(vec![0xd800]))),
            manifest_source: Some("kit.json".into()),
        };
        let mut bmp = part(22, None, 1);
        bmp["partKey"] = json!("\u{e000}");
        let mut first_input = input();
        first_input["sourceLayer"] = json!("\u{e000}");
        let mut second_input = input();
        second_input["sourceLayer"] = json!("\u{10000}");
        let draft = json!({"baseRevisionId":11,"basePlanVersion":2});
        assert_eq!(
            planning(
                &draft,
                &[first_input.clone(), second_input.clone()],
                &[bmp.clone(), astral.clone()]
            )
            .unwrap(),
            "78ca8945b89737afac2571265f2de528391441d4b877f821415c478a0e3131b3"
        );
        assert_eq!(
            planning(&draft, &[second_input, first_input], &[astral, bmp]).unwrap(),
            "78ca8945b89737afac2571265f2de528391441d4b877f821415c478a0e3131b3"
        );
    }

    #[test]
    fn malformed_decision_shapes_and_ids_return_errors() {
        for raw in [
            json!({"kind":"unknown","target_draft_part_id":21}),
            json!({"kind":"replace","target_draft_part_id":0}),
            json!({"kind":"replace","target_draft_part_id":9_007_199_254_740_992u64}),
            json!({"kind":"replace","target_draft_part_id":"21"}),
            json!({"kind":"replace","target_draft_part_id":21,"extra":true}),
            json!({"kind":"select_exact_predecessor","target_draft_part_id":21}),
            json!({"kind":"accept_prior_completion","target_draft_part_id":21,"predecessor_revision_part_id":-1}),
        ] {
            assert!(
                serde_json::from_value::<Decision>(raw.clone()).is_err(),
                "{raw}"
            );
        }
        let replace = decision(json!({"kind":"replace","target_draft_part_id":21}));
        assert_eq!(
            decisions(&[replace.clone(), replace])
                .unwrap_err()
                .to_string(),
            "Required-unit reconciliation decision target is invalid"
        );
        let select = decision(
            json!({"kind":"select_exact_predecessor","target_draft_part_id":21,"predecessor_revision_part_id":10}),
        );
        let accept = decision(
            json!({"kind":"accept_prior_completion","target_draft_part_id":22,"predecessor_revision_part_id":10}),
        );
        assert_eq!(
            decisions(&[select, accept]).unwrap_err().to_string(),
            "Required-unit reconciliation predecessor is already selected"
        );
    }

    #[test]
    fn corrupt_predecessor_mappings_return_errors() {
        for (units, expected) in [
            (
                vec![unit(1, 1, false)],
                "Required-unit predecessor mapping is invalid",
            ),
            (
                vec![unit(1, 0, false), unit(1, 1, false)],
                "Required-unit predecessor mapping is invalid",
            ),
            (
                vec![unit(1, 0, false), unit(2, 0, false)],
                "Required-unit predecessor mapping is invalid",
            ),
            (
                vec![unit(1, -1, false)],
                "Required-unit predecessor index is invalid",
            ),
            (
                vec![Unit {
                    assembled: true,
                    ..unit(1, 0, false)
                }],
                "Required-unit predecessor progress is corrupt",
            ),
        ] {
            let mut prior = base(10);
            prior.units = units;
            assert_eq!(
                reconcile(&[part(21, None, 1)], &[input()], &[prior], &[])
                    .err()
                    .unwrap()
                    .to_string(),
                expected
            );
        }
        for bases in [
            vec![base(0)],
            vec![base(10), base(10)],
            vec![base(9_007_199_254_740_992)],
        ] {
            assert_eq!(
                reconcile(&[], &[], &bases, &[]).err().unwrap().to_string(),
                "Required-unit predecessor Part ID is invalid"
            );
        }
        let mut prior = base(11);
        prior.units[0].token = "ppu_0000000000000000000000000000000a".into();
        assert_eq!(
            reconcile(&[], &[], &[base(10), prior], &[])
                .err()
                .unwrap()
                .to_string(),
            "Required-unit predecessor mapping is invalid"
        );
    }

    #[test]
    fn token_validation_accepts_only_128_bit_lowercase_hex_tokens() {
        validate_token("ppu_0123456789abcdef0123456789abcdef").unwrap();
        for invalid in [
            "",
            "ppu_0123456789ABCDEF0123456789abcdef",
            "ppu_0123456789abcdef0123456789abcdeg",
            "ppu_0123456789abcdef0123456789abcde",
            "xyz_0123456789abcdef0123456789abcdef",
        ] {
            assert_eq!(
                validate_token(invalid).unwrap_err().to_string(),
                "Required-unit token is invalid"
            );
            let mut prior = base(10);
            prior.units[0].token = invalid.into();
            assert_eq!(
                reconcile(&[], &[], &[prior], &[])
                    .err()
                    .unwrap()
                    .to_string(),
                "Required-unit token is invalid"
            );
        }
    }

    #[test]
    fn invalid_targets_and_source_json_return_errors() {
        for (parts, expected) in [
            (
                vec![part(0, None, 1)],
                "Required-unit target Part ID is invalid",
            ),
            (
                vec![part(21, None, 0)],
                "Required-unit target quantity is invalid",
            ),
            (
                vec![part(21, None, 10_001)],
                "Required-unit target quantity is invalid",
            ),
            (
                vec![part(21, None, 1), part(21, None, 1)],
                "Required-unit target Part ID is invalid",
            ),
            (
                vec![part(9_007_199_254_740_992, None, 1)],
                "Required-unit target Part ID is invalid",
            ),
        ] {
            assert_eq!(
                reconcile(&parts, &[input()], &[], &[])
                    .err()
                    .unwrap()
                    .to_string(),
                expected
            );
        }
        for raw in [
            json!({}),
            json!({"sourceLayer":"source","sourceId":0}),
            json!({"sourceLayer":3,"sourceId":5}),
        ] {
            assert!(reconcile(&[part(21, None, 1)], &[raw], &[], &[]).is_err());
        }
        let mut target = part(21, None, 1);
        target["quantityEffective"] = json!(1.5);
        assert!(reconcile(&[target], &[input()], &[], &[]).is_err());
        assert_eq!(
            run(&[part(21, None, 1)], &[], &[]),
            json!({"kind":"ready","assignments":[{"kind":"create","draftPartId":21,"unitIndex":0}],"surplus":[],"selectionBasis":[]})
        );
    }

    #[test]
    fn assignment_cap_refuses_the_sum_of_quantities_before_expansion() {
        let cap = super::super::storage::ASSIGNMENT_CAP;
        assert_eq!(cap, 2_000);
        let left = cap / 2;
        let right = cap - left;
        let mut over_parts = [part(1, None, left), part(2, None, right + 1)];
        over_parts[1]["included"] = json!(false);
        let mut checks = 0;
        let over = reconcile_checked(&over_parts, &[input()], &[], &[], &mut || {
            checks += 1;
            Ok(())
        });
        let error = over.err().unwrap();
        assert_eq!(
            error
                .downcast_ref::<AssignmentLimitExceeded>()
                .unwrap()
                .limit,
            cap
        );
        assert_eq!(checks, 0);
        let mut at_cap_parts = [part(1, None, left), part(2, None, right)];
        at_cap_parts[1]["included"] = json!(false);
        let ready = reconcile(&at_cap_parts, &[input()], &[], &[]).unwrap();
        assert_eq!(
            ready.result["assignments"].as_array().unwrap().len(),
            cap as usize
        );
    }

    #[test]
    fn expansion_checks_cancellation_once_per_part() {
        let mut checks = 0;
        let error = reconcile_checked(
            &[part(1, None, 1), part(2, None, 1), part(3, None, 1)],
            &[input()],
            &[],
            &[],
            &mut || {
                checks += 1;
                if checks == 2 {
                    return Err(anyhow::anyhow!(super::super::Failure::Cancelled));
                }
                Ok(())
            },
        )
        .err()
        .unwrap();
        assert_eq!(checks, 2);
        assert!(matches!(
            error.downcast_ref(),
            Some(super::super::Failure::Cancelled)
        ));
    }

    #[test]
    fn expansion_checks_cancellation_once_per_assignment() {
        let mut checks = 0;
        let result = reconcile_checked(&[part(1, None, 3)], &[input()], &[], &[], &mut || {
            checks += 1;
            if checks == 3 {
                return Err(anyhow::anyhow!(super::super::Failure::Cancelled));
            }
            Ok(())
        });
        let error = result
            .err()
            .expect("cancellation must interrupt assignment expansion");
        assert_eq!(checks, 3);
        assert!(matches!(
            error.downcast_ref(),
            Some(super::super::Failure::Cancelled)
        ));
    }

    #[test]
    fn decisions_reject_missing_targets_wrong_predecessors_and_inapplicable_modes() {
        let replace = decision(json!({"kind":"replace","target_draft_part_id":22}));
        assert_eq!(
            reconcile(&[part(21, None, 1)], &[input()], &[], &[replace])
                .err()
                .unwrap()
                .to_string(),
            "Required-unit reconciliation decision target is missing"
        );
        let replace = decision(json!({"kind":"replace","target_draft_part_id":21}));
        assert_eq!(
            reconcile(
                &[part(21, None, 1)],
                &[input()],
                &[],
                std::slice::from_ref(&replace)
            )
            .err()
            .unwrap()
            .to_string(),
            "Required-unit reconciliation decision is not applicable"
        );
        assert_eq!(
            reconcile(
                &[part(21, Some(10), 1)],
                &[input()],
                &[base(10)],
                &[replace]
            )
            .err()
            .unwrap()
            .to_string(),
            "Required-unit reconciliation decision is not applicable"
        );
        let mut target = part(21, Some(10), 1);
        target["artifactDigest"] = json!("b".repeat(64));
        let select = decision(
            json!({"kind":"select_exact_predecessor","target_draft_part_id":21,"predecessor_revision_part_id":10}),
        );
        assert_eq!(
            reconcile(&[target.clone()], &[input()], &[base(10)], &[select])
                .err()
                .unwrap()
                .to_string(),
            "Exact predecessor selection is not applicable to an unsafe predecessor"
        );
        let accept = decision(
            json!({"kind":"accept_prior_completion","target_draft_part_id":21,"predecessor_revision_part_id":11}),
        );
        assert_eq!(
            reconcile(
                &[target],
                &[input()],
                &[base(10)],
                std::slice::from_ref(&accept)
            )
            .err()
            .unwrap()
            .to_string(),
            "Accepted prior completion predecessor is not the known predecessor"
        );
        assert_eq!(
            reconcile(
                &[part(21, None, 1)],
                &[input()],
                &[base(10), base(11)],
                &[accept]
            )
            .err()
            .unwrap()
            .to_string(),
            "Prior completion requires a known unsafe predecessor"
        );
        let select = decision(
            json!({"kind":"select_exact_predecessor","target_draft_part_id":21,"predecessor_revision_part_id":12}),
        );
        assert_eq!(
            reconcile(
                &[part(21, None, 1)],
                &[input()],
                &[base(10), base(11)],
                &[select]
            )
            .err()
            .unwrap()
            .to_string(),
            "Selected predecessor is not an exact-match candidate"
        );
    }
}
