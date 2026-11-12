//! The typed `printer.fleet` settings document, one row per printer in
//! stored order, and the printer read.
//!
//! The shape uses the field names Farm, Accept, Checkoff and the host lookup
//! already read. It is strict and has no version field, like
//! [`super::integrations`]: it changes only through a numbered schema step.
//!
//! Lengths are whole millimetres, as integers. A fractional value is not this
//! shape, and Node accepted one. The import (GRE-412) is to normalise such a
//! row: bed width, depth and height round down, the margin rounds up, and each
//! change is listed in the import report. That conversion is not built yet
//! (`import/inventory.rs` lists `printer.fleet` as pending), so until it is, a
//! document with a fractional length is refused whole.
//!
//! `loaded_filaments`, `device_id`, `preset_id` and `preferred_slicer` are
//! stored and kept; no operation here sets them.

use super::{DocumentFault, InvalidDocument, PrinterSetupClient, PrinterSetupError, SetupDocument};
use crate::{
    read_model::Credential,
    write::{Installation, authorize},
};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use std::{sync::atomic::AtomicBool, time::Duration};

const FLEET_KEY: &str = "printer.fleet";

/// One stored printer. `bed_width_mm`, `bed_depth_mm`, `bed_height_mm` and
/// `margin_mm` are whole millimetres.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Printer {
    pub id: String,
    pub name: String,
    pub model: String,
    pub bed_width_mm: u32,
    pub bed_depth_mm: u32,
    /// `None` only in a row that came from an installation without one;
    /// Accept cannot place on such a printer.
    pub bed_height_mm: Option<u32>,
    pub margin_mm: u32,
    pub max_filament_slots: u8,
    pub loaded_filaments: Vec<LoadedFilament>,
    /// The Moonraker, PrusaLink or Bambu integration that reaches the printer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_slicer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoadedFilament {
    pub slot: u8,
    pub filament_color_id: Option<String>,
    pub label: String,
}

/// The stored document, or which entry is not the typed shape and why.
fn parse(text: &str) -> Result<Vec<Printer>, InvalidDocument> {
    let fault = |entry, reason| InvalidDocument {
        document: SetupDocument::Printers,
        entry,
        reason,
    };
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(text).map_err(|_| fault(None, DocumentFault::NotAList))?;
    let mut printers: Vec<Printer> = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        let printer = serde_json::from_value::<Printer>(row)
            .ok()
            .filter(|printer| !printer.id.is_empty())
            .ok_or_else(|| fault(Some(index), DocumentFault::WrongShape))?;
        if printers.iter().any(|known| known.id == printer.id) {
            return Err(fault(Some(index), DocumentFault::DuplicateId));
        }
        printers.push(printer);
    }
    Ok(printers)
}

fn load(
    tx: &Transaction<'_>,
    installation: &Installation,
) -> Result<Vec<Printer>, PrinterSetupError> {
    match super::stored(tx, installation, FLEET_KEY)? {
        Some(text) => parse(&text).map_err(super::invalid_document),
        None => Ok(Vec::new()),
    }
}

impl PrinterSetupClient {
    /// The stored printer with `id`.
    pub fn read_printer(
        &self,
        credential: Credential,
        id: &str,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Printer, PrinterSetupError> {
        let name = "printer_setup.read_printer";
        self.readers.read(name, cancelled, wait, |tx| {
            let installation = authorize(tx, &credential, self.policy)?;
            let printers = load(tx, &installation)?;
            let found = printers.into_iter().find(|printer| printer.id == id);
            found.ok_or(PrinterSetupError::NotFound)
        })?
    }
}
