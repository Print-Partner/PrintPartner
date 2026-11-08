//! Print-file assignment routes (GRE-1220), at the paths Node's
//! `routes/printer-checkoff.ts` serves at the root and under `/api/v1`:
//!
//! - `POST /printer-checkoff/file-assignments/preview` answers what the file
//!   is and which Required units it suggests, and writes nothing.
//! - `POST /printer-checkoff/file-assignments` records the print as a new
//!   printer Checkoff link through storage's create path.
//! - `POST /printer-checkoff/file-assignments/upload` takes a print file off
//!   the operator's computer as `multipart/form-data`, classifies it, and
//!   holds it under an `upload_token` in an [`UploadStore`] (GRE-1255).
//!
//! Storage parses the body and resolves the printer in Node's order, then the
//! route reads the file. A `snapshot_token` is redeemed from the
//! [`SnapshotStore`] the files/content route captures into; a token for
//! another Build, printer, path or file name, an expired one, or one whose
//! printer host changed is refused. After a successful assignment the token
//! is spent. An `upload_token` is redeemed the same way for the Build it was
//! uploaded for, and an unknown, expired or foreign one is refused as Node
//! refuses it. A `remote_path` on a Moonraker or PrusaLink host without a
//! snapshot is read off the printer through the [`PrinterHostPort`], and an
//! assignment of it first records drift on the links that watch that path
//! (GRE-1256).

use crate::{
    accepted::{AcceptedHttpConfig, Gate, failure, ok_json},
    loopback::PresentedCredential,
    printer_host::{HostAnswer, PrinterHostPort, Windows, safe_storage_path, throttled},
};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, Multipart, State, multipart::Field},
    http::{StatusCode, header::CONTENT_TYPE},
    response::Response,
    routing::post,
};
use pp_storage::auth::AuthFailure;
use pp_storage::checkoff_progress::{
    AssignedLink, Assignment, Body, FileRequest, FileRoute, ProgressClient, REPLY_LOST, Request,
    print_file::{
        self, HeldUpload, Inspected, MAX_UPLOAD_BYTES, Reservation, SNAPSHOT_EXPIRED,
        SnapshotStore, TOO_LARGE_TO_INSPECT, UPLOAD_GONE, UploadRefusal, UploadStore, upload_form,
    },
};
use serde_json::{Value, json};
use std::sync::Arc;

/// Node's `rateLimit` and JSON body limit for the preview and assignment routes.
const LIMIT: u32 = 20;
const BODY_LIMIT: usize = 1024 * 1024;
/// The upload route's `rateLimit`, and its multipart limits: one file part,
/// four fields of up to 64 KiB, and room for them beside the file.
const UPLOAD_LIMIT: u32 = 10;
const UPLOAD_FIELDS: usize = 4;
const UPLOAD_FIELD_BYTES: usize = 64 * 1024;
const UPLOAD_BODY_LIMIT: usize = MAX_UPLOAD_BYTES + 1024 * 1024;
const UPLOAD_TOO_LARGE: &str =
    "That print file is larger than the 64 MB PrintPartner will take as an upload";
