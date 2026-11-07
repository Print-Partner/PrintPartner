//! Checkoff burst undo from Build history (GRE-1259).
//!
//! History lists printer bursts from the `print_jobs` rows their outcomes
//! wrote and manual bursts from their receipts. A burst's undo state is the
//! answer its undo would give now, from the same checks
//! ([`burst::undo_blocked`], [`manual::undo_blocked`]), read without writing.
//! `POST /plans/{id}/history/{entry_id}/undo` resolves the entry here, so the
//! undo token never leaves the core, and runs the burst's own undo in the
//! Checkoff transaction.

use super::{
    Body, Outcome, Response,
    burst::{self, Blocked},
    manual,
};
use crate::read_model::history::{self, Entry, EntryType, Receipt, Undo, UndoReason, Undone};
use anyhow::Result;
use rusqlite::{Transaction, params};
use serde_json::Value;
use std::{collections::HashMap, path::Path, sync::atomic::AtomicBool};

/// The kept printer and manual burst receipts of one tenant.
pub(crate) struct Receipts {
    printer: Vec<Value>,
    manual: Vec<Value>,
}

impl Receipts {
    pub(crate) fn load(tx: &Transaction<'_>, tenant: &str) -> Result<Self> {
        Ok(Self {
            printer: burst::load_receipts(tx, tenant)?,
            manual: burst::load_list(tx, tenant, manual::KEY)?,
        })
    }

    /// The Build's manual burst receipts with their History row id:
    /// `manual-` and the SHA-256 of the idempotency key, which no other kept
    /// receipt shares and which never changes.
    pub(crate) fn manual(&self, build: i64) -> impl Iterator<Item = (String, &Value)> {
        self.manual
            .iter()
            .filter(move |receipt| receipt["profile_id"] == build)
            .filter_map(|receipt| {
                let key = receipt["idempotency_key"].as_str()?;
                Some((format!("manual-{}", burst::key_hash(key)), receipt))
            })
    }

    /// Each `print_jobs` row of the Build that a kept printer burst wrote, with
    /// that burst's undo token. An undone burst deleted its rows, so it maps
    /// none.
    pub(crate) fn printer_jobs(
        &self,
        tx: &Transaction<'_>,
        tenant: &str,
        build: i64,
    ) -> Result<HashMap<String, String>> {
        let mut statement = tx.prepare(
            "SELECT DISTINCT part.job_id FROM json_each(?3) outcome
             JOIN print_job_parts part ON part.id=outcome.value
             WHERE part.tenant_id=?1 AND part.profile_id=?2 AND part.job_id IS NOT NULL",
        )?;
        let mut jobs = HashMap::new();
        for receipt in &self.printer {
            let Some(token) = receipt["undo_token"].as_str() else {
                continue;
            };
            let outcomes = receipt["undo"]["outcomes"].to_string();
            for job in statement.query_map(params![tenant, build, outcomes], |r| r.get(0))? {
                jobs.insert(job?, token.to_owned());
            }
        }
        Ok(jobs)
    }

    /// What undoing the printer burst reverses, when that is more than one bed
    /// of `build`: its undo covers every bed and Build of the burst.
    fn printer_scope(&self, token: &str, build: i64) -> Option<String> {
        let receipt = self.printer.iter().find(|r| r["undo_token"] == token)?;
        let beds = receipt["response"]["beds"].as_array()?;
        let mut builds: Vec<i64> = beds
            .iter()
            .filter_map(|bed| bed["link"]["profile_id"].as_i64())
            .collect();
        builds.sort_unstable();
        builds.dedup();
        if beds.len() < 2 && builds.iter().all(|b| *b == build) {
            return None;
        }
        let names: Vec<String> = builds.iter().map(i64::to_string).collect();
        let names = match names.split_last()? {
            (last, []) => format!("Build {last}"),
            (last, rest) => format!("Builds {} and {last}", rest.join(", ")),
        };
        let bed = if beds.len() == 1 { "bed" } else { "beds" };
        Some(format!(
            "Undo reverses the whole burst: {} {bed} on {names}",
            beds.len()
        ))
    }

