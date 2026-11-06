//! Send queue writes (GRE-365, GRE-1215) over
//! `pp_storage::send_queue::writes`. Each is an explicit user action:
//!
//! - `POST /api/v2/printer-send-queue/{id}/dispatch` (Send now): sends one
//!   `queued` or `error` item. With `wait_for_idle` the printer must be
//!   idle unless the body says `{"force": true}`.
//! - `POST /api/v2/printer-send-queue/drain` (Send ready): sends up to eight
//!   `queued` items, optionally for one `printer_id`, at most one per printer.
//!   An item whose printer is busy or reserved stays queued.
//! - `POST /api/v2/printer-send-queue/{id}/reconcile`: the user's answer for a
//!   send with an unknown outcome, `{"printed": true|false}`.
//! - `DELETE /api/v2/printer-send-queue/{id}` (Remove).
//!
//! A dispatch only claims the item and enqueues its durable printer-upload
//! job in one writer transaction; the send worker uploads it. Send now on an
//! item whose last send has an unknown outcome is 409 `send_outcome_unknown`.
//! Admission is `accepted::Gate`.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, internal, ok_json, read_error},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    routing::{delete, post},
};
use pp_storage::send_queue::writes::{Reply, Request, SendQueueClient};
use serde_json::{Map, Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// Whether a stored item can go to its printer now. `pp-core` implements it
/// over the fleet settings, the staged file and the status watchers.
pub trait SendTargets: Send + Sync + 'static {
    /// The host name to label the send with, or why it cannot be sent.
    /// `idle` asks for an idle printer. Called on a blocking thread.
    fn prepare(&self, item: &Value, idle: bool) -> Prepared;
    /// Deletes a removed item's staged file directory.
    fn discard(&self, item: &Value);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Prepared {
    Ready { host_name: String },
    /// The staged file is gone: the item becomes an error.
    Missing,
    Refused { status: StatusCode, detail: String },
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: SendQueueClient,
    targets: Arc<dyn SendTargets>,
}

/// Mounts the send queue writes under `/api/v2` when `PP_DESKTOP_REBUILD`
/// was exactly `1` or `true`; otherwise nothing.
pub fn send_queue_router(
    config: AcceptedHttpConfig,
    client: SendQueueClient,
    targets: Arc<dyn SendTargets>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/printer-send-queue/drain", post(drain))
        .route("/printer-send-queue/{id}/dispatch", post(dispatch))
        .route("/printer-send-queue/{id}/reconcile", post(reconcile))
        .route("/printer-send-queue/{id}", delete(remove));
    gate.guard(Router::new().nest("/api/v2", routes))
        .with_state(App {
            gate,
            client,
            targets,
        })
}

/// Node reads a missing or empty body as `{}`.
fn object(body: &[u8]) -> Option<Map<String, Value>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Some(Map::new());
    }
    match serde_json::from_slice(body) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

fn not_object() -> Response {
    failure(StatusCode::BAD_REQUEST, "Request body must be a JSON object")
}

/// The storage writes one request makes, on a blocking thread.
struct Writes<'a> {
    client: &'a SendQueueClient,
    presented: &'a PresentedCredential,
    cancelled: &'a AtomicBool,
    wait: Duration,
}

impl Writes<'_> {
    fn apply(&self, request: Request) -> anyhow::Result<Reply> {
        self.client.apply(
            self.presented.clone().into_credential(),
            request,
            self.cancelled,
            self.wait,
        )
    }

    /// Send now: storage's own refusals first, so an unknown outcome is
    /// always `send_outcome_unknown`, then the printer, then the claim.
    fn dispatch(&self, targets: &dyn SendTargets, id: String, force: bool) -> anyhow::Result<Reply> {
        let item = match self.apply(Request::Check { id: id.clone() })? {
            Reply::Item(item) => item,
            refused => return Ok(refused),
        };
        let idle = item["wait_for_idle"] == true && !force;
        match targets.prepare(&item, idle) {
            Prepared::Ready { host_name } => self.apply(Request::Dispatch { id, host_name }),
            Prepared::Missing => match self.apply(Request::Missing { id })? {
                Reply::Item(_) => Ok(Reply::Refused {
                    status: 409,
                    detail: pp_storage::send_queue::writes::MISSING_ARTIFACT,
                    code: None,
                }),
                refused => Ok(refused),
            },
            Prepared::Refused { status, detail } => Err(Refusal(status, detail).into()),
        }
    }
}

/// A target refusal with its own text.
#[derive(Debug)]
struct Refusal(StatusCode, String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.1)
    }
}

impl std::error::Error for Refusal {}