const UPLOAD_UNREADABLE: &str = "That upload could not be read";
const UPLOADS_FULL: &str = "PrintPartner is holding as many uploaded print files as it will; record one of them, then try again";
/// Field names `@fastify/multipart` refuses because they are
/// `Object.prototype` members.
const PROTOTYPE: [&str; 12] = [
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

#[derive(Clone)]
struct App {
    gate: Gate,
    client: ProgressClient,
    snapshots: Arc<SnapshotStore>,
    uploads: Arc<UploadStore>,
    hosts: Arc<dyn PrinterHostPort>,
    windows: Arc<Windows>,
}

/// Mounts the routes at `/` and `/api/v1`, each mount with its own rate
/// windows as Node registers them, when `PP_DESKTOP_REBUILD` was exactly `1`
/// or `true`; otherwise nothing.
pub fn file_assignments_router(
    config: AcceptedHttpConfig,
    client: ProgressClient,
    snapshots: Arc<SnapshotStore>,
    hosts: Arc<dyn PrinterHostPort>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let uploads = Arc::new(UploadStore::default());
    let mount = || {
        Router::new()
            .route("/printer-checkoff/file-assignments/preview", post(preview))
            .route("/printer-checkoff/file-assignments", post(assign))
            .route(
                "/printer-checkoff/file-assignments/upload",
                post(upload).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
            )
            .layer(DefaultBodyLimit::max(BODY_LIMIT))
            .with_state(App {
                gate: gate.clone(),
                client: client.clone(),
                snapshots: snapshots.clone(),
                uploads: uploads.clone(),
                hosts: hosts.clone(),
                windows: Arc::default(),
            })
    };
    gate.guard(Router::new().merge(mount()).nest("/api/v1", mount()))
}

/// What reading the request's file found.
enum Inspection {
    Inspected(Inspected),
    Refused(&'static str),
    /// Nothing about the file is known, and nothing may be assumed.
    Unreadable,
    /// The printer's settings could not be read in time.
    Busy,
    /// Classifying the bytes failed inside PrintPartner.
    Failed,
}

impl Inspection {
    /// The inspected file, `None` when it is unread, or the answer that
    /// refuses the request.
    fn settle(self) -> Result<Option<Inspected>, Box<Response>> {
        match self {
            Self::Inspected(inspected) => Ok(Some(inspected)),
            Self::Unreadable => Ok(None),
            Self::Refused(detail) => Err(Box::new(conflict(detail))),
            Self::Busy => Err(Box::new(busy())),
            Self::Failed => Err(Box::new(crate::accepted::internal())),
        }
    }
}

/// The bytes of a host reply, or `Ok(None)` past `cap` as Node's
/// `readBoundedResponseChunks` refuses them: a declared length past `cap`
/// is refused before any byte is read.
async fn bounded(
    mut body: axum::body::Body,
    declared: Option<u64>,
    cap: usize,
) -> Result<Option<Vec<u8>>, ()> {
    use http_body::Body as _;
    if declared.is_some_and(|length| length > cap as u64) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    while let Some(frame) =
        std::future::poll_fn(|context| std::pin::Pin::new(&mut body).poll_frame(context)).await
    {
        let Ok(data) = frame.map_err(|_| ())?.into_data() else {
            continue;
        };
        if bytes.len() + data.len() > cap {
            return Ok(None);
        }
        bytes.extend_from_slice(&data);
    }
    Ok(Some(bytes))
}

/// Node's `inspectRemoteFile`: the provider's listing entry for the path,
/// then the bytes the host serves, classified. Unlike Node, the host serves
/// only a file its directory listing shows, so a listing that fails or omits
/// the file leaves it unread.
async fn remote(app: &App, printer: &str, raw: &str) -> Inspection {
    let Some(path) = safe_storage_path(raw) else {
        return Inspection::Unreadable;
    };
    let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
    let provider = match app
        .hosts
        .files(printer.to_owned(), directory.to_owned())
        .await
    {
        HostAnswer::Ok(listing) => listing["entries"]
            .as_array()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry["kind"] == "file" && entry["path"] == path.as_str())
            })
            .map_or(Value::Null, |entry| {
                let keys = ["size_bytes", "modified_at", "provider_revision"];
                Value::Object(
                    keys.iter()
                        .filter_map(|&key| Some((key.to_owned(), entry.get(key)?.clone())))
                        .collect(),
                )
            }),
        HostAnswer::Busy => return Inspection::Busy,
        _ => Value::Null,
    };
    let stream = match app.hosts.file(printer.to_owned(), path).await {
        HostAnswer::Ok(Some(stream)) if (200..300).contains(&stream.status) => stream,
        HostAnswer::Busy => return Inspection::Busy,
        _ => return Inspection::Unreadable,
    };
    let cap = print_file::MAX_CLASSIFIABLE_BYTES;
    let bytes = match bounded(stream.body, stream.content_length, cap).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Inspection::Refused(TOO_LARGE_TO_INSPECT),
        Err(()) => return Inspection::Unreadable,
    };
    match tokio::task::spawn_blocking(move || print_file::inspect(&bytes, provider)).await {
        Ok(Ok(inspected)) => Inspection::Inspected(inspected),
        Ok(Err(detail)) => Inspection::Refused(detail),
        Err(_) => Inspection::Failed,
    }
}

