//! The typed `printer.fleet` settings document, one row per printer in
//! stored order, and the four printer operations.
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
//! New values are checked in [`apply`] against what Accept can use: a bed
//! size is 1 to [`MAX_BED_MM`]; the margin is less than half the bed's width
//! and depth; a printer has 1 to [`MAX_FILAMENT_SLOTS`] filament slots; a new
//! link names a stored integration that reaches a printer. Values already
//! stored are not checked again, so a row that predates a check can still be
//! edited. The one exception is the stored margin, which is compared with a
//! bed size the edit changes.
//!
//! `loaded_filaments`, `device_id`, `preset_id` and `preferred_slicer` are
//! stored and kept; no operation here sets them. Lowering the slot count
//! drops the empty `loaded_filaments` entries above it.

use super::{
    DocumentFault, Field, InvalidDocument, InvalidField, MAX_PRINTERS, PrinterSetupClient,
    PrinterSetupError, Reason, ServiceKind, SetupDocument, integrations::MAX_NAME_BYTES,
};
use crate::{
    read_model::Credential,
    write::{Installation, authorize},
};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use std::{sync::atomic::AtomicBool, time::Duration};

const FLEET_KEY: &str = "printer.fleet";
pub const MAX_BED_MM: u32 = 10_000;
pub const MAX_FILAMENT_SLOTS: u8 = 4;

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

/// A new printer, before validation. Lengths are whole millimetres.
#[derive(Debug)]
pub struct NewPrinter {
    pub name: String,
    pub model: String,
    pub bed_width_mm: u32,
    pub bed_depth_mm: u32,
    pub bed_height_mm: u32,
    pub margin_mm: u32,
    pub max_filament_slots: u8,
    pub integration_id: Option<String>,
}

