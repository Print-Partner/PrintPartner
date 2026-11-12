//! `GET`/`PUT` `/settings/date-format`, `/settings/build-tracking` and
//! `/settings/spoolman-default`, as Node's `routes/settings.ts` serves them
//! (GRE-1357). `fixtures/admin/settings-oracle.json` records Node's replies.

mod secrets;

use crate::accepted::{failure, read_error};
use crate::admin::{AdminHttpConfig, Gate, response};
use crate::loopback::PresentedCredential;
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::get,
};
use pp_storage::{
    app_settings::{AppSettingsClient, Key, Outcome, Write},
    build_graph::variants::json_body_refused,
    read_model::ReadClient,
};
use serde_json::{Value, json};
use std::{sync::atomic::AtomicBool, time::Duration};

/// `DATE_FORMAT_PRESETS` ids in `@print-partner/contracts`.
const DATE_FORMATS: [&str; 6] = [
    "mdy_12h",
    "dmy_12h",
    "mdy_short",
    "dmy_short",
    "ymd_24h",
    "iso",
];
/// `DATE_FORMAT_DEFAULT`.
const DATE_FORMAT_DEFAULT: &str = "mdy_12h";
/// Fastify's default `bodyLimit`.
const BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
struct App {
    settings: AppSettingsClient,
    wait: Duration,
}

/// Mounts the supported settings at `/` and `/api/v1` when `PP_DESKTOP_REBUILD`
/// was exactly `1` or `true`; otherwise nothing. The admin gate proves the
/// credential first, as Node's auth hook runs before each handler.
pub fn settings_router(
    config: AdminHttpConfig,
    client: ReadClient,
    settings: AppSettingsClient,
) -> Router {
    let wait = config.wait;
    let Some(gate) = Gate::new(config, client) else {
        return Router::new();
    };
    let routes = Router::new()
        .merge(secrets::routes())
        .route(
            "/settings/date-format",
            get(get_date_format).put(put_date_format),
        )
        .route(
            "/settings/build-tracking",
            get(get_build_tracking).put(put_build_tracking),
        )
        .route(
            "/settings/spoolman-default",
            get(get_spoolman_default).put(put_spoolman_default),
        );
    gate.guard(
        Router::new()
            .merge(routes.clone())
            .nest("/api/v1", routes)
            .layer(DefaultBodyLimit::max(BODY_LIMIT)),
    )
    .with_state(App { settings, wait })
}

async fn get_date_format(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    match read(&app, presented, Key::DateFormat).await {
        Ok(value) => ok(&json!({ "format": value.as_deref().unwrap_or(DATE_FORMAT_DEFAULT) })),
        Err(refused) => *refused,
    }
}

async fn put_date_format(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let format = match field(&headers, &body, "format") {
        Ok(Value::String(format)) if DATE_FORMATS.contains(&format.as_str()) => format,
        Ok(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "format must be one of the supported date formats",
            );
        }
        Err(refused) => return *refused,
    };
    match write(&app, presented, Write::DateFormat(format.clone())).await {
        Ok(_) => ok(&json!({ "format": format })),
        Err(refused) => *refused,
    }
}

async fn get_build_tracking(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    match read(&app, presented, Key::BuildTracking).await {
        Ok(value) => ok(&json!({ "assembly_tracking": value.is_some_and(|value| value != "0") })),
        Err(refused) => *refused,
    }
}

/// Node stores any truthy `assembly_tracking` as `"1"` but answers `true`
/// only for the boolean `true`.
async fn put_build_tracking(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let value = match field(&headers, &body, "assembly_tracking") {
        Ok(value) => value,
        Err(refused) => return *refused,
    };
    match write(&app, presented, Write::BuildTracking(truthy(&value))).await {
        Ok(_) => ok(&json!({ "assembly_tracking": value == Value::Bool(true) })),
        Err(refused) => *refused,
    }
}

async fn get_spoolman_default(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
) -> Response {
    match read(&app, presented, Key::SpoolmanDefault).await {
        Ok(value) => ok(&json!({ "integration_id": value })),
        Err(refused) => *refused,
    }
}

/// A non-string id clears the default, as Node's `trimmedString` reads it as
/// empty. Only the named integration's `type` is read.
async fn put_spoolman_default(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let id = match field(&headers, &body, "integration_id") {
        Ok(Value::String(id)) => js_trim(&id).to_owned(),
        Ok(_) => String::new(),
        Err(refused) => return *refused,
    };
    match write(&app, presented, Write::SpoolmanDefault(id.clone())).await {
        Ok(Outcome::NotSpoolman) => failure(
            StatusCode::BAD_REQUEST,
            "integration_id must be a Spoolman connector",
        ),
        Ok(_) => ok(&json!({ "integration_id": (!id.is_empty()).then_some(id) })),
        Err(refused) => *refused,
    }
}

/// `request.body[name]` as Node's handler sees it, `Null` when absent. Fastify
/// refuses an empty or unparsable JSON body, reads `text/plain` as a string,
/// and leaves the body `undefined` without a content type. Rust refuses a
/// `null` or absent body as malformed before admitting a write.
fn field(headers: &HeaderMap, body: &[u8], name: &str) -> Result<Value, Box<Response>> {
    match body_value(headers, body)? {
        Value::Object(members) => Ok(members.get(name).cloned().unwrap_or(Value::Null)),
        _ => Ok(Value::Null),
    }
}

