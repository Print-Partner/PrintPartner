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
use std::sync::{Arc, Mutex};

/// Implementations must apply the existing Scrubber before returning records.
pub trait LogRecords: Send + Sync {
    fn records(&self) -> std::io::Result<Vec<Value>>;
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
    min_severity: Option<Severity>,
    enable_workflow_tracking: Option<bool>,
}
struct View {
    config: Config,
    cleared: Vec<Value>,
}
#[derive(Clone)]
struct App {
    source: Arc<dyn LogRecords>,
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
        view: Arc::new(Mutex::new(View {
            config: Config {
                min_severity: Severity::Info,
                max_logs: 1000,
                enable_workflow_tracking: true,
            },
            cleared: Vec::new(),
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
    update: Result<Json<Update>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(update)) = update else {
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
/// File I/O runs outside the async executor. Clear remembers the current tail;
/// overlap with its last row identifies the visible suffix after later appends.
async fn snapshot(app: &App, clear: bool) -> Result<Vec<Value>, Response> {
    let source = app.source.clone();
    let rows = tokio::task::spawn_blocking(move || source.records())
        .await
        .map_err(|_| failed())?
        .map_err(|_| failed())?;
    let mut view = app.view.lock().map_err(|_| failed())?;
    if clear {
        view.cleared = rows;
        return Ok(Vec::new());
    }
    let start = (1..=view.cleared.len().min(rows.len()))
        .rev()
        .find(|&n| view.cleared[view.cleared.len() - n..] == rows[..n])
        .unwrap_or(0);
    Ok(rows
        .into_iter()
        .skip(start)
        .filter(|row| {
            view.config.enable_workflow_tracking && Severity::of(row) >= view.config.min_severity
        })
        .collect())
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
        Err(error) => return error,
    };
    rows.retain(|row| {
        filter.severity.is_none_or(|s| Severity::of(row) == s)
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
        Err(error) => error,
    }
}
fn summary(rows: &[Value]) -> Value {
    let mut severity = json!({"debug":0,"info":0,"warn":0,"error":0});
    let mut methods = serde_json::Map::new();
    let mut duration = 0.0;
    for row in rows {
        let key = Severity::of(row).name();
        severity[key] = json!(severity[key].as_u64().unwrap_or(0) + 1);
        if let Some(method) = row["method"].as_str() {
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
        Err(error) => error,
    }
}
async fn metrics(State(app): State<App>) -> Response {
    let rows = match snapshot(&app, false).await {
        Ok(rows) => rows,
        Err(error) => return error,
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
