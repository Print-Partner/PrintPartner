//! Authenticated diagnostics over a bounded, scrubbed snapshot of core.log.
use crate::{
    accepted::failure,
    admin::{AdminHttpConfig, Gate, response},
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::Response,
    routing::get,
};
use pp_storage::read_model::ReadClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// One scrubbed row and its stable cursor.
#[derive(Clone)]
pub struct LogRecord {
    pub file: String,
    /// Keeps this identity from being reused while a clear cursor exists.
    pub retained_file: Arc<dyn Send + Sync>,
    pub offset: u64,
    pub value: Value,
}
/// Implementations must apply the unchanged Scrubber before returning records.
pub trait LogRecords: Send + Sync {
    fn records(&self) -> std::io::Result<Vec<LogRecord>>;
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    min_severity: Severity,
    max_logs: usize,
    enable_workflow_tracking: bool,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Debug,
    Info,
    Warn,
    Error,
}
impl Severity {
    fn of(row: &Value) -> Self {
        match row["level"].as_u64().unwrap_or(30) {
            0..=20 => Self::Debug,
            21..=30 => Self::Info,
            31..=40 => Self::Warn,
            _ => Self::Error,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Update {
    #[serde(default, deserialize_with = "present")]
    min_severity: Option<Severity>,
    #[serde(default, deserialize_with = "present")]
    enable_workflow_tracking: Option<bool>,
}
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
struct Cursor {
    offset: u64,
    _file: Arc<dyn Send + Sync>,
}
struct View {
    config: Config,
    cleared: HashMap<String, Cursor>,
}
#[derive(Clone)]
struct App {
    source: Arc<dyn LogRecords>,
    clear: Arc<tokio::sync::Mutex<()>>,
    view: Arc<Mutex<View>>,
}

pub fn diagnostics_router(
    config: AdminHttpConfig,
    client: ReadClient,
    source: Arc<dyn LogRecords>,
) -> Router {
    let Some(gate) = Gate::new(config, client) else {
        return Router::new();
    };
    let app = App {
        source,
        clear: Arc::new(tokio::sync::Mutex::new(())),
        view: Arc::new(Mutex::new(View {
            config: Config {
                min_severity: Severity::Info,
                max_logs: 1000,
                enable_workflow_tracking: true,
            },
            cleared: HashMap::new(),
        })),
    };
    let routes = Router::new()
        .route(
            "/settings/logging/config",
            get(config_get).post(config_post),
        )
        .route("/settings/logging/logs", get(logs).delete(clear))
        .route("/settings/logging/stats", get(stats))
        .route("/metrics", get(metrics));
    gate.guard(Router::new().merge(routes.clone()).nest("/api/v1", routes))
        .with_state(app)
}
fn reply(value: Value) -> Response {
    response(
        StatusCode::OK,
        "application/json; charset=utf-8",
        value.to_string(),
    )
}
fn failed() -> Response {
    failure(StatusCode::INTERNAL_SERVER_ERROR, "Diagnostics unavailable")
}
async fn config_get(State(app): State<App>) -> Response {
    match app.view.lock() {
        Ok(view) => reply(json!(view.config)),
        Err(_) => failed(),
    }
}
async fn config_post(
    State(app): State<App>,
    update: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(update)) = update else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    if !update.is_object() {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    }
    let Ok(update) = serde_json::from_value::<Update>(update) else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    let Ok(mut view) = app.view.lock() else {
        return failed();
    };
    if let Some(value) = update.min_severity {
        view.config.min_severity = value;
    }
    if let Some(value) = update.enable_workflow_tracking {
        view.config.enable_workflow_tracking = value;
    }
    reply(json!(view.config))
}
/// Clear retains per-file high-water offsets across renames and bounded reads.
async fn snapshot(app: &App, clear: bool) -> Result<Vec<Value>, ()> {
    // Concurrent clears must not publish older cutoffs after newer ones.
    let _clear = if clear {
        Some(app.clear.lock().await)
    } else {
        None
    };
    let source = app.source.clone();
    let rows = tokio::task::spawn_blocking(move || source.records())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    let mut view = app.view.lock().map_err(|_| ())?;
    if clear {
        view.cleared.clear();
        for row in rows {
            view.cleared
                .entry(row.file)
                .and_modify(|n| n.offset = n.offset.max(row.offset))
                .or_insert(Cursor {
                    offset: row.offset,
                    _file: row.retained_file,
                });
        }
        return Ok(Vec::new());
    }
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter(|row| row.offset > view.cleared.get(&row.file).map_or(0, |n| n.offset))
        .filter(|row| {
            view.config.enable_workflow_tracking
                && Severity::of(&row.value) >= view.config.min_severity
        })
        .map(project)
        .collect();
    let start = rows.len().saturating_sub(1000);
    rows.drain(..start);
    Ok(rows)
}
/// The existing settings UI consumes WorkflowLog. Non-HTTP events use an
/// empty method/URL and zero duration/status, with original fields in context.
fn project(row: LogRecord) -> Value {
    let value = row.value;
    let timestamp = value["time"]
        .as_i64()
        .and_then(|n| time::OffsetDateTime::from_unix_timestamp(n).ok())
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    json!({"id":format!("{}:{}", row.file, row.offset), "timestamp":timestamp,
        "method":value["method"].as_str().unwrap_or(""), "url":value["url"].as_str().unwrap_or(""),
        "duration":value["duration"].as_f64().unwrap_or(0.0), "statusCode":value["statusCode"].as_u64().unwrap_or(0),
        "severity":Severity::of(&value).name(), "message":value["message"].as_str().unwrap_or(""), "context":value})
}

#[derive(Default, Deserialize)]
struct Filter {
    severity: Option<Severity>,
    method: Option<String>,
    limit: Option<usize>,
}
async fn logs(
    State(app): State<App>,
    query: Result<Query<Filter>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(filter)) = query else {
        return failure(StatusCode::BAD_REQUEST, "Request is invalid");
    };
    let mut rows = match snapshot(&app, false).await {
        Ok(rows) => rows,
        Err(_) => return failed(),
    };
    rows.retain(|row| {
        filter
            .severity
            .is_none_or(|s| row["severity"].as_str() == Some(s.name()))
            && filter
                .method
                .as_ref()
                .is_none_or(|m| row["method"].as_str() == Some(m))
    });
    let start = rows
        .len()
        .saturating_sub(filter.limit.unwrap_or(100).min(1000));
    reply(json!(&rows[start..]))
}
async fn clear(State(app): State<App>) -> Response {
    match snapshot(&app, true).await {
        Ok(_) => reply(json!({"success": true})),
        Err(_) => failed(),
    }
}
fn summary(rows: &[Value]) -> Value {
    let mut severity = json!({"debug":0,"info":0,"warn":0,"error":0});
    let mut methods = serde_json::Map::new();
    let mut duration = 0.0;
    for row in rows {
        let key = row["severity"].as_str().unwrap_or("info");
        severity[key] = json!(severity[key].as_u64().unwrap_or(0) + 1);
        if let Some(method) = row["method"].as_str().filter(|s| !s.is_empty()) {
            let count = methods.entry(method).or_insert(json!(0));
            *count = json!(count.as_u64().unwrap_or(0) + 1);
        }
        duration += row["duration"].as_f64().unwrap_or(0.0);
    }
    json!({"totalLogs":rows.len(), "byMethod":methods,"bySeverity":severity,
        "avgDuration":if rows.is_empty() {0.0} else {duration / rows.len() as f64}, "errorCount":severity["error"]})
}
async fn stats(State(app): State<App>) -> Response {
    match snapshot(&app, false).await {
        Ok(rows) => reply(summary(&rows)),
        Err(_) => failed(),
    }
}
async fn metrics(State(app): State<App>) -> Response {
    let rows = match snapshot(&app, false).await {
        Ok(rows) => rows,
        Err(_) => return failed(),
    };
    let summary = summary(&rows);
    let mut body = String::from(
        "# HELP core_log_records Retained visible core log records\n# TYPE core_log_records gauge\n",
    );
    for level in ["debug", "info", "warn", "error"] {
        body.push_str(&format!(
            "core_log_records{{severity=\"{level}\"}} {}\n",
            summary["bySeverity"][level]
        ));
    }
    body.push_str(&format!("# HELP app_info Application build info\n# TYPE app_info gauge\napp_info{{version=\"{}\",runtime=\"rust\"}} 1\n", env!("CARGO_PKG_VERSION")));
    response(
        StatusCode::OK,
        "text/plain; version=0.0.4; charset=utf-8",
        body,
    )
}