fn body_value(headers: &HeaderMap, body: &[u8]) -> Result<Value, Box<Response>> {
    let media = headers.get(header::CONTENT_TYPE).map(|value| {
        let essence = value.to_str().unwrap_or_default().split(';').next();
        essence.unwrap_or_default().trim().to_ascii_lowercase()
    });
    let parsed = match media.as_deref() {
        Some("application/json") => {
            if body.is_empty() || json_body_refused(body) {
                return Err(Box::new(unreadable()));
            }
            serde_json::from_slice(body).map_err(|_| Box::new(unreadable()))?
        }
        Some("text/plain") => Value::String(String::new()),
        None if body.is_empty() => Value::Null,
        _ => {
            return Err(Box::new(failure(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported media type",
            )));
        }
    };
    match parsed {
        Value::Null => Err(Box::new(unreadable())),
        value => Ok(value),
    }
}

fn unreadable() -> Response {
    crate::problem::typed(
        crate::problem::ProblemType::MalformedRequest,
        StatusCode::BAD_REQUEST,
        "Request is invalid",
    )
}

/// JavaScript truthiness of a parsed JSON value: false only for `null`,
/// `false`, `0` and `""`.
fn truthy(value: &Value) -> bool {
    !(value.is_null()
        || value == &Value::Bool(false)
        || value.as_f64() == Some(0.0)
        || value.as_str() == Some(""))
}

/// `String.prototype.trim`: Unicode white space and line terminators, plus
/// U+FEFF but not U+0085.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

async fn read(
    app: &App,
    presented: PresentedCredential,
    key: Key,
) -> Result<Option<String>, Box<Response>> {
    match read_outcome(app, presented, key).await? {
        Outcome::Value(value) => Ok(value),
        _ => Err(Box::new(unreadable())),
    }
}

async fn read_outcome(
    app: &App,
    presented: PresentedCredential,
    key: Key,
) -> Result<Outcome, Box<Response>> {
    let client = app.settings.clone();
    let wait = app.wait;
    let outcome = tokio::task::spawn_blocking(move || {
        client.read(
            presented.into_credential(),
            key,
            &AtomicBool::new(false),
            wait,
        )
    })
    .await;
    settled(outcome)
}

async fn write(
    app: &App,
    presented: PresentedCredential,
    change: Write,
) -> Result<Outcome, Box<Response>> {
    let client = app.settings.clone();
    let wait = app.wait;
    let outcome = tokio::task::spawn_blocking(move || {
        client.write(
            presented.into_credential(),
            change,
            &AtomicBool::new(false),
            wait,
        )
    })
    .await;
    settled(outcome)
}

/// A full queue or stopped writer, or a write lock past its admission
/// deadline, answers 503; credential failures keep `read_error`'s mapping.
fn settled(
    outcome: Result<anyhow::Result<Outcome>, tokio::task::JoinError>,
) -> Result<Outcome, Box<Response>> {
    const BUSY: &str = "Service temporarily unavailable";
    match outcome {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => {
            if let Some(refused) = crate::problem::admission_response(&error, BUSY) {
                return Err(Box::new(refused));
            }
            let locked = error.chain().any(|cause| {
                matches!(
                    cause.to_string().as_str(),
                    "Writer transaction admission deadline expired" | "database is locked"
                )
            });
            Err(Box::new(if locked {
                crate::problem::coded(
                    StatusCode::SERVICE_UNAVAILABLE,
                    BUSY,
                    "transaction_unavailable",
                )
            } else {
                read_error(&error)
            }))
        }
        Err(_) => Err(Box::new(failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
        ))),
    }
}

fn ok(body: &Value) -> Response {
    response(
        StatusCode::OK,
        "application/json; charset=utf-8",
        body.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{js_trim, settled, truthy};
    use serde_json::json;

    #[test]
    fn settings_writer_refusals_answer_503() {
        for message in [
            "Writer transaction admission deadline expired",
            "database is locked",
            "Storage stopped",
            "Writer queue full",
        ] {
            let refused = settled(Ok(Err(anyhow::anyhow!(message)))).unwrap_err();
            assert_eq!(refused.status().as_u16(), 503, "{message}");
        }
        let refused = settled(Ok(Err(anyhow::anyhow!("Authentication required")))).unwrap_err();
        assert_eq!(refused.status().as_u16(), 401);
    }

    #[test]
    fn settings_body_values_follow_javascript() {
        assert_eq!(js_trim("\u{feff} sp1 \n\u{3000}"), "sp1");
        assert_eq!(js_trim("\u{85}sp1"), "\u{85}sp1");
        for value in [
            json!(true),
            json!(1),
            json!(-0.5),
            json!("0"),
            json!([]),
            json!({}),
        ] {
            assert!(truthy(&value), "{value}");
        }
        for value in [json!(null), json!(false), json!(0), json!(-0.0), json!("")] {
            assert!(!truthy(&value), "{value}");
        }
    }
}
