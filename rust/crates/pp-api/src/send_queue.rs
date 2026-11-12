//! Send queue writes (GRE-365, GRE-1215, GRE-1227) over
//! `pp_storage::send_queue::writes`. Each is an explicit user action:
//!
//! - `POST /api/v2/printer-send-queue` (Add to queue): a `multipart/form-data`
//!   upload of one sliced file for a Moonraker or PrusaLink printer. The file
//!   is staged under `exports/printer-uploads/` and the item is `queued`;
//!   nothing is sent until Send now or Send ready.
//! - `POST /api/v2/printer-send-queue/{id}/dispatch` (Send now): sends one
//!   `queued` or `error` item. With `wait_for_idle` the printer must be
//!   idle unless the body says `{"force": true}`.
//! - `POST /api/v2/printer-send-queue/drain` (Send ready): sends up to eight
//!   `queued` items, optionally for one `printer_id`, at most one per printer.
//!   An item whose printer is busy or reserved stays queued.
//! - `POST /api/v2/printer-send-queue/{id}/reconcile`: the user's answer for a
//!   send with an unknown outcome, [`ReconcileSendRequest`], where `job_id`
//!   is the item's `upload_job_id` the user answered about.
//! - `DELETE /api/v2/printer-send-queue/{id}` (Remove).
//!
//! Add, Reconcile and Remove answer [`QueuedSendReceipt`]; Send now answers
//! [`SendNowReceipt`]. Both carry the item as the queue read shows it,
//! without its staged file path or host name. [`CLIENT_ROUTES`] lists these
//! four for the generated client; Send ready answers untyped JSON.
//!
//! A dispatch only claims the item and enqueues its durable printer-upload
//! job in one writer transaction; the send worker uploads it. Send now on an
//! item whose last send has an unknown outcome is 409 `send_outcome_unknown`.
//! Admission is `accepted::Gate`.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, internal, ok_json, read_error},
    client_contract::{ClientRoute, route},
    loopback::PresentedCredential,
    multipart_form::{self, Limits},
    printer_host::{Windows, throttled},
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::Response,
    routing::{delete, post},
};
use pp_storage::{
    checkoff_progress::print_file::MAX_UPLOAD_BYTES,
    read_model::farm::QueuedSend,
    send_queue::writes::{NewItem, Reply, Request, SendQueueClient, may_have_written},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// Add, Reconcile and Remove: the item as it now is.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct QueuedSendReceipt {
    pub item: QueuedSend,
}

/// Send now: the claimed `sending` item and the upload job that settles it.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SendNowReceipt {
    pub item: QueuedSend,
    pub job_id: String,
}

/// Reconcile's body: whether the send `job_id` printed. `job_id` is the
/// item's `upload_job_id` the user answered about, so a late answer never
/// closes a newer send.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReconcileSendRequest {
    pub printed: bool,
    /// May be omitted: serde reads a missing `Option` as `None`.
    pub job_id: Option<String>,
}

/// Whether a stored item can go to its printer now. `pp-core` implements it
/// over the fleet settings, the staged file and the status watchers.
pub trait SendTargets: Send + Sync + 'static {
    /// The host name to label the send with, or why it cannot be sent.
    /// `idle` asks for an idle printer. Called on a blocking thread.
    fn prepare(&self, item: &Value, idle: bool) -> Prepared;
    /// The host name to label a new item for `printer_id` with, or why that
    /// printer cannot take queued sends. Called on a blocking thread.
    fn host(&self, printer_id: &str) -> Result<String, (StatusCode, String)>;
    /// Writes a new item's file as `name` in its own directory under
    /// `exports/printer-uploads/` and returns its path. Called on a blocking
    /// thread.
    fn stage(&self, name: &str, file: &[u8]) -> std::io::Result<String>;
    /// A dispatch committed: wake the send worker.
    fn dispatched(&self);
    /// Deletes a removed item's staged file directory.
    fn discard(&self, item: &Value);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Prepared {
    Ready {
        host_name: String,
    },
    /// The staged file is gone: the item becomes an error.
    Missing,
    Refused {
        status: StatusCode,
        detail: String,
    },
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: SendQueueClient,
    targets: Arc<dyn SendTargets>,
    windows: Arc<Windows>,
}