    /// What the burst's undo would answer now. Writes nothing.
    pub(crate) fn undo(
        &self,
        tx: &Transaction<'_>,
        tenant: &str,
        build: i64,
        receipt: &Receipt,
        repos: &Path,
    ) -> Result<Undo> {
        let blocked = match receipt {
            Receipt::Printer(token) => {
                burst::undo_blocked(tx, tenant, &self.printer, token, repos)?
            }
            Receipt::Manual(token) => {
                manual::undo_blocked(tx, tenant, &self.manual, (build, token), repos)?
            }
        };
        let Some(blocked) = blocked else {
            let scope = match receipt {
                Receipt::Printer(token) => self.printer_scope(token, build),
                Receipt::Manual(_) => None,
            };
            return Ok(Undo {
                scope,
                ..Undo::allowed()
            });
        };
        let reason = match blocked {
            Blocked::NotFound => UndoReason::NoUndoToken,
            Blocked::Undone => UndoReason::AlreadyUndone,
            Blocked::Archived => UndoReason::Archived,
            Blocked::Changed(_) => UndoReason::StateChanged,
        };
        Ok(Undo::refused(reason, blocked.detail()))
    }
}

/// The Build's History entry `id` with its undo state now, if History lists it.
fn find(
    tx: &Transaction<'_>,
    tenant: &str,
    build: i64,
    id: &str,
    repos: &Path,
) -> Result<Option<Entry>> {
    let receipts = Receipts::load(tx, tenant)?;
    let entries = history::entries(tx, tenant, build, &receipts, &AtomicBool::new(false))?;
    let Some(mut entry) = entries.into_iter().find(|entry| entry.id == id) else {
        return Ok(None);
    };
    if let Some(receipt) = &entry.receipt {
        entry.undo = Some(receipts.undo(tx, tenant, build, receipt, repos)?);
    }
    Ok(Some(entry))
}

/// `POST /plans/{id}/history/{entry_id}/undo`: undoes a burst History lists
/// through that burst's own undo, and answers the entry as History lists it
/// afterwards.
pub(super) fn undo_entry(
    tx: &Transaction<'_>,
    tenant: &str,
    build: i64,
    id: &str,
    repos: &Path,
) -> Outcome {
    let refuse = |status, detail: &str| Ok(Ok(Response::failure(status, detail, None)));
    let owned: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM build_profiles WHERE tenant_id=?1 AND id=?2)",
        params![tenant, build],
        |row| row.get(0),
    )?;
    if !owned {
        return refuse(404, "Profile not found");
    }
    let Some(entry) = find(tx, tenant, build, id, repos)? else {
        return refuse(404, "History entry not found");
    };
    let undone = match (entry.kind, entry.receipt) {
        (EntryType::CheckoffBurst, Some(Receipt::Printer(token))) => {
            burst::undo_batch(tx, tenant, &token, repos)?
        }
        (EntryType::CheckoffBurst, Some(Receipt::Manual(token))) => {
            manual::undo(tx, tenant, build, &token, repos)?
        }
        (EntryType::CheckoffBurst, None) => return refuse(409, history::NO_UNDO),
        (EntryType::Send, _) => return refuse(409, history::PRINTER_COMMAND),
        _ => return refuse(409, "This History entry cannot be undone"),
    };
    let undone_at = match undone {
        Ok(Response {
            status: 200,
            body: Body::Receipt(receipt),
        }) => receipt["undone_at"].as_str().unwrap_or_default().to_owned(),
        other => return Ok(other),
    };
    let entry = find(tx, tenant, build, id, repos)?;
    Ok(Ok(Response::ok(Body::HistoryUndo(Box::new(Undone {
        undone_at,
        entry,
    })))))
}