async fn inspect(app: &App, parsed: &FileRequest) -> Inspection {
    if let (Some(token), Some(scope)) = (&parsed.snapshot_token, parsed.scope()) {
        return app
            .snapshots
            .read(token, &scope, parsed.printer.source.as_deref())
            .map_or(Inspection::Refused(SNAPSHOT_EXPIRED), Inspection::Inspected);
    }
    if let Some(path) = &parsed.remote_path {
        return if parsed.printer.host_files {
            remote(app, &parsed.printer.printer_id, path).await
        } else {
            Inspection::Unreadable
        };
    }
    if let Some(token) = &parsed.upload_token {
        return app
            .uploads
            .read(token, parsed.profile_id)
            .map_or(Inspection::Refused(UPLOAD_GONE), Inspection::Inspected);
    }
    Inspection::Unreadable
}

/// The file one assignment holds while it records the print.
enum Held {
    Snapshot(Reservation),
    Upload(HeldUpload),
}

impl Held {
    fn inspected(&self) -> Option<&Inspected> {
        match self {
            Self::Snapshot(held) => held.inspected(),
            Self::Upload(held) => held.inspected(),
        }
    }

    fn spend(self) {
        match self {
            Self::Snapshot(held) => held.spend(),
            Self::Upload(held) => held.spend(),
        }
    }
}

/// The fields a preview adds for a file whose bytes were read.
fn read(answer: &mut serde_json::Map<String, Value>, inspected: &Inspected) {
    let classification = inspected.classification;
    answer.insert("inspected".into(), json!(true));
    answer.insert("classification".into(), json!(classification));
    answer.insert("print_ready".into(), json!(classification.is_print_ready()));
    answer.insert("next_action".into(), json!(classification.next_action()));
}

fn busy() -> Response {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "Printer settings are unavailable",
    )
}

fn conflict(detail: &str) -> Response {
    failure(StatusCode::CONFLICT, detail)
}

/// Storage's answer: a parsed request, or the response that ends this one.
fn answer(
    response: pp_storage::checkoff_progress::Response,
) -> Result<Box<FileRequest>, Box<Response>> {
    match response.body {
        Body::FileRequest(parsed) if response.status == 200 => Ok(parsed),
        body => Err(Box::new(crate::checkoff::present(
            pp_storage::checkoff_progress::Response {
                status: response.status,
                body,
            },
        ))),
    }
}

async fn storage(
    app: &App,
    presented: PresentedCredential,
    request: Request,
    held: Option<Held>,
) -> Result<pp_storage::checkoff_progress::Response, Box<Response>> {
    let client = app.client.clone();
    // The held file lives in the blocking call, which finishes an admitted
    // write even when the request is dropped. A committed assignment spends the
    // snapshot, and so does an outcome that is unknown after admission (a lost
    // writer reply or an unknown commit), since the write may have landed. Every
    // other answer, refusals and credential failures included, wrote nothing
    // and puts it back.
    match app
        .gate
        .read(presented, move |credential, cancelled, wait| {
            let result = client.apply(credential, request, &cancelled, wait);
            let spend = match &result {
                Ok(response) => response.status == 200,
                Err(error) => {
                    matches!(error.downcast_ref(), Some(AuthFailure::CommitUnknown))
                        || error.to_string() == REPLY_LOST
                }
            };
            if spend && let Some(held) = held {
                held.spend();
            }
            result
        })
        .await
    {
        Some(Ok(response)) => Ok(response),
        Some(Err(error)) => Err(Box::new(crate::accepted::read_error(&error))),
        None => Err(Box::new(crate::accepted::internal())),
    }
}

async fn parse(
    app: &App,
    presented: PresentedCredential,
    body: &[u8],
    route: FileRoute,
) -> Result<Box<FileRequest>, Box<Response>> {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return Err(Box::new(failure(
            StatusCode::BAD_REQUEST,
            "Request body must be a JSON object",
        )));
    };
    answer(storage(app, presented, Request::FileRequest { body, route }, None).await?)
}

