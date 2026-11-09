//! Filament grams per Printer Checkoff link (GRE-1165).
//!
//! A link is one send of a file to a printer, and its Checkoff units are the
//! units that send prints. Its grams cover all of those units. The send path
//! records the slicer's estimate when the upload lands ([`record_estimate`]),
//! with the slicer's grams per millimetre of filament when the file states
//! both. When the print finishes, the status watcher records the length the
//! printer measured ([`record_measured`]), and that length times the stored
//! grams per millimetre replaces the estimate, as the owner decided on
//! 2026-11-04. The table is Rust-only and installed by schema step
//! [`SCHEMA_STEP`]; Node never writes it.
//!
//! The Spoolman shortfall reader (`read_model::stock`) spreads a link's grams
//! evenly over its `units` and gives each Part the grams of the units checked
//! off for it under that link.

use crate::{Envelope, Shared};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};

/// The schema step that installs the table.
pub(crate) const SCHEMA_STEP: u64 = 46;

/// Where a link's grams came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// The slicer's estimate, read from the file at send.
    Estimated,
    /// The printer's measured length at finish, in grams.
    Measured,
}

/// What [`record_measured`] did with a finished print's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measurement {
    /// The measured grams replaced the estimate.
    Recorded,
    /// The length is stored, but the send kept no grams per millimetre, so
    /// the estimate stays.
    NoFactor,
    /// The send recorded no estimate, so there is nothing to convert with.
    NoEstimate,
    /// The link already has a measured length; the first measurement stays.
    AlreadyMeasured,
}

/// Records the slicer's estimate of `grams` for the `units` the send of
/// `link_id` prints, with the slicer's `grams_per_mm` when known. An
/// estimate never replaces anything recorded after a measurement. Returns
/// whether the stored row changed.
pub fn record_estimate(
    tx: &Transaction<'_>,
    tenant: &str,
    link_id: &str,
    grams: f64,
    units: u64,
    grams_per_mm: Option<f64>,
) -> Result<bool> {
    ensure!(positive(grams), "Grams must be a positive number");
    ensure!(units > 0, "A link's grams must cover at least one unit");
    ensure!(!link_id.is_empty(), "A link ID is required");
    ensure!(
        grams_per_mm.is_none_or(positive),
        "Grams per millimetre must be a positive number"
    );
    let changed = tx.execute(
        "INSERT INTO print_link_grams(tenant_id,link_id,grams,units,basis,grams_per_mm,measured_mm,updated_at)
         VALUES(?1,?2,?3,?4,'estimated',?5,NULL,?6)
         ON CONFLICT(tenant_id,link_id) DO UPDATE SET
           grams=excluded.grams,units=excluded.units,grams_per_mm=excluded.grams_per_mm,updated_at=excluded.updated_at
         WHERE print_link_grams.measured_mm IS NULL",
        params![
            tenant,
            link_id,
            grams,
            i64::try_from(units)?,
            grams_per_mm,
            crate::auth::timestamp(0)
        ],
    )?;
    Ok(changed != 0)
}

/// Records the filament length, in millimetres, the printer reported using
/// for the send of `link_id`, and converts it with the send's grams per
/// millimetre. A link is measured once: a later length for it is ignored.
pub fn record_measured(
    tx: &Transaction<'_>,
    tenant: &str,
    link_id: &str,
    measured_mm: f64,
) -> Result<Measurement> {
    ensure!(
        positive(measured_mm),
        "A measured length must be a positive number"
    );
    let row: Option<(Option<f64>, Option<f64>)> = tx
        .query_row(
            "SELECT grams_per_mm,measured_mm FROM print_link_grams WHERE tenant_id=?1 AND link_id=?2",
            params![tenant, link_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((factor, measured)) = row else {
        return Ok(Measurement::NoEstimate);
    };
    if measured.is_some() {
        return Ok(Measurement::AlreadyMeasured);
    }
    let at = crate::auth::timestamp(0);
    let Some(grams_per_mm) = factor else {
        tx.execute(
            "UPDATE print_link_grams SET measured_mm=?3,updated_at=?4 WHERE tenant_id=?1 AND link_id=?2",
            params![tenant, link_id, measured_mm, at],
        )?;
        return Ok(Measurement::NoFactor);
    };
    let grams = measured_mm * grams_per_mm;
    ensure!(positive(grams), "Measured grams must be a positive number");
    tx.execute(
        "UPDATE print_link_grams SET grams=?3,basis='measured',measured_mm=?4,updated_at=?5 WHERE tenant_id=?1 AND link_id=?2",
        params![tenant, link_id, grams, measured_mm, at],
    )?;
    Ok(Measurement::Recorded)
}

/// Records the lengths the status watcher reads when a print finishes. The
/// watcher writes nothing else: Checkoff state is never touched here.
#[derive(Clone)]
pub struct PrintGramsClient {
    shared: Arc<Shared>,
    tenant: String,
}

impl crate::WriterOwner {
    pub fn print_grams(&self, tenant: &str) -> PrintGramsClient {
        PrintGramsClient {
            shared: self.client.shared.clone(),
            tenant: tenant.to_owned(),
        }
    }
}

pub(crate) struct Command {
    tenant: String,
    integration_id: String,
    filename: String,
    measured_mm: f64,
    finished_at: String,
}

impl PrintGramsClient {
    /// Records `measured_mm` for the newest `watching`, `awaiting_verify` or
    /// `verified` Checkoff link of host
    /// `integration_id` whose file is `filename` and that existed at
    /// `finished_at`, when the watcher saw the print finish. A link created
    /// later is a newer send of the same file name and never gets this
    /// length. `None` when no such link matches.
    pub fn measured(
        &self,
        integration_id: &str,
        filename: &str,
        measured_mm: f64,
        finished_at: SystemTime,
        wait: Duration,
    ) -> Result<Option<Measurement>> {
        crate::read_model::submit(
            &self.shared,
            &AtomicBool::new(false),
            wait,
            "Storage stopped without a print grams result",
            |reply| Envelope::PrintGrams {
                command: Command {
                    tenant: self.tenant.clone(),
                    integration_id: integration_id.to_owned(),
                    filename: filename.to_owned(),
                    measured_mm,
                    finished_at: crate::auth::timestamp_at(finished_at.into()),
                },
                reply,
            },
        )
    }
}

pub(crate) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Option<Measurement>> {
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    let candidates = crate::checkoff_progress::finished_links(
        &tx,
        &command.tenant,
        &command.integration_id,
        &command.filename,
        &command.finished_at,
    )?;
    // The newest send of that file by creation time, never by list position
    // (ties broken by link ID). When it is already measured, the finish is a
    // reprint and records nothing; an older link is never filled instead.
    let Some((_, link)) = candidates.into_iter().max() else {
        return Ok(None);
    };
    let measured = record_measured(&tx, &command.tenant, &link, command.measured_mm)?;
    tx.commit()?;
    Ok(Some(measured))
}

fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

#[cfg(test)]
mod tests;
