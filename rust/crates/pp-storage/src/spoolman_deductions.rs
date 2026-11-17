//! Spoolman deduction intent (GRE-411, part 1).
//!
//! A confirmed Checkoff burst records, in its own transaction, how many grams
//! each pinned spool should lose: the link's grams divided by its units, times
//! the burst's confirmed units whose Accepted Part pins that spool
//! (`spoolman:{integration}:spool:{n}`). Nothing here calls Spoolman. A later
//! worker sends each `pending` row once.
//!
//! Confirmed units that cannot be deducted give one `skipped` row per bed and
//! reason, and the burst still succeeds. With the Spoolman Module off for the
//! Build, nothing is written. A link unit is claimed by at most one row
//! (`spoolman_deduction_units`); undoing a burst removes its `pending` and
//! `skipped` rows and frees their units.
//!
//! A claim moves one `pending` row to `sending` and stores the grams Spoolman
//! reported before the request. The claim commits on its own, before the
//! caller sends anything. A finish changes a row only while it is `sending`.
//! Opening the database marks every `sending` row `unknown` with failure
//! `interrupted`, before seeding and before job recovery, and does not send
//! it. Nothing here returns `sending`, `unknown` or `failed` to `pending`.
//! With the Module off at send time the row stays `pending`; that is not a
//! new skip reason.
//!
//! [`SpoolmanDeductionsClient::next_send`] is what the send worker reads.
//! [`SpoolmanDeductionsClient::claim_for_send`] claims one row only while the
//! Module is on, and [`SpoolmanDeductionsClient::finish_for_send`] records the
//! outcome. Both act as the single-account installation: the burst already
//! checked the caller's credential. A committed verify burst runs the callback
//! registered with [`SpoolmanDeductionsClient::on_burst`].
//!
//! Errors are [`DeductionError`]. This module holds no status codes and no
//! response text; `pp-api` maps the variants.

use crate::{
    SettingsClient, Shared, WriterFailure, WriterOwner,
    auth::AuthPolicy,
    printer_setup::{ServiceKind, integrations},
    read_model::Credential,
    write::authorize,
};
#[cfg(feature = "desktop_rebuild")]
use crate::{
    auth::{AuthFailure, SessionTenantPolicy},
    read_model::stock::SpoolmanIntegration,
    write::single_account,
};
use pp_domain::{PositiveId, features::Feature};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

pub(crate) const SCHEMA_STEP: u64 = 49;
/// Columns that record one send. Appended; the table is not rebuilt.
pub(crate) const SEND_STATE_STEP: u64 = 50;
/// The list answers at most this many rows, newest first.
pub const LIST_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    Sending,
    Applied,
    Unknown,
    Failed,
    Skipped,
    Dismissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// The link has no grams yet.
    NoGrams,
    /// The Accepted Part pins no Spoolman spool.
    NoPinnedSpool,
    /// No stored Spoolman integration has the pinned id.
    IntegrationMissing,
    IntegrationDisabled,
    /// The stored integrations document is not the typed shape.
    IntegrationsUnreadable,
    /// The stored Module setting, the user's or the Build's, is not the typed
    /// shape, so whether the Spoolman Module is on is unknown.
    ModuleSettingUnreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GramsBasis {
    Estimated,
    Measured,
}

/// Why a send did not apply. Set only on `failed`, `unknown` and `dismissed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Unreachable,
    Refused,
    ReplyLost,
    Interrupted,
    IntegrationMissing,
    IntegrationDisabled,
}

/// How a claimed send ended. Applied only while the row is `sending`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    /// The reply never came back. The row becomes `unknown`.
    ReplyLost,
    /// Nothing was accepted. The row becomes `failed`.
    Unreachable,
    /// Spoolman answered this HTTP status. The row becomes `failed`.
    Refused(u16),
    /// The integration was gone at send time. The row becomes `failed`.
    IntegrationMissing,
    /// The integration was disabled at send time. The row becomes `failed`.
    IntegrationDisabled,
}

