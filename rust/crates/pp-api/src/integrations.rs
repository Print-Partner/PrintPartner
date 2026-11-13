//! `POST /api/v2/integrations` and `PATCH` and `DELETE
//! /api/v2/integrations/{id}`: the replacement Settings screen adds, edits
//! and removes a Moonraker, PrusaLink or Spoolman integration. Rust-only runtime: the routes mount only when
//! `PP_DESKTOP_REBUILD` is on, and Node keeps its own routes in mixed mode.
//!
//! Storage owns the rules and answers a typed error
//! ([`PrinterSetupError`]). This module is the one place that turns it into a
//! status code and a problem body. No response or problem carries an API key
//! or a password: the request type has no `Debug`, and the view has no secret
//! field.

use crate::accepted::{AcceptedHttpConfig, Gate, internal, ok_json};
use crate::loopback::PresentedCredential;
use crate::problem::{self, FieldError, Problem, ProblemType};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::Response,
    routing::{patch, post},
};
use pp_storage::{
    auth::Secret,
    printer_setup::{
        Field, IntegrationPatch, IntegrationSummary, InvalidField, NewIntegration,
        PrinterSetupClient, PrinterSetupError, Reason, ServiceKind, SetupDocument,
    },
};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const BODY_LIMIT: usize = 16 * 1024;

/// Every `code` the integration and printer routes answer.
pub mod code {
    pub const INVALID_INTEGRATION: &str = "invalid_integration";
    pub const INTEGRATION_NOT_FOUND: &str = "integration_not_found";
    pub const INTEGRATION_LIMIT: &str = "integration_limit";
    pub const INTEGRATION_NOT_EDITABLE: &str = "integration_not_editable";
    pub const INTEGRATIONS_UNREADABLE: &str = "integrations_unreadable";
    pub const INVALID_PRINTER: &str = "invalid_printer";
    pub const PRINTER_NOT_FOUND: &str = "printer_not_found";
    pub const PRINTER_LIMIT: &str = "printer_limit";
    pub const PRINTERS_UNREADABLE: &str = "printers_unreadable";
    pub const FILAMENT_SLOT_IN_USE: &str = "filament_slot_in_use";
}

/// The text and codes that differ between integrations and printers. The
/// mapping itself is [`refused`], once, for both.
pub(crate) struct Wording {
    pub(crate) invalid: &'static str,
    pub(crate) not_found: (&'static str, &'static str),
    pub(crate) limit: (&'static str, &'static str),
}

const WORDING: Wording = Wording {
    invalid: code::INVALID_INTEGRATION,
    not_found: ("Integration not found", code::INTEGRATION_NOT_FOUND),
    limit: (
        "The integration limit is reached. Delete one first.",
        code::INTEGRATION_LIMIT,
    ),
};

/// `POST /api/v2/integrations` body. `kind` is `moonraker`, `prusalink` or
/// `spoolman`. Moonraker and Spoolman take `api_key`; PrusaLink takes
/// `username` and `password`.
#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CreateIntegrationRequest {
    pub kind: ServiceKind,
    pub name: String,
    /// An `http` or `https` URL without a username or password in it.
    pub base_url: String,
    /// False stores the integration without polling it. Defaults to true.
    pub enabled: Option<bool>,
    pub username: Option<String>,
    pub api_key: Option<String>,
    pub password: Option<String>,
}

/// `PATCH /api/v2/integrations/{id}` body. A field that is absent keeps its
/// stored value. For `name`, `base_url` and `enabled`, null keeps it too; for
/// `username`, `api_key` and `password`, null clears it. The kind never
/// changes. A new `base_url` must come with each stored `api_key` and
/// `password`, as a value or as null, so no stored secret follows it.
#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpdateIntegrationRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "sent")]
    pub username: Option<Option<String>>,
    #[serde(default, deserialize_with = "sent")]
    pub api_key: Option<Option<String>>,
    #[serde(default, deserialize_with = "sent")]
    pub password: Option<Option<String>>,
}

/// `Some(None)` for a key that is present and null, where serde's default
/// would read it as absent.
pub(crate) fn sent<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