async fn preview(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    if let Some(after) = app.windows.consume("preview", LIMIT) {
        return throttled(after);
    }
    let mut parsed = match parse(&app, presented, &body, FileRoute::Preview).await {
        Ok(parsed) => parsed,
        Err(response) => return *response,
    };
    let mut answer = match parsed.suggestion.take() {
        Some(Value::Object(suggestion)) => suggestion,
        _ => return crate::accepted::internal(),
    };
    match inspect(&app, &parsed).await.settle() {
        Err(response) => return *response,
        Ok(None) => {
            answer.insert("inspected".into(), json!(false));
        }
        Ok(Some(inspected)) => read(&mut answer, &inspected),
    }
    ok_json(&answer)
}

async fn assign(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    body: Bytes,
) -> Response {
    if let Some(after) = app.windows.consume("assign", LIMIT) {
        return throttled(after);
    }
    let parsed = match parse(&app, presented.clone(), &body, FileRoute::Assign).await {
        Ok(parsed) => parsed,
        Err(response) => return *response,
    };
    let Some(input) = parsed.assign.clone() else {
        return crate::accepted::internal();
    };
    // A redeemed snapshot or upload is held for this assignment alone, so a
    // concurrent one cannot record the same file, and returns if this one fails.
    let held = match (&parsed.snapshot_token, parsed.scope(), &parsed.upload_token) {
        (Some(token), Some(scope), _) => {
            match app
                .snapshots
                .reserve(token, &scope, parsed.printer.source.as_deref())
            {
                Some(reservation) => Some(Held::Snapshot(reservation)),
                None => return conflict(SNAPSHOT_EXPIRED),
            }
        }
        (None, _, Some(token)) => match app.uploads.reserve(token, parsed.profile_id) {
            Some(upload) => Some(Held::Upload(upload)),
            None => return conflict(UPLOAD_GONE),
        },
        _ => None,
    };
    let inspection = match held.as_ref().and_then(Held::inspected) {
        Some(held) => Inspection::Inspected(held.clone()),
        None => inspect(&app, &parsed).await,
    };
    // The bytes decide. A file recorded as already printed is evidence of
    // what was made, so only a file PrintPartner might still print must be
    // print-ready.
    let inspected = match inspection.settle() {
        Err(response) => return *response,
        Ok(None) => None,
        Ok(Some(inspected)) => {
            if !input.completed && !inspected.classification.is_print_ready() {
                return conflict(inspected.classification.next_action());
            }
            Some(inspected)
        }
    };
    // Re-observing a file read off the printer is what records drift on any
    // link that already points at that path, whatever this assignment does.
    if let (Some(observed), Some(path), None) = (&inspected, &parsed.remote_path, &held) {
        let body = json!({
            "integration_id": parsed.printer.integration_id,
            "remote_path": path,
            "observed": observed.identity,
        });
        let route = FileRoute::Observe;
        let request = Request::FileRequest { body, route };
        if let Err(response) = storage(&app, presented.clone(), request, None).await {
            return *response;
        }
    }
    if inspected.is_none() && parsed.filename.to_lowercase().ends_with(".3mf") {
        return conflict(
            "PrintPartner has to read a 3MF to tell whether it holds printer instructions; upload the file so PrintPartner can read it",
        );
    }
    let assignment = Assignment {
        profile_id: parsed.profile_id,
        plan_revision_id: input.plan_revision_id,
        object_names: parsed.object_names.clone(),
        units: input.units,
        mappings: input.mappings,
        retain_unmatched: input.retain_unmatched,
        completed: input.completed,
        link: AssignedLink {
            integration_id: parsed.printer.integration_id.clone(),
            printer_id: parsed.printer.printer_id.clone(),
            host_name: parsed.printer.printer_name.clone(),
            filename: parsed.filename.clone(),
            remote_path: parsed.remote_path.clone(),
            classification: inspected
                .as_ref()
                .map(|inspected| json!(inspected.classification)),
            remote_identity: inspected.map(|inspected| inspected.identity),
        },
        snapshot_source: matches!(held, Some(Held::Snapshot(_)))
            .then(|| parsed.printer.source.clone())
            .flatten(),
    };
    let request = Request::AssignFile(Box::new(assignment));
    match storage(&app, presented, request, held).await {
        Ok(response) => crate::checkoff::present(response),
        Err(response) => *response,
    }
}

