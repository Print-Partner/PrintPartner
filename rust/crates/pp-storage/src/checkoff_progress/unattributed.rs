//! Unattributed prints: `POST /printer-checkoff/unattributed/:id/claim` and
//! `/dismiss`. Mirrors the routes in `routes/printer-checkoff.ts`, the claim
//! path of `materializeAcceptedPrinterLink` in `db/repository.ts`,
//! `resolveAcceptedPrinterAttribution` in `db/accepted-printer-attribution.ts`
//! and `services/unattributed-print-store.ts`.
//!
//! The prints are one JSON array in the `printer.unattributed_prints` setting.
//! A claim attributes the print's object names to incomplete Required units of
//! the Build, creates its printer Checkoff link awaiting verify, marks every
//! open print of that file on that integration claimed and records the
//! printer's plan binding, with its domain event, in one transaction.

use super::{
    Body, Outcome, Response,
    attribution::{self, Attribution},
    links::{self, field},
};
use anyhow::{Result, anyhow, ensure};
use pp_domain::gcode_objects::group_objects_by_part;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};

const PRINTS: &str = "printer.unattributed_prints";
const BINDINGS: &str = "printer.plan_bindings";
const MAX_ENTRIES: usize = 100;
const RETENTION_MS: i128 = 7 * 24 * 60 * 60 * 1000;

/// A validated claim request.
pub(super) struct Claim {
    id: String,
    profile_id: i64,
    selected: Option<Vec<String>>,
}

fn refuse(status: u16, detail: &str) -> Outcome {
    Ok(Ok(Response::failure(status, detail, None)))
}

/// JavaScript truthiness for the values a stored print carries.
fn truthy(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false)) && value != "" && value != 0
}

/// `Number(value)` for the JSON a request can carry, then a positive
/// `Number.isInteger`.
fn js_positive_integer(value: &Value) -> Option<i64> {
    let number = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) if links::js_trim(s).is_empty() => 0.0,
        Value::String(s) => links::js_trim(s).parse::<f64>().ok()?,
        Value::Bool(b) => f64::from(u8::from(*b)),
        _ => return None,
    };
    (number.is_finite()
        && number.fract() == 0.0
        && (1.0..=9_007_199_254_740_991.0).contains(&number))
    .then_some(number as i64)
}

impl Claim {
    /// The route's checks before it reads any print.
    pub(super) fn parse(id: String, body: &Value) -> std::result::Result<Self, Response> {
        let profile_id = js_positive_integer(field(body, "profile_id"))
            .ok_or_else(|| Response::failure(400, "profile_id is required", None))?;
        let selected = field(body, "selected_stl_basenames")
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| links::js_trim(name).to_lowercase())
                    .filter(|name| !name.is_empty())
                    .collect()
            });
        Ok(Self {
            id,
            profile_id,
            selected,
        })
    }
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

fn put(tx: &Transaction<'_>, tenant: &str, key: &str, value: &Value) -> Result<()> {
    tx.execute(
        "INSERT INTO app_settings (tenant_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (tenant_id, key) DO UPDATE SET value=excluded.value",
        params![tenant, key, serde_json::to_string(value)?],
    )?;
    Ok(())
}

/// `loadRaw`: an unreadable setting reads as empty, and entries without a
/// string id and filename drop out.
fn load_raw(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<Value>> {
    let raw = setting(tx, tenant, PRINTS)?.unwrap_or_default();
    if links::js_trim(&raw).is_empty() {
        return Ok(Vec::new());
    }
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&links::js_json(&raw)) else {
        return Ok(Vec::new());
    };
    Ok(items
        .into_iter()
        .filter(|p| p["id"].is_string() && p["filename"].is_string())
        .collect())
}