/// A stored integration without its secrets.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct IntegrationView {
    pub id: String,
    pub kind: ServiceKind,
    pub name: String,
    pub enabled: bool,
    /// Null for a kind these routes cannot create.
    pub base_url: Option<String>,
    pub username: Option<String>,
    pub has_api_key: bool,
    pub has_password: bool,
}

impl From<IntegrationSummary> for IntegrationView {
    fn from(summary: IntegrationSummary) -> Self {
        Self {
            id: summary.id,
            kind: summary.kind,
            name: summary.name,
            enabled: summary.enabled,
            base_url: summary.base_url,
            username: summary.username,
            has_api_key: summary.has_api_key,
            has_password: summary.has_password,
        }
    }
}

#[derive(Clone)]
pub(crate) struct App {
    pub(crate) gate: Gate,
    pub(crate) setup: PrinterSetupClient,
}

/// Mounts `routes` when `PP_DESKTOP_REBUILD` was exactly `1` or `true`;
/// otherwise nothing. The accepted gate admits loopback requests with an
/// exact Host and Origin and a credential; storage proves the credential in
/// the operation's own transaction.
pub(crate) fn mounted(
    config: AcceptedHttpConfig,
    setup: PrinterSetupClient,
    routes: Router<App>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    gate.guard(routes).with_state(App { gate, setup })
}

/// The integration create, edit and delete routes, under [`mounted`]'s guard.
pub fn integrations_router(config: AcceptedHttpConfig, setup: PrinterSetupClient) -> Router {
    let routes = Router::new()
        .route("/api/v2/integrations", post(create))
        .route("/api/v2/integrations/{id}", patch(update).delete(remove))
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    mounted(config, setup, routes)
}

async fn create(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    let request = match problem::parse_body::<CreateIntegrationRequest>(&body) {
        Ok(request) => request,
        Err(errors) => return unreadable(&body, errors, &WORDING),
    };
    let new = NewIntegration {
        kind: request.kind,
        name: request.name,
        base_url: request.base_url,
        enabled: request.enabled.unwrap_or(true),
        username: request.username,
        api_key: request.api_key.map(Secret::new),
        password: request.password.map(Secret::new),
    };
    answer(&app, &WORDING, move |setup, cancelled, wait| {
        let created = setup.create_integration(presented.into_credential(), new, &cancelled, wait);
        created.map(IntegrationView::from)
    })
    .await
}

async fn update(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let request = match problem::parse_body::<UpdateIntegrationRequest>(&body) {
        Ok(request) => request,
        Err(errors) => return unreadable(&body, errors, &WORDING),
    };
    let secret = |sent: Option<Option<String>>| sent.map(|value| value.map(Secret::new));
    let patch = IntegrationPatch {
        name: request.name,
        base_url: request.base_url,
        enabled: request.enabled,
        username: request.username,
        api_key: secret(request.api_key),
        password: secret(request.password),
    };
    answer(&app, &WORDING, move |setup, cancelled, wait| {
        let credential = presented.into_credential();
        let updated = setup.update_integration(credential, id, patch, &cancelled, wait);
        updated.map(IntegrationView::from)
    })
    .await
}

async fn remove(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    answer(&app, &WORDING, move |setup, cancelled, wait| {
        let removed = setup.delete_integration(presented.into_credential(), id, &cancelled, wait);
        removed.map(IntegrationView::from)
    })
    .await
}

/// Runs `operation` off the async threads and answers its view or its
/// problem, for the integration and the printer routes alike.
pub(crate) async fn answer<V: Serialize + Send + 'static>(
    app: &App,
    wording: &Wording,
    operation: impl FnOnce(
        PrinterSetupClient,
        Arc<AtomicBool>,
        Duration,
    ) -> Result<V, PrinterSetupError>
    + Send
    + 'static,
) -> Response {
    let setup = app.setup.clone();
    let done = app
        .gate
        .blocking(move |cancelled, wait| operation(setup, cancelled, wait));
    match done.await {
        Ok(Ok(view)) => ok_json(&view),
        Ok(Err(error)) => refused(&error, wording),
        Err(_) => internal(),
    }
}

