//! Node's `reconcilePrinterCheckoff` (GRE-1897): a host status moves that
//! host's `watching` Printer Checkoff links on, and nothing else. The rules,
//! and the two places they differ from Node (an explicit cancel never counts
//! as a finish; a status moves at most one send of a file), are in
//! `docs/farm-reads.md#checkoff-reconcile`. A status that changes no link
//! writes nothing, and no Required unit is checked off here.
use super::links::{self, Link};
use crate::{
    Shared, WriterFailure, auth::AuthPolicy, read_model::farm::HostState,
    write::runtime_installation,
};
use anyhow::Result;
use rusqlite::Transaction;
use serde_json::Value;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Node's `STALE_MS`: a `watching` link this old that never saw its print
/// run is dismissed while its host is idle or unreachable.
const STALE: time::Duration = time::Duration::hours(48);

/// One host status, as the watchers last answered it.
#[derive(Clone, Debug, PartialEq)]
pub struct HostReport {
    pub state: HostState,
    pub filename: Option<String>,
    /// Percent done, 0 to 100.
    pub progress: Option<f64>,
    /// An idle host whose print was stopped on request.
    pub cancelled: bool,
}

/// What a reconcile did to one link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The print is running: `saw_active` and `last_progress` advanced.
    Active,
    /// The host reported a finish; the link waits for the operator.
    AwaitingVerify,
    /// The host reported an error (`failed`) or a cancel (`cancelled`).
    HostFailed,
    /// A stale link whose print never ran.
    Dismissed,
}

#[derive(Debug, PartialEq)]
enum Decision {
    Noop,
    MarkActive(Option<f64>),
    AwaitVerify,
    HostFailed(&'static str),
}

/// `printerFilenamesMatch`.
fn same_file(host: Option<&str>, link: &Value) -> bool {
    let host = links::normalized_filename(host.unwrap_or_default());
    !host.is_empty() && link.as_str().map(links::normalized_filename) == Some(host)
}

/// `decideCheckoffReconcile`. A link finishes or fails only once it ran: it
/// was seen printing, or its send started the print. So a finish the printer
/// keeps showing never moves an upload that never ran, after a missed poll or
/// a restart alike. Idle and error never await verify, unless the print was
/// seen past 99% and its brief `complete` was missed; an explicit cancel
/// never does.
fn decide(link: &Link, report: &HostReport) -> Decision {
    if link["state"] != "watching" {
        return Decision::Noop;
    }
    let filename = report.filename.as_deref();
    let matches =
        same_file(filename, &link["filename"]) || same_file(filename, &link["remote_path"]);
    let saw_active = link["saw_active"] == true;
    let ran = saw_active || link["started"] == true;
    match report.state {
        HostState::Printing | HostState::Paused if matches => {
            Decision::MarkActive(report.progress.filter(|p| p.is_finite()))
        }
        HostState::Complete if ran => {
            // A host may clear its file name after the finish.
            let cleared = links::normalized_filename(filename.unwrap_or_default()).is_empty();
            if matches || cleared {
                Decision::AwaitVerify
            } else {
                Decision::Noop
            }
        }
        HostState::Error if saw_active || (ran && matches) => Decision::HostFailed("failed"),
        HostState::Idle if saw_active => {
            let near_done = link["last_progress"].as_f64().is_some_and(|p| p >= 99.0);
            if near_done && !report.cancelled {
                Decision::AwaitVerify
            } else {
                Decision::HostFailed("cancelled")
            }
        }
        _ => Decision::Noop,
    }
}

fn stale(link: &Link, now: OffsetDateTime) -> bool {
    let created = link["created_at"]
        .as_str()
        .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok());
    link["saw_active"] != true && created.is_some_and(|at| now - at > STALE)
}

/// Ends the link's watch in `state` with the host's `outcome`.
fn settle(link: &mut Link, state: &str, outcome: &str) {
    link.insert("state".into(), state.into());
    link.insert("host_outcome".into(), outcome.into());
}

/// Applies `decision` to `link` at `at`, or `None` when it changes nothing.
fn apply(link: &mut Link, decision: Decision, at: &str) -> Option<Change> {
    match decision {
        Decision::Noop => None,
        Decision::MarkActive(progress) => {
            let before = link["last_progress"].as_f64();
            let after = progress.map(|p| {
                before
                    .unwrap_or(0.0)
                    .max((p.clamp(0.0, 100.0) + 0.5).floor())
            });
            if link["saw_active"] == true && (after.is_none() || after == before) {
                return None;
            }
            link.insert("saw_active".into(), true.into());
            if let Some(after) = after {
                link.insert("last_progress".into(), (after as i64).into());
            }
            Some(Change::Active)
        }
        Decision::HostFailed(outcome) => {
            settle(link, "host_failed", outcome);
            Some(Change::HostFailed)
        }
        Decision::AwaitVerify => {
            settle(link, "awaiting_verify", "success");
            link.insert("completed_at".into(), at.into());
            link.insert("saw_active".into(), true.into());
            Some(Change::AwaitingVerify)
        }
    }
}

