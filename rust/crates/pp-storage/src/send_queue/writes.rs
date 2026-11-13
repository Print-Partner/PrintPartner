//! The send queue's user actions (GRE-365), each one writer transaction.
//!
//! - [`Request::Dispatch`] is Send now, and Send ready for each item it
//!   picks: a `queued` or `error` item becomes `sending` and gets a new
//!   durable `printer-upload` job in the same transaction, so an item is never
//!   `sending` without the job that settles it. Nothing here talks to a
//!   printer; the job worker does, after commit. An item whose last send has
//!   an unknown printer outcome is refused with `send_outcome_unknown`.
//! - [`Request::Reconcile`] records what the user says happened to that send.
//!   "It printed" makes the item `done`. "It didn't print" leaves it `error`
//!   and sendable again, but only through a new Send now. Either way the
//!   unresolved upload stops reserving its printer.
//! - [`Request::Remove`] cancels a `queued` or `error` item. An unresolved
//!   upload of that item is marked failed with the reason, since removing it
//!   never sends.
//! - [`Request::Missing`] marks an item whose staged file is gone, as Node's
//!   dispatch does before it refuses.
//! - [`Request::Add`] writes a new `queued` item for a staged file
//!   (GRE-1227).
//! - [`Request::Get`] and [`Request::Ready`] read what a route checks before
//!   it dispatches.
//!
//! A tenant whose queue stayed in Node's `printer.send_queue` setting (see
//! [`super::recover`]) cannot change it here: every request is refused
//! with 409.

mod enqueue;

pub use enqueue::NewItem;

use super::{legacy, rows};
use crate::{
    Envelope, Shared, auth,
    jobs::{self, CheckoffUnit, JobFailure, Outcome, Payload, UserOperation},
    read_model::{Credential, farm::iso_time},
};
use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};

const REPLY_LOST: &str = "Send queue reply lost after admission; reread the queue";

/// True when `error` from [`SendQueueClient::apply`] leaves the request's
/// outcome unknown: its transaction may have committed. Any other error
/// rolled the transaction back or never reached the writer.
pub fn may_have_written(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref(), Some(JobFailure::CommitUnknown))
        || matches!(error.downcast_ref(), Some(auth::AuthFailure::CommitUnknown))
        || error.to_string() == REPLY_LOST
}

/// Node's `DRAIN_ITEM_CAP`: queued items one Send ready examines.
pub const DRAIN_ITEM_CAP: usize = 8;
/// Node's error for an item whose staged file is gone.
pub const MISSING_ARTIFACT: &str = "Artifact file missing on disk";
/// Problem code for a send whose printer outcome the user has not reconciled.
pub const OUTCOME_UNKNOWN_CODE: &str = "send_outcome_unknown";
/// The item error after the user said the send did not print.
pub const NOT_PRINTED: &str =
    "You reconciled this send: the printer did not print it. Send now sends it again.";
const REMOVED: &str = "Removed from the send queue after the printer outcome was unknown";
pub(crate) const OUTCOME_UNKNOWN: Reply = Reply::Refused {
    status: 409,
    detail: "The printer outcome of this item's last send is unknown. Reconcile it first: say whether it printed.",
    code: Some(OUTCOME_UNKNOWN_CODE),
};

pub enum Request {
    /// Authenticates only, before a route reads an upload.
    Admit,
    /// Writes a new `queued` item for the file staged at `artifact_path`.
    Add {
        item: NewItem,
        artifact_path: String,
        host_name: String,
    },
    /// The item `id`.
    Get {
        id: String,
    },
    /// The item `id` when Send now may claim it, or [`Request::Dispatch`]'s
    /// refusal, without writing. Routes check the printer only after this.
    Check {
        id: String,
    },
    /// Up to [`DRAIN_ITEM_CAP`] `queued` items in stored order, optionally
    /// for one printer.
    Ready {
        printer_id: Option<String>,
    },
    /// Claim `id` for an upload to its own printer, labelled `host_name`.
    Dispatch {
        id: String,
        host_name: String,
    },
    Remove {
        id: String,
    },
    Missing {
        id: String,
    },
    /// The user's answer for an item whose last send has an unknown outcome.
    /// `job_id` is the item's `upload_job_id` the user answered about, so
    /// a late answer never closes a newer send.
    Reconcile {
        id: String,
        job_id: Option<String>,
        printed: bool,
    },
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Item(Value),
    Items(Vec<Value>),
    Dispatched {
        item: Value,
        job_id: String,
    },
    /// `code` is the problem code a route adds, when there is one.
    Refused {
        status: u16,
        detail: &'static str,
        code: Option<&'static str>,
    },
}

