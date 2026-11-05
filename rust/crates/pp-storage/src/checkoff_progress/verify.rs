//! Physical-result Checkoff: `verifyPrinterCheckoff` in
//! `services/printer-checkoff-verify.ts` with `verifyAcceptedPrint` and
//! `applyAcceptedUnitDecisionsInternal`, and `dismissHostFailedLink`.
//!
//! One writer transaction reads the link, checks every decision, then writes
//! progress, the link and its print outcomes together. Node writes each Part's
//! confirmed units before checking the next Part, so a refused multi-Part
//! burst can leave earlier Parts marked; here a refusal writes nothing.

use super::{
    Body, Outcome, Refusal, Response, archived, complete_rows,
    links::{self, Link, field},
    read, set_completed,
};
use crate::read_model::AcceptedRead;
use rusqlite::Transaction;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};

const LEFT_FILL: &str =
    "Confirm must include lower incomplete units first (Progress fills from the left)";

fn refuse(status: u16, detail: &str) -> Outcome {
    Ok(Ok(Response::failure(status, detail, None)))
}

struct Decision {
    part_id: i64,
    unit_index: i64,
    /// `Some` for a rejected unit.
    reason: Option<String>,
    note: Option<String>,
}

/// `parseDecisions`. A confirmed decision may carry a valid reason, which the
/// Accepted write then drops.
fn decisions(raw: &Value, allow_empty: bool) -> std::result::Result<Vec<Decision>, String> {
    let Some(items) = raw.as_array().filter(|a| allow_empty || !a.is_empty()) else {
        return Err("decisions required (reject needs a reason)".into());
    };
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for (i, row) in items.iter().enumerate() {
        if !row.is_object() && !row.is_array() {
            return Err(format!("decisions[{i}] is invalid"));
        }
        let Some(part_id) = links::js_integer(row.get("part_id"), 1.0) else {
            return Err(format!("decisions[{i}].part_id is invalid"));
        };
        let Some(unit_index) = links::js_integer(row.get("unit_index"), 0.0) else {
            return Err(format!("decisions[{i}].unit_index is invalid"));
        };
        let rejected = match field(row, "result").as_str() {
            Some("confirmed") => false,
            Some("rejected") => true,
            _ => {
                return Err(format!(
                    "decisions[{i}].result must be confirmed or rejected"
                ));
            }
        };
        if !seen.insert((part_id, unit_index)) {
            return Err(format!("decisions[{i}] duplicates another unit"));
        }
        let reason = field(row, "reason");
        if rejected && !links::is_reason(reason) {
            return Err(format!("decisions[{i}] reject needs a valid reason"));
        }
        if !rejected && !reason.is_null() && reason != "" && !links::is_reason(reason) {
            return Err(format!("decisions[{i}].reason is invalid"));
        }
        let note = match field(row, "note") {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            Value::String(s) => {
                Some(links::js_slice(links::js_trim(s), 500)).filter(|n| !n.is_empty())
            }
            _ => return Err(format!("decisions[{i}].note is invalid")),
        };
        out.push(Decision {
            part_id,
            unit_index,
            reason: rejected.then(|| reason.as_str().unwrap_or_default().to_owned()),
            note,
        });
    }
    Ok(out)
}

/// `readAdditionalDecisions`: the index of a pending imported extra and its result.
fn additional(raw: &Value) -> Option<Vec<(usize, Map<String, Value>)>> {
    let items = raw.as_array().filter(|a| a.len() <= 500)?;
    let mut seen = HashSet::new();
    items
        .iter()
        .map(|item| {
            let Value::Object(result) = links::extra_result(item)? else {
                return None;
            };
            let index = item
                .get("index")?
                .as_f64()
                .filter(|n| n.fract() == 0.0 && *n >= 0.0)? as usize;
            seen.insert(index).then_some((index, result))
        })
        .collect()
}

/// `resolveAdditionalDecisions`. `None` refuses the request.
fn resolve_additional(
    inventory: &Value,
    decisions: &[(usize, Map<String, Value>)],
    now: &str,
) -> Option<Value> {
    if decisions.is_empty() {
        return Some(inventory.clone());
    }
    let mut extras = inventory.get("extras")?.as_array()?.clone();
    let mut seen = HashSet::new();
    for (index, result) in decisions {
        let extra = extras.get_mut(*index)?;
        if extra["checkoff"]["result"] != "pending" || !seen.insert(index) {
            return None;
        }
        let mut checkoff = result.clone();
        checkoff.insert("checked_at".into(), now.into());
        extra["checkoff"] = Value::Object(checkoff);
    }
    Some(json!({ "extras": extras }))
}