/// The one send of `moving` that `report` is about: all of them when there
/// is one, else the only one seen printing, else the only one the send
/// started, else none.
fn one_send(all: &[Link], moving: Vec<(usize, Decision)>) -> Vec<(usize, Decision)> {
    if moving.len() < 2 {
        return moving;
    }
    for flag in ["saw_active", "started"] {
        let count = moving.iter().filter(|(i, _)| all[*i][flag] == true).count();
        if count == 1 {
            return moving
                .into_iter()
                .filter(|(i, _)| all[*i][flag] == true)
                .collect();
        }
    }
    Vec::new()
}

/// Reconciles host `integration_id`'s `watching` links against `report`
/// at `now`. Returns each changed link's ID and change, in list order; an
/// empty list leaves the stored links as they were.
pub(crate) fn reconcile(
    tx: &Transaction<'_>,
    tenant: &str,
    integration_id: &str,
    report: &HostReport,
    now: OffsetDateTime,
) -> Result<Vec<(String, Change)>> {
    let at = crate::auth::timestamp_at(now);
    let mut all = links::load(tx, tenant, &at)?;
    let quiet = matches!(
        report.state,
        HostState::Idle | HostState::Offline | HostState::Unknown
    );
    let (mut stale_links, mut moving) = (Vec::new(), Vec::new());
    for (index, link) in all.iter().enumerate() {
        if link["integration_id"] != integration_id || link["state"] != "watching" {
            continue;
        }
        if quiet && stale(link, now) {
            stale_links.push(index);
        } else {
            match decide(link, report) {
                Decision::Noop => {}
                decision => moving.push((index, decision)),
            }
        }
    }
    let mut changed = Vec::new();
    for index in stale_links {
        settle(&mut all[index], "dismissed", "unknown");
        changed.push((index, Change::Dismissed));
    }
    for (index, decision) in one_send(&all, moving) {
        if let Some(change) = apply(&mut all[index], decision, &at) {
            changed.push((index, change));
        }
    }
    changed.sort_by_key(|(index, _)| *index);
    let changed: Vec<(usize, String, Change)> = changed
        .into_iter()
        .map(|(index, change)| {
            let id = all[index]["id"].as_str().unwrap_or_default().to_owned();
            (index, id, change)
        })
        .collect();
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    // Each `updatePrinterCheckoffLink` moves its link to the end of the list.
    let moved: Vec<usize> = changed.iter().map(|(index, ..)| *index).collect();
    let (mut kept, mut patched) = (Vec::new(), Vec::new());
    for (index, link) in all.into_iter().enumerate() {
        if moved.contains(&index) {
            patched.push(link);
        } else {
            kept.push(link);
        }
    }
    kept.append(&mut patched);
    links::save_all(tx, tenant, kept)?;
    Ok(changed
        .into_iter()
        .map(|(_, id, change)| (id, change))
        .collect())
}

/// Reconciles Checkoff links against the host statuses the watchers read.
/// It writes nothing but the links' host progress and outcome.
#[derive(Clone)]
pub struct ReconcileClient {
    shared: Arc<Shared>,
    policy: AuthPolicy,
}

impl crate::WriterOwner {
    /// The watchers act with no user session, so only the desktop's
    /// single-account `policy` reconciles; any other refuses each call.
    pub fn checkoff_reconcile(&self, policy: AuthPolicy) -> anyhow::Result<ReconcileClient> {
        crate::auth::validate_policy(policy)?;
        Ok(ReconcileClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}

impl ReconcileClient {
    /// Reconciles host `integration_id`'s `watching` links against `report`.
    pub fn reconcile(
        &self,
        integration_id: &str,
        report: HostReport,
        wait: Duration,
    ) -> Result<Vec<(String, Change)>, WriterFailure> {
        let (policy, integration_id) = (self.policy, integration_id.to_owned());
        let now = OffsetDateTime::from(SystemTime::now());
        self.shared.write(
            "checkoff.reconcile",
            &AtomicBool::new(false),
            wait,
            move |tx, _| {
                let installation = runtime_installation(tx, policy)?;
                reconcile(tx, installation.id(), &integration_id, &report, now)
                    .map_err(WriterFailure::from)
            },
        )?
    }
}

#[cfg(test)]
mod tests;
