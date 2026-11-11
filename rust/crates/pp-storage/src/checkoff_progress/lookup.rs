//! Read-only lookups for callers that must reconcile a Checkoff burst
//! (GRE-1170): what an idempotency key settled to, and one printer Checkoff
//! link. MCP `get_printer_checkoff` (GRE-1297) reads links and unattributed
//! prints here too. None of them writes Checkoff state.

use super::{
    Body, Outcome, Response,
    burst::{MAX_RETIRED, RETIRED_KEY, key_hash, load_list, load_receipts},
    links, unattributed,
};
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// The link id as the propose step reads it: JS-trimmed and nonempty.
pub(super) fn link_id(raw: &str) -> std::result::Result<String, Response> {
    let id = links::js_trim(raw);
    if id.is_empty() {
        return Err(Response::failure(400, "link_id is required", None));
    }
    Ok(id.to_owned())
}

/// `applied` or `undone` with the stored receipt, `expired` when the receipt
/// was trimmed (the burst ran and its key cannot run again), `unknown` when the
/// key is in neither list but the retired list is full, so an older key may
/// have been dropped, or 404 `burst_not_found` when the key is in neither list
/// and nothing was ever dropped, so no burst used it.
pub(super) fn burst_status(tx: &Transaction<'_>, tenant: &str, key: &str) -> Outcome {
    let status = |state: &str, receipt: Option<&Value>| {
        let receipt = receipt.unwrap_or(&Value::Null);
        Ok(Ok(Response::ok(Body::Receipt(json!({
            "idempotency_key": key,
            "state": state,
            "applied_at": receipt.get("at").cloned().unwrap_or(Value::Null),
            "undone_at": receipt.get("undone_at").cloned().unwrap_or(Value::Null),
            "receipt": receipt.get("response").cloned().unwrap_or(Value::Null),
        })))))
    };
    let receipts = load_receipts(tx, tenant)?;
    if let Some(receipt) = receipts.iter().find(|r| r["idempotency_key"] == key) {
        let state = if receipt.get("undone_at").is_some() {
            "undone"
        } else {
            "applied"
        };
        return status(state, Some(receipt));
    }
    let retired = load_list(tx, tenant, RETIRED_KEY)?;
    if retired.contains(&Value::from(key_hash(key))) {
        return status("expired", None);
    }
    if retired.len() >= MAX_RETIRED {
        return status("unknown", None);
    }
    Ok(Ok(Response::failure(
        404,
        "No Checkoff burst used this idempotency_key, so nothing was applied. Resend the same request with the same key.",
        Some("burst_not_found"),
    )))
}

/// One stored printer Checkoff link, as Node serializes it.
pub(super) fn link(tx: &Transaction<'_>, tenant: &str, id: &str) -> Outcome {
    Ok(Ok(
        match links::load(tx, tenant, &links::now())?
            .into_iter()
            .find(|link| link.get("id").and_then(Value::as_str) == Some(id))
        {
            Some(link) => Response::ok(Body::Link {
                link: links::public(&link),
            }),
            None => Response::failure(404, "Checkoff link not found", None),
        },
    ))
}

/// MCP `get_printer_checkoff` arguments, already read as Node reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// `asInt(plan_id)`.
    pub plan_id: Option<i64>,
    /// The trimmed `state`, empty for any; the caller refuses an unknown one.
    pub state: String,
    /// The trimmed `integration_id`, empty for any.
    pub integration_id: String,
}

/// `loadFleet` reduced to what `fleetPrinterResolver` reads: each printer's
/// `id`, `name` and `integration_id`. Like Node, any row that makes
/// `parsePrinterMachine` throw empties the whole fleet.
fn fleet(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<(String, String, Option<String>)>> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='printer.fleet'",
            params![tenant],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(Ok(Value::Array(items))) = raw
        .filter(|raw| !raw.is_empty())
        .map(|raw| serde_json::from_str::<Value>(&links::js_json(&raw)))
    else {
        return Ok(Vec::new());
    };
    let throws = |item: &Value| {
        item.is_null()
            || item["loaded_filaments"]
                .as_array()
                .is_some_and(|slots| slots.iter().any(Value::is_null))
    };
    if items.iter().any(throws) {
        return Ok(Vec::new());
    }
    Ok(items
        .iter()
        .map(|item| {
            let id = item.get("id").map_or("undefined".into(), links::js_string);
            let name = match &item["name"] {
                Value::Null => "Printer".into(),
                name => links::js_string(name),
            };
            let integration = match &item["integration_id"] {
                Value::Null => None,
                value => Some(links::js_trim(&links::js_string(value)).to_owned())
                    .filter(|id| !id.is_empty()),
            };
            (id, name, integration)
        })
        .collect())
}

/// `{...row, host_name}` with `fleetPrinterResolver`'s name: the fleet
/// printer with this `printer_id` (the last such row), else the only one on
/// this `integration_id`, else the row's own `host_name`.
fn fleet_name(fleet: &[(String, String, Option<String>)], mut row: Value) -> Value {
    let by_id = row["printer_id"]
        .as_str()
        .and_then(|id| fleet.iter().rev().find(|(printer, ..)| printer == id));
    let by_host = || {
        let host = row["integration_id"].as_str().filter(|id| !id.is_empty())?;
        let mut on_host = fleet.iter().filter(|(.., i)| i.as_deref() == Some(host));
        let only = on_host.next()?;
        on_host.next().is_none().then_some(only)
    };
    if let Some((_, name, _)) = by_id.or_else(by_host)
        && let Some(object) = row.as_object_mut()
    {
        object.insert("host_name".into(), Value::String(name.clone()));
    }
    row
}

/// `get_printer_checkoff`: the tenant's links and undismissed unattributed
/// prints, filtered and named as Node's tool answers them.
pub(super) fn printer_checkoff(tx: &Transaction<'_>, tenant: &str, filter: &Filter) -> Outcome {
    let fleet = fleet(tx, tenant)?;
    let plan = |value: &Value| {
        filter
            .plan_id
            .is_none_or(|plan| value.as_f64() == Some(plan as f64))
    };
    let links: Vec<Value> = links::load(tx, tenant, &links::now())?
        .iter()
        .map(links::public)
        .filter(|link| {
            plan(&link["profile_id"])
                && (filter.state.is_empty() || link["state"] == filter.state.as_str())
                && (filter.integration_id.is_empty()
                    || link["integration_id"] == filter.integration_id.as_str())
        })
        .map(|link| fleet_name(&fleet, link))
        .collect();
    let unattributed: Vec<Value> = unattributed::list(tx, tenant)?
        .into_iter()
        .filter(|print| !links::truthy(&print["dismissed"]) && plan(&print["claimed_profile_id"]))
        .map(|print| fleet_name(&fleet, print))
        .collect();
    let pending = links
        .iter()
        .filter(|link| link["state"] == "awaiting_verify")
        .count();
    Ok(Ok(Response::ok(Body::PrinterCheckoff(json!({
        "links": links,
        "unattributed": unattributed,
        "pending_verification": pending,
    })))))
}