/// The fields to change. `None` keeps the stored value; `integration_id:
/// Some(None)` unlinks the printer.
#[derive(Debug, Default)]
pub struct PrinterPatch {
    pub name: Option<String>,
    pub model: Option<String>,
    pub integration_id: Option<Option<String>>,
    pub max_filament_slots: Option<u8>,
    pub margin_mm: Option<u32>,
    pub bed_width_mm: Option<u32>,
    pub bed_depth_mm: Option<u32>,
    pub bed_height_mm: Option<u32>,
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

/// A name or model without surrounding whitespace, checked.
fn text(field: Field, value: &str, fail: &mut impl FnMut(Field, Reason)) -> String {
    let value = value.trim();
    if value.is_empty() {
        fail(field, Reason::Empty);
    } else if value.len() > MAX_NAME_BYTES {
        fail(field, Reason::TooLong);
    }
    value.to_owned()
}

/// Whether `id` names a stored integration that reaches a printer.
fn reaches_a_printer(
    tx: &Transaction<'_>,
    installation: &Installation,
    id: &str,
) -> Result<bool, PrinterSetupError> {
    let hosts = [
        ServiceKind::Moonraker,
        ServiceKind::Prusalink,
        ServiceKind::Bambu,
    ];
    let integrations = super::load(tx, installation)?;
    Ok(integrations
        .iter()
        .any(|entry| entry.id() == id && hosts.contains(&entry.summary().kind)))
}

/// Applies `patch` to `row` and checks each value it sets. Create and edit
/// both go through here, so the rules live in one place. Every invalid field
/// is reported together; a slot that holds filament is reported only when the
/// values themselves are valid.
fn apply(
    tx: &Transaction<'_>,
    installation: &Installation,
    row: &mut Printer,
    patch: PrinterPatch,
) -> Result<(), PrinterSetupError> {
    let mut invalid = Vec::new();
    let mut fail = |field, reason| invalid.push(InvalidField { field, reason });
    if let Some(name) = patch.name {
        row.name = text(Field::Name, &name, &mut fail);
    }
    if let Some(model) = patch.model {
        row.model = text(Field::Model, &model, &mut fail);
    }
    let bed = |mm: u32| (1..=MAX_BED_MM).contains(&mm);
    let resized = patch.bed_width_mm.is_some() || patch.bed_depth_mm.is_some();
    let sizes = [
        (Field::BedWidth, patch.bed_width_mm),
        (Field::BedDepth, patch.bed_depth_mm),
        (Field::BedHeight, patch.bed_height_mm),
    ];
    let mut sized = true;
    for (field, _) in sizes.iter().filter(|(_, mm)| mm.is_some_and(|mm| !bed(mm))) {
        fail(*field, Reason::OutOfRange);
        sized = false;
    }
    row.bed_width_mm = patch.bed_width_mm.unwrap_or(row.bed_width_mm);
    row.bed_depth_mm = patch.bed_depth_mm.unwrap_or(row.bed_depth_mm);
    row.bed_height_mm = patch.bed_height_mm.or(row.bed_height_mm);
    // A new margin is checked against the bed, and so is the stored margin
    // when the bed changes. A bed that is itself refused is not compared.
    row.margin_mm = patch.margin_mm.unwrap_or(row.margin_mm);
    if sized && (patch.margin_mm.is_some() || resized) {
        // The test Accept applies: twice the margin must leave some bed.
        let doubled = u64::from(row.margin_mm) * 2;
        if doubled >= u64::from(row.bed_width_mm.min(row.bed_depth_mm)) {
            fail(Field::Margin, Reason::OutOfRange);
        }
    }
    let mut slot_in_use = false;
    match patch.max_filament_slots {
        Some(slots) if !(1..=MAX_FILAMENT_SLOTS).contains(&slots) => {
            fail(Field::FilamentSlots, Reason::OutOfRange);
        }
        Some(slots) => {
            let removed = |filament: &LoadedFilament| filament.slot > slots;
            let holds = |filament: &LoadedFilament| {
                filament.filament_color_id.is_some() || !filament.label.is_empty()
            };
            slot_in_use = row
                .loaded_filaments
                .iter()
                .any(|one| removed(one) && holds(one));
            row.loaded_filaments.retain(|filament| !removed(filament));
            row.max_filament_slots = slots;
        }
        None => {}
    }
    // A link is checked only when it changes, so a printer whose integration
    // was deleted can still be edited without unlinking it first.
    if let Some(link) = patch
        .integration_id
        .filter(|link| *link != row.integration_id)
    {
        if let Some(id) = link.as_deref()
            && !reaches_a_printer(tx, installation, id)?
        {
            fail(Field::IntegrationId, Reason::NotAPrinterHost);
        }
        row.integration_id = link;
    }
    if !invalid.is_empty() {
        return Err(PrinterSetupError::Invalid(invalid));
    }
    if slot_in_use {
        return Err(PrinterSetupError::SlotHoldsFilament);
    }
    Ok(())
}

impl PrinterSetupClient {
    /// One write to the stored printers: it proves the credential, loads the
    /// document, runs `change` and stores the document when `change` answers.
    fn change_printers(
        &self,
        name: &'static str,
        (credential, cancelled, wait): (Credential, &AtomicBool, Duration),
        change: impl FnOnce(
            &Transaction<'_>,
            &Installation,
            &mut Vec<Printer>,
        ) -> Result<Printer, PrinterSetupError>
        + Send
        + 'static,
    ) -> Result<Printer, PrinterSetupError> {
        let policy = self.policy;
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let mut printers = load(tx, &installation)?;
            let answer = change(tx, &installation, &mut printers)?;
            super::store(tx, &installation, FLEET_KEY, &printers)?;
            Ok(answer)
        })?
    }

    /// Stores a new printer and answers it.
    pub fn create_printer(
        &self,
        credential: Credential,
        new: NewPrinter,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Printer, PrinterSetupError> {
        let caller = (credential, cancelled, wait);
        let name = "printer_setup.create_printer";
        self.change_printers(name, caller, move |tx, installation, printers| {
            let mut row = Printer {
                id: hex::encode(rand::random::<[u8; 16]>()),
                name: String::new(),
                model: String::new(),
                bed_width_mm: 0,
                bed_depth_mm: 0,
                bed_height_mm: None,
                margin_mm: 0,
                max_filament_slots: 1,
                loaded_filaments: Vec::new(),
                integration_id: None,
                device_id: None,
                preset_id: None,
                preferred_slicer: None,
            };
            let values = PrinterPatch {
                name: Some(new.name),
                model: Some(new.model),
                bed_width_mm: Some(new.bed_width_mm),
                bed_depth_mm: Some(new.bed_depth_mm),
                bed_height_mm: Some(new.bed_height_mm),
                margin_mm: Some(new.margin_mm),
                max_filament_slots: Some(new.max_filament_slots),
                integration_id: Some(new.integration_id),
            };
            apply(tx, installation, &mut row, values)?;
            if printers.len() >= MAX_PRINTERS {
                return Err(PrinterSetupError::LimitReached);
            }
            printers.push(row.clone());
            Ok(row)
        })
    }

    /// Changes the fields `patch` sets and answers the stored printer.
    pub fn update_printer(
        &self,
        credential: Credential,
        id: String,
        patch: PrinterPatch,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Printer, PrinterSetupError> {
        let caller = (credential, cancelled, wait);
        let name = "printer_setup.update_printer";
        self.change_printers(name, caller, move |tx, installation, printers| {
            let row = printers.iter_mut().find(|printer| printer.id == id);
            let row = row.ok_or(PrinterSetupError::NotFound)?;
            apply(tx, installation, row, patch)?;
            Ok(row.clone())
        })
    }

    /// Removes the printer with `id` and answers it. Its integration stays.
    pub fn delete_printer(
        &self,
        credential: Credential,
        id: String,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Printer, PrinterSetupError> {
        let caller = (credential, cancelled, wait);
        let name = "printer_setup.delete_printer";
        self.change_printers(name, caller, move |_, _, printers| {
            let index = printers.iter().position(|printer| printer.id == id);
            Ok(printers.remove(index.ok_or(PrinterSetupError::NotFound)?))
        })
    }

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