fn epoch_ms(iso: &Value) -> Option<i128> {
    let parsed = time::OffsetDateTime::parse(
        iso.as_str()?,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()?;
    Some(parsed.unix_timestamp_nanos() / 1_000_000)
}

/// `pruneEntries`: terminal entries stay seven days so an unchanged completed
/// host job is not rediscovered, then the newest 100 by `completed_at` stay.
fn prune(prints: Vec<Value>) -> Vec<Value> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    let mut kept: Vec<Value> = prints
        .into_iter()
        .filter(|p| {
            let terminal = match (truthy(&p["dismissed"]), truthy(&p["dismissed_at"])) {
                (true, true) => &p["dismissed_at"],
                (true, false) => &p["completed_at"],
                (false, _) => &p["claimed_at"],
            };
            !truthy(terminal) || epoch_ms(terminal).is_none_or(|at| now - at <= RETENTION_MS)
        })
        .collect();
    if kept.len() > MAX_ENTRIES {
        kept.sort_by_key(|p| links::js_string(&p["completed_at"]));
        kept.drain(..kept.len() - MAX_ENTRIES);
    }
    kept
}

/// `listUnattributedPrints`: every stored print that pruning keeps.
pub(super) fn list(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<Value>> {
    Ok(prune(load_raw(tx, tenant)?))
}

fn save(tx: &Transaction<'_>, tenant: &str, prints: Vec<Value>) -> Result<()> {
    put(tx, tenant, PRINTS, &Value::Array(prune(prints)))
}

fn resolved(tx: &Transaction<'_>, tenant: &str, print_id: &str, payload: Value) -> Result<()> {
    crate::domain_events::append(
        tx,
        tenant,
        crate::domain_events::Change {
            kind: crate::domain_events::UNATTRIBUTED_RESOLVED,
            aggregate_type: "unattributed_print",
            aggregate_id: print_id.to_owned(),
            aggregate_version: None,
            payload,
        },
    )
}

/// `dismissUnattributedPrint`: any stored print, claimed or not.
pub(super) fn dismiss(tx: &Transaction<'_>, tenant: &str, id: &str) -> Outcome {
    let mut all = load_raw(tx, tenant)?;
    let Some(print) = all.iter_mut().find(|p| p["id"] == id) else {
        return refuse(404, "Unattributed print not found");
    };
    print["dismissed"] = Value::Bool(true);
    print["dismissed_at"] = Value::from(links::now());
    save(tx, tenant, all)?;
    resolved(tx, tenant, id, json!({ "dismissed": true }))?;
    Ok(Ok(Response::ok(Body::Dismissed { ok: true })))
}

/// The object names a claim attributes: every object on the plate, or only the
/// objects of the selected plate files. `Err` holds the route's 400 detail.
fn plate_objects(
    print: &Value,
    selected: Option<&[String]>,
) -> std::result::Result<Vec<String>, &'static str> {
    let objects: Vec<String> = print["gcode_objects"]
        .as_array()
        .map(|names| names.iter().map(links::js_string).collect())
        .unwrap_or_default();
    let Some(selected) = selected else {
        return Ok(objects);
    };
    if selected.is_empty() {
        return Err("Select at least one plate file");
    }
    let selected: HashSet<&str> = selected.iter().map(String::as_str).collect();
    let names: Vec<String> = group_objects_by_part(&objects)
        .into_iter()
        .filter(|group| selected.contains(group.stl_basename.to_lowercase().as_str()))
        .flat_map(|group| group.objects.into_iter().map(|o| o.name))
        .collect();
    if names.is_empty() {
        return Err("Selected files are not on this plate");
    }
    Ok(names)
}

/// The claim route and the claim path of `materializeAcceptedPrinterLink`.
pub(super) fn claim(tx: &Transaction<'_>, tenant: &str, claim: Claim, repos: &Path) -> Outcome {
    let prints = prune(load_raw(tx, tenant)?);
    let Some(print) = prints
        .iter()
        .find(|p| p["id"] == claim.id.as_str())
        .cloned()
    else {
        return refuse(404, "Unattributed print not found");
    };
    if truthy(&print["claimed_at"]) {
        return refuse(409, "Print already claimed");
    }
    let object_names = match plate_objects(&print, claim.selected.as_deref()) {
        Ok(names) => names,
        Err(detail) => return refuse(400, detail),
    };
    if truthy(&print["dismissed"]) {
        return refuse(409, "Print changed or was already claimed");
    }
    let snapshot = match attribution::accepted(tx, tenant, claim.profile_id, repos)? {
        Ok(snapshot) => snapshot,
        Err(refusal) => return Ok(refusal),
    };
    let attribution = attribution::resolve(
        &snapshot,
        &object_names,
        Some(&links::js_string(&print["filename"])),
        false,
    );
    if attribution.units.is_empty() {
        return refuse(
            409,
            "That print file does not map to an incomplete Required unit in this Build",
        );
    }

    record(
        tx,
        tenant,
        &claim,
        &print,
        prints,
        snapshot.revision_id,
        attribution,
    )
}

