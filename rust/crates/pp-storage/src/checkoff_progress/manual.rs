//! Manual Checkoff bursts: `POST /plans/{id}/checkoff/bursts` records printed
//! and assembled changes to one Build's Required units in one writer
//! transaction, with no printer link, and its undo token reverses the burst as
//! a compensating write.
//!
//! Receipts live under the `checkoff.manual_bursts` setting with the replay,
//! retention and retirement rules of printer bursts ([`super::burst`]). A burst
//! writes only `print_progress` rows and the domain event log: no draft, no
//! printer command, no Spoolman deduction or reservation.

use super::{
    Basis, Body, Outcome, Refusal, Response, archived,
    burst::{
        self, canonical, load_list, mark_undone, replayed, restore_progress, state_changed,
        state_hash,
    },
    complete_rows, current, is_token, links, progress_changed, read,
};
use crate::read_model::{AcceptedRead, Snapshot};
use anyhow::{Result, ensure};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const KEY: &str = "checkoff.manual_bursts";
const RETIRED_KEY: &str = "checkoff.manual_bursts_retired";
pub(super) const MAX_UNITS: usize = 1000;

fn refuse(status: u16, detail: &str) -> Outcome {
    Ok(Ok(Response::failure(status, detail, None)))
}

/// One Required unit and the state the burst sets on it.
struct Target {
    token: String,
    printed: Option<bool>,
    assembled: Option<bool>,
}

/// A validated manual burst for one Build.
pub(super) struct ManualBurst {
    profile: i64,
    key: String,
    hash: String,
    expected: Basis,
    units: Vec<Target>,
}

fn flag(unit: &Value, key: &str) -> std::result::Result<Option<bool>, ()> {
    match unit.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(()),
    }
}

impl ManualBurst {
    /// Checks the burst's shape. Tokens are matched against the Accepted Plan
    /// inside the transaction.
    pub(super) fn parse(profile: i64, body: &Value) -> std::result::Result<Self, Response> {
        let bad = |detail: &str| Response::failure(400, detail, None);
        let Some(object) = body.as_object() else {
            return Err(bad("Request body must be a JSON object"));
        };
        if let Some(extra) = object
            .keys()
            .find(|k| !matches!(k.as_str(), "idempotency_key" | "expected" | "units"))
        {
            return Err(bad(&format!("Unknown field {extra}")));
        }
        let key = object
            .get("idempotency_key")
            .and_then(Value::as_str)
            .map(links::js_trim)
            .filter(|k| !k.is_empty() && k.chars().count() <= 200)
            .ok_or_else(|| bad("idempotency_key must be 1 to 200 characters"))?;
        let expected = object
            .get("expected")
            .cloned()
            .and_then(|value| serde_json::from_value::<Basis>(value).ok())
            .filter(Basis::valid)
            .ok_or_else(|| bad("expected must be the Accepted Plan basis the burst was made on"))?;
        if expected.profile_id != profile {
            return Err(Response::failure(404, "Plan not found", None));
        }
        let units = object
            .get("units")
            .and_then(Value::as_array)
            .filter(|units| (1..=MAX_UNITS).contains(&units.len()))
            .ok_or_else(|| bad("units must list 1 to 1000 Required units"))?;
        let mut seen = BTreeSet::new();
        let mut targets = Vec::with_capacity(units.len());
        for (i, unit) in units.iter().enumerate() {
            let Some(fields) = unit.as_object() else {
                return Err(bad(&format!("units[{i}] must be an object")));
            };
            if fields
                .keys()
                .any(|k| !matches!(k.as_str(), "token" | "printed" | "assembled"))
            {
                return Err(bad(&format!(
                    "units[{i}] takes only token, printed and assembled"
                )));
            }
            let token = fields
                .get("token")
                .and_then(Value::as_str)
                .filter(|t| is_token(t))
                .ok_or_else(|| bad(&format!("units[{i}].token must be a Required-unit token")))?;
            let (Ok(printed), Ok(assembled)) = (flag(unit, "printed"), flag(unit, "assembled"))
            else {
                return Err(bad(&format!(
                    "units[{i}].printed and assembled must be booleans"
                )));
            };
            if printed.is_none() && assembled.is_none() {
                return Err(bad(&format!(
                    "units[{i}] sets neither printed nor assembled"
                )));
            }
            if !seen.insert(token) {
                return Err(bad(&format!("units[{i}] repeats a Required unit")));
            }
            targets.push(Target {
                token: token.to_owned(),
                printed,
                assembled,
            });
        }
        Ok(Self {
            profile,
            key: key.to_owned(),
            hash: hex::encode(Sha256::digest(canonical(body).to_string())),
            expected,
            units: targets,
        })
    }
}

/// Counts of the units whose printed or assembled state the burst changed.
#[derive(Default)]
struct Moved {
    printed_added: usize,
    printed_removed: usize,
    assembled_added: usize,
    assembled_removed: usize,
}

