//! The one builder for Rust API error responses.
//!
//! Every pp-api router answers errors through [`build`], which serves a
//! [`Problem`] as `application/problem+json` with `Cache-Control: private, no-store`.
//! [`layer`] is the backstop for errors the framework writes itself (axum
//! extractor rejections, 405 and body-limit refusals): it drops their text and
//! answers a problem for the same status. MCP keeps JSON-RPC errors and does not
//! use this module.
//!
//! Callers pass fixed text or bounded user input, never `error.to_string()` of
//! a storage, filesystem or network error. `problem_response_canary` checks that
//! SQL, paths and URLs from underlying errors never reach a response.
use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
mod wire;
use pp_storage::{WriterFailure, auth::AuthFailure};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
pub use wire::*;

/// Members a route's extensions may not replace.
const RESERVED: [&str; 6] = ["type", "title", "status", "detail", "code", "errors"];
const FALLBACK: &[u8] = br#"{"type":"urn:print-partner:problem:internal","title":"Internal Error","status":500,"detail":"Internal Server Error"}"#;

/// A problem typed by `status` alone.
pub fn response(status: StatusCode, detail: &str) -> Response {
    build(problem(status, detail, None), Map::new())
}

/// The Build rename rule's 409: another Build of the tenant has `name`. Accept's
/// header copy and PATCH /plans/{id} both answer it.
pub fn duplicate_build_name(name: &str) -> Response {
    let mut extensions = Map::new();
    extensions.insert("name".into(), Value::String(name.to_owned()));
    build(
        problem(StatusCode::CONFLICT, "", Some("duplicate_build_name")).without_detail(),
        extensions,
    )
}

/// A problem with a Node-compatible `code`; the code may refine the type.
pub fn coded(status: StatusCode, detail: &str, code: &str) -> Response {
    build(problem(status, detail, Some(code)), Map::new())
}

/// A problem whose type the caller states, for causes the status cannot show
/// (an unreadable body is 400 but not a validation failure).
pub fn typed(kind: ProblemType, status: StatusCode, detail: &str) -> Response {
    build(Problem::new(kind, status.as_u16(), detail), Map::new())
}

/// The detail every route uses for a body that is not the JSON it expects.
pub const INVALID_JSON: &str = "Invalid JSON body";

/// 503 for a full admission semaphore or writer queue.
pub fn overloaded(detail: &str) -> Response {
    typed(
        ProblemType::Overloaded,
        StatusCode::SERVICE_UNAVAILABLE,
        detail,
    )
}

/// `code` of a write whose transaction could not start before its deadline.
pub const TRANSACTION_UNAVAILABLE: &str = "transaction_unavailable";
/// `code` of a write that may or may not have committed.
pub const OUTCOME_UNKNOWN: &str = "outcome_unknown";
const UNAVAILABLE: &str = "Service temporarily unavailable";

/// The problem for a [`WriterFailure`], the same for every storage family
/// that writes through the write seam. A family maps its own error variants
/// and hands this one here. The cause of an internal failure is never sent.
pub fn writer_failure(failure: &WriterFailure) -> Response {
    use AuthFailure::{
        CommitUnknown, CredentialChanged, InvalidCredentials, OwnerMappingRequired, QueueFull,
        SessionRequired, Stopped, Storage,
    };
    let unavailable = StatusCode::SERVICE_UNAVAILABLE;
    match failure {
        WriterFailure::Auth(SessionRequired | InvalidCredentials | CredentialChanged) => {
            response(StatusCode::UNAUTHORIZED, "Authentication required")
        }
        WriterFailure::Auth(OwnerMappingRequired) => response(
            StatusCode::FORBIDDEN,
            "Explicit account owner mapping is required",
        ),
        WriterFailure::QueueFull | WriterFailure::Auth(QueueFull) => overloaded(UNAVAILABLE),
        WriterFailure::Busy => coded(unavailable, UNAVAILABLE, TRANSACTION_UNAVAILABLE),
        WriterFailure::Auth(CommitUnknown) => coded(unavailable, UNAVAILABLE, OUTCOME_UNKNOWN),
        WriterFailure::Stopped
        | WriterFailure::Cancelled
        | WriterFailure::Auth(Stopped | Storage) => response(unavailable, UNAVAILABLE),
        WriterFailure::Auth(_) | WriterFailure::Internal(_) => {
            response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
    }
}

pub fn problem(status: StatusCode, detail: &str, code: Option<&str>) -> Problem {
    let kind = if status == StatusCode::BAD_REQUEST && detail == INVALID_JSON {
        ProblemType::MalformedRequest
    } else {
        ProblemType::classify(status.as_u16(), code)
    };
    let problem = Problem::new(kind, status.as_u16(), detail);
    match code {
        Some(code) => problem.with_code(code),
        None => problem,
    }
}

/// Serves `problem` plus extension members. An extension cannot replace a
/// standard member.
pub fn build(problem: Problem, extensions: Map<String, Value>) -> Response {
    let status = StatusCode::from_u16(problem.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = match serde_json::to_value(&problem) {
        Ok(Value::Object(mut members)) => {
            for (key, value) in extensions {
                if !RESERVED.contains(&key.as_str()) {
                    members.insert(key, value);
                }
            }
            serde_json::to_vec(&members).unwrap_or_else(|_| FALLBACK.to_vec())
        }
        _ => FALLBACK.to_vec(),
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(PROBLEM_CONTENT_TYPE),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

/// True when `response` already carries a problem document.
pub fn is_problem(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes() == PROBLEM_CONTENT_TYPE.as_bytes())
}

/// `map_response` backstop: an error that is not already a problem becomes
/// one for the same status, keeping headers such as `Allow` but none of the
/// original body.
pub async fn layer(response: Response) -> Response {
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) || is_problem(&response) {
        return response;
    }
    let kind = match status.as_u16() {
        400 | 422 => ProblemType::MalformedRequest,
        other => ProblemType::classify(other, None),
    };
    let mut replaced = typed(kind, status, framework_detail(status));
    for (name, value) in response.headers() {
        if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
            replaced.headers_mut().append(name.clone(), value.clone());
        }
    }
    replaced
}

fn framework_detail(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 422 => "Request is invalid",
        404 => "Not found",
        405 => "Method not allowed",
        413 => "Request body too large",
        414 => "Request target too long",
        415 => "Unsupported media type",
        _ if status.is_client_error() => "Request refused",
        _ => "Internal Server Error",
    }
}

