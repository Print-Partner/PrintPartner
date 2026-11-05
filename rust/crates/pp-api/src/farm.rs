//! Farm reads (GRE-1125) for Today, Farm and Production, read by
//! `pp_storage::read_model::farm`:
//!
//! - `GET /api/v2/printers`: configured printers with execution, bed-clear and
//!   display state, the reason a state is not live, and last-heard time.
//! - `GET /api/v2/printer-checkoff`: printer Checkoff results.
//! - `GET /api/v2/printer-checkoff/unattributed`: unmatched prints.
//! - `GET /api/v2/printer-checkoff/unattributed/suggestions`: a proposed Build
//!   and Plate per unmatched print with its confidence and evidence (GRE-1091).
//!   It never confirms one.
//! - `GET /api/v2/printer-send-queue`: the send queue.
//!
//! Every route writes nothing and never polls a printer: printer state comes
//! from the status watchers through [`FarmObservations`]. Admission is
//! `accepted::Gate`.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, internal, ok_json, read_error},
    loopback::PresentedCredential,
};
use axum::{Extension, Router, extract::State, http::StatusCode, response::Response, routing::get};
use pp_storage::read_model::farm::{Farm, FarmClient, Observation, PrinterIntegration, TooLarge};
use std::sync::Arc;

/// What the status watchers observed for one printer integration. `pp-core`
/// implements it over the gateway's observation port.
pub trait FarmObservations: Send + Sync + 'static {
    fn observe(&self, integration: &PrinterIntegration) -> Observation;
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: FarmClient,
    observations: Arc<dyn FarmObservations>,
}

/// Mounts the farm reads under `/api/v2` when `PP_DESKTOP_REBUILD` was
/// exactly `1` or `true`; otherwise nothing.
pub fn farm_router(
    config: AcceptedHttpConfig,
    client: FarmClient,
    observations: Arc<dyn FarmObservations>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/printers", get(printers))
        .route("/printer-checkoff", get(checkoff))
        .route("/printer-checkoff/unattributed", get(unmatched))
        .route(
            "/printer-checkoff/unattributed/suggestions",
            get(suggestions),
        )
        .route("/printer-send-queue", get(send_queue));
    gate.guard(Router::new().nest("/api/v2", routes))
        .with_state(App {
            gate,
            client,
            observations,
        })
}

async fn printers(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    let observations = app.observations.clone();
    read(app, presented, move |farm| {
        ok_json(&farm.printers(|integration| observations.observe(integration)))
    })
    .await
}

async fn checkoff(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read(app, presented, |farm| ok_json(&farm.checkoff())).await
}

async fn unmatched(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read(app, presented, |farm| ok_json(&farm.unmatched())).await
}

async fn suggestions(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read_with(app, presented, true, |farm| {
        farm.suggestions()
            .map_or_else(internal, |suggestions| ok_json(&suggestions))
    })
    .await
}

async fn send_queue(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    read(app, presented, |farm| ok_json(&farm.send_queue())).await
}

async fn read(
    app: App,
    presented: PresentedCredential,
    respond: impl FnOnce(Farm) -> Response,
) -> Response {
    read_with(app, presented, false, respond).await
}

async fn read_with(
    app: App,
    presented: PresentedCredential,
    for_suggestions: bool,
    respond: impl FnOnce(Farm) -> Response,
) -> Response {
    let client = app.client;
    let farm = app
        .gate
        .read(presented, move |credential, cancelled, wait| {
            if for_suggestions {
                client.read_for_suggestions(credential, cancelled, wait)
            } else {
                client.read(credential, cancelled, wait)
            }
        })
        .await;
    let Some(farm) = farm else {
        return internal();
    };
    farm.map_or_else(
        |error| match error.downcast_ref::<TooLarge>() {
            Some(too_large) => failure(StatusCode::INTERNAL_SERVER_ERROR, &too_large.to_string()),
            None => read_error(&error),
        },
        respond,
    )
}
