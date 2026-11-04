use super::*;
use crate::manifest_text::DraftPart;
use crate::required_units::storage::{normalize_parts, num, part_rows};
use required_units::model::{INPUT_FIELDS, PART_FIELDS, js_cmp, project};
use rusqlite::Transaction;
use std::collections::{HashMap, HashSet};

pub(crate) fn read(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: &Value,
    d: &Draft,
) -> Result<DraftDiff> {
    let saved = snapshot(d)?;
    let mut inputs = Vec::new();
    let mut parts = Vec::new();
    if let Some(id) = d.header["baseRevisionId"].as_i64() {
        let revision = one(
            tx,
            "SELECT * FROM plan_revisions WHERE tenant_id=? AND profile_id=? AND id=?",
            &[&tenant, &num(profile, "id")?, &id],
        )?
        .ok_or_else(|| anyhow!("Plan draft base revision is missing"))?;
        if let Some(input_set) = revision["inputSetId"].as_i64() {
            inputs = rows(
                tx,
                "SELECT * FROM plan_revision_inputs WHERE tenant_id=? AND input_set_id=?",
                &[&tenant, &input_set],
            )?
            .iter()
            .map(|row| {
                let mut row = project(row, INPUT_FIELDS);
                if row["trackingKind"] != "untracked" {
                    row["trackingKind"] = json!("revision");
                }
                if row["effectiveNamingDigest"].is_null() {
                    row["effectiveNamingDigest"] = json!("");
                }
                row
            })
            .collect();
        }
        parts = part_rows(
            tx,
            "SELECT * FROM plan_revision_parts WHERE tenant_id=? AND revision_id=?",
            &[&tenant, &id],
        )?;
        normalize_parts(&mut parts)?;
        parts = parts
            .iter()
            .map(|part| part.projected(&[vec!["id"], PART_FIELDS.to_vec()].concat()))
            .collect();
    }
    let after_inputs = saved.ordinary["inputs"]
        .as_array()
        .ok_or_else(|| anyhow!("Invalid inputs"))?;
    let inputs_by_source = inputs
        .iter()
        .map(|row| Ok((num(row, "sourceId")?, row)))
        .collect::<Result<HashMap<_, _>>>()?;
    let mut after_sources = HashSet::new();
    let mut added_inputs = Vec::new();
    let mut removed_inputs = Vec::new();
    let mut changed_inputs = Vec::new();
    for after in after_inputs {
        let source = num(after, "sourceId")?;
        after_sources.insert(source);
        match inputs_by_source.get(&source) {
            None => added_inputs.push(after.clone()),
            Some(before) if **before != project(after, INPUT_FIELDS) => {
                changed_inputs.push(json!({"before":before,"after":after}))
            }
            Some(_) => {}
        }
    }
    for before in &inputs {
        if !after_sources.contains(&num(before, "sourceId")?) {
            removed_inputs.push(before.clone());
        }
    }
    let input_order = |a: &Value, b: &Value| {
        a["layerOrder"]
            .as_i64()
            .cmp(&b["layerOrder"].as_i64())
            .then(a["sourceId"].as_i64().cmp(&b["sourceId"].as_i64()))
    };
    added_inputs.sort_by(input_order);
    removed_inputs.sort_by(|a, b| input_order(a, b).then_with(|| js_cmp(a, b)));
    changed_inputs.sort_by_key(|row| row["after"]["sourceId"].as_i64());
    let parts_by_id = parts
        .iter()
        .map(|part| Ok((num(part, "id")?, part)))
        .collect::<Result<HashMap<_, _>>>()?;
    let mut linked = HashSet::new();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    for after in &saved.parts {
        let before = after["baseRevisionPartId"]
            .as_i64()
            .and_then(|id| parts_by_id.get(&id))
            .copied();
        if let Some(before) = before {
            linked.insert(num(before, "id")?);
            let fields: Vec<_> = PART_FIELDS
                .iter()
                .filter(|f| !before.same_field(after, f))
                .copied()
                .collect();
            if !fields.is_empty() {
                changed.push(ChangedPart {
                    before: before.clone(),
                    after: after.clone(),
                    fields,
                });
            }
        } else {
            added.push(after.clone());
        }
    }
    let mut removed = Vec::new();
    for before in parts {
        if !linked.contains(&num(&before, "id")?) {
            removed.push(before);
        }
    }
    added.sort_by(|a, b| js_cmp(&a["id"], &b["id"]));
    changed.sort_by(|a, b| js_cmp(&a.after["id"], &b.after["id"]));
    removed.sort_by(|a, b| js_cmp(&a["id"], &b["id"]));
    let ordinary = Map::from_iter([
        ("baseRevisionId".into(), d.header["baseRevisionId"].clone()),
        (
            "basePlanVersion".into(),
            d.header["basePlanVersion"].clone(),
        ),
        (
            "baseIsCurrent".into(),
            json!(
                profile["acceptedPlanRevisionId"] == d.header["baseRevisionId"]
                    && profile["acceptedPlanVersion"] == d.header["basePlanVersion"]
            ),
        ),
        (
            "inputs".into(),
            json!({"added":added_inputs,"removed":removed_inputs,"changed":changed_inputs}),
        ),
    ]);
    Ok(DraftDiff(DraftDiffModel {
        ordinary,
        added,
        removed,
        changed,
    }))
}