/// A problem body without its RFC 9457 envelope (`type`, `title`, `status`),
/// for comparing with Node's `{detail, code}` bodies. `body` comes back
/// unchanged unless the envelope is consistent: a known type, that type's
/// title, and a numeric status.
pub fn without_envelope(mut body: Value) -> Value {
    let Some(members) = body.as_object_mut() else {
        return body;
    };
    let kind = members
        .get("type")
        .and_then(|kind| serde_json::from_value::<ProblemType>(kind.clone()).ok());
    let consistent = kind.is_some_and(|kind| {
        members.get("title").and_then(Value::as_str) == Some(kind.title())
            && members.get("status").is_some_and(Value::is_u64)
    });
    if consistent {
        for key in ["type", "title", "status"] {
            members.remove(key);
        }
    }
    body
}

/// A writer-admission failure that storage reports as a plain message rather
/// than a typed error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The writer queue stayed full past the wait budget: retry later.
    Full,
    /// The writer is stopping or refused admission: unavailable.
    Stopped,
}

/// Classifies storage's plain writer-admission messages anywhere in the error
/// chain. Compared, never echoed.
pub fn writer_admission(error: &anyhow::Error) -> Option<Admission> {
    error
        .chain()
        .find_map(|cause| match cause.to_string().as_str() {
            "Writer queue full" => Some(Admission::Full),
            "Cancelled before admission" | "Writer admission poisoned" => Some(Admission::Stopped),
            message if message.starts_with("Storage stopped") => Some(Admission::Stopped),
            _ => None,
        })
}

/// 503 for a plain writer-admission message: `overloaded` when the queue is
/// full, `unavailable` when the writer is stopping. `None` for anything else.
pub fn admission_response(error: &anyhow::Error, detail: &str) -> Option<Response> {
    Some(match writer_admission(error)? {
        Admission::Full => overloaded(detail),
        Admission::Stopped => response(StatusCode::SERVICE_UNAVAILABLE, detail),
    })
}

/// True when `bytes` is a JSON object: a readable body, so a refusal of it is a
/// validation problem, not a malformed request.
pub fn is_json_object(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes).is_ok_and(|body| body.is_object())
}

/// Parses a JSON request body. On failure returns the field errors: one naming
/// the failing field, or none when no field is to blame.
pub fn parse_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Vec<FieldError>> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let error = match serde_path_to_error::deserialize::<_, T>(&mut decoder) {
        Ok(value) => match decoder.end() {
            Ok(()) => return Ok(value),
            Err(_) => return Err(Vec::new()),
        },
        Err(error) => error,
    };
    Err(field_errors(&error))
}

/// The field error for a serde failure at a tracked path. None for a syntax
/// error, a wrong top-level shape (`null`, `[]`), or a pointer with no whole
/// token under [`MAX_POINTER_BYTES`].
pub fn field_errors(error: &serde_path_to_error::Error<serde_json::Error>) -> Vec<FieldError> {
    if !error.inner().is_data() {
        return Vec::new();
    }
    let mut pointer = String::new();
    for segment in error.path().iter() {
        use serde_path_to_error::Segment;
        pointer.push('/');
        match segment {
            Segment::Seq { index } => pointer.push_str(&index.to_string()),
            Segment::Map { key } => push_token(&mut pointer, key),
            Segment::Enum { variant } => push_token(&mut pointer, variant),
            Segment::Unknown => pointer.push('-'),
        }
    }
    let message = error.inner().to_string();
    let named = |prefix: &str| -> Option<String> {
        message
            .strip_prefix(prefix)
            .and_then(|rest| rest.split('`').next())
            .map(str::to_owned)
    };
    // The path already ends at an unknown field, but stops at a missing
    // field's parent.
    let detail = if let Some(field) = named("missing field `") {
        pointer.push('/');
        push_token(&mut pointer, &field);
        "is required"
    } else if message.starts_with("unknown field `") {
        "is not allowed"
    } else {
        "is invalid"
    };
    let error = FieldError::new(&pointer, detail);
    if error.pointer.is_empty() {
        return Vec::new();
    }
    vec![error]
}

/// Appends an RFC 6901 reference token.
fn push_token(pointer: &mut String, token: &str) {
    pointer.push_str(&token.replace('~', "~0").replace('/', "~1"));
}

#[cfg(test)]
#[path = "problem/tests/canary.rs"]
mod canary;
#[cfg(test)]
#[path = "problem/tests.rs"]
mod tests;
