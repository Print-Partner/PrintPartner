//! `POST /api/v2/integrations` and `DELETE /api/v2/integrations/{id}`: the
//! replacement Settings screen adds and removes a Moonraker, PrusaLink or
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
use crate::problem::{self, FieldError, Problem, ProblemType};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::Response,
    routing::{delete, post},
};
use pp_storage::{
    auth::Secret,
    printer_setup::{
        Field, IntegrationSummary, InvalidField, NewIntegration, PrinterSetupClient,
        PrinterSetupError, Reason, ServiceKind,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const BODY_LIMIT: usize = 16 * 1024;

/// Every `code` these routes answer.
pub mod code {
    pub const INVALID_INTEGRATION: &str = "invalid_integration";
    pub const INTEGRATION_NOT_FOUND: &str = "integration_not_found";
    pub const INTEGRATION_LIMIT: &str = "integration_limit";
    pub const INTEGRATIONS_UNREADABLE: &str = "integrations_unreadable";
}

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
struct App {
    gate: Gate,
    setup: PrinterSetupClient,
}

/// Mounts the routes when `PP_DESKTOP_REBUILD` was exactly `1` or `true`;
/// otherwise nothing. The accepted gate admits loopback requests with an
/// exact Host and Origin and a credential; storage proves the credential in
/// the write's own transaction.
pub fn integrations_router(config: AcceptedHttpConfig, setup: PrinterSetupClient) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/api/v2/integrations", post(create))
        .route("/api/v2/integrations/{id}", delete(remove))
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    gate.guard(routes).with_state(App { gate, setup })
}

async fn create(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    let request = match problem::parse_body::<CreateIntegrationRequest>(&body) {
        Ok(request) => request,
        Err(errors) => {
            let readable = problem::is_json_object(&body) || !errors.is_empty();
            return invalid(readable, errors);
        }
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
        Ok(Err(error)) => refused(&error),
        Err(_) => internal(),
    }
}

/// The one mapping from the family's error to a response.
fn refused(error: &PrinterSetupError) -> Response {
    match error {
        PrinterSetupError::Invalid(fields) => invalid(true, fields.iter().map(field_error)),
        PrinterSetupError::NotFound => problem::coded(
            StatusCode::NOT_FOUND,
            "Integration not found",
            code::INTEGRATION_NOT_FOUND,
        ),
        PrinterSetupError::LimitReached => problem::coded(
            StatusCode::CONFLICT,
            "The integration limit is reached. Delete one first.",
            code::INTEGRATION_LIMIT,
        ),
        PrinterSetupError::StoredDocumentInvalid(_) => problem::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored integrations are unreadable",
            code::INTEGRATIONS_UNREADABLE,
        ),
        PrinterSetupError::Writer(failure) => problem::writer_failure(failure),
    }
}

/// 400 naming each invalid field. A body that is not JSON at all is malformed
/// (`readable` false), not a validation failure.
fn invalid(readable: bool, errors: impl IntoIterator<Item = FieldError>) -> Response {
    let kind = if readable {
        ProblemType::Validation
    } else {
        ProblemType::MalformedRequest
    };
    let problem =
        Problem::new(kind, 400, "Request is invalid").with_code(code::INVALID_INTEGRATION);
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