/// Creates the claim's link, marks every open print of that file claimed,
/// records the plan binding and appends the event, refusing a file that already
/// has an open link.
fn record(
    tx: &Transaction<'_>,
    tenant: &str,
    claim: &Claim,
    print: &Value,
    mut prints: Vec<Value>,
    revision_id: i64,
    attribution: Attribution,
) -> Outcome {
    let now = links::now();
    let filename = links::normalized_filename(print["filename"].as_str().unwrap_or_default());
    let mut all = links::load(tx, tenant, &now)?;
    if all.iter().any(|link| {
        matches!(link["state"].as_str(), Some("watching" | "awaiting_verify"))
            && link["integration_id"] == print["integration_id"]
            && links::normalized_filename(link["filename"].as_str().unwrap_or_default()) == filename
    }) {
        return refuse(409, "That print file is already assigned");
    }
    let raw = json!({
        "id": links::uuid(),
        "profile_id": claim.profile_id,
        "integration_id": print["integration_id"],
        "printer_id": print["printer_id"],
        "host_name": print["host_name"],
        "filename": print["filename"],
        "plan_revision_id": revision_id,
        "units": attribution.units,
        "unlabeled_names": attribution.unmatched_object_names,
        "state": "awaiting_verify",
        "host_outcome": "success",
        "saw_active": true,
        "started": true,
        "created_at": now,
        "completed_at": print["completed_at"],
    });
    let link =
        links::parse(&raw, &now).ok_or_else(|| anyhow!("Accepted printer link creation failed"))?;
    let public = links::public(&link);
    all.push(link);
    links::save_all(tx, tenant, all)?;

    for open in prints.iter_mut().filter(|p| {
        !truthy(&p["claimed_at"])
            && !truthy(&p["dismissed"])
            && p["integration_id"] == print["integration_id"]
            && links::normalized_filename(p["filename"].as_str().unwrap_or_default()) == filename
    }) {
        open["claimed_at"] = Value::from(now.clone());
        open["claimed_profile_id"] = Value::from(claim.profile_id);
    }
    save(tx, tenant, prints)?;

    let mut bindings = plan_bindings(setting(tx, tenant, BINDINGS)?)?;
    let binding = json!({
        "integration_id": print["integration_id"],
        "profile_id": claim.profile_id,
        "updated_at": now,
    });
    match bindings
        .iter_mut()
        .find(|b| b["integration_id"] == print["integration_id"])
    {
        Some(existing) => *existing = binding,
        None => bindings.push(binding),
    }
    put(tx, tenant, BINDINGS, &Value::Array(bindings))?;

    resolved(
        tx,
        tenant,
        &claim.id,
        json!({ "profile_id": claim.profile_id, "link_id": public["id"] }),
    )?;
    Ok(Ok(Response::ok(Body::Claimed {
        link: public,
        ok: true,
    })))
}

/// `parsePrinterPlanBindings`: a corrupt list fails the claim.
fn plan_bindings(raw: Option<String>) -> Result<Vec<Value>> {
    let raw = raw.unwrap_or_default();
    if links::js_trim(&raw).is_empty() {
        return Ok(Vec::new());
    }
    let corrupt = "Printer plan bindings are corrupt";
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&raw) else {
        return Err(anyhow!(corrupt));
    };
    for item in &items {
        let profile = &item["profile_id"];
        ensure!(
            item["integration_id"]
                .as_str()
                .is_some_and(|i| !links::js_trim(i).is_empty())
                && (profile.is_null() || profile.as_i64().is_some_and(|p| p > 0))
                && item["updated_at"].is_string(),
            corrupt
        );
    }
    Ok(items)
}