const NOT_FOUND: Reply = Reply::Refused {
    status: 404,
    detail: "Queue item not found",
    code: None,
};
const LEGACY: Reply = Reply::Refused {
    status: 409,
    detail: "This send queue is still in its legacy setting and cannot change",
    code: None,
};
const NOT_DISPATCHABLE: Reply = Reply::Refused {
    status: 409,
    detail: "Item is not dispatchable",
    code: None,
};
const UNSENDABLE: Reply = Reply::Refused {
    status: 400,
    detail: "Item cannot be sent to a printer",
    code: None,
};

pub(crate) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    storage: Arc<Shared>,
    request: Request,
}

#[derive(Clone)]
pub struct SendQueueClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}

impl crate::WriterOwner {
    pub fn send_queue(&self, policy: auth::AuthPolicy) -> Result<SendQueueClient> {
        auth::validate_policy(policy)?;
        Ok(SendQueueClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}

impl SendQueueClient {
    /// `Err` means authentication failed, the writer was unavailable, or the
    /// outcome is unknown after admission; reread the queue before retrying.
    pub fn apply(
        &self,
        credential: Credential,
        request: Request,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Reply> {
        crate::read_model::submit(&self.shared, cancelled, wait, REPLY_LOST, |reply| {
            Envelope::SendQueue {
                command: Command {
                    credential,
                    policy: self.policy,
                    storage: self.shared.clone(),
                    request,
                },
                reply,
            }
        })
    }
}

/// Runs `command` in one immediate transaction that commits unless the
/// request is refused, then publishes the job it enqueued or settled.
pub(crate) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
    subscriptions: &jobs::JobSubscriptions,
) -> Result<Reply> {
    jobs::begin_job_collection();
    let result = transact(connection, command, admission);
    jobs::end_job_collection(
        subscriptions,
        matches!(&result, Ok(reply) if !matches!(reply, Reply::Refused { .. })),
        result.as_ref().err(),
    );
    result
}

fn transact(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Reply> {
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    let credential = match command.credential {
        Credential::Session(token) => jobs::Credential::Session(token),
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => jobs::Credential::RoutedKey {
            tenant: routed_tenant,
            key: secret,
        },
    };
    let (tenant, subject, authority) =
        auth::authority::admit(&tx, credential, command.policy, &command.storage)?;
    let writes = !matches!(
        command.request,
        Request::Admit | Request::Get { .. } | Request::Check { .. } | Request::Ready { .. }
    );
    let reply = if legacy(&tx, &tenant)? {
        LEGACY
    } else {
        match command.request {
            Request::Admit => Reply::Items(Vec::new()),
            Request::Add {
                item,
                artifact_path,
                host_name,
            } => enqueue::add(&tx, &tenant, &item, &artifact_path, &host_name)?,
            Request::Get { id } => item(&tx, &tenant, &id)?.map_or(NOT_FOUND, Reply::Item),
            Request::Check { id } => check(&tx, &tenant, &id)?,
            Request::Ready { printer_id } => ready(&tx, &tenant, printer_id.as_deref())?,
            Request::Dispatch { id, host_name } => {
                dispatch(&tx, &tenant, &subject, authority, &id, &host_name)?
            }
            Request::Remove { id } => remove(&tx, &tenant, &subject, &id)?,
            Request::Missing { id } => missing(&tx, &tenant, &id)?,
            Request::Reconcile {
                id,
                job_id,
                printed,
            } => reconcile(&tx, &tenant, &subject, &id, job_id.as_deref(), printed)?,
        }
    };
    if !matches!(reply, Reply::Refused { .. }) {
        if writes {
            super::trim(&tx, &tenant)?;
        }
        tx.commit()
            .map_err(|_| anyhow!(JobFailure::CommitUnknown))?;
    }
    Ok(reply)
}

/// The table item in Node's item shape with its computed
/// `outcome_unknown`, or `None`.
fn item(tx: &Connection, tenant: &str, id: &str) -> Result<Option<Value>> {
    let mut found: Vec<Value> = rows(tx, tenant)?
        .unwrap_or_default()
        .into_iter()
        .filter(|item| item["id"] == id)
        .take(1)
        .collect();
    super::mark_outcomes(Some(tx), tenant, &mut found)?;
    Ok(found.pop())
}

struct Stored {
    state: String,
    printer_id: String,
    upload_job_id: Option<String>,
    error: Option<String>,
}

/// True when the item's last send has an unknown printer outcome: its Rust
/// upload awaits reconciliation, or, for a send Node started, Node's restart
/// marked it interrupted.
fn outcome_unknown(tx: &Transaction<'_>, tenant: &str, stored: &Stored) -> Result<bool> {
    if stored.state != "error" {
        return Ok(false);
    }
    let Some(job) = &stored.upload_job_id else {
        return Ok(stored.error.as_deref() == Some(super::INTERRUPTED_ERROR));
    };
    match jobs::printer_upload_state(tx, tenant, job)? {
        Some(state) => Ok(state == jobs::PersistentState::ReconciliationRequired),
        None => Ok(stored.error.as_deref() == Some(super::INTERRUPTED_ERROR)),
    }
}

/// The item's state, printer and last upload, when it may be sent or
/// removed; otherwise the refusal.
fn resendable(tx: &Connection, tenant: &str, id: &str) -> Result<Result<Stored, Reply>> {
    let stored = tx
        .query_row(
            "SELECT state,printer_id,upload_job_id,error FROM printer_send_queue WHERE tenant_id=?1 AND id=?2",
            params![tenant, id],
            |row| {
                Ok(Stored {
                    state: row.get(0)?,
                    printer_id: row.get(1)?,
                    upload_job_id: row.get(2)?,
                    error: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(match stored {
        None => Err(NOT_FOUND),
        Some(stored) if matches!(stored.state.as_str(), "queued" | "error") => Ok(stored),
        Some(_) => Err(NOT_DISPATCHABLE),
    })
}

/// True when another item of `printer_id` holds a send Node started whose
/// outcome is unknown. Node's sends have no Rust job to reserve the printer,
/// so the item itself does until the user reconciles or removes it.
fn node_send_unresolved(tx: &Transaction<'_>, tenant: &str, printer_id: &str) -> Result<bool> {
    let sends = tx
        .prepare("SELECT upload_job_id FROM printer_send_queue WHERE tenant_id=?1 AND printer_id=?2 AND state='error' AND error=?3")?
        .query_map(params![tenant, printer_id, super::INTERRUPTED_ERROR], |row| {
            row.get::<_, Option<String>>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for job in sends {
        match job {
            None => return Ok(true),
            Some(job) if jobs::printer_upload_state(tx, tenant, &job)?.is_none() => {
                return Ok(true);
            }
            Some(_) => {}
        }
    }
    Ok(false)
}

fn dispatch(
    tx: &Transaction<'_>,
    tenant: &str,
    subject: &str,
    authority: Option<auth::authority::AuthorityBasis>,
    id: &str,
    host_name: &str,
) -> Result<Reply> {
    let stored = match sendable(tx, tenant, id)? {
        Ok(stored) => stored,
        Err(refusal) => return Ok(refusal),
    };
    let sending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM printer_send_queue WHERE tenant_id=?1 AND printer_id=?2 AND state='sending')",
        params![tenant, stored.printer_id],
        |row| row.get(0),
    )?;
    if sending {
        return Ok(Reply::Refused {
            status: 409,
            detail: "Item changed concurrently",
            code: None,
        });
    }
    if jobs::printer_reserved(tx, tenant, &stored.printer_id)?
        || node_send_unresolved(tx, tenant, &stored.printer_id)?
    {
        return Ok(Reply::Refused {
            status: 409,
            detail: "Another send to this printer is still unresolved",
            code: None,
        });
    }
    let (filename, artifact_path, start, profile_id, units): (
        String,
        String,
        bool,
        Option<i64>,
        Option<String>,
    ) = tx.query_row(
        "SELECT filename,artifact_path,start,profile_id,checkoff_units FROM printer_send_queue WHERE tenant_id=?1 AND id=?2",
        params![tenant, id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    let checkoff_units = match units
        .as_deref()
        .map(serde_json::from_str::<Vec<CheckoffUnit>>)
    {
        None => Vec::new(),
        Some(Ok(units)) => units,
        Some(Err(_)) => return Ok(UNSENDABLE),
    };
    let payload = Payload::PrinterUpload {
        printer_id: stored.printer_id,
        artifact_path,
        filename,
        start,
        profile_id: profile_id.map(|id| id as u64),
        host_name: Some(host_name.to_owned()).filter(|name| !name.trim().is_empty()),
        checkoff_units,
        unlabeled_names: Vec::new(),
    };
    if payload.validate().is_err() {
        return Ok(UNSENDABLE);
    }
    let key = format!(
        "printer-send-queue:{}",
        hex::encode(rand::random::<[u8; 16]>())
    );
    let Outcome::Job(job, _) = jobs::user(
        tx,
        tenant,
        subject,
        authority,
        UserOperation::Enqueue {
            key,
            payload_version: 1,
            payload,
        },
    )?
    else {
        return Err(anyhow!("Printer upload enqueue returned no job"));
    };
    tx.execute(
        "UPDATE printer_send_queue SET state='sending',error=NULL,host_name=COALESCE(?1,host_name),upload_job_id=?2,updated_at=?3 WHERE tenant_id=?4 AND id=?5",
        params![
            Some(host_name.trim()).filter(|name| !name.is_empty()),
            job.job_id,
            iso_time(SystemTime::now()),
            tenant,
            id
        ],
    )?;
    let item = item(tx, tenant, id)?.ok_or_else(|| anyhow!("Dispatched item vanished"))?;
    Ok(Reply::Dispatched {
        item,
        job_id: job.job_id,
    })
}

/// Up to [`DRAIN_ITEM_CAP`] `queued` items, or the first item's Farm refusal
/// for the whole drain, as Node's drain route checks
/// `storedQueueAdmissionProblem` once before it sends anything.
fn ready(tx: &Transaction<'_>, tenant: &str, printer_id: Option<&str>) -> Result<Reply> {
    let items: Vec<Value> = rows(tx, tenant)?
        .unwrap_or_default()
        .into_iter()
        .filter(|item| {
            item["state"] == "queued"
                && printer_id.is_none_or(|printer| item["printer_id"] == printer)
        })
        .take(DRAIN_ITEM_CAP)
        .collect();
    for item in &items {
        if let Some(id) = item["id"].as_str()
            && let Some(refusal) = item_farm_refusal(tx, tenant, id)?
        {
            return Ok(refusal);
        }
    }
    Ok(Reply::Items(items))
}

/// The item's Farm refusal, as [`farm_refusal`] answers for its Plan.
fn item_farm_refusal(tx: &Transaction<'_>, tenant: &str, id: &str) -> Result<Option<Reply>> {
    let profile: Option<i64> = tx.query_row(
        "SELECT profile_id FROM printer_send_queue WHERE tenant_id=?1 AND id=?2",
        params![tenant, id],
        |row| row.get(0),
    )?;
    farm_refusal(tx, tenant, profile)
}

/// Node's `storedQueueAdmissionProblem` and its add route's Farm check: a
/// send needs the Farm feature on for the Plan, or for the user without one.
fn farm_refusal(tx: &Transaction<'_>, tenant: &str, profile: Option<i64>) -> Result<Option<Reply>> {
    let plan = profile.and_then(|id| pp_domain::PositiveId::new(id.unsigned_abs()).ok());
    Ok(match crate::features::resolve_for_send(tx, tenant, plan) {
        Ok(view) if view.effective().enabled(pp_domain::features::Feature::Farm) => None,
        Ok(_) => Some(Reply::Refused {
            status: 403,
            detail: "Feature farm is disabled.",
            code: Some("FEATURE_DISABLED"),
        }),
        Err(_) => Some(Reply::Refused {
            status: 500,
            detail: "Stored feature toggles are invalid.",
            code: Some("FEATURE_TOGGLES_INVALID"),
        }),
    })
}

fn check(tx: &Transaction<'_>, tenant: &str, id: &str) -> Result<Reply> {
    if let Err(refusal) = sendable(tx, tenant, id)? {
        return Ok(refusal);
    }
    Ok(item(tx, tenant, id)?.map_or(NOT_FOUND, Reply::Item))
}

/// The item when Send now may send it: resendable, its last outcome known,
/// and Farm on for its Plan; otherwise the refusal.
fn sendable(tx: &Transaction<'_>, tenant: &str, id: &str) -> Result<Result<Stored, Reply>> {
    let stored = match resendable(tx, tenant, id)? {
        Ok(stored) => stored,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if outcome_unknown(tx, tenant, &stored)? {
        return Ok(Err(OUTCOME_UNKNOWN));
    }
    if let Some(refusal) = item_farm_refusal(tx, tenant, id)? {
        return Ok(Err(refusal));
    }
    Ok(Ok(stored))
}

fn remove(tx: &Transaction<'_>, tenant: &str, subject: &str, id: &str) -> Result<Reply> {
    let stored = match resendable(tx, tenant, id)? {
        Ok(stored) => stored,
        Err(Reply::Refused { status: 409, .. }) => {
            return Ok(Reply::Refused {
                status: 409,
                detail: "Item not cancellable",
                code: None,
            });
        }
        Err(refusal) => return Ok(refusal),
    };
    if let Some(job) = &stored.upload_job_id {
        jobs::release_printer_upload(tx, tenant, subject, job, REMOVED, "send_queue_removed")?;
    }
    tx.execute(
        "UPDATE printer_send_queue SET state='cancelled',error=NULL,updated_at=?1 WHERE tenant_id=?2 AND id=?3",
        params![iso_time(SystemTime::now()), tenant, id],
    )?;
    let item = item(tx, tenant, id)?.ok_or_else(|| anyhow!("Removed item vanished"))?;
    Ok(Reply::Item(item))
}

fn missing(tx: &Transaction<'_>, tenant: &str, id: &str) -> Result<Reply> {
    let stored = match resendable(tx, tenant, id)? {
        Ok(stored) => stored,
        Err(refusal) => return Ok(refusal),
    };
    if outcome_unknown(tx, tenant, &stored)? {
        return Ok(OUTCOME_UNKNOWN);
    }
    tx.execute(
        "UPDATE printer_send_queue SET state='error',error=?1,updated_at=?2 WHERE tenant_id=?3 AND id=?4",
        params![MISSING_ARTIFACT, iso_time(SystemTime::now()), tenant, id],
    )?;
    let item = item(tx, tenant, id)?.ok_or_else(|| anyhow!("Marked item vanished"))?;
    Ok(Reply::Item(item))
}

fn reconcile(
    tx: &Transaction<'_>,
    tenant: &str,
    subject: &str,
    id: &str,
    job_id: Option<&str>,
    printed: bool,
) -> Result<Reply> {
    let stored = match resendable(tx, tenant, id)? {
        Ok(stored) => stored,
        Err(Reply::Refused { status: 404, .. }) => return Ok(NOT_FOUND),
        Err(_) => return Ok(OUTCOME_KNOWN),
    };
    if !outcome_unknown(tx, tenant, &stored)? {
        return Ok(OUTCOME_KNOWN);
    }
    if stored.upload_job_id.as_deref() != job_id {
        return Ok(SEND_CHANGED);
    }
    if let Some(job) = &stored.upload_job_id
        && jobs::printer_upload_state(tx, tenant, job)?.is_some()
    {
        jobs::reconcile_printer_upload(tx, tenant, subject, job, printed)?;
    }
    let (state, error) = if printed {
        ("done", None)
    } else {
        ("error", Some(NOT_PRINTED))
    };
    tx.execute(
        "UPDATE printer_send_queue SET state=?1,error=?2,updated_at=?3 WHERE tenant_id=?4 AND id=?5",
        params![state, error, iso_time(SystemTime::now()), tenant, id],
    )?;
    let item = item(tx, tenant, id)?.ok_or_else(|| anyhow!("Reconciled item vanished"))?;
    Ok(Reply::Item(item))
}

const OUTCOME_KNOWN: Reply = Reply::Refused {
    status: 409,
    detail: "This item has no send with an unknown printer outcome",
    code: None,
};

const SEND_CHANGED: Reply = Reply::Refused {
    status: 409,
    detail: "This answer is for an earlier send of this item",
    code: Some("send_changed"),
};

#[cfg(test)]
mod tests;
