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
//! Errors are [`DeductionError`]. This module holds no status codes and no
//! response text; `pp-api` maps the variants.

use crate::{
    SettingsClient, WriterFailure, WriterOwner,
    auth::AuthPolicy,
    printer_setup::{ServiceKind, integrations},
    read_model::Credential,
    write::authorize,
};
use pp_domain::{PositiveId, features::Feature};
use rusqlite::{OptionalExtension, Row, Transaction, params};
use std::{collections::BTreeMap, sync::atomic::AtomicBool, time::Duration};

pub(crate) const SCHEMA_STEP: u64 = 49;
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
    pub created_at: String,
    pub updated_at: String,
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
    let (integration, number) = id.trim().strip_prefix("spoolman:")?.split_once(":spool:")?;
    let number = number.parse::<i64>().ok().filter(|n| *n > 0)?;
    (!integration.is_empty() && !integration.contains(':'))
        .then(|| (integration.to_owned(), number))
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
    Ok(Deduction {
        id,
        state,
        reason,
        link_id: row.get("link_id")?,
        profile_id: row.get("profile_id")?,
        units: row.get("units")?,
        target,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

#[derive(Clone)]
pub struct SpoolmanDeductionsClient {
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
}

#[cfg(test)]
mod tests {
    use super::{share, spool_pin};

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
}