/// The upload form's fields and file part.
#[derive(Default)]
struct Form {
    profile_id: String,
    object_names: String,
    name: String,
    file: Option<Vec<u8>>,
}

/// One part's bytes, or `Err(true)` past `cap` or the body limit and
/// `Err(false)` when the body cannot be read. A
/// `truncate` part keeps its first `cap` bytes instead, as busboy cuts a
/// field at `fieldSize`.
async fn part_bytes(part: &mut Field<'_>, cap: usize, truncate: bool) -> Result<Vec<u8>, bool> {
    let mut bytes = Vec::new();
    while let Some(chunk) = part
        .chunk()
        .await
        .map_err(|error| error.status() == StatusCode::PAYLOAD_TOO_LARGE)?
    {
        if bytes.len() + chunk.len() > cap && !truncate {
            return Err(true);
        }
        let room = cap - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    Ok(bytes)
}

/// The multipart form under Node's limits. A part is a file when it has a
/// file name or an `application/octet-stream` type, as busboy decides, and a
/// part named after an `Object.prototype` member is refused as
/// `@fastify/multipart` refuses it, when the form ends. One file part of up to 64 MB (a
/// file part not named `file` is read and ignored), and four fields cut at
/// 64 KiB, of which `profile_id` and `object_names` are kept.
async fn read_form(request: axum::extract::Request) -> Result<Form, Box<Response>> {
    let multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("multipart/form-data")
        });
    if !multipart {
        return Err(Box::new(failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Send the print file as multipart/form-data",
        )));
    }
    let bad = |detail: &str| Box::new(failure(StatusCode::BAD_REQUEST, detail));
    let refused = |too_large: bool| {
        if too_large {
            Box::new(failure(StatusCode::PAYLOAD_TOO_LARGE, UPLOAD_TOO_LARGE))
        } else {
            bad(UPLOAD_UNREADABLE)
        }
    };
    let mut parts = Multipart::from_request(request, &())
        .await
        .map_err(|_| bad(UPLOAD_UNREADABLE))?;
    let mut form = Form::default();
    let (mut files, mut fields) = (0, 0);
    let mut prototype = false;
    loop {
        let mut part = match parts.next_field().await {
            Ok(Some(part)) => part,
            Ok(None) if prototype => return Err(bad(UPLOAD_UNREADABLE)),
            Ok(None) => return Ok(form),
            Err(_) => return Err(bad(UPLOAD_UNREADABLE)),
        };
        let name = part.name().map(str::to_owned);
        // @fastify/multipart records this refusal and raises it when the
        // stream ends, so a limit hit later still answers first.
        let proto = name
            .as_deref()
            .is_some_and(|name| PROTOTYPE.contains(&name));
        prototype |= proto;
        let octets = part.content_type().is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/octet-stream")
        });
        let file_name = part.file_name().map(str::to_owned);
        if file_name.is_none() && !octets {
            fields += 1;
            let value = part_bytes(&mut part, UPLOAD_FIELD_BYTES, true)
                .await
                .map_err(|_| bad(UPLOAD_UNREADABLE))?;
            if fields > UPLOAD_FIELDS {
                return Err(bad(UPLOAD_UNREADABLE));
            }
            let value = String::from_utf8_lossy(&value).into_owned();
            match name.as_deref() {
                Some("profile_id") => form.profile_id = value,
                Some("object_names") => form.object_names = value,
                _ => {}
            }
            continue;
        }
        files += 1;
        if files > 1 {
            return Err(bad("Upload one print file at a time"));
        }
        // Node drains a refused part without its size limit.
        let bytes = part_bytes(&mut part, MAX_UPLOAD_BYTES, proto)
            .await
            .map_err(|too_large| refused(too_large && !proto))?;
        if name.as_deref() == Some("file") {
            form.name = file_name.unwrap_or_default();
            form.file = Some(bytes);
        }
    }
}

