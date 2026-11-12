//! `GET` and `POST /api/v2/integrations` and `DELETE /api/v2/integrations/{id}`:
//! the replacement Settings screen lists every integration, and adds and
//! removes a Moonraker, PrusaLink or Spoolman integration. Rust-only runtime: the routes mount only when
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
    routing::{delete, get},
};
use pp_storage::{
    auth::Secret,
    printer_setup::{
        Field, IntegrationSummary, InvalidField, NewIntegration, PrinterSetupClient,
        PrinterSetupError, Reason, ServiceKind, SetupDocument,
    },
};
use serde::{Deserialize, Serialize};
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
    pub const INTEGRATIONS_UNREADABLE: &str = "integrations_unreadable";
    pub const INVALID_PRINTER: &str = "invalid_printer";
    pub const PRINTER_NOT_FOUND: &str = "printer_not_found";
    pub const PRINTER_LIMIT: &str = "printer_limit";
    pub const PRINTERS_UNREADABLE: &str = "printers_unreadable";
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

/// The integration list, create and delete routes, under [`mounted`]'s guard.
pub fn integrations_router(config: AcceptedHttpConfig, setup: PrinterSetupClient) -> Router {
    let routes = Router::new()
        .route("/api/v2/integrations", get(list).post(create))
        .route("/api/v2/integrations/{id}", delete(remove))
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
    answer(&app, move |setup, cancelled, wait| {
        setup.create_integration(presented.into_credential(), new, &cancelled, wait)
    })
    .await
}

async fn remove(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    answer(&app, move |setup, cancelled, wait| {
        setup.delete_integration(presented.into_credential(), id, &cancelled, wait)
    })
    .await
}

/// Runs `write` off the async threads and answers its view or its problem.
async fn answer(
    app: &App,
    write: impl FnOnce(
        PrinterSetupClient,
        Arc<AtomicBool>,
        Duration,
    ) -> Result<IntegrationSummary, PrinterSetupError>
    + Send
    + 'static,
) -> Response {
    let setup = app.setup.clone();
    let written = app
        .gate
        .blocking(move |cancelled, wait| write(setup, cancelled, wait));
    match written.await {
        Ok(Ok(summary)) => ok_json(&IntegrationView::from(summary)),
        Ok(Err(error)) => refused(&error, &WORDING),
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
    };
    let detail = match invalid.reason {
        Reason::Empty => "must not be empty",
        Reason::TooLong => "is too long",
        Reason::NotHttpUrl => "must be an http or https URL",
        Reason::HasCredentials => "must not contain a username or password",
        Reason::NotCreatable => "cannot be created here",
        Reason::NotUsedByKind => "is not used by this kind",
    };
    FieldError::new(pointer, detail)
}

#[cfg(test)]
mod tests;