/// A Part's final (printed, assembled) state per unit, by its position in the
/// snapshot.
type Finals = BTreeMap<usize, Vec<(bool, bool)>>;

/// The state the burst leaves on each named Part, or the refusal. Printed
/// units fill from the left: printing a unit prints every lower unit, and
/// clearing one clears every higher unit and its assembly.
fn plan(snapshot: &Snapshot, units: &[Target]) -> std::result::Result<Finals, Response> {
    let refuse = |status: u16, detail: String| Response::failure(status, &detail, None);
    let mut located = Vec::with_capacity(units.len());
    for (i, target) in units.iter().enumerate() {
        let found = snapshot.parts.iter().enumerate().find_map(|(p, part)| {
            let u = part.units.iter().position(|u| u.token == target.token)?;
            part.included.then_some((p, u))
        });
        located.push(found.ok_or_else(|| {
            refuse(
                404,
                format!("units[{i}]: Required unit not found on this Build's Accepted Plan"),
            )
        })?);
    }
    let mut bounds: BTreeMap<usize, (Option<i64>, Option<i64>)> = BTreeMap::new();
    let mut finals = Finals::new();
    for (target, &(p, u)) in units.iter().zip(&located) {
        let part = &snapshot.parts[p];
        finals.entry(p).or_insert_with(|| {
            part.units
                .iter()
                .map(|u| (u.completed, u.assembled))
                .collect()
        });
        let index = part.units[u].unit_index;
        let (highest_printed, lowest_cleared) = bounds.entry(p).or_default();
        match target.printed {
            Some(true) => *highest_printed = (*highest_printed).max(Some(index)),
            Some(false) => *lowest_cleared = Some(lowest_cleared.map_or(index, |m| m.min(index))),
            None => {}
        }
    }
    for (&p, &(high, low)) in &bounds {
        let part = &snapshot.parts[p];
        if let (Some(high), Some(low)) = (high, low)
            && high >= low
        {
            return Err(refuse(
                400,
                format!(
                    "Part {}: the burst prints unit {} and clears unit {}, but printed units fill from the left",
                    part.projection_part_id,
                    high + 1,
                    low + 1
                ),
            ));
        }
        let Some(state) = finals.get_mut(&p) else {
            continue;
        };
        for (slot, unit) in state.iter_mut().zip(&part.units) {
            if high.is_some_and(|high| unit.unit_index <= high) {
                slot.0 = true;
            }
            if low.is_some_and(|low| unit.unit_index >= low) {
                *slot = (false, false);
            }
        }
    }
    for (i, (target, &(p, u))) in units.iter().zip(&located).enumerate() {
        let (Some(assembled), Some(state)) = (target.assembled, finals.get_mut(&p)) else {
            continue;
        };
        if assembled && !state[u].0 {
            return Err(refuse(
                400,
                format!("units[{i}]: an unprinted unit cannot be assembled"),
            ));
        }
        state[u].1 = assembled;
    }
    Ok(finals)
}

/// What a burst wrote: counts, the rows as they were, the units it changed and
/// each named Part's new counts.
#[derive(Default)]
struct Written {
    moved: Moved,
    before: Vec<Value>,
    touched: BTreeSet<(i64, i64)>,
    parts: Vec<Value>,
}

fn write(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    snapshot: &Snapshot,
    finals: &Finals,
) -> Result<Written> {
    let mut out = Written::default();
    for (&p, state) in finals {
        let part = &snapshot.parts[p];
        let part_id = part.projection_part_id;
        let mut changed = false;
        for (unit, &(printed, assembled)) in part.units.iter().zip(state) {
            if (unit.completed, unit.assembled) == (printed, assembled) {
                continue;
            }
            let rows = tx.execute(
                "UPDATE print_progress SET completed=?1, assembled=?2 WHERE tenant_id=?3 AND part_id=?4 AND unit_index=?5",
                params![printed, assembled, tenant, part_id, unit.unit_index],
            )?;
            ensure!(
                rows == 1,
                "Progress update did not find exactly one current unit"
            );
            changed = true;
            let moved = &mut out.moved;
            moved.printed_added += usize::from(printed && !unit.completed);
            moved.printed_removed += usize::from(!printed && unit.completed);
            moved.assembled_added += usize::from(assembled && !unit.assembled);
            moved.assembled_removed += usize::from(!assembled && unit.assembled);
            out.before.push(json!({ "part_id": part_id, "unit_index": unit.unit_index, "completed": unit.completed, "assembled": unit.assembled }));
            out.touched.insert((part_id, unit.unit_index));
        }
        if changed {
            progress_changed(tx, tenant, profile, Some(part_id))?;
        }
        out.parts.push(json!({
            "part_id": part_id,
            "quantity": part.units.len(),
            "printed_count": state.iter().filter(|s| s.0).count(),
            "assembled_count": state.iter().filter(|s| s.1).count(),
        }));
    }
    Ok(out)
}

