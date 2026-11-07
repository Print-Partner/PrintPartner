//! Filament grams per Printer Checkoff link (GRE-1165).
//!
//! A link is one send of a file to a printer, and its Checkoff units are the
//! units that send prints. Its grams cover all of those units. The send path
//! records the slicer's estimate when the upload lands, and the status watcher
//! replaces it with the measured value when the print finishes, as the owner
//! decided on 2026-11-04. The table is Rust-only and installed by schema step
//! [`SCHEMA_STEP`]; Node never writes it.
//!
//! The Spoolman shortfall reader (`read_model::stock`) spreads a link's grams
//! evenly over its `units` and gives each Part the grams of the units checked
//! off for it under that link.

use anyhow::{Result, ensure};
use rusqlite::{Transaction, params};

/// The schema step that installs the table.
pub(crate) const SCHEMA_STEP: u64 = 46;

/// Where a link's grams came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// The slicer's estimate, read from the file at send.
    Estimated,
    /// What the printer reported using when the print finished.
    Measured,
}

impl Basis {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Estimated => "estimated",
            Self::Measured => "measured",
        }
    }
}

/// Records `grams` for the `units` the send of `link_id` prints. A measured
/// value replaces an estimate, and an estimate never replaces a measured value.
/// Returns whether the stored row changed.
pub fn record(
    tx: &Transaction<'_>,
    tenant: &str,
    link_id: &str,
    grams: f64,
    units: u64,
    basis: Basis,
) -> Result<bool> {
    ensure!(
        grams.is_finite() && grams > 0.0,
        "Grams must be a positive number"
    );
    ensure!(units > 0, "A link's grams must cover at least one unit");
    ensure!(!link_id.is_empty(), "A link ID is required");
    let changed = tx.execute(
        "INSERT INTO print_link_grams(tenant_id,link_id,grams,units,basis,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(tenant_id,link_id) DO UPDATE SET
           grams=excluded.grams,units=excluded.units,basis=excluded.basis,updated_at=excluded.updated_at
         WHERE NOT (print_link_grams.basis='measured' AND excluded.basis='estimated')",
        params![
            tenant,
            link_id,
            grams,
            i64::try_from(units)?,
            basis.name(),
            crate::auth::timestamp(0)
        ],
    )?;
    Ok(changed != 0)
}

#[cfg(test)]
mod tests;