/// The one mapping from the family's error to a response.
pub(crate) fn refused(error: &PrinterSetupError, wording: &Wording) -> Response {
    let coded = |status, (detail, code): (&str, &str)| problem::coded(status, detail, code);
    match error {
        PrinterSetupError::Invalid(fields) => {
            invalid(true, fields.iter().map(field_error), wording)
        }
        PrinterSetupError::NotFound => coded(StatusCode::NOT_FOUND, wording.not_found),
        PrinterSetupError::LimitReached => coded(StatusCode::CONFLICT, wording.limit),
        PrinterSetupError::NotEditable => coded(
            StatusCode::CONFLICT,
            (
                "This kind of integration cannot be edited here",
                code::INTEGRATION_NOT_EDITABLE,
            ),
        ),
        PrinterSetupError::SlotHoldsFilament => coded(
            StatusCode::CONFLICT,
            (
                "A filament slot this removes still holds filament. Unload it first.",
                code::FILAMENT_SLOT_IN_USE,
            ),
        ),
        PrinterSetupError::StoredDocumentInvalid(fault) => coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            match fault.document {
                SetupDocument::Integrations => (
                    "Stored integrations are unreadable",
                    code::INTEGRATIONS_UNREADABLE,
                ),
                SetupDocument::Printers => {
                    ("Stored printers are unreadable", code::PRINTERS_UNREADABLE)
                }
            },
        ),
        PrinterSetupError::Writer(failure) => problem::writer_failure(failure),
    }
}

/// 400 for a body serde refused: it names the field, never a value. A body
/// that is not JSON at all is malformed, not a validation failure.
pub(crate) fn unreadable(body: &[u8], errors: Vec<FieldError>, wording: &Wording) -> Response {
    let readable = problem::is_json_object(body) || !errors.is_empty();
    invalid(readable, errors, wording)
}

fn invalid(
    readable: bool,
    errors: impl IntoIterator<Item = FieldError>,
    wording: &Wording,
) -> Response {
    let kind = if readable {
        ProblemType::Validation
    } else {
        ProblemType::MalformedRequest
    };
    let problem = Problem::new(kind, 400, "Request is invalid").with_code(wording.invalid);
    problem::build(problem.with_errors(errors), serde_json::Map::new())
}

fn field_error(invalid: &InvalidField) -> FieldError {
    let pointer = match invalid.field {
        Field::Kind => "/kind",
        Field::Name => "/name",
        Field::BaseUrl => "/base_url",
        Field::Username => "/username",
        Field::ApiKey => "/api_key",
        Field::Password => "/password",
        Field::Model => "/model",
        Field::BedWidth => "/bed_width_mm",
        Field::BedDepth => "/bed_depth_mm",
        Field::BedHeight => "/bed_height_mm",
        Field::Margin => "/margin_mm",
        Field::FilamentSlots => "/max_filament_slots",
        Field::IntegrationId => "/integration_id",
    };
    let detail = match (invalid.reason, invalid.field) {
        (Reason::Empty, _) => "must not be empty",
        (Reason::TooLong, _) => "is too long",
        (Reason::NotHttpUrl, _) => "must be an http or https URL",
        (Reason::HasCredentials, _) => "must not contain a username or password",
        (Reason::NotCreatable, _) => "cannot be created here",
        (Reason::NotUsedByKind, _) => "is not used by this kind",
        (Reason::NotResent, _) => "must be sent again when the address changes",
        (Reason::NotAPrinterHost, _) => {
            "must name a stored Moonraker, PrusaLink or Bambu integration"
        }
        (Reason::OutOfRange, Field::Margin) => "must be less than half the bed width and depth",
        (Reason::OutOfRange, Field::FilamentSlots) => "must be between 1 and 4",
        (Reason::OutOfRange, _) => "must be between 1 and 10000 mm",
    };
    FieldError::new(pointer, detail)
}

#[cfg(test)]
pub(crate) mod tests;