macro_rules! text_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}
text_enum!(State { Pending => "pending", Sending => "sending", Applied => "applied", Unknown => "unknown", Failed => "failed", Skipped => "skipped", Dismissed => "dismissed" });
text_enum!(SkipReason { NoGrams => "no_grams", NoPinnedSpool => "no_pinned_spool", IntegrationMissing => "integration_missing", IntegrationDisabled => "integration_disabled", IntegrationsUnreadable => "integrations_unreadable", ModuleSettingUnreadable => "module_setting_unreadable" });
text_enum!(GramsBasis { Estimated => "estimated", Measured => "measured" });
text_enum!(Failure {
    Unreachable => "unreachable",
    Refused => "refused",
    ReplyLost => "reply_lost",
    Interrupted => "interrupted",
    IntegrationMissing => "integration_missing",
    IntegrationDisabled => "integration_disabled",
});

/// What a row deducts: present on every row except a `skipped` one.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub integration_id: String,
    pub spool_id: i64,
    pub grams: f64,
    pub basis: GramsBasis,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deduction {
    pub id: i64,
    pub state: State,
    /// Set exactly when `state` is `Skipped`.
    pub reason: Option<SkipReason>,
    pub link_id: String,
    /// The Build.
    pub profile_id: i64,
    /// The burst's confirmed units this row covers.
    pub units: u32,
    pub target: Option<Target>,
    /// Set only on `failed`, `unknown` and `dismissed`.
    pub failure: Option<Failure>,
    /// 100 to 599, and only when `failure` is `refused`.
    pub failure_status: Option<i64>,
    /// Grams Spoolman reported before the send. Stored when the row is claimed.
    pub spool_grams_before: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

/// What the send worker should do with the oldest pending row.
#[cfg(feature = "desktop_rebuild")]
#[derive(Clone, Debug, PartialEq)]
pub enum SendWork {
    /// No `pending` row.
    Idle,
    /// Pending rows exist, and each one's Module is off or unreadable. They
    /// stay `pending`.
    Held,
    /// The Module is on and the integration is not in the stored list, or the
    /// list is not readable. Claim, then finish `integration_missing`.
    Missing { id: i64 },
    /// The Module is on and the integration is disabled. Claim, then finish
    /// `integration_disabled`.
    Disabled { id: i64 },
    /// The Module is on and the integration can be contacted.
    Ready {
        id: i64,
        spool_id: i64,
        grams: f64,
        integration: SpoolmanIntegration,
    },
}

/// The result of claiming one row for a send.
#[cfg(feature = "desktop_rebuild")]
#[derive(Clone, Debug, PartialEq)]
pub enum ModuleClaim {
    /// The row is not `pending`. Nothing changed.
    Gone,
    /// The Module is off or unreadable. The row stays `pending`.
    Held,
    Claimed(Claimed),
}

/// The row a claim moved to `sending`, with the grams the caller will send.
#[derive(Clone, Debug, PartialEq)]
pub struct Claimed {
    pub id: i64,
    pub integration_id: String,
    pub spool_id: i64,
    pub grams: f64,
    pub spool_grams_before: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub state: Option<State>,
    pub link_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeductionError {
    /// A stored row breaks the shape the schema checks; it is left as it is.
    #[error("stored deduction row {id} is not the typed shape")]
    StoredRowInvalid { id: i64 },
    /// `spool_grams_before` was not a finite number. The row was not claimed.
    #[error("spool grams before a deduction must be a finite number")]
    GramsBeforeNotFinite,
    /// A refused send named a status outside 100 to 599. The row was not finished.
    #[error("a refused deduction needs an HTTP status from 100 to 599")]
    StatusOutOfRange,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for DeductionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

/// One bed of a burst: its link, its Build and the units it confirmed.
pub(crate) struct ConfirmedBed {
    pub(crate) link_id: String,
    pub(crate) profile_id: i64,
    pub(crate) units: Vec<(i64, i64)>,
}

/// Records the deduction intent of a confirmed burst, inside the burst's own
/// transaction. `burst` is the burst's undo token.
pub(crate) fn record_burst(
    tx: &Transaction<'_>,
    tenant: &str,
    burst: &str,
    beds: &[ConfirmedBed],
) -> Result<(), DeductionError> {
    let now = now(tx)?;
    let mut hosts: Option<Result<BTreeMap<String, bool>, ()>> = None;
    for bed in beds {
        let units = unclaimed(tx, tenant, bed)?;
        if units.is_empty() {
            continue;
        }
        let row =
            |state: State, reason: Option<SkipReason>, count: usize, target: Option<&Target>| {
                insert(
                    tx,
                    (tenant, burst, &now),
                    bed,
                    (state, reason, count),
                    target,
                )
            };
        match module_on(tx, tenant, bed.profile_id)? {
            Some(true) => {}
            Some(false) => continue,
            None => {
                let reason = Some(SkipReason::ModuleSettingUnreadable);
                row(State::Skipped, reason, units.len(), None)?;
                continue;
            }
        }
        let Some((grams, link_units, basis)) = link_grams(tx, tenant, &bed.link_id)? else {
            row(State::Skipped, Some(SkipReason::NoGrams), units.len(), None)?;
            continue;
        };
        let mut skipped = BTreeMap::<SkipReason, usize>::new();
        let mut by_spool = BTreeMap::<(String, i64), Vec<(i64, i64)>>::new();
        for unit in units {
            match pinned_spool(tx, tenant, unit.0)? {
                Some(spool) => by_spool.entry(spool).or_default().push(unit),
                None => *skipped.entry(SkipReason::NoPinnedSpool).or_default() += 1,
            }
        }
        for ((integration_id, spool_id), group) in by_spool {
            let known = hosts
                .get_or_insert_with(|| spoolman_hosts(tx, tenant))
                .as_ref();
            let share = share(grams, link_units, group.len());
            let reason = match known.map(|hosts| hosts.get(&integration_id)) {
                Err(()) => Some(SkipReason::IntegrationsUnreadable),
                Ok(None) => Some(SkipReason::IntegrationMissing),
                Ok(Some(false)) => Some(SkipReason::IntegrationDisabled),
                Ok(Some(true)) if share.is_none() => Some(SkipReason::NoGrams),
                Ok(Some(true)) => None,
            };
            let (None, Some(grams)) = (reason, share) else {
                *skipped
                    .entry(reason.unwrap_or(SkipReason::NoGrams))
                    .or_default() += group.len();
                continue;
            };
            let target = Target {
                integration_id,
                spool_id,
                grams,
                basis,
            };
            let id = row(State::Pending, None, group.len(), Some(&target))?;
            for (part, index) in group {
                tx.execute(
                    "INSERT INTO spoolman_deduction_units (tenant_id,link_id,part_id,unit_index,deduction_id) VALUES (?1,?2,?3,?4,?5)",
                    params![tenant, bed.link_id, part, index, id],
                )?;
            }
        }
        for (reason, count) in skipped {
            row(State::Skipped, Some(reason), count, None)?;
        }
    }
    Ok(())
}

/// The grams `count` of the link's `link_units` use: multiplied before
/// dividing, so whole grams stay whole. `None` when the result is not a
/// positive finite number, which then skips as `no_grams`.
fn share(grams: f64, link_units: u32, count: usize) -> Option<f64> {
    let share = grams * count as f64 / f64::from(link_units);
    (share.is_finite() && share > 0.0).then_some(share)
}

/// Removes the burst's `pending` and `skipped` rows and frees their units.
/// A row that has started sending stays.
pub(crate) fn remove_burst(
    tx: &Transaction<'_>,
    tenant: &str,
    burst: &str,
) -> Result<(), DeductionError> {
    tx.execute(
        "DELETE FROM spoolman_deduction_units WHERE tenant_id=?1 AND deduction_id IN
           (SELECT id FROM spoolman_deductions WHERE tenant_id=?1 AND burst=?2 AND state='pending')",
        params![tenant, burst],
    )?;
    tx.execute(
        "DELETE FROM spoolman_deductions WHERE tenant_id=?1 AND burst=?2 AND state IN ('pending','skipped')",
        params![tenant, burst],
    )?;
    Ok(())
}

/// The bed's units that no row claims yet, in order.
fn unclaimed(
    tx: &Transaction<'_>,
    tenant: &str,
    bed: &ConfirmedBed,
) -> rusqlite::Result<Vec<(i64, i64)>> {
    let mut claimed = tx.prepare_cached(
        "SELECT 1 FROM spoolman_deduction_units WHERE tenant_id=?1 AND link_id=?2 AND part_id=?3 AND unit_index=?4",
    )?;
    let mut units = Vec::new();
    for &(part, index) in &bed.units {
        if !claimed.exists(params![tenant, bed.link_id, part, index])?
            && !units.contains(&(part, index))
        {
            units.push((part, index));
        }
    }
    Ok(units)
}

/// Whether the Spoolman Module is on for the Build, or `None` when a stored
/// Module setting is not the typed shape. Checkoff must not fail on that.
fn module_on(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
) -> Result<Option<bool>, DeductionError> {
    let plan = u64::try_from(profile)
        .ok()
        .and_then(|id| PositiveId::new(id).ok());
    match crate::features::resolve_for_send(tx, tenant, plan) {
        Ok(view) => Ok(Some(view.effective().enabled(Feature::Spoolman))),
        Err(error)
            if error
                .downcast_ref::<crate::features::InvalidLayer>()
                .is_some() =>
        {
            Ok(None)
        }
        Err(error) => Err(WriterFailure::Internal(error).into()),
    }
}

/// The link's grams, its units and their basis.
fn link_grams(
    tx: &Transaction<'_>,
    tenant: &str,
    link: &str,
) -> Result<Option<(f64, u32, GramsBasis)>, DeductionError> {
    let row = tx
        .query_row(
            "SELECT grams, units, basis FROM print_link_grams WHERE tenant_id=?1 AND link_id=?2",
            params![tenant, link],
            |row| {
                Ok((
                    row.get::<_, f64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    // The schema keeps grams and units positive and the basis known; a row
    // outside that reads as no grams rather than failing Checkoff.
    Ok(row.and_then(|(grams, units, basis)| {
        let units = u32::try_from(units).ok().filter(|units| *units > 0)?;
        (grams > 0.0 && grams.is_finite()).then_some(())?;
        Some((grams, units, GramsBasis::parse(&basis)?))
    }))
}

/// The `(integration, spool)` the Accepted Part pins, if any.
fn pinned_spool(
    tx: &Transaction<'_>,
    tenant: &str,
    part: i64,
) -> rusqlite::Result<Option<(String, i64)>> {
    // A pin that is not text reads as no pin rather than failing Checkoff.
    let pin: Option<Option<String>> = tx
        .query_row(
            "SELECT spoolman_spool_id FROM parts WHERE tenant_id=?1 AND id=?2",
            params![tenant, part],
            |row| Ok(row.get_ref(0)?.as_str().ok().map(str::to_owned)),
        )
        .optional()?;
    Ok(pin.flatten().as_deref().and_then(spool_pin))
}

/// `(integration, spool)` for `spoolman:{integration}:spool:{n}`.
fn spool_pin(id: &str) -> Option<(String, i64)> {
    crate::read_model::stock::spoolman_ref(id.trim(), ":spool:")
        .map(|(integration, spool)| (integration.to_owned(), spool))
}

/// Every stored Spoolman integration and whether it is enabled; `Err` when
/// the integrations document is not the typed shape.
fn spoolman_hosts(tx: &Transaction<'_>, tenant: &str) -> Result<BTreeMap<String, bool>, ()> {
    let text: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='integrations' AND value<>''",
            params![tenant],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| ())?;
    let Some(text) = text else {
        return Ok(BTreeMap::new());
    };
    let entries = integrations::parse(&text).map_err(|_| ())?;
    Ok(entries
        .iter()
        .map(|entry| entry.summary())
        .filter(|summary| summary.kind == ServiceKind::Spoolman)
        .map(|summary| (summary.id, summary.enabled))
        .collect())
}

fn insert(
    tx: &Transaction<'_>,
    (tenant, burst, now): (&str, &str, &str),
    bed: &ConfirmedBed,
    (state, reason, units): (State, Option<SkipReason>, usize),
    target: Option<&Target>,
) -> Result<i64, DeductionError> {
    tx.execute(
        "INSERT INTO spoolman_deductions (tenant_id,burst,link_id,profile_id,state,reason,units,integration_id,spool_id,grams,grams_basis,created_at,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
        params![
            tenant,
            burst,
            bed.link_id,
            bed.profile_id,
            state.as_str(),
            reason.map(SkipReason::as_str),
            i64::try_from(units).unwrap_or(i64::MAX),
            target.map(|t| &t.integration_id),
            target.map(|t| t.spool_id),
            target.map(|t| t.grams),
            target.map(|t| t.basis.as_str()),
            now,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

fn now(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
        row.get(0)
    })
}

/// Appends the send-state columns. A column that is already there is left as
/// it is, so a restarted step does not fail on its own write.
pub(crate) fn install_send_states(connection: &Connection) -> anyhow::Result<()> {
    for statement in include_str!("spoolman_deductions/columns50.sql").lines() {
        let statement = statement.trim();
        if statement.is_empty() || statement.starts_with("--") {
            continue;
        }
        let column = statement
            .split_whitespace()
            .nth(5)
            .ok_or_else(|| anyhow::anyhow!("Invalid deduction column migration"))?;
        crate::schema::add_column(connection, "spoolman_deductions", column, statement)?;
    }
    Ok(())
}

/// Every `sending` row becomes `unknown` / `interrupted`. Nothing is sent.
/// Called from schema open, after the columns exist and before other recovery.
pub(crate) fn interrupt_sending(connection: &mut Connection) -> anyhow::Result<()> {
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = now(&tx)?;
    tx.execute(
        "UPDATE spoolman_deductions
         SET state='unknown', failure='interrupted', failure_status=NULL, updated_at=?1
         WHERE state='sending'",
        [now],
    )?;
    tx.commit()?;
    Ok(())
}

/// `(state, failure, failure_status)` a finish stores, or a refusal that
/// writes nothing.
fn outcome(
    end: Outcome,
) -> Result<(&'static str, Option<&'static str>, Option<i64>), DeductionError> {
    Ok(match end {
        Outcome::Applied => ("applied", None, None),
        Outcome::ReplyLost => ("unknown", Some(Failure::ReplyLost.as_str()), None),
        Outcome::Unreachable => ("failed", Some(Failure::Unreachable.as_str()), None),
        Outcome::IntegrationMissing => ("failed", Some(Failure::IntegrationMissing.as_str()), None),
        Outcome::IntegrationDisabled => {
            ("failed", Some(Failure::IntegrationDisabled.as_str()), None)
        }
        Outcome::Refused(status) => {
            if !(100..=599).contains(&status) {
                return Err(DeductionError::StatusOutOfRange);
            }
            (
                "failed",
                Some(Failure::Refused.as_str()),
                Some(i64::from(status)),
            )
        }
    })
}

fn deduction(row: &Row<'_>) -> Result<Deduction, DeductionError> {
    let id: i64 = row.get("id")?;
    let invalid = || DeductionError::StoredRowInvalid { id };
    let state = State::parse(&row.get::<_, String>("state")?).ok_or_else(invalid)?;
    let reason = match row.get::<_, Option<String>>("reason")? {
        Some(text) => Some(SkipReason::parse(&text).ok_or_else(invalid)?),
        None => None,
    };
    let target = match (
        row.get::<_, Option<String>>("integration_id")?,
        row.get::<_, Option<i64>>("spool_id")?,
        row.get::<_, Option<f64>>("grams")?,
        row.get::<_, Option<String>>("grams_basis")?,
    ) {
        (Some(integration_id), Some(spool_id), Some(grams), Some(basis)) => Some(Target {
            integration_id,
            spool_id,
            grams,
            basis: GramsBasis::parse(&basis).ok_or_else(invalid)?,
        }),
        (None, None, None, None) => None,
        _ => return Err(invalid()),
    };
    if (state == State::Skipped) != reason.is_some() || (target.is_none() != reason.is_some()) {
        return Err(invalid());
    }
    let failure = match row.get::<_, Option<String>>("failure")? {
        Some(text) => Some(Failure::parse(&text).ok_or_else(invalid)?),
        None => None,
    };
    let failure_status = row.get::<_, Option<i64>>("failure_status")?;
    let spool_grams_before = row.get::<_, Option<f64>>("spool_grams_before")?;
    if failure.is_some() && !matches!(state, State::Failed | State::Unknown | State::Dismissed) {
        return Err(invalid());
    }
    if failure_status.is_some() && failure != Some(Failure::Refused) {
        return Err(invalid());
    }
    if let Some(status) = failure_status
        && !(100..=599).contains(&status)
    {
        return Err(invalid());
    }
    if spool_grams_before.is_some_and(|grams| !grams.is_finite()) {
        return Err(invalid());
    }
    Ok(Deduction {
        id,
        state,
        reason,
        link_id: row.get("link_id")?,
        profile_id: row.get("profile_id")?,
        units: row.get("units")?,
        target,
        failure,
        failure_status,
        spool_grams_before,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

#[derive(Clone)]
pub struct SpoolmanDeductionsClient {
    shared: Arc<Shared>,
    readers: SettingsClient,
    policy: AuthPolicy,
}

impl WriterOwner {
    pub fn spoolman_deductions(
        &self,
        policy: AuthPolicy,
    ) -> anyhow::Result<SpoolmanDeductionsClient> {
        crate::auth::validate_policy(policy)?;
        Ok(SpoolmanDeductionsClient {
            shared: self.client.shared.clone(),
            readers: self.client(),
            policy,
        })
    }
}

impl SpoolmanDeductionsClient {
    /// The installation's deductions that match `filter`, newest first, at
    /// most [`LIST_LIMIT`].
    pub fn list(
        &self,
        credential: Credential,
        filter: Filter,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Vec<Deduction>, DeductionError> {
        let policy = self.policy;
        self.readers
            .read("spoolman_deductions.list", cancelled, wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                let mut statement = tx.prepare(
                "SELECT * FROM spoolman_deductions WHERE tenant_id=?1 AND (?2 IS NULL OR state=?2)
                 AND (?3 IS NULL OR link_id=?3) ORDER BY id DESC LIMIT ?4",
            )?;
                let mut rows = statement.query(params![
                    installation.id(),
                    filter.state.map(State::as_str),
                    filter.link_id,
                    LIST_LIMIT as i64,
                ])?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(deduction(row)?);
                }
                Ok(out)
            })?
    }

    /// Moves `id` from `pending` to `sending` and stores `grams_before`.
    /// The write commits before this returns, so the caller can send after
    /// it. `Ok(None)` when the row is not `pending`: nothing changes, and a
    /// later call cannot claim it either.
    pub fn claim(
        &self,
        credential: Credential,
        id: i64,
        grams_before: Option<f64>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Option<Claimed>, DeductionError> {
        if grams_before.is_some_and(|grams| !grams.is_finite()) {
            return Err(DeductionError::GramsBeforeNotFinite);
        }
        let policy = self.policy;
        self.shared.write(
            "spoolman_deductions.claim",
            cancelled,
            wait,
            move |tx, _| {
                let installation = authorize(tx, &credential, policy)?;
                claim_pending(tx, installation.id(), id, grams_before)
            },
        )?
    }

    /// Records `end` on `id`. Applied only while the row is `sending`.
    /// `Ok(false)` when it is not: the row is unchanged.
    pub fn finish(
        &self,
        credential: Credential,
        id: i64,
        end: Outcome,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<bool, DeductionError> {
        let (state, failure, status) = outcome(end)?;
        let policy = self.policy;
        self.shared.write(
            "spoolman_deductions.finish",
            cancelled,
            wait,
            move |tx, _| {
                let installation = authorize(tx, &credential, policy)?;
                finish_sending(tx, installation.id(), id, state, failure, status)
            },
        )?
    }

    /// Registers `wake` to run after a verify burst commits. The callback
    /// runs on the writer thread and must not use storage. A later call
    /// replaces it.
    #[cfg(feature = "desktop_rebuild")]
    pub fn on_burst(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.shared.set_deduction_wake(wake);
    }

    /// The oldest `pending` row whose Module is on, or why nothing is sendable.
    /// Does not claim.
    #[cfg(feature = "desktop_rebuild")]
    pub fn next_send(
        &self,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<SendWork, DeductionError> {
        let policy = self.policy;
        self.readers.read(
            "spoolman_deductions.next_send",
            cancelled,
            wait,
            move |tx| {
                let installation = runtime_tenant(tx, policy)?;
                next_pending(tx, installation.id())
            },
        )?
    }

    /// Claims `id` when it is `pending` and the Module is on for its Build.
    /// The write commits before this returns. `Held` leaves the row `pending`.
    #[cfg(feature = "desktop_rebuild")]
    pub fn claim_for_send(
        &self,
        id: i64,
        grams_before: Option<f64>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<ModuleClaim, DeductionError> {
        if grams_before.is_some_and(|grams| !grams.is_finite()) {
            return Err(DeductionError::GramsBeforeNotFinite);
        }
        let policy = self.policy;
        self.shared.write(
            "spoolman_deductions.claim_for_send",
            cancelled,
            wait,
            move |tx, _| {
                let installation = runtime_tenant(tx, policy)?;
                claim_for_module(tx, installation.id(), id, grams_before)
            },
        )?
    }

    /// Records `end` on `id` while it is `sending`. `Ok(false)` leaves the row
    /// unchanged.
    #[cfg(feature = "desktop_rebuild")]
    pub fn finish_for_send(
        &self,
        id: i64,
        end: Outcome,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<bool, DeductionError> {
        let (state, failure, status) = outcome(end)?;
        let policy = self.policy;
        self.shared.write(
            "spoolman_deductions.finish_for_send",
            cancelled,
            wait,
            move |tx, _| {
                let installation = runtime_tenant(tx, policy)?;
                finish_sending(tx, installation.id(), id, state, failure, status)
            },
        )?
    }
}

#[cfg(feature = "desktop_rebuild")]
fn runtime_tenant(
    tx: &Transaction<'_>,
    policy: AuthPolicy,
) -> Result<crate::write::Installation, WriterFailure> {
    if !matches!(
        policy.session_tenant,
        SessionTenantPolicy::SingleAccountDefault
    ) {
        return Err(WriterFailure::Auth(AuthFailure::SessionRequired));
    }
    single_account(tx)
}

#[cfg(feature = "desktop_rebuild")]
fn next_pending(tx: &Transaction<'_>, tenant: &str) -> Result<SendWork, DeductionError> {
    let pending = {
        let mut statement = tx.prepare(
            "SELECT id, profile_id, integration_id, spool_id, grams
             FROM spoolman_deductions
             WHERE tenant_id=?1 AND state='pending' ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![tenant], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<(i64, i64, String, i64, f64)>, _>>()?
    };
    let mut held = false;
    for (id, profile, integration_id, spool_id, grams) in pending {
        if !grams.is_finite() || grams <= 0.0 || spool_id <= 0 || integration_id.is_empty() {
            return Err(DeductionError::StoredRowInvalid { id });
        }
        match module_on(tx, tenant, profile)? {
            Some(true) => return ready(tx, tenant, id, spool_id, grams, &integration_id),
            Some(false) | None => held = true,
        }
    }
    Ok(if held { SendWork::Held } else { SendWork::Idle })
}

#[cfg(feature = "desktop_rebuild")]
fn ready(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    spool_id: i64,
    grams: f64,
    integration_id: &str,
) -> Result<SendWork, DeductionError> {
    let found = crate::read_model::stock::spoolman_integration(tx, tenant, integration_id)
        .map_err(WriterFailure::from)?;
    Ok(match found {
        Some(integration) if integration.enabled => SendWork::Ready {
            id,
            spool_id,
            grams,
            integration,
        },
        Some(_) => SendWork::Disabled { id },
        None => SendWork::Missing { id },
    })
}

#[cfg(feature = "desktop_rebuild")]
fn claim_for_module(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    grams_before: Option<f64>,
) -> Result<ModuleClaim, DeductionError> {
    let row = tx
        .query_row(
            "SELECT state, profile_id FROM spoolman_deductions WHERE tenant_id=?1 AND id=?2",
            params![tenant, id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((state, profile)) = row else {
        return Ok(ModuleClaim::Gone);
    };
    if state != State::Pending.as_str() {
        return Ok(ModuleClaim::Gone);
    }
    if module_on(tx, tenant, profile)? != Some(true) {
        return Ok(ModuleClaim::Held);
    }
    Ok(match claim_pending(tx, tenant, id, grams_before)? {
        Some(claimed) => ModuleClaim::Claimed(claimed),
        None => ModuleClaim::Gone,
    })
}

fn finish_sending(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    state: &str,
    failure: Option<&str>,
    status: Option<i64>,
) -> Result<bool, DeductionError> {
    let now = now(tx)?;
    let changed = tx.execute(
        "UPDATE spoolman_deductions
         SET state=?1, failure=?2, failure_status=?3, updated_at=?4
         WHERE tenant_id=?5 AND id=?6 AND state='sending'",
        params![state, failure, status, now, tenant, id],
    )?;
    Ok(changed == 1)
}

fn claim_pending(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    grams_before: Option<f64>,
) -> Result<Option<Claimed>, DeductionError> {
    let now = now(tx)?;
    let changed = tx.execute(
        "UPDATE spoolman_deductions
         SET state='sending', spool_grams_before=?1, updated_at=?2
         WHERE tenant_id=?3 AND id=?4 AND state='pending'",
        params![grams_before, now, tenant, id],
    )?;
    if changed != 1 {
        return Ok(None);
    }
    let (integration_id, spool_id, grams): (Option<String>, Option<i64>, Option<f64>) = tx
        .query_row(
            "SELECT integration_id, spool_id, grams FROM spoolman_deductions WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let (Some(integration_id), Some(spool_id), Some(grams)) = (integration_id, spool_id, grams)
    else {
        return Err(DeductionError::StoredRowInvalid { id });
    };
    if !grams.is_finite() || grams <= 0.0 {
        return Err(DeductionError::StoredRowInvalid { id });
    }
    Ok(Some(Claimed {
        id,
        integration_id,
        spool_id,
        grams,
        spool_grams_before: grams_before,
    }))
}

#[cfg(test)]
mod tests {
    use super::{install_send_states, share, spool_pin};
    use rusqlite::params;

    #[test]
    fn a_share_multiplies_before_it_divides_and_is_positive_and_finite() {
        // Dividing first gives 7.000000000000001, which rounds up to 8 g.
        assert_eq!(share(7.0, 100, 100), Some(7.0));
        assert_eq!(share(40.0, 4, 2), Some(20.0));
        assert_eq!(share(f64::from_bits(1), 4, 1), None);
        assert_eq!(share(f64::MAX, 1, 2), None);
    }

    #[test]
    fn spool_pins_name_one_integration_and_a_positive_spool() {
        assert_eq!(
            spool_pin(" spoolman:sm-1:spool:7 "),
            Some(("sm-1".into(), 7))
        );
        for refused in [
            "spoolman:sm-1:spool:0",
            "spoolman::spool:7",
            "spoolman:a:b:spool:7",
            "spoolman:sm-1:filament:7",
            "sm-1:spool:7",
        ] {
            assert_eq!(spool_pin(refused), None, "{refused}");
        }
    }

    #[test]
    fn send_state_columns_accept_only_a_failure_on_a_terminal_row() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("spoolman_deductions/schema.sql"))
            .unwrap();
        install_send_states(&conn).unwrap();
        install_send_states(&conn).unwrap();
        let insert = |state: &str, failure: Option<&str>, status: Option<i64>| {
            conn.execute(
                "INSERT INTO spoolman_deductions
                 (tenant_id,burst,link_id,profile_id,state,reason,units,integration_id,spool_id,grams,grams_basis,created_at,updated_at,failure,failure_status)
                 VALUES ('default','b','link',1,?1,NULL,1,'sm',7,1.5,'estimated','t','t',?2,?3)",
                params![state, failure, status],
            )
        };
        insert("failed", Some("unreachable"), None).unwrap();
        insert("unknown", Some("reply_lost"), None).unwrap();
        insert("dismissed", Some("interrupted"), None).unwrap();
        insert("failed", Some("refused"), Some(422)).unwrap();
        insert("failed", None, None).unwrap();
        insert("unknown", None, None).unwrap();
        insert("pending", None, None).unwrap();
        let skipped = conn.execute(
            "INSERT INTO spoolman_deductions
             (tenant_id,burst,link_id,profile_id,state,reason,units,created_at,updated_at)
             VALUES ('default','skip','link',1,'skipped','module_off',1,'t','t')",
            [],
        );
        assert!(
            skipped
                .unwrap_err()
                .to_string()
                .contains("CHECK constraint"),
            "a new skip reason must stay refused"
        );
        for (state, failure, status) in [
            ("pending", Some("unreachable"), None),
            ("sending", Some("interrupted"), None),
            ("applied", Some("reply_lost"), None),
            ("failed", Some("no_grams"), None),
            ("failed", Some("refused"), Some(99)),
            ("failed", Some("refused"), Some(600)),
            ("failed", Some("unreachable"), Some(500)),
            ("pending", None, Some(422)),
        ] {
            let Err(error) = insert(state, failure, status) else {
                panic!("{state} {failure:?} {status:?} was accepted");
            };
            assert!(
                error.to_string().contains("CHECK constraint"),
                "{state} {failure:?} {status:?}: {error}"
            );
        }
    }
}
