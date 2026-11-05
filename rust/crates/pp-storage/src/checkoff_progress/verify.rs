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
use crate::read_model::{AcceptedRead, Part, Snapshot, graph::Unit};
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

/// Every link, the index of the requested one, and the time the load used.
type Found = std::result::Result<(Vec<Link>, usize, String), Response>;

fn unit(snapshot: &Snapshot, part_id: i64, unit_index: i64) -> Option<(&Part, &Unit)> {
    let part = snapshot
        .parts
        .iter()
        .find(|p| p.projection_part_id == part_id)?;
    Some((
        part,
        part.units.iter().find(|u| u.unit_index == unit_index)?,
    ))
}

/// Loads every link and finds `link_id`, refusing as Node does when it is
/// missing or not in `state`.
fn load_in_state(
    tx: &Transaction<'_>,
    tenant: &str,
    link_id: &str,
    state: &str,
    wrong_state: &str,
) -> anyhow::Result<Found> {
    let now = links::now();
    let all = links::load(tx, tenant, &now)?;
    Ok(match all.iter().position(|link| link["id"] == link_id) {
        None => Err(Response::failure(404, "Checkoff link not found", None)),
        Some(index) if all[index]["state"] != state => {
            Err(Response::failure(409, wrong_state, None))
        }
        Some(index) => Ok((all, index, now)),
    })
}

/// Checks the left-fill rule for every Part before marking any unit, then
/// marks the confirmed units printed. Returns how many were not printed yet.
fn mark_confirmed(
    tx: &Transaction<'_>,
    tenant: &str,
    snapshot: &Snapshot,
    decisions: &[Decision],
) -> anyhow::Result<std::result::Result<usize, Response>> {
    let mut confirmed: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for d in decisions.iter().filter(|d| d.reason.is_none()) {
        confirmed.entry(d.part_id).or_default().insert(d.unit_index);
    }
    for (&part_id, indices) in &confirmed {
        let max = indices.last().copied().unwrap_or_default();
        let fills_left = unit(snapshot, part_id, max).is_some_and(|(part, _)| {
            part.units
                .iter()
                .all(|u| u.unit_index > max || u.completed || indices.contains(&u.unit_index))
        });
        if !fills_left {
            return Ok(Err(Response::failure(400, LEFT_FILL, None)));
        }
    }
    let mut newly = 0;
    for (&part_id, indices) in &confirmed {
        for &index in indices {
            newly += usize::from(unit(snapshot, part_id, index).is_some_and(|(_, u)| !u.completed));
            set_completed(tx, tenant, part_id, index, true)?;
        }
    }
    Ok(Ok(newly))
}

/// The link's `resolved_units` entries and the outcome rows for the decisions.
fn record(snapshot: &Snapshot, decisions: &[Decision]) -> (Vec<Value>, Vec<links::Outcome>) {
    decisions
        .iter()
        .filter_map(|d| {
            let (part, _) = unit(snapshot, d.part_id, d.unit_index)?;
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
            let outcome = links::Outcome {
                part_id: d.part_id,
                unit_index: d.unit_index,
                result,
                reason: d.reason.clone(),
                note: d.note.clone(),
                match_key: part.part_key.clone(),
                role: part.effective_role.clone(),
            };
            Some((Value::Object(row), outcome))
        })
        .unzip()
}

pub(super) fn verify(
    tx: &Transaction<'_>,
    tenant: &str,
    link_id: &str,
    body: &Value,
    repos: &Path,
) -> Outcome {
    let (mut all, index, now) = match load_in_state(
        tx,
        tenant,
        link_id,
        "awaiting_verify",
        "Link is not awaiting verify",
    )? {
        Ok(found) => found,
        Err(response) => return Ok(Ok(response)),
    };
    let extra = match body.get("additional_decisions").map(additional) {
        None => Vec::new(),
        Some(Some(extra)) => extra,
        Some(None) => return refuse(400, "Invalid additional part decisions"),
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
    if units.iter().any(|&(p, u)| unit(&snapshot, p, u).is_none()) {
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
    let units_confirmed = match mark_confirmed(tx, tenant, &snapshot, &decisions)? {
        Ok(newly) => newly,
        Err(response) => return Ok(Ok(response)),
    };
    let (rows, outcomes) = record(&snapshot, &decisions);
    let mut resolved_units = link["resolved_units"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    resolved_units.extend(rows);
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
    let state = if fully_done {
        "verified"
    } else {
        "awaiting_verify"
    };
    link.insert("state".into(), state.into());
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
    let (mut all, index, _) = match load_in_state(
        tx,
        tenant,
        link_id,
        "host_failed",
        "Link is not host_failed",
    )? {
        Ok(found) => found,
        Err(response) => return Ok(Ok(response)),
    };
    all[index].insert("state".into(), "dismissed".into());
    let link = links::public(&all[index]);
    links::save(tx, tenant, all, index)?;
    Ok(Ok(Response::ok(Body::Link { link })))
}