/// Node's upload route: the credential, then the form fields in its order, then the Build's
/// Accepted Plan, then the bytes, which are classified off the async workers
/// and held under a new `upload_token`. The answer is the preview's body.
async fn upload(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    request: axum::extract::Request,
) -> Response {
    // Node authenticates in `onRequest` hooks, before the route's rate window
    // and before it reads a byte of the body.
    let admit = Request::FileRequest {
        body: Value::Null,
        route: FileRoute::Admit,
    };
    if let Err(response) = storage(&app, presented.clone(), admit, None).await {
        return *response;
    }
    if let Some(after) = app.windows.consume("upload", UPLOAD_LIMIT) {
        return throttled(after);
    }
    let form = match read_form(request).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    let (profile_id, filename) =
        match upload_form(&form.profile_id, form.file.is_some(), &form.name) {
            Ok(fields) => fields,
            Err(detail) => return failure(StatusCode::BAD_REQUEST, detail),
        };
    let body = json!({ "profile_id": profile_id, "filename": filename, "object_names": form.object_names });
    let route = FileRoute::Upload;
    let response = match storage(&app, presented, Request::FileRequest { body, route }, None).await
    {
        Ok(response) => response,
        Err(response) => return *response,
    };
    let mut answer = match response.body {
        Body::Suggestion(suggestion) if response.status == 200 => match *suggestion {
            Value::Object(suggestion) => suggestion,
            _ => return crate::accepted::internal(),
        },
        body => {
            return crate::checkoff::present(pp_storage::checkoff_progress::Response {
                status: response.status,
                body,
            });
        }
    };
    let uploads = app.uploads.clone();
    let bytes = form.file.unwrap_or_default();
    let held = tokio::task::spawn_blocking(move || uploads.hold(profile_id, &bytes)).await;
    let (token, inspected) = match held {
        Ok(Ok(held)) => held,
        Ok(Err(UploadRefusal::Rejected(detail))) => return conflict(detail),
        Ok(Err(UploadRefusal::Full)) => {
            return failure(StatusCode::SERVICE_UNAVAILABLE, UPLOADS_FULL);
        }
        Err(_) => return crate::accepted::internal(),
    };
    read(&mut answer, &inspected);
    answer.insert("upload_token".into(), json!(token));
    ok_json(&answer)
}

#[cfg(test)]
mod tests {
    use super::bounded;
    use axum::body::{Body, Bytes};
    use http_body::Frame;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// A host reply that arrives in frames of `sizes` bytes, then fails
    /// when `fail` is set.
    struct Frames(VecDeque<Result<Bytes, std::io::Error>>);

    impl http_body::Body for Frames {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
            Poll::Ready(self.0.pop_front().map(|chunk| chunk.map(Frame::data)))
        }
    }

    fn frames(sizes: &[usize], fail: bool) -> Body {
        let mut chunks: VecDeque<_> = sizes
            .iter()
            .map(|&size| Ok(Bytes::from(vec![b'G'; size])))
            .collect();
        if fail {
            chunks.push_back(Err(std::io::Error::other("reset")));
        }
        Body::new(Frames(chunks))
    }

    async fn read(sizes: &[usize], declared: Option<u64>) -> Result<Option<usize>, ()> {
        Ok(bounded(frames(sizes, false), declared, 8)
            .await?
            .map(|bytes| bytes.len()))
    }

    #[tokio::test]
    async fn the_cap_is_inclusive_and_checked_per_frame_and_against_the_declared_length() {
        assert_eq!(read(&[3, 4], None).await, Ok(Some(7)));
        assert_eq!(read(&[4, 4], Some(8)).await, Ok(Some(8)), "exactly the cap");
        assert_eq!(
            read(&[4, 4, 1], None).await,
            Ok(None),
            "one byte past the cap"
        );
        assert_eq!(
            read(&[1], Some(9)).await,
            Ok(None),
            "a declared length past the cap"
        );
        assert_eq!(
            bounded(frames(&[2], true), None, 8).await,
            Err(()),
            "a body that fails partway"
        );
    }
}
