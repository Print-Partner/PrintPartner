//! Undo of a dismissed unattributed print (GRE-1549), the operation behind
//! `POST /api/v2/printer-checkoff/unattributed/{id}/dismiss/undo`.
//!
//! [`ProgressClient::undo_dismiss`] runs on the write seam. It removes
//! `dismissed` and `dismissed_at` from the stored print and keeps every other
//! field as stored, so the print is open again as it was before the dismiss.
//! It appends `checkoff.unattributed_resolved` with `{"dismissed": false}`.
//!
//! Undo is safe to repeat: a stored print that is not dismissed is answered
//! as [`DismissUndo::NotDismissed`], and nothing is written. A print that is
//! not stored, because pruning dropped it or it never existed, is
//! [`UndoDismissError::NotFound`]. A print whose file a watching,
//! awaiting-verify or verified Checkoff link has matched since stays
//! dismissed: restored, it would be an open print the reads hide while that
//! link lives.
//!
//! Errors are [`UndoDismissError`]. This module holds no status codes;
//! `pp-api` maps the variants.

use super::{load_raw, prune, resolved, save};
use crate::{
    WriterFailure,
    checkoff_progress::ProgressClient,
    read_model::{Credential, farm},
    write::authorize,
};
use rusqlite::Transaction;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::{sync::atomic::AtomicBool, time::Duration};

const LINKS: &str = "printer.checkoff_links";

/// What an undo did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DismissUndo {
    /// The print was dismissed and is open again.
    Restored,
    /// The print was not dismissed. Nothing was written.
    NotDismissed,
}

#[derive(Debug, thiserror::Error)]
pub enum UndoDismissError {
    /// No stored print has the id: pruning dropped it, or it never existed.
    #[error("the print is not stored")]
    NotFound,
    /// A watching, awaiting-verify or verified Checkoff link has the print's
    /// integration and file name. The print stays dismissed.
    #[error("a Checkoff link has matched the print's file")]
    AlreadyMatched,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

/// The fields of a stored print this operation reads.
#[derive(Deserialize)]
struct StoredPrint {
    #[serde(default, deserialize_with = "text")]
    integration_id: Option<String>,
    filename: String,
    /// Only `true` dismisses, as the reads take it.
    #[serde(default, deserialize_with = "exactly_true")]
    dismissed: bool,
}

/// A trimmed, non-empty string; any other value is none.
fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    })
}

fn exactly_true<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Value::deserialize(deserializer)? == Value::Bool(true))
}

impl ProgressClient {
    /// Opens the dismissed print `id` again, in one write transaction.
    pub fn undo_dismiss(
        &self,
        credential: Credential,
        id: String,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<DismissUndo, UndoDismissError> {
        let policy = self.policy;
        let name = "checkoff_progress.undo_dismiss";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            undo(tx, installation.id(), &id)
        })?
    }
}

fn undo(tx: &Transaction<'_>, tenant: &str, id: &str) -> Result<DismissUndo, UndoDismissError> {
    // Pruned as every read and save prunes, so a print the list no longer
    // keeps is not stored here either.
    let mut prints = prune(load_raw(tx, tenant).map_err(WriterFailure::from)?);
    let entry = prints
        .iter_mut()
        .find(|print| print["id"] == id)
        .ok_or(UndoDismissError::NotFound)?;
    let print = StoredPrint::deserialize(&*entry)
        .map_err(|error| WriterFailure::from(anyhow::Error::from(error)))?;
    if !print.dismissed {
        return Ok(DismissUndo::NotDismissed);
    }
    let links = farm::setting_list(tx, tenant, LINKS).map_err(WriterFailure::from)?;
    if print
        .integration_id
        .is_some_and(|integration| farm::matched_by_link(&links, &integration, &print.filename))
    {
        return Err(UndoDismissError::AlreadyMatched);
    }
    if let Some(fields) = entry.as_object_mut() {
        fields.remove("dismissed");
        fields.remove("dismissed_at");
    }
    save(tx, tenant, prints).map_err(WriterFailure::from)?;
    resolved(tx, tenant, id, json!({ "dismissed": false })).map_err(WriterFailure::from)?;
    Ok(DismissUndo::Restored)
}
