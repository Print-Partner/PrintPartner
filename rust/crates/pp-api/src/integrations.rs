//! `GET` and `POST /api/v2/integrations` and `PATCH` and `DELETE
//! /api/v2/integrations/{id}`: the replacement Settings screen lists every
//! integration, and adds, edits and removes a Moonraker, PrusaLink or
//! Spoolman integration. Rust-only runtime: the routes mount only when
//! `PP_DESKTOP_REBUILD` is on, and Node keeps its own routes in mixed mode.
//!
//! Storage owns the rules and answers a typed error
//! ([`PrinterSetupError`]). This module is the one place that turns it into a
//! status code and a problem body. No response or problem carries an API key
//! or a password: the request type has no `Debug`, and the view has no secret
//! field.

use crate::accepted::{AcceptedHttpConfig, Gate, internal, ok_json};
use crate::loopback::PresentedCredential;
use crate::problem::{self, FieldError, Problem, ProblemCode, ProblemType, ToProblem};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    response::Response,
    routing::{get, patch},
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

/// Every `code` the integration and printer routes answer, as its
/// [`ProblemCode`]'s wire value.
pub mod code {
    use crate::problem::ProblemCode;
    pub const INVALID_INTEGRATION: &str = ProblemCode::InvalidIntegration.as_str();
    pub const INTEGRATION_NOT_FOUND: &str = ProblemCode::IntegrationNotFound.as_str();
    pub const INTEGRATION_LIMIT: &str = ProblemCode::IntegrationLimit.as_str();
    pub const INTEGRATION_NOT_EDITABLE: &str = ProblemCode::IntegrationNotEditable.as_str();
    pub const INTEGRATIONS_UNREADABLE: &str = ProblemCode::IntegrationsUnreadable.as_str();
    pub const INVALID_PRINTER: &str = ProblemCode::InvalidPrinter.as_str();
    pub const PRINTER_NOT_FOUND: &str = ProblemCode::PrinterNotFound.as_str();
    pub const PRINTER_LIMIT: &str = ProblemCode::PrinterLimit.as_str();
    pub const PRINTERS_UNREADABLE: &str = ProblemCode::PrintersUnreadable.as_str();
    pub const FILAMENT_SLOT_IN_USE: &str = ProblemCode::FilamentSlotInUse.as_str();
}

/// The text and codes that differ between integrations and printers. The
/// mapping itself is [`ToProblem`] for the error with its wording, once, for
/// both.
pub(crate) struct Wording {
    pub(crate) invalid: ProblemCode,
    pub(crate) not_found: (&'static str, ProblemCode),
    pub(crate) limit: (&'static str, ProblemCode),
}

const WORDING: Wording = Wording {
    invalid: ProblemCode::InvalidIntegration,
    not_found: ("Integration not found", ProblemCode::IntegrationNotFound),
    limit: (
        "The integration limit is reached. Delete one first.",
        ProblemCode::IntegrationLimit,
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
    /// Null for a kind these routes cannot create. Always sent.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_storage::build_graph::wire::RequiredNullable<String>")
    )]
    pub base_url: Option<String>,
    /// Always sent; null when the integration has none.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_storage::build_graph::wire::RequiredNullable<String>")
    )]
    pub username: Option<String>,
    pub has_api_key: bool,
    pub has_password: bool,
}

/// `GET /api/v2/integrations` body: every stored integration in stored order,
/// kinds these routes cannot create included, without secrets.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct IntegrationList {
    pub integrations: Vec<IntegrationView>,
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

/// The integration list, create, edit and delete routes, under [`mounted`]'s guard.
pub fn integrations_router(config: AcceptedHttpConfig, setup: PrinterSetupClient) -> Router {
    let routes = Router::new()
        .route("/api/v2/integrations", get(list).post(create))
        .route("/api/v2/integrations/{id}", patch(update).delete(remove))
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    mounted(config, setup, routes)
}

async fn list(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    let setup = app.setup.clone();
    let read = app.gate.blocking(move |cancelled, wait| {
        setup.list_integrations(presented.into_credential(), &cancelled, wait)
    });
    match read.await {
        Ok(Ok(summaries)) => ok_json(&IntegrationList {
            integrations: summaries.into_iter().map(IntegrationView::from).collect(),
        }),
        Ok(Err(error)) => refused(&error, &WORDING),
        Err(_) => internal(),
    }
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

/// The one mapping from the family's error to a problem. Integrations and
/// printers share the error, so it is paired with the caller's [`Wording`].
impl ToProblem for (&PrinterSetupError, &Wording) {
    fn to_problem(&self) -> Problem {
        let (error, wording) = *self;
        let coded = |kind, status, (detail, code): (&str, ProblemCode)| {
            Problem::new(kind, status, detail).with_problem_code(code)
        };
        let conflict = |answer| coded(ProblemType::Conflict, 409, answer);
        match error {
            PrinterSetupError::Invalid(fields) => {
                invalid(true, fields.iter().map(field_error), wording)
            }
            PrinterSetupError::NotFound => coded(ProblemType::NotFound, 404, wording.not_found),
            PrinterSetupError::LimitReached => conflict(wording.limit),
            PrinterSetupError::NotEditable => conflict((
                "This kind of integration cannot be edited here",
                ProblemCode::IntegrationNotEditable,
            )),
            PrinterSetupError::SlotHoldsFilament => conflict((
                "A filament slot this removes still holds filament. Unload it first.",
                ProblemCode::FilamentSlotInUse,
            )),
            PrinterSetupError::StoredDocumentInvalid(fault) => coded(
                ProblemType::Internal,
                500,
                match fault.document {
                    SetupDocument::Integrations => (
                        "Stored integrations are unreadable",
                        ProblemCode::IntegrationsUnreadable,
                    ),
                    SetupDocument::Printers => (
                        "Stored printers are unreadable",
                        ProblemCode::PrintersUnreadable,
                    ),
                },
            ),
            PrinterSetupError::Writer(failure) => failure.to_problem(),
        }
    }
}

pub(crate) fn refused(error: &PrinterSetupError, wording: &Wording) -> Response {
    problem::refused(&(error, wording))
}

/// 400 for a body serde refused: it names the field, never a value. A body
/// that is not JSON at all is malformed, not a validation failure.
pub(crate) fn unreadable(body: &[u8], errors: Vec<FieldError>, wording: &Wording) -> Response {
    let readable = problem::is_json_object(body) || !errors.is_empty();
    problem::build(invalid(readable, errors, wording), serde_json::Map::new())
}

fn invalid(
    readable: bool,
    errors: impl IntoIterator<Item = FieldError>,
    wording: &Wording,
) -> Problem {
    let kind = if readable {
        ProblemType::Validation
    } else {
        ProblemType::MalformedRequest
    };
    let problem = Problem::new(kind, 400, "Request is invalid").with_problem_code(wording.invalid);
    problem.with_errors(errors)
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
