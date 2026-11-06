//! Read-only lookups for callers that must reconcile a Checkoff burst
//! (GRE-1170): what an idempotency key settled to, and one printer Checkoff
//! link. Neither writes Checkoff state.

use super::{
    Body, Outcome, Response,
    burst::{MAX_RETIRED, RETIRED_KEY, key_hash, load_list, load_receipts},
    links,
};
use rusqlite::Transaction;
use serde_json::{Value, json};

/// The key as `POST /printer-checkoff/verify-batch` reads it: JS-trimmed, 1 to
/// 200 characters.
pub(super) fn key(raw: &str) -> std::result::Result<String, Response> {
    let key = links::js_trim(raw);
    if key.is_empty() || key.chars().count() > 200 {
        return Err(Response::failure(
            400,
            "idempotency_key must be 1 to 200 characters",
            None,
        ));
    }
    Ok(key.to_owned())
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