async fn writes<T: Send + 'static>(
    app: &App,
    presented: PresentedCredential,
    run: impl FnOnce(&Writes, &dyn SendTargets) -> anyhow::Result<T> + Send + 'static,
) -> Option<anyhow::Result<T>> {
    let (client, targets) = (app.client.clone(), Arc::clone(&app.targets));
    app.gate
        .blocking(move |cancelled, wait| {
            let writes = Writes {
                client: &client,
                presented: &presented,
                cancelled: &cancelled,
                wait,
            };
            run(&writes, &*targets)
        })
        .await
        .ok()
}

fn present(reply: Option<anyhow::Result<Reply>>) -> Response {
    match reply {
        Some(Ok(Reply::Item(item))) => ok_json(&json!({ "item": item })),
        Some(Ok(Reply::Dispatched { item, job_id })) => {
            ok_json(&json!({ "item": item, "job_id": job_id }))
        }
        Some(Ok(Reply::Refused {
            status,
            detail,
            code,
        })) => refused(status, detail, code),
        Some(Ok(Reply::Items(_))) | None => internal(),
        Some(Err(error)) => match error.downcast_ref::<Refusal>() {
            Some(Refusal(status, detail)) => failure(*status, detail),
            None => read_error(&error),
        },
    }
}

fn refused(status: u16, detail: &str, code: Option<&str>) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    crate::problem::build(crate::problem::problem(status, detail, code), Map::new())
}

async fn dispatch(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Some(body) = object(&body) else {
        return not_object();
    };
    let id = id.trim().to_owned();
    let force = body.get("force") == Some(&Value::Bool(true));
    present(writes(&app, presented, move |writes, targets| writes.dispatch(targets, id, force)).await)
}

async fn reconcile(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Some(body) = object(&body) else {
        return not_object();
    };
    let Some(printed) = body.get("printed").and_then(Value::as_bool) else {
        return failure(StatusCode::BAD_REQUEST, "printed must be true or false");
    };
    let id = id.trim().to_owned();
    present(writes(&app, presented, move |writes, _| writes.apply(Request::Reconcile { id, printed })).await)
}

async fn remove(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    let id = id.trim().to_owned();
    let removed = writes(&app, presented, move |writes, targets| {
        let reply = writes.apply(Request::Remove { id })?;
        if let Reply::Item(item) = &reply {
            targets.discard(item);
        }
        Ok(reply)
    })
    .await;
    present(removed)
}

/// Send ready, as Node's `drainPrinterSendQueue`: a result per item it sent
/// or could not send; an item refused with 409 (busy, reserved, missing
/// file) is skipped and stays as it is.
async fn drain(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    let Some(body) = object(&body) else {
        return not_object();
    };
    let printer_id = match body.get("printer_id") {
        None => None,
        Some(Value::String(id)) if !id.trim().is_empty() => Some(id.trim().to_owned()),
        Some(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "printer_id must be a non-empty string",
            );
        }
    };
    let drained = writes(&app, presented, move |writes, targets| {
        let items = match writes.apply(Request::Ready { printer_id })? {
            Reply::Items(items) => items,
            refused => return Ok(Err(refused)),
        };
        let mut used = HashSet::new();
        let mut results = Vec::new();
        for item in items {
            let (Some(id), Some(printer)) = (item["id"].as_str(), item["printer_id"].as_str())
            else {
                continue;
            };
            if used.contains(printer) {
                continue;
            }
            let reply = match writes.dispatch(targets, id.to_owned(), false) {
                Ok(reply) => reply,
                Err(error) => match error.downcast::<Refusal>() {
                    Ok(Refusal(status, _)) if status == StatusCode::CONFLICT => continue,
                    Ok(Refusal(_, detail)) => {
                        results.push(json!({ "item_id": id, "error": detail }));
                        continue;
                    }
                    Err(error) => {
                        results.push(json!({ "item_id": id, "error": unsettled(&error) }));
                        break;
                    }
                },
            };
            match reply {
                Reply::Dispatched { job_id, .. } => {
                    used.insert(printer.to_owned());
                    results.push(json!({ "item_id": id, "job_id": job_id }));
                }
                Reply::Refused { status: 409, .. } => {}
                Reply::Refused { detail, .. } => {
                    results.push(json!({ "item_id": id, "error": detail }));
                }
                Reply::Item(_) | Reply::Items(_) => {}
            }
        }
        Ok(Ok(results))
    })
    .await;
    match drained {
        Some(Ok(Ok(results))) => ok_json(&json!({ "results": results })),
        Some(Ok(Err(refused))) => present(Some(Ok(refused))),
        Some(Err(error)) => read_error(&error),
        None => internal(),
    }
}

/// A fixed text for a storage failure part way through a drain: the item may
/// or may not have been claimed, so the client rereads the queue.
fn unsettled(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "Writer queue full" | "Storage stopped" | "Cancelled before admission" => {
            "Storage is busy; this item was not sent"
        }
        _ => "This item's send could not be confirmed; reread the queue",
    }
}

