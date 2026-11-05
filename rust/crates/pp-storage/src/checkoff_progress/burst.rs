//! Checkoff bursts: `POST /printer-checkoff/verify-batch` confirms what came
//! off several beds in one writer transaction, and its undo token reverses the
//! burst as a compensating write.
//!
//! A burst stores its receipt in the same transaction as its writes, under the
//! `printer.checkoff_bursts` setting: the idempotency key, the canonical request
//! hash, the original response and what undo must restore. A repeated key with
//! the same request returns that response before any other check and writes
//! nothing; a changed request under the same key is refused. Undo restores the
//! links, progress rows and outcome rows the burst wrote, and refuses when any
//! of them, or a touched Accepted Plan, changed since.

use super::{
    Basis, Body, Outcome, Refusal, Response, archived,
    links::{self, Link, field},
    progress_changed, read, verify,
};
use crate::read_model::AcceptedRead;
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

const KEY: &str = "printer.checkoff_bursts";
const RETIRED_KEY: &str = "printer.checkoff_bursts_retired";
const MAX_RECEIPTS: usize = 100;
const MAX_RETIRED: usize = 5000;
pub(super) const MAX_BEDS: usize = 50;

fn refuse(status: u16, detail: &str) -> Outcome {
    Ok(Ok(Response::failure(status, detail, None)))
}

/// A validated burst request.
pub(super) struct Burst {
    key: String,
    hash: String,
    beds: Vec<(String, Value)>,
}

/// Object keys sorted at every level, so key order never changes the hash.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => items.iter().map(canonical).collect(),
        other => other.clone(),
    }
}

impl Burst {
    /// Checks the burst's shape. Each bed is checked against its link inside
    /// the transaction, as `POST /printer-checkoff/verify` checks it.
    pub(super) fn parse(body: &Value) -> std::result::Result<Self, Response> {
        let bad = |detail: &str| Response::failure(400, detail, None);
        let key = field(body, "idempotency_key")
            .as_str()
            .map(links::js_trim)
            .filter(|k| !k.is_empty() && k.chars().count() <= 200)
            .ok_or_else(|| bad("idempotency_key must be 1 to 200 characters"))?;
        let beds = field(body, "beds")
            .as_array()
            .filter(|beds| (1..=MAX_BEDS).contains(&beds.len()))
            .ok_or_else(|| bad("beds must list 1 to 50 beds"))?;
        let mut seen = BTreeSet::new();
        let mut parsed = Vec::with_capacity(beds.len());
        for (i, bed) in beds.iter().enumerate() {
            let id = match field(bed, "link_id") {
                Value::Null => String::new(),
                other => links::js_trim(&links::js_string(other)).to_owned(),
            };
            if !bed.is_object() || id.is_empty() {
                return Err(bad(&format!("beds[{i}].link_id is required")));
            }
            if !seen.insert(id.clone()) {
                return Err(bad(&format!("beds[{i}] repeats a link")));
            }
            parsed.push((id, bed.clone()));
        }
        let hash = hex::encode(Sha256::digest(canonical(body).to_string()));
        Ok(Self {
            key: key.to_owned(),
            hash,
            beds: parsed,
        })
    }
}

/// `undo_token` in an undo request body.
pub(super) fn undo_token(body: &Value) -> std::result::Result<String, Response> {
    field(body, "undo_token")
        .as_str()
        .map(links::js_trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Response::failure(400, "undo_token is required", None))
}

fn setting(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key],
            |r| r.get(0),
        )
        .optional()?)
}

