//! `POST /api/v2/printers` and `GET`, `PATCH` and `DELETE
//! /api/v2/printers/{id}`: the replacement Settings screen adds, reads, edits
//! and removes a printer. Rust-only runtime: the routes mount only when
//! `PP_DESKTOP_REBUILD` is on. `GET /api/v2/printers`, the Farm list with
//! live state, stays in [`crate::farm`].
//!
//! Storage owns the rules and answers `PrinterSetupError`. The mapping to a
//! status code and a problem body is [`crate::integrations::refused`], shared
//! with the integration routes; this module only supplies its own wording.

use crate::accepted::AcceptedHttpConfig;
use crate::integrations::{App, Wording, answer, code, mounted, unreadable};
use crate::loopback::PresentedCredential;
use crate::problem;
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    response::Response,
    routing::{get, post},
};
use pp_storage::{
    printer_setup::{NewPrinter, Printer, PrinterPatch, PrinterSetupClient, PrinterSetupError},
    read_model::Credential,
};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use std::{sync::atomic::AtomicBool, time::Duration};

const BODY_LIMIT: usize = 16 * 1024;
const WORDING: Wording = Wording {
    invalid: code::INVALID_PRINTER,
    not_found: ("Printer not found", code::PRINTER_NOT_FOUND),
    limit: (
        "The printer limit is reached. Delete one first.",
        code::PRINTER_LIMIT,
    ),
};

/// `POST /api/v2/printers` body. Lengths are whole millimetres: a fractional
/// value is refused as a validation error that names the field.
#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CreatePrinterRequest {
    pub name: String,
    pub model: String,
    pub bed_width_mm: u32,
    pub bed_depth_mm: u32,
    pub bed_height_mm: u32,
    /// The strip along each bed edge that stays empty. Defaults to 4.
    pub margin_mm: Option<u32>,
    /// 1 to 4. Defaults to 1.
    pub max_filament_slots: Option<u8>,
    /// A stored Moonraker, PrusaLink or Bambu integration.
    pub integration_id: Option<String>,
}

/// `PATCH /api/v2/printers/{id}` body. A field that is absent or null keeps
/// its stored value, except `integration_id`: there null unlinks the printer
/// and only an absent field keeps the link.
#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpdatePrinterRequest {
    pub name: Option<String>,
    pub model: Option<String>,
    pub bed_width_mm: Option<u32>,
    pub bed_depth_mm: Option<u32>,
    pub bed_height_mm: Option<u32>,
    pub margin_mm: Option<u32>,
    pub max_filament_slots: Option<u8>,
    #[serde(default, deserialize_with = "sent")]
    pub integration_id: Option<Option<String>>,
}

/// `Some(None)` for a key that is present and null, where serde's default
/// would read it as absent.
fn sent<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

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
    let routes = Router::new()
        .route("/api/v2/printers", post(create))
        .route(
            "/api/v2/printers/{id}",
            get(read).patch(update).delete(remove),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    mounted(config, setup, routes)
}

fn parsed<T: DeserializeOwned>(body: &[u8]) -> Result<T, Box<Response>> {
    problem::parse_body(body).map_err(|errors| Box::new(unreadable(body, errors, &WORDING)))
}

async fn create(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    let request: CreatePrinterRequest = match parsed(&body) {
        Ok(request) => request,
        Err(refusal) => return *refusal,
    };
    let new = NewPrinter {
        name: request.name,
        model: request.model,
        bed_width_mm: request.bed_width_mm,
        bed_depth_mm: request.bed_depth_mm,
        bed_height_mm: request.bed_height_mm,
        margin_mm: request.margin_mm.unwrap_or(4),
        max_filament_slots: request.max_filament_slots.unwrap_or(1),
        integration_id: request.integration_id,
    };
    answer(&app, &WORDING, move |setup, cancelled, wait| {
        let created = setup.create_printer(presented.into_credential(), new, &cancelled, wait);
        created.map(PrinterConfig::from)
    })
    .await
}

async fn update(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let request: UpdatePrinterRequest = match parsed(&body) {
        Ok(request) => request,
        Err(refusal) => return *refusal,
    };
    let patch = PrinterPatch {
        name: request.name,
        model: request.model,
        bed_width_mm: request.bed_width_mm,
        bed_depth_mm: request.bed_depth_mm,
        bed_height_mm: request.bed_height_mm,
        margin_mm: request.margin_mm,
        max_filament_slots: request.max_filament_slots,
        integration_id: request.integration_id,
    };
    answer(&app, &WORDING, move |setup, cancelled, wait| {
        let credential = presented.into_credential();
        let updated = setup.update_printer(credential, id, patch, &cancelled, wait);
        updated.map(PrinterConfig::from)
    })
    .await
}

/// Read and delete differ only in the storage call.
type ById = fn(&PrinterSetupClient, Credential, String, &AtomicBool, Duration) -> Stored;
type Stored = Result<Printer, PrinterSetupError>;

async fn one(app: App, presented: PresentedCredential, id: String, operation: ById) -> Response {
    answer(&app, &WORDING, move |setup, cancelled, wait| {
        let stored = operation(&setup, presented.into_credential(), id, &cancelled, wait);
        stored.map(PrinterConfig::from)
    })
    .await
}

async fn read(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    let read: ById = |setup, credential, id, cancelled, wait| {
        setup.read_printer(credential, &id, cancelled, wait)
    };
    one(app, presented, id, read).await
}

async fn remove(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    one(app, presented, id, PrinterSetupClient::delete_printer).await
}

#[cfg(test)]
mod tests;