fn coordinates(units: &Value) -> Vec<(i64, i64)> {
    units
        .as_array()
        .map(|units| {
            units
                .iter()
                .filter_map(|u| Some((u["part_id"].as_i64()?, u["unit_index"].as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

fn find(links: &[Link], id: &str) -> Option<usize> {
    links.iter().position(|link| link["id"] == id)
}

pub(super) fn verify(
    tx: &Transaction<'_>,
    tenant: &str,
    link_id: &str,
    body: &Value,
    repos: &Path,
) -> Outcome {
    let now = links::now();
    let mut all = links::load(tx, tenant, &now)?;
    let Some(index) = find(&all, link_id) else {
        return refuse(404, "Checkoff link not found");
    };
    if all[index]["state"] != "awaiting_verify" {
        return refuse(409, "Link is not awaiting verify");
    }
    let extra = match body.get("additional_decisions") {
        None => Vec::new(),
        Some(raw) => match additional(raw) {
            Some(extra) => extra,
            None => return refuse(400, "Invalid additional part decisions"),
        },
    };
    let decisions = match decisions(field(body, "decisions"), !extra.is_empty()) {
        Ok(decisions) => decisions,
        Err(detail) => return refuse(400, &detail),
    };
    let link = &all[index];
    let resolved: HashSet<(i64, i64)> = coordinates(&link["resolved_units"]).into_iter().collect();
    let units = coordinates(&link["units"]);
    if decisions.iter().any(|d| {
        let key = (d.part_id, d.unit_index);
        resolved.contains(&key) || !units.contains(&key)
    }) {
        return refuse(400, "All decisions must target pending units on this link");
    }
    let profile = link["profile_id"].as_i64().unwrap_or_default();
    let snapshot = match read(tx, tenant, profile, repos)? {
        AcceptedRead::Ready { snapshot } => *snapshot,
        AcceptedRead::Empty { .. } => return refuse(409, "Accepted Plan has no required units"),
        AcceptedRead::CompatibilityDirty => return Ok(Err(Refusal::Dirty)),
        AcceptedRead::Uninitialized => return Ok(Err(Refusal::Uninitialized)),
        AcceptedRead::Missing => return Ok(Err(Refusal::Internal)),
        AcceptedRead::IntegrityFailure { .. } => return Ok(Err(Refusal::Integrity)),
    };
    let unit = |part_id: i64, unit_index: i64| {
        let part = snapshot
            .parts
            .iter()
            .find(|p| p.projection_part_id == part_id)?;
        Some((
            part,
            part.units.iter().find(|u| u.unit_index == unit_index)?,
        ))
    };
    if units.iter().any(|&(p, u)| unit(p, u).is_none()) {
        return refuse(
            409,
            "Legacy Checkoff link no longer maps to the accepted Plan",
        );
    }
    let Some(inventory) = resolve_additional(&link["imported_inventory"], &extra, &now) else {
        return refuse(400, LEFT_FILL);
    };
    if archived(&snapshot) {
        return Ok(Err(Refusal::Archived));
    }
    complete_rows(tx, tenant, &snapshot)?;

    let mut confirmed: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for d in decisions.iter().filter(|d| d.reason.is_none()) {
        confirmed.entry(d.part_id).or_default().insert(d.unit_index);
    }
    for (&part_id, indices) in &confirmed {
        let max = indices.last().copied().unwrap_or_default();
        let Some((part, _)) = unit(part_id, max) else {
            return refuse(400, LEFT_FILL);
        };
        if part
            .units
            .iter()
            .any(|u| u.unit_index <= max && !u.completed && !indices.contains(&u.unit_index))
        {
            return refuse(400, LEFT_FILL);
        }
    }
    let mut units_confirmed = 0;
    for (&part_id, indices) in &confirmed {
        for &index in indices {
            if unit(part_id, index).is_some_and(|(_, u)| !u.completed) {
                units_confirmed += 1;
            }
            set_completed(tx, tenant, part_id, index, true)?;
        }
    }

    let mut resolved_units = link["resolved_units"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut outcomes = Vec::with_capacity(decisions.len());
    for d in &decisions {
        let Some((part, _)) = unit(d.part_id, d.unit_index) else {
            return refuse(400, LEFT_FILL);
        };
        let result = if d.reason.is_some() {
            "rejected"
        } else {
            "confirmed"
        };
        let mut row = Map::new();
        row.insert("part_id".into(), d.part_id.into());
        row.insert("unit_index".into(), d.unit_index.into());
        row.insert("result".into(), result.into());
        if let Some(reason) = &d.reason {
            row.insert("reason".into(), reason.clone().into());
        }
        if let Some(note) = &d.note {
            row.insert("note".into(), note.clone().into());
        }
        resolved_units.push(Value::Object(row));
        outcomes.push(links::Outcome {
            part_id: d.part_id,
            unit_index: d.unit_index,
            result,
            reason: d.reason.clone(),
            note: d.note.clone(),
            match_key: part.part_key.clone(),
            role: part.effective_role.clone(),
        });
    }
    let done: HashSet<(i64, i64)> = coordinates(&Value::from(resolved_units.clone()))
        .into_iter()
        .collect();
    let fully_done = units.iter().all(|u| done.contains(u))
        && !inventory["extras"]
            .as_array()
            .is_some_and(|extras| extras.iter().any(|e| e["checkoff"]["result"] == "pending"));
    let link = &mut all[index];
    let units_marked = links::add(&link["units_marked"], units_confirmed);
    link.insert("resolved_units".into(), resolved_units.into());
    link.insert("imported_inventory".into(), inventory);
    link.insert(
        "state".into(),
        if fully_done {
            "verified"
        } else {
            "awaiting_verify"
        }
        .into(),
    );
    if fully_done {
        link.insert("applied_at".into(), now.clone().into());
    }
    link.insert("units_marked".into(), units_marked);
    let link = link.clone();
    links::save(tx, tenant, all, index)?;
    let outcomes = links::append_outcomes(tx, tenant, &link, &outcomes, &now)?;
    Ok(Ok(Response::ok(Body::Verified {
        link: links::public(&link),
        units_confirmed,
        units_rejected: decisions.iter().filter(|d| d.reason.is_some()).count(),
        outcomes,
    })))
}

pub(super) fn dismiss(tx: &Transaction<'_>, tenant: &str, link_id: &str) -> Outcome {
    let now = links::now();
    let mut all = links::load(tx, tenant, &now)?;
    let Some(index) = find(&all, link_id) else {
        return refuse(404, "Checkoff link not found");
    };
    if all[index]["state"] != "host_failed" {
        return refuse(409, "Link is not host_failed");
    }
    all[index].insert("state".into(), "dismissed".into());
    let link = links::public(&all[index]);
    links::save(tx, tenant, all, index)?;
    Ok(Ok(Response::ok(Body::Link { link })))
}
