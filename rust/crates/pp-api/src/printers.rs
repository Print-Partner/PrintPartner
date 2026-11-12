//! `GET /api/v2/printers/{id}`: the replacement Settings screen reads one
//! printer's configuration. Rust-only runtime: the route mounts only when
//! `PP_DESKTOP_REBUILD` is on. `GET /api/v2/printers`, the Farm list with
//! live state, stays in [`crate::farm`].
//!
//! Storage answers `PrinterSetupError`. The mapping to a status code and a
//! problem body is [`crate::integrations::refused`], shared with the
//! integration routes; this module only supplies its own wording.

use crate::accepted::{AcceptedHttpConfig, internal, ok_json};
use crate::integrations::{App, Wording, code, mounted, refused};
use crate::loopback::PresentedCredential;
use axum::{
    Extension, Router,
    extract::{Path, State},
    response::Response,
    routing::get,
};
use pp_storage::printer_setup::{Printer, PrinterSetupClient};
use serde::Serialize;

const WORDING: Wording = Wording {
    invalid: code::INVALID_PRINTER,
    not_found: ("Printer not found", code::PRINTER_NOT_FOUND),
    limit: (
        "The printer limit is reached. Delete one first.",
        code::PRINTER_LIMIT,
    ),
};

/// A stored printer's configuration. Lengths are whole millimetres. Its live
/// state is in the Farm list.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct PrinterConfig {
    pub id: String,
    pub name: String,
    pub model: String,
    pub bed_width_mm: u32,
    pub bed_depth_mm: u32,
    /// Null only for a printer that came from an installation without one.
    pub bed_height_mm: Option<u32>,
    pub margin_mm: u32,
    pub max_filament_slots: u8,
    pub integration_id: Option<String>,
}

impl From<Printer> for PrinterConfig {
    fn from(printer: Printer) -> Self {
        Self {
            id: printer.id,
            name: printer.name,
            model: printer.model,
            bed_width_mm: printer.bed_width_mm,
            bed_depth_mm: printer.bed_depth_mm,
            bed_height_mm: printer.bed_height_mm,
            margin_mm: printer.margin_mm,
            max_filament_slots: printer.max_filament_slots,
            integration_id: printer.integration_id,
        }
    }
}

/// Mounted and guarded like the integration routes.
pub fn printers_router(config: AcceptedHttpConfig, setup: PrinterSetupClient) -> Router {
    let routes = Router::new().route("/api/v2/printers/{id}", get(read));
    mounted(config, setup, routes)
}

async fn read(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    let setup = app.setup.clone();
    let read = app.gate.blocking(move |cancelled, wait| {
        setup.read_printer(presented.into_credential(), &id, &cancelled, wait)
    });
    match read.await {
        Ok(Ok(printer)) => ok_json(&PrinterConfig::from(printer)),
        Ok(Err(error)) => refused(&error, &WORDING),
        Err(_) => internal(),
    }
}

#[cfg(test)]
mod tests;