pub(super) fn apply(
    tx: &Transaction<'_>,
    tenant: &str,
    burst: ManualBurst,
    repos: &Path,
) -> Outcome {
    let mut receipts = load_list(tx, tenant, KEY)?;
    let undone = "This Checkoff burst was undone; send the change again as a new burst";
    let key = (burst.key.as_str(), burst.hash.as_str());
    if let Some(answer) = replayed(tx, tenant, &receipts, key, RETIRED_KEY, Some(undone))? {
        return Ok(Ok(answer));
    }
    let snapshot = match current(tx, tenant, &burst.expected, repos)? {
        Ok(snapshot) => snapshot,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if archived(&snapshot) {
        return Ok(Err(Refusal::Archived));
    }
    complete_rows(tx, tenant, &snapshot)?;
    let finals = match plan(&snapshot, &burst.units) {
        Ok(finals) => finals,
        Err(refused) => return Ok(Ok(refused)),
    };
    let Written {
        moved,
        before,
        touched,
        parts,
    } = write(tx, tenant, burst.profile, &snapshot, &finals)?;
    let token = format!("ppm_{}", hex::encode(rand::random::<[u8; 16]>()));
    let response = json!({
        "idempotency_key": burst.key,
        "undo_token": token,
        "profile_id": burst.profile,
        "printed_added": moved.printed_added,
        "printed_removed": moved.printed_removed,
        "assembled_added": moved.assembled_added,
        "assembled_removed": moved.assembled_removed,
        "parts": parts,
    });
    receipts.push(json!({
        "idempotency_key": burst.key,
        "request_hash": burst.hash,
        "undo_token": token,
        "profile_id": burst.profile,
        "at": links::now(),
        "response": response,
        "undo": {
            "basis": serde_json::to_value(&burst.expected)?,
            "progress": before,
            "units": touched.iter().map(|&(part, index)| json!([part, index])).collect::<Vec<_>>(),
            "state": state_hash(tx, tenant, &touched)?,
        },
    }));
    burst::save_receipts_under(tx, tenant, (KEY, RETIRED_KEY), &receipts)?;
    Ok(Ok(Response::ok(Body::Receipt(response))))
}

/// A manual burst or its undo.
pub(super) enum Op {
    Burst(ManualBurst),
    Undo {
        profile: i64,
        token: String,
    },
    /// `POST /printer-checkoff/{id}/manual-complete`: a print on a printer
    /// tracked by hand has finished (GRE-1169).
    #[cfg(feature = "desktop_rebuild")]
    CompleteLink {
        link_id: String,
    },
}

pub(super) fn run(tx: &Transaction<'_>, tenant: &str, op: Op, repos: &Path) -> Outcome {
    match op {
        Op::Burst(burst) => apply(tx, tenant, burst, repos),
        Op::Undo { profile, token } => undo(tx, tenant, profile, &token, repos),
        #[cfg(feature = "desktop_rebuild")]
        Op::CompleteLink { link_id } => super::verify::manual_complete(tx, tenant, &link_id),
    }
}

/// Why undo must refuse, or `None` when the Accepted Plan and everything that
/// can depend on the burst's units are as the burst left them.
fn undo_conflict(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    undo: &Value,
    repos: &Path,
) -> Result<std::result::Result<Option<String>, Refusal>> {
    let changed_plan = format!("The Accepted Plan for Build {profile} changed since this burst");
    let snapshot = match read(tx, tenant, profile, repos)? {
        AcceptedRead::Ready { snapshot } => snapshot,
        _ => return Ok(Ok(Some(changed_plan))),
    };
    if archived(&snapshot) {
        return Ok(Err(Refusal::Archived));
    }
    if serde_json::to_value(Basis::from(&*snapshot))? != undo["basis"] {
        return Ok(Ok(Some(changed_plan)));
    }
    if state_changed(tx, tenant, undo)? {
        return Ok(Ok(Some(
            "Checkoff changed since this burst: a later change on its Parts depends on it".into(),
        )));
    }
    Ok(Ok(None))
}

pub(super) fn undo(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    token: &str,
    repos: &Path,
) -> Outcome {
    let mut receipts = load_list(tx, tenant, KEY)?;
    let Some(position) = receipts
        .iter()
        .position(|r| r["undo_token"] == token && r["profile_id"] == profile)
    else {
        return refuse(404, "Checkoff burst not found");
    };
    if receipts[position].get("undone_at").is_some() {
        return refuse(409, "This Checkoff burst was already undone");
    }
    let undo = receipts[position]["undo"].clone();
    match undo_conflict(tx, tenant, profile, &undo, repos)? {
        Err(refusal) => return Ok(Err(refusal)),
        Ok(Some(reason)) => return refuse(409, &reason),
        Ok(None) => {}
    }
    for part in restore_progress(tx, tenant, &undo)? {
        progress_changed(tx, tenant, profile, Some(part))?;
    }
    let undone_at = mark_undone(tx, tenant, (KEY, RETIRED_KEY), &mut receipts, position)?;
    Ok(Ok(Response::ok(Body::Receipt(json!({
        "undo_token": token,
        "undone_at": undone_at,
    })))))
}
