//! The typed `printer.fleet` settings document, one row per printer in
//! stored order, and the four printer operations.
//!
//! Rows are written with the field names Farm, Accept, Checkoff and the host
//! lookup already read. The shape is strict and has no version field, like
//! [`super::integrations`]: it changes only through a numbered schema step.
//!
//! Lengths are whole millimetres. New values are checked here against what
//! Accept can use: a bed size is 1 to [`MAX_BED_MM`]; the margin is less than
//! half the bed's width and depth; a printer has 1 to [`MAX_FILAMENT_SLOTS`]
//! filament slots. Values already stored are not checked again, so a row that
//! predates a check can still be edited.
//!
//! `loaded_filaments`, `device_id`, `preset_id` and `preferred_slicer` are
//! stored and kept; no operation here sets them.

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

/// One stored printer.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Printer {
    pub id: String,
    pub name: String,
    pub model: String,
    pub bed_width_mm: u32,
    pub bed_depth_mm: u32,
    /// `None` only in a row that came from an installation without one;
    /// Accept cannot place on such a printer. Create requires a height.
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

/// A new printer, before validation.
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
    pub bed_width_mm: Option<u32>,
    pub bed_depth_mm: Option<u32>,
    pub bed_height_mm: Option<u32>,
    pub margin_mm: Option<u32>,
    pub max_filament_slots: Option<u8>,
    pub integration_id: Option<Option<String>>,
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
fn text(field: Field, value: &str, invalid: &mut Vec<InvalidField>) -> String {
    let value = value.trim();
    let reason = match value.len() {
        0 => Some(Reason::Empty),
        length if length > MAX_NAME_BYTES => Some(Reason::TooLong),
        _ => None,
    };
    invalid.extend(reason.map(|reason| InvalidField { field, reason }));
    value.to_owned()
}

/// Applies `patch` to `row` and checks each value it sets. Create and edit
/// both go through here, so the rules live in one place.
fn apply(row: &mut Printer, patch: PrinterPatch) -> Result<(), PrinterSetupError> {
    let mut invalid = Vec::new();
    let out_of_range = |field| InvalidField {
        field,
        reason: Reason::OutOfRange,
    };
    if let Some(name) = patch.name {
        row.name = text(Field::Name, &name, &mut invalid);
    }
    if let Some(model) = patch.model {
        row.model = text(Field::Model, &model, &mut invalid);
    }
    let bed = |mm: u32| (1..=MAX_BED_MM).contains(&mm);
    let resized = patch.bed_width_mm.is_some() || patch.bed_depth_mm.is_some();
    let sizes = [
        (Field::BedWidth, patch.bed_width_mm),
        (Field::BedDepth, patch.bed_depth_mm),
        (Field::BedHeight, patch.bed_height_mm),
    ];
    for (field, _) in sizes.iter().filter(|(_, mm)| mm.is_some_and(|mm| !bed(mm))) {
        invalid.push(out_of_range(*field));
    }
    row.bed_width_mm = patch.bed_width_mm.unwrap_or(row.bed_width_mm);
    row.bed_depth_mm = patch.bed_depth_mm.unwrap_or(row.bed_depth_mm);
    row.bed_height_mm = patch.bed_height_mm.or(row.bed_height_mm);
    if let Some(margin) = patch.margin_mm.or(resized.then_some(row.margin_mm)) {
        row.margin_mm = margin;
        // The test Accept applies: twice the margin must leave some bed.
        let doubled = u64::from(margin) * 2;
        if doubled >= u64::from(row.bed_width_mm.min(row.bed_depth_mm)) {
            invalid.push(out_of_range(Field::Margin));
        }
    }
    match patch.max_filament_slots {
        Some(slots) if !(1..=MAX_FILAMENT_SLOTS).contains(&slots) => {
            invalid.push(out_of_range(Field::FilamentSlots));
        }
        Some(slots) => {
            let holds = |filament: &LoadedFilament| {
                filament.slot > slots
                    && (filament.filament_color_id.is_some() || !filament.label.is_empty())
            };
            if row.loaded_filaments.iter().any(holds) {
                return Err(PrinterSetupError::SlotHoldsFilament);
            }
            row.loaded_filaments
                .retain(|filament| filament.slot <= slots);
            row.max_filament_slots = slots;
        }
        None => {}
    }
    if let Some(integration_id) = patch.integration_id {
        row.integration_id = integration_id;
    }
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(PrinterSetupError::Invalid(invalid))
    }
}

/// Refuses a link to anything but a stored integration that reaches a printer.
fn check_link(
    tx: &Transaction<'_>,
    installation: &Installation,
    integration_id: Option<&str>,
) -> Result<(), PrinterSetupError> {
    let Some(id) = integration_id else {
        return Ok(());
    };
    let hosts = [
        ServiceKind::Moonraker,
        ServiceKind::Prusalink,
        ServiceKind::Bambu,
    ];
    let integrations = super::load(tx, installation)?;
    let linked = integrations
        .iter()
        .any(|entry| entry.id() == id && hosts.contains(&entry.summary().kind));
    if linked {
        return Ok(());
    }
    let invalid = InvalidField {
        field: Field::IntegrationId,
        reason: Reason::NotAPrinterHost,
    };
    Err(PrinterSetupError::Invalid(vec![invalid]))
}

impl PrinterSetupClient {
    /// Stores a new printer and answers it.
    pub fn create_printer(
        &self,
        credential: Credential,
        new: NewPrinter,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Printer, PrinterSetupError> {
        let policy = self.policy;
        let name = "printer_setup.create_printer";
        let shared = &self.storage.shared;
        shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let mut printers = load(tx, &installation)?;
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
            apply(&mut row, values)?;
            check_link(tx, &installation, row.integration_id.as_deref())?;
            if printers.len() >= MAX_PRINTERS {
                return Err(PrinterSetupError::LimitReached);
            }
            printers.push(row.clone());
            super::store(tx, &installation, FLEET_KEY, &printers)?;
            Ok(row)
        })?
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
        self.storage.read(name, cancelled, wait, |tx| {
            let installation = authorize(tx, &credential, self.policy)?;
            let printers = load(tx, &installation)?;
            let found = printers.into_iter().find(|printer| printer.id == id);
            found.ok_or(PrinterSetupError::NotFound)
        })?
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
        let policy = self.policy;
        let name = "printer_setup.update_printer";
        let shared = &self.storage.shared;
        shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let mut printers = load(tx, &installation)?;
            let row = printers.iter_mut().find(|printer| printer.id == id);
            let row = row.ok_or(PrinterSetupError::NotFound)?;
            let relinked = matches!(patch.integration_id, Some(Some(_)));
            apply(row, patch)?;
            if relinked {
                check_link(tx, &installation, row.integration_id.as_deref())?;
            }
            let updated = row.clone();
            super::store(tx, &installation, FLEET_KEY, &printers)?;
            Ok(updated)
        })?
    }

    /// Removes the printer with `id` and answers it. Its integration stays.
    pub fn delete_printer(
        &self,
        credential: Credential,
        id: String,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Printer, PrinterSetupError> {
        let policy = self.policy;
        let name = "printer_setup.delete_printer";
        let shared = &self.storage.shared;
        shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let mut printers = load(tx, &installation)?;
            let index = printers.iter().position(|printer| printer.id == id);
            let removed = printers.remove(index.ok_or(PrinterSetupError::NotFound)?);
            super::store(tx, &installation, FLEET_KEY, &printers)?;
            Ok(removed)
        })?
    }
}