fn load_list(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Vec<Value>> {
    Ok(
        match setting(tx, tenant, key)?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()?
        {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        },
    )
}

fn save_list(tx: &Transaction<'_>, tenant: &str, key: &str, items: &[Value]) -> Result<()> {
    tx.execute(
        "INSERT INTO app_settings (tenant_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (tenant_id, key) DO UPDATE SET value=excluded.value",
        params![tenant, key, serde_json::to_string(items)?],
    )?;
    Ok(())
}

fn load_receipts(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<Value>> {
    load_list(tx, tenant, KEY)
}

fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Keeps the newest receipts. A dropped receipt leaves the hash of its key
/// behind, so replaying that key is refused instead of running again.
fn save_receipts(tx: &Transaction<'_>, tenant: &str, receipts: &[Value]) -> Result<()> {
    let (dropped, kept) = receipts.split_at(receipts.len().saturating_sub(MAX_RECEIPTS));
    if !dropped.is_empty() {
        let mut retired = load_list(tx, tenant, RETIRED_KEY)?;
        retired.extend(
            dropped
                .iter()
                .map(|r| Value::from(key_hash(r["idempotency_key"].as_str().unwrap_or_default()))),
        );
        save_list(
            tx,
            tenant,
            RETIRED_KEY,
            &retired[retired.len().saturating_sub(MAX_RETIRED)..],
        )?;
    }
    save_list(tx, tenant, KEY, kept)
}

/// SHA-256 of what can depend on the burst's units: every progress row of
/// their Parts, the outcome rows that name one of them, and every stored link
/// whose units or resolved units name one of them. Undo compares it with the
/// value the burst left, so unrelated prints, jobs and links do not block it.
fn state_hash(tx: &Transaction<'_>, tenant: &str, units: &BTreeSet<(i64, i64)>) -> Result<String> {
    let mut hasher = Sha256::new();
    let parts: BTreeSet<i64> = units.iter().map(|&(part, _)| part).collect();
    let mut progress = tx.prepare(
        "SELECT unit_index || ':' || completed || ':' || assembled FROM print_progress
         WHERE tenant_id=?1 AND part_id=?2 ORDER BY unit_index",
    )?;
    for part in &parts {
        hasher.update(part.to_string());
        for row in progress.query_map(params![tenant, part], |r| r.get::<_, String>(0))? {
            hasher.update(row?);
            hasher.update([1]);
        }
    }
    hasher.update([0]);
    let mut outcomes = tx.prepare(
        "SELECT CAST(id AS TEXT) FROM print_job_parts
         WHERE tenant_id=?1 AND part_id=?2 AND unit_index=?3 ORDER BY id",
    )?;
    for &(part, index) in units {
        for id in outcomes.query_map(params![tenant, part, index], |r| r.get::<_, String>(0))? {
            hasher.update(id?);
            hasher.update([1]);
        }
    }
    hasher.update([0]);
    let names = |link: &Value, key: &str| {
        link[key].as_array().into_iter().flatten().any(|u| {
            matches!(
                (links::js_integer(u.get("part_id"), 1.0), links::js_integer(u.get("unit_index"), 0.0)),
                (Some(part), Some(index)) if units.contains(&(part, index))
            )
        })
    };
    let stored = setting(tx, tenant, links::KEY)?
        .and_then(|raw| serde_json::from_str::<Value>(&links::js_json(&raw)).ok());
    for link in stored.iter().filter_map(Value::as_array).flatten() {
        if names(link, "units") || names(link, "resolved_units") {
            hasher.update(link.to_string());
            hasher.update([1]);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn progress_row(
    tx: &Transaction<'_>,
    tenant: &str,
    part: i64,
    index: i64,
) -> Result<Option<(bool, bool)>> {
    Ok(tx
        .query_row(
            "SELECT completed, assembled FROM print_progress WHERE tenant_id=?1 AND part_id=?2 AND unit_index=?3",
            params![tenant, part, index],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

fn public_links(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<Value>> {
    Ok(links::load(tx, tenant, &links::now())?
        .iter()
        .map(links::public)
        .collect())
}

fn find<'a>(links: &'a [Value], id: &str) -> Option<&'a Value> {
    links.iter().find(|link| link["id"] == id)
}

/// The basis of each touched profile's Accepted Plan, for undo's conflict check.
fn bases(
    tx: &Transaction<'_>,
    tenant: &str,
    profiles: &BTreeSet<i64>,
    repos: &Path,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for &profile in profiles {
        if let AcceptedRead::Ready { snapshot } = read(tx, tenant, profile, repos)? {
            out.push(serde_json::to_value(Basis::from(&*snapshot))?);
        }
    }
    Ok(out)
}

/// The coordinates a burst's decisions name, the ones it confirms, and the
/// progress rows they had before the burst.
struct Decided {
    units: BTreeSet<(i64, i64)>,
    confirmed: BTreeSet<(i64, i64)>,
    touched: Vec<(i64, i64, (bool, bool))>,
}

fn decided(tx: &Transaction<'_>, tenant: &str, beds: &[(String, Value)]) -> Result<Decided> {
    let mut out = Decided {
        units: BTreeSet::new(),
        confirmed: BTreeSet::new(),
        touched: Vec::new(),
    };
    for (_, bed) in beds {
        for d in field(bed, "decisions").as_array().into_iter().flatten() {
            let (Some(part), Some(index)) = (
                links::js_integer(d.get("part_id"), 1.0),
                links::js_integer(d.get("unit_index"), 0.0),
            ) else {
                continue;
            };
            out.units.insert((part, index));
            if d.get("result").and_then(Value::as_str) == Some("confirmed") {
                out.confirmed.insert((part, index));
            }
            if let Some(row) = progress_row(tx, tenant, part, index)? {
                out.touched.push((part, index, row));
            }
        }
    }
    out.touched.sort_unstable();
    out.touched.dedup();
    Ok(out)
}

/// The progress rows the burst changed, as they were before it.
fn changed_progress(
    tx: &Transaction<'_>,
    tenant: &str,
    touched: Vec<(i64, i64, (bool, bool))>,
) -> Result<Vec<Value>> {
    let mut progress = Vec::new();
    for (part, index, (completed, assembled)) in touched {
        if progress_row(tx, tenant, part, index)? != Some((completed, assembled)) {
            progress.push(json!({ "part_id": part, "unit_index": index, "completed": completed, "assembled": assembled }));
        }
    }
    Ok(progress)
}

/// Each bed's link before and after the burst, with its place before it.
fn undo_links(beds: &[(String, Value)], before: &[Value], after: &[Value]) -> Vec<Value> {
    beds.iter()
        .map(|(id, _)| {
            let index = before.iter().position(|link| link["id"] == id.as_str());
            json!({ "id": id, "index": index, "before": find(before, id), "after": find(after, id) })
        })
        .collect()
}

pub(super) fn verify_batch(
    tx: &Transaction<'_>,
    tenant: &str,
    burst: Burst,
    repos: &Path,
) -> Outcome {
    let mut receipts = load_receipts(tx, tenant)?;
    if let Some(receipt) = receipts
        .iter()
        .find(|r| r["idempotency_key"] == burst.key.as_str())
    {
        if receipt["request_hash"] != burst.hash.as_str() {
            return refuse(
                409,
                "idempotency_key was already used for a different request",
            );
        }
        return Ok(Ok(Response::ok(Body::Receipt(receipt["response"].clone()))));
    }
    if load_list(tx, tenant, RETIRED_KEY)?.contains(&Value::from(key_hash(&burst.key))) {
        return refuse(
            409,
            "idempotency_key belongs to a Checkoff burst too old to replay",
        );
    }
    let before = public_links(tx, tenant)?;
    let Decided {
        units,
        confirmed,
        touched,
    } = decided(tx, tenant, &burst.beds)?;
    let mut results = Vec::with_capacity(burst.beds.len());
    for (i, (id, bed)) in burst.beds.iter().enumerate() {
        match verify::verify(tx, tenant, id, bed, repos, &confirmed)? {
            Ok(Response {
                status: 200,
                body:
                    Body::Verified {
                        link,
                        units_confirmed,
                        units_rejected,
                        outcomes,
                    },
            }) => {
                results.push((link, units_confirmed, units_rejected, outcomes));
            }
            Ok(Response {
                status,
                body: Body::Failure { detail, .. },
            }) => {
                return refuse(status, &format!("beds[{i}]: {detail}"));
            }
            Ok(_) => return Ok(Err(Refusal::Internal)),
            Err(refusal) => return Ok(Err(refusal)),
        }
    }
    let after = public_links(tx, tenant)?;
    let progress = changed_progress(tx, tenant, touched)?;
    let outcome_ids: Vec<Value> = results
        .iter()
        .flat_map(|r| r.3.iter().map(|o| o["id"].clone()))
        .collect();
    let profiles: BTreeSet<i64> = results
        .iter()
        .filter_map(|r| r.0["profile_id"].as_i64())
        .collect();
    let undo_links = undo_links(&burst.beds, &before, &after);
    let token = format!("ppb_{}", hex::encode(rand::random::<[u8; 16]>()));
    let response = json!({
        "idempotency_key": burst.key,
        "undo_token": token,
        "units_confirmed": results.iter().map(|r| r.1).sum::<usize>(),
        "units_rejected": results.iter().map(|r| r.2).sum::<usize>(),
        "beds": results.iter().map(|(link, confirmed, rejected, outcomes)| json!({
            "link": link, "units_confirmed": confirmed, "units_rejected": rejected, "outcomes": outcomes,
        })).collect::<Vec<_>>(),
    });
    let state = state_hash(tx, tenant, &units)?;
    receipts.push(json!({
        "idempotency_key": burst.key,
        "request_hash": burst.hash,
        "undo_token": token,
        "at": links::now(),
        "response": response,
        "undo": {
            "links": undo_links,
            "progress": progress,
            "outcomes": outcome_ids,
            "bases": bases(tx, tenant, &profiles, repos)?,
            "units": units.iter().map(|&(part, index)| json!([part, index])).collect::<Vec<_>>(),
            "state": state,
        },
    }));
    save_receipts(tx, tenant, &receipts)?;
    Ok(Ok(Response::ok(Body::Receipt(response))))
}

/// The reason undo must refuse, or `None` when everything the burst wrote is
/// still exactly as the burst left it.
fn undo_conflict(
    tx: &Transaction<'_>,
    tenant: &str,
    undo: &Value,
    current: &[Value],
    repos: &Path,
) -> Result<Option<String>> {
    for basis in undo["bases"].as_array().into_iter().flatten() {
        let profile = basis["profile_id"].as_i64().unwrap_or_default();
        let unchanged = match read(tx, tenant, profile, repos)? {
            AcceptedRead::Ready { snapshot } => {
                serde_json::to_value(Basis::from(&*snapshot))? == *basis
            }
            _ => false,
        };
        if !unchanged {
            return Ok(Some(format!(
                "The Accepted Plan for Build {profile} changed since this burst"
            )));
        }
    }
    for link in undo["links"].as_array().into_iter().flatten() {
        let id = link["id"].as_str().unwrap_or_default();
        if find(current, id) != Some(&link["after"]) || !link["before"].is_object() {
            return Ok(Some(format!("Checkoff link {id} changed since this burst")));
        }
    }
    for row in undo["progress"].as_array().into_iter().flatten() {
        let (part, index) = (
            row["part_id"].as_i64().unwrap_or_default(),
            row["unit_index"].as_i64().unwrap_or_default(),
        );
        let assembled = row["assembled"].as_bool().unwrap_or_default();
        if progress_row(tx, tenant, part, index)? != Some((true, assembled)) {
            return Ok(Some(format!(
                "Progress for Part {part} unit {index} changed since this burst"
            )));
        }
    }
    let ids = undo["outcomes"].as_array().cloned().unwrap_or_default();
    for id in &ids {
        let present: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM print_job_parts WHERE tenant_id=?1 AND id=?2)",
            params![tenant, id.as_str()],
            |r| r.get(0),
        )?;
        if !present {
            return Ok(Some(
                "A print outcome this burst recorded was removed".into(),
            ));
        }
    }
    if undo.get("state").is_none() || undo.get("units").is_none() {
        return Ok(Some(
            "This Checkoff burst predates the undo state check and cannot be undone".into(),
        ));
    }
    let units: BTreeSet<(i64, i64)> = undo["units"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|unit| Some((unit[0].as_i64()?, unit[1].as_i64()?)))
        .collect();
    if undo["state"] != state_hash(tx, tenant, &units)?.as_str() {
        return Ok(Some(
            "Checkoff changed since this burst: a later verify or progress change on its Parts depends on it".into(),
        ));
    }
    Ok(None)
}

pub(super) fn undo_batch(tx: &Transaction<'_>, tenant: &str, token: &str, repos: &Path) -> Outcome {
    let mut receipts = load_receipts(tx, tenant)?;
    let Some(position) = receipts.iter().position(|r| r["undo_token"] == token) else {
        return refuse(404, "Checkoff burst not found");
    };
    if receipts[position].get("undone_at").is_some() {
        return refuse(409, "This Checkoff burst was already undone");
    }
    let undo = receipts[position]["undo"].clone();
    for basis in undo["bases"].as_array().into_iter().flatten() {
        let profile = basis["profile_id"].as_i64().unwrap_or_default();
        if let AcceptedRead::Ready { snapshot } = read(tx, tenant, profile, repos)?
            && archived(&snapshot)
        {
            return Ok(Err(Refusal::Archived));
        }
    }
    let mut all = links::load(tx, tenant, &links::now())?;
    let current: Vec<Value> = all.iter().map(links::public).collect();
    if let Some(reason) = undo_conflict(tx, tenant, &undo, &current, repos)? {
        return refuse(409, &reason);
    }
    // Each touched link goes back to its content and place before the burst.
    let mut touched: Vec<&Value> = undo["links"].as_array().into_iter().flatten().collect();
    touched.sort_by_key(|link| link["index"].as_u64());
    all.retain(|l| !touched.iter().any(|link| link["id"] == l["id"]));
    let mut restored = Vec::new();
    for link in touched {
        if let Value::Object(before) = &link["before"] {
            let index = link["index"]
                .as_u64()
                .map_or(all.len(), |i| (i as usize).min(all.len()));
            all.insert(index, Link::clone(before));
            restored.push(link["before"].clone());
        }
    }
    links::save_all(tx, tenant, all)?;
    let mut moved = BTreeSet::new();
    for row in undo["progress"].as_array().into_iter().flatten() {
        let changed = tx.execute(
            "UPDATE print_progress SET completed=?1, assembled=?2 WHERE tenant_id=?3 AND part_id=?4 AND unit_index=?5 AND (completed IS NOT ?1 OR assembled IS NOT ?2)",
            params![row["completed"].as_bool(), row["assembled"].as_bool(), tenant, row["part_id"].as_i64(), row["unit_index"].as_i64()],
        )?;
        if changed > 0 {
            moved.extend(row["part_id"].as_i64());
        }
    }
    for part in moved {
        let profile: i64 = tx.query_row(
            "SELECT profile_id FROM parts WHERE tenant_id=?1 AND id=?2",
            params![tenant, part],
            |r| r.get(0),
        )?;
        progress_changed(tx, tenant, profile, Some(part))?;
    }
    for id in undo["outcomes"].as_array().into_iter().flatten() {
        let job: Option<String> = tx.query_row(
            "DELETE FROM print_job_parts WHERE tenant_id=?1 AND id=?2 RETURNING job_id",
            params![tenant, id.as_str()],
            |r| r.get(0),
        )?;
        tx.execute(
            "DELETE FROM print_jobs WHERE tenant_id=?1 AND id=?2
             AND NOT EXISTS (SELECT 1 FROM print_job_parts WHERE job_id=?2)",
            params![tenant, job],
        )?;
    }
    let undone_at = links::now();
    if let Value::Object(receipt) = &mut receipts[position] {
        receipt.insert("undone_at".into(), undone_at.clone().into());
    }
    save_receipts(tx, tenant, &receipts)?;
    let mut body = Map::new();
    body.insert("undo_token".into(), token.into());
    body.insert("undone_at".into(), undone_at.into());
    body.insert("links".into(), restored.into());
    Ok(Ok(Response::ok(Body::Receipt(Value::Object(body)))))
}