/// Node's `rateLimit` for Add to queue, and its upload form: one file part
/// named `file` or `gcode` of up to 64 MiB, and eight fields cut at 64 KiB.
const ADD_LIMIT: u32 = 10;
const ADD_FORM: Limits = Limits {
    fields: 8,
    field_bytes: 64 * 1024,
    file_bytes: MAX_UPLOAD_BYTES,
    file_names: &["file", "gcode"],
    second_file: "Only one G-code file is allowed",
    too_large: "Print file exceeds the 64 MiB upload limit",
    unreadable: "That upload could not be read",
};

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
        .route(
            "/printer-send-queue",
            post(add).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1024 * 1024)),
        )
        .route("/printer-send-queue/drain", post(drain))
        .route("/printer-send-queue/{id}/dispatch", post(dispatch))
        .route("/printer-send-queue/{id}/reconcile", post(reconcile))
        .route("/printer-send-queue/{id}", delete(remove));
    gate.guard(Router::new().nest("/api/v2", routes))
        .with_state(App {
            gate,
            client,
            targets,
            windows: Arc::default(),
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

const NOT_OBJECT: &str = "Request body must be a JSON object";

fn not_object() -> Response {
    failure(StatusCode::BAD_REQUEST, NOT_OBJECT)
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
    fn dispatch(
        &self,
        targets: &dyn SendTargets,
        id: String,
        force: bool,
    ) -> anyhow::Result<Reply> {
        let item = match self.apply(Request::Check { id: id.clone() })? {
            Reply::Item(item) => item,
            refused => return Ok(refused),
        };
        let idle = item["wait_for_idle"] == true && !force;
        match targets.prepare(&item, idle) {
            Prepared::Ready { host_name } => {
                let reply = self.apply(Request::Dispatch { id, host_name })?;
                if matches!(reply, Reply::Dispatched { .. }) {
                    targets.dispatched();
                }
                Ok(reply)
            }
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
        Some(Ok(Reply::Item(item))) => match QueuedSend::from_item(&item) {
            Some(item) => ok_json(&QueuedSendReceipt { item }),
            None => internal(),
        },
        Some(Ok(Reply::Dispatched { item, job_id })) => match QueuedSend::from_item(&item) {
            Some(item) => ok_json(&SendNowReceipt { item, job_id }),
            None => internal(),
        },
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

/// Add to queue, in Node's order: the credential before a byte of the body,
/// the rate window, the form, the printer's host, then the staged file and
/// the item. The staged file is deleted again unless the item was written or
/// storage's outcome is unknown, since the item may then exist.
async fn add(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    request: axum::extract::Request,
) -> Response {
    let admitted = writes(&app, presented.clone(), |writes, _| {
        writes.apply(Request::Admit)
    })
    .await;
    if !matches!(admitted, Some(Ok(Reply::Items(_)))) {
        return present(admitted);
    }
    if let Some(after) = app.windows.consume("add", ADD_LIMIT) {
        return throttled(after);
    }
    let form = match multipart_form::read(request, &ADD_FORM).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    let name = form.file.as_ref().map(|(name, _)| name.as_deref());
    let item = match NewItem::parse(&form.fields, name) {
        Ok(item) => item,
        Err(detail) => return failure(StatusCode::BAD_REQUEST, detail),
    };
    let file = form.file.map(|(_, bytes)| bytes).unwrap_or_default();
    let added = writes(&app, presented, move |writes, targets| {
        let host_name = targets
            .host(&item.printer_id)
            .map_err(|(status, detail)| Refusal(status, detail))?;
        let artifact_path = targets.stage(&item.staged_name(), &file)?;
        let reply = writes.apply(Request::Add {
            item,
            artifact_path: artifact_path.clone(),
            host_name,
        });
        let kept = match &reply {
            Ok(reply) => matches!(reply, Reply::Item(_)),
            Err(error) => may_have_written(error),
        };
        if !kept {
            targets.discard(&json!({ "artifact_path": artifact_path }));
        }
        reply
    })
    .await;
    present(added)
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
    present(
        writes(&app, presented, move |writes, targets| {
            writes.dispatch(targets, id, force)
        })
        .await,
    )
}

async fn reconcile(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    match reconcile_request(&id, &body) {
        Ok(request) => {
            present(writes(&app, presented, move |writes, _| writes.apply(request)).await)
        }
        Err(detail) => failure(StatusCode::BAD_REQUEST, detail),
    }
}

/// The user's answer as a [`Request::Reconcile`], or the 400 detail for a
/// bad body.
fn reconcile_request(id: &str, body: &[u8]) -> Result<Request, &'static str> {
    let body: ReconcileSendRequest = serde_json::from_slice(body)
        .map_err(|_| "Send {\"printed\": true or false, \"job_id\": the send's upload_job_id}")?;
    Ok(Request::Reconcile {
        id: id.trim().to_owned(),
        job_id: body.job_id,
        printed: body.printed,
    })
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
                Reply::Refused { status, detail, .. } if status != 409 => {
                    results.push(json!({ "item_id": id, "error": detail }));
                }
                Reply::Refused { .. } | Reply::Item(_) | Reply::Items(_) => {}
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

/// The send routes in the generated client, composed by `export-rust-client`
/// and checked by `pp-core`'s mounted-route contract test. Add to queue takes
/// a `multipart/form-data` body, so it lists no request DTO; Send now sends no
/// body, since the replacement UI never sends `force`.
pub const CLIENT_ROUTES: [ClientRoute; 4] = [
    route(
        "addQueuedSend",
        "POST",
        "/api/v2/printer-send-queue",
        None,
        "QueuedSendReceipt",
    ),
    ClientRoute {
        text_params: &["id"],
        ..route(
            "sendQueuedNow",
            "POST",
            "/api/v2/printer-send-queue/{id}/dispatch",
            None,
            "SendNowReceipt",
        )
    },
    ClientRoute {
        text_params: &["id"],
        ..route(
            "reconcileQueuedSend",
            "POST",
            "/api/v2/printer-send-queue/{id}/reconcile",
            Some("ReconcileSendRequest"),
            "QueuedSendReceipt",
        )
    },
    ClientRoute {
        text_params: &["id"],
        ..route(
            "removeQueuedSend",
            "DELETE",
            "/api/v2/printer-send-queue/{id}",
            None,
            "QueuedSendReceipt",
        )
    },
];

/// Adds [`CLIENT_ROUTES`], their DTO schemas and Rust-serialized fixtures to
/// the generator input.
#[cfg(feature = "codegen")]
pub fn client_document(document: &mut Value) -> Result<(), serde_json::Error> {
    use pp_storage::read_model::farm::SendState;
    document["dtos"]["QueuedSendReceipt"] =
        serde_json::to_value(schemars::schema_for!(QueuedSendReceipt))?;
    document["dtos"]["SendNowReceipt"] =
        serde_json::to_value(schemars::schema_for!(SendNowReceipt))?;
    // The client always sends `job_id`, null included; the route also
    // takes it omitted.
    let mut reconcile = schemars::schema_for!(ReconcileSendRequest);
    if let Some(object) = reconcile.schema.object.as_mut() {
        object.required.insert("job_id".into());
    }
    document["dtos"]["ReconcileSendRequest"] = serde_json::to_value(reconcile)?;
    if let Some(routes) = document["routes"].as_array_mut() {
        for route in CLIENT_ROUTES {
            routes.push(serde_json::to_value(route)?);
        }
    }
    let item = |state, filled: bool| QueuedSend {
        id: "send-1".into(),
        filename: "Ünïcødé 🧵 層.gcode".into(),
        printer_id: "printer-0".into(),
        profile_id: Some(9_007_199_254_740_991),
        state,
        start: !filled,
        wait_for_idle: !filled,
        unit_count: 0,
        created_at: Some("2026-11-12T00:00:00.000Z".into()),
        updated_at: Some("2026-11-12T00:10:00.000Z".into()),
        error: filled.then(|| "Upload failed (HTTP 500)".into()),
        upload_job_id: filled.then(|| "job-1".into()),
        outcome_unknown: filled,
    };
    let served = |dto: &str, name: &str, value: Value| json!({"dto": dto, "name": name, "value": value, "rust_accepts": true});
    let request = |name: &str, value: Value| {
        let rust_accepts = serde_json::from_value::<ReconcileSendRequest>(value.clone()).is_ok();
        json!({"dto": "ReconcileSendRequest", "name": name, "value": value, "rust_accepts": rust_accepts})
    };
    let cases = [
        served(
            "QueuedSendReceipt",
            "queued",
            serde_json::to_value(QueuedSendReceipt {
                item: item(SendState::Queued, false),
            })?,
        ),
        served(
            "QueuedSendReceipt",
            "outcome-unknown",
            serde_json::to_value(QueuedSendReceipt {
                item: item(SendState::Error, true),
            })?,
        ),
        served(
            "SendNowReceipt",
            "sending",
            serde_json::to_value(SendNowReceipt {
                item: QueuedSend {
                    upload_job_id: Some("job-1".into()),
                    ..item(SendState::Sending, false)
                },
                job_id: "job-1".into(),
            })?,
        ),
        request("printed", json!({"printed": true, "job_id": "job-1"})),
        request("no-job", json!({"printed": false, "job_id": null})),
        request(
            "unknown-key",
            json!({"printed": true, "job_id": null, "force": true}),
        ),
        request("printed-text", json!({"printed": "no"})),
        request("printed-missing", json!({"job_id": "job-1"})),
    ];
    if let Some(fixtures) = document["fixtures"].as_array_mut() {
        fixtures.extend(cases);
    }
    Ok(())
}