#[derive(Debug)]
pub(crate) struct ChangedPart {
    pub(crate) before: DraftPart,
    pub(crate) after: DraftPart,
    pub(crate) fields: Vec<&'static str>,
}
#[derive(Debug)]
pub(crate) struct DraftDiffModel {
    pub(crate) ordinary: Map<String, Value>,
    pub(crate) added: Vec<DraftPart>,
    pub(crate) removed: Vec<DraftPart>,
    pub(crate) changed: Vec<ChangedPart>,
}
fn before_json(part: &DraftPart) -> Vec<u8> {
    part.json_fields(&[vec!["id"], PART_FIELDS.to_vec()].concat(), false)
}
fn array<T>(values: &[T], output: &mut Vec<u8>, write: impl Fn(&T, &mut Vec<u8>)) {
    output.push(b'[');
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            output.push(b',');
        }
        write(value, output);
    }
    output.push(b']');
}
impl DraftDiffModel {
    /// No Part or input differs from the base revision.
    pub(crate) fn is_empty(&self) -> bool {
        let inputs = &self.ordinary["inputs"];
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && ["added", "removed", "changed"]
                .iter()
                .all(|key| inputs[*key].as_array().is_none_or(Vec::is_empty))
    }
    pub(super) fn write_json(&self) -> Vec<u8> {
        let object = &self.ordinary;
        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
        keys.push("parts");
        let mut output = vec![b'{'];
        for (i, key) in keys.iter().enumerate() {
            if i > 0 {
                output.push(b',');
            }
            crate::manifest_text::JsText::scalar(key).write_json(&mut output);
            output.push(b':');
            if *key != "parts" {
                output.extend_from_slice(object[*key].to_string().as_bytes());
                continue;
            }
            output.extend_from_slice(b"{\"added\":");
            array(&self.added, &mut output, |part, output| {
                output.extend_from_slice(b"{\"after\":");
                output.extend_from_slice(&part.json_fields(&snapshot_part_fields(), false));
                output.push(b'}');
            });
            output.extend_from_slice(b",\"removed\":");
            array(&self.removed, &mut output, |part, output| {
                output.extend_from_slice(b"{\"before\":");
                output.extend_from_slice(&before_json(part));
                output.push(b'}');
            });
            output.extend_from_slice(b",\"changed\":");
            array(&self.changed, &mut output, |part, output| {
                output.extend_from_slice(b"{\"before\":");
                output.extend_from_slice(&before_json(&part.before));
                output.extend_from_slice(b",\"after\":");
                output.extend_from_slice(&part.after.json_fields(&snapshot_part_fields(), false));
                output.extend_from_slice(b",\"fields\":");
                output.extend_from_slice(json!(part.fields).to_string().as_bytes());
                output.push(b'}');
            });
            output.push(b'}');
        }
        output.push(b'}');
        output
    }
}
