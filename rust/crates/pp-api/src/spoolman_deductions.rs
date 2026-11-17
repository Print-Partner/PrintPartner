//! Spoolman deduction routes. Rust-only runtime: they mount only when
//! `PP_DESKTOP_REBUILD` is on.
//!
//! `GET /api/v2/spoolman/deductions` lists the rows Checkoff recorded, newest
//! first, at most 100. Filters: `state` and `link_id`.
//! `GET /api/v2/spoolman/deductions/{id}` adds the grams Spoolman reported
//! before the send and the grams it reports now.
//! `POST /api/v2/spoolman/deductions/{id}/reconcile` answers `deducted`,
//! `retry` or `dismiss`. It does not call Spoolman. `retry` returns the row
//! to `pending` and the send worker makes one more call.
//!
//! Storage owns the rows and answers `DeductionError`; this module maps it to
//! a status code and a problem body, once.

use crate::accepted::{AcceptedHttpConfig, Gate, internal, ok_json};
use crate::loopback::PresentedCredential;
use crate::problem::{self, ProblemType};
use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State as AxumState},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use pp_storage::{
    read_model::stock::SpoolmanIntegration,
    spoolman_deductions::{
        Answer, Deduction, DeductionError, Failure, Filter, GramsBasis, SkipReason,
        SpoolmanDeductionsClient, State,
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

pub mod code {
    pub const INVALID_FILTER: &str = "invalid_deduction_filter";
    pub const DEDUCTION_UNREADABLE: &str = "deduction_unreadable";
    pub const DEDUCTION_NOT_FOUND: &str = "deduction_not_found";
    pub const DEDUCTION_STATE: &str = "deduction_state";
}

/// Largest integer this API states. A larger id misses, and a larger gram
/// report reads as unknown.
const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
const BODY_LIMIT: usize = 4 * 1024;

/// Reads the grams one spool reports now. `None` means the read failed.
/// Implementations only read: they never reserve or deduct stock.
pub trait SpoolGrams: Send + Sync {
    fn remaining(
        &self,
        integration: SpoolmanIntegration,
        spool_id: i64,
    ) -> Pin<Box<dyn Future<Output = Option<f64>> + Send>>;
}

/// The longest `link_id` filter; stored link ids are far shorter.
const MAX_LINK_ID: usize = 200;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DeductionList {
    pub deductions: Vec<DeductionView>,
}

/// One recorded deduction. `integration_id`, `spool_id`, `grams` and
/// `grams_basis` are null exactly when `state` is `skipped`, and `reason` is
/// set exactly then. `failure` is set only on `failed`, `unknown` and
/// `dismissed`. `failure_status` is set only when `failure` is `refused`.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DeductionView {
    pub id: i64,
    /// `pending`, `sending`, `applied`, `unknown`, `failed`, `skipped` or
    /// `dismissed`.
    pub state: &'static str,
    /// `no_grams`, `no_pinned_spool`, `integration_missing`,
    /// `integration_disabled`, `integrations_unreadable` or
    /// `module_setting_unreadable`.
    pub reason: Option<&'static str>,
    pub link_id: String,
    /// The Build.
    pub profile_id: i64,
    /// The burst's confirmed units this row covers.
    pub units: u32,
    pub integration_id: Option<String>,
    pub spool_id: Option<i64>,
    /// Grams to deduct, rounded up to a whole gram, as every grams field
    /// of this API is whole. The deduction itself uses the exact value.
    pub grams: Option<u64>,
    /// `estimated` (slicer) or `measured` (at print finish).
    pub grams_basis: Option<&'static str>,
    /// `unreachable`, `refused`, `reply_lost`, `interrupted`,
    /// `integration_missing` or `integration_disabled`.
    pub failure: Option<&'static str>,
    /// HTTP status from 100 to 599. Set only when `failure` is `refused`.
    pub failure_status: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// One deduction plus the grams Spoolman reported before the send and reports
/// now. Each is whole grams, rounded down, or `unknown`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DeductionDetail {
    pub deduction: DeductionView,
    pub grams_before: ReportedGrams,
    pub grams_now: ReportedGrams,
}

/// Grams from Spoolman, or `unknown` when that number was not stored or the
/// read failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportedGrams {
    Known { grams: u64 },
    Unknown,
}

/// `POST /api/v2/spoolman/deductions/{id}/reconcile`.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReconcileDeductionRequest {
    pub answer: ReconcileAnswer,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReconcileAnswer {
    Deducted,
    Retry,
    Dismiss,
}

impl From<Deduction> for DeductionView {
    fn from(row: Deduction) -> Self {
        let target = row.target;
        Self {
            id: row.id,
            state: row.state.as_str(),
            reason: row.reason.map(SkipReason::as_str),
            link_id: row.link_id,
            profile_id: row.profile_id,
            units: row.units,
            integration_id: target.as_ref().map(|t| t.integration_id.clone()),
            spool_id: target.as_ref().map(|t| t.spool_id),
            grams: target.as_ref().map(|t| t.grams.ceil() as u64),
            grams_basis: target.as_ref().map(|t| GramsBasis::as_str(t.basis)),
            failure: row.failure.map(Failure::as_str),
            failure_status: row.failure_status,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: SpoolmanDeductionsClient,
    grams: Arc<dyn SpoolGrams>,
}

/// Mounts the list, read and reconcile routes under the accepted gate when
/// `PP_DESKTOP_REBUILD` was exactly `1` or `true`; otherwise nothing.
pub fn spoolman_deductions_router(
    config: AcceptedHttpConfig,
    client: SpoolmanDeductionsClient,
    grams: Arc<dyn SpoolGrams>,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new()
        .route("/api/v2/spoolman/deductions", get(list))
        .route("/api/v2/spoolman/deductions/{id}", get(read_one))
        .route(
            "/api/v2/spoolman/deductions/{id}/reconcile",
            post(reconcile),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    gate.guard(routes).with_state(App {
        gate,
        client,
        grams,
    })
}

/// The filter the query names, or `None` for a query this route refuses.
fn filter(query: &HashMap<String, String>) -> Option<Filter> {
    if query.keys().any(|key| key != "state" && key != "link_id") {
        return None;
    }
    let state = match query.get("state") {
        Some(text) => Some(State::parse(text)?),
        None => None,
    };
    let link_id = match query.get("link_id") {
        Some(id) if id.is_empty() || id.chars().count() > MAX_LINK_ID => return None,
        other => other.cloned(),
    };
    Some(Filter { state, link_id })
}

async fn list(
    AxumState(app): AxumState<App>,
    Extension(presented): Extension<PresentedCredential>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(filter) = filter(&query) else {
        return problem::coded(
            StatusCode::BAD_REQUEST,
            "state must be a deduction state and link_id 1 to 200 characters; no other filter is known",
            code::INVALID_FILTER,
        );
    };
    let client = app.client.clone();
    let done = app.gate.blocking(move |cancelled, wait| {
        client.list(presented.into_credential(), filter, &cancelled, wait)
    });
    match done.await {
        Ok(Ok(rows)) => ok_json(&DeductionList {
            deductions: rows.into_iter().map(DeductionView::from).collect(),
        }),
        Ok(Err(error)) => refused(&error),
        Err(_) => internal(),
    }
}

async fn read_one(
    AxumState(app): AxumState<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    let id = deduction_id(&id);
    let client = app.client.clone();
    let loaded = app.gate.blocking(move |cancelled, wait| {
        client.get(presented.into_credential(), id, &cancelled, wait)
    });
    let record = match loaded.await {
        Ok(Ok(Some(record))) => record,
        Ok(Ok(None)) => return not_found(),
        Ok(Err(error)) => return refused(&error),
        Err(_) => return internal(),
    };
    let grams_before = reported(record.deduction.spool_grams_before);
    let spool_id = record
        .deduction
        .target
        .as_ref()
        .map(|target| target.spool_id);
    let grams_now = match (record.integration, spool_id) {
        (Some(integration), Some(spool_id)) => {
            reported(app.grams.remaining(integration, spool_id).await)
        }
        _ => ReportedGrams::Unknown,
    };
    ok_json(&DeductionDetail {
        deduction: record.deduction.into(),
        grams_before,
        grams_now,
    })
}

async fn reconcile(
    AxumState(app): AxumState<App>,
    Extension(presented): Extension<PresentedCredential>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Ok(request) = serde_json::from_slice::<ReconcileDeductionRequest>(&body) else {
        return problem::typed(
            ProblemType::MalformedRequest,
            StatusCode::BAD_REQUEST,
            "Send {\"answer\": \"deducted\"}, \"retry\" or \"dismiss\"",
        );
    };
    let id = deduction_id(&id);
    let answer = match request.answer {
        ReconcileAnswer::Deducted => Answer::Deducted,
        ReconcileAnswer::Retry => Answer::Retry,
        ReconcileAnswer::Dismiss => Answer::Dismiss,
    };
    let client = app.client.clone();
    let done = app.gate.blocking(move |cancelled, wait| {
        client.reconcile(presented.into_credential(), id, answer, &cancelled, wait)
    });
    match done.await {
        Ok(Ok(row)) => ok_json(&DeductionView::from(row)),
        Ok(Err(error)) => refused(&error),
        Err(_) => internal(),
    }
}

/// A positive safe-integer id. Anything else still authenticates, then misses.
fn deduction_id(text: &str) -> i64 {
    let Ok(id) = text.parse::<i64>() else {
        return 0;
    };
    if text.bytes().all(|byte| byte.is_ascii_digit()) && (1..=MAX_SAFE as i64).contains(&id) {
        id
    } else {
        0
    }
}

/// Whole grams, rounded down. A missing, negative or non-finite report is
/// unknown, and so is a number past the JavaScript safe integer range.
fn reported(grams: Option<f64>) -> ReportedGrams {
    match grams {
        Some(grams) if grams.is_finite() && (0.0..=MAX_SAFE).contains(&grams) => {
            ReportedGrams::Known {
                grams: grams.floor() as u64,
            }
        }
        _ => ReportedGrams::Unknown,
    }
}

fn not_found() -> Response {
    problem::coded(
        StatusCode::NOT_FOUND,
        "Deduction not found",
        code::DEDUCTION_NOT_FOUND,
    )
}

fn refused(error: &DeductionError) -> Response {
    match error {
        DeductionError::NotFound => not_found(),
        DeductionError::StateConflict => problem::coded(
            StatusCode::CONFLICT,
            "This deduction's state does not accept that answer",
            code::DEDUCTION_STATE,
        ),
        DeductionError::StoredRowInvalid { .. } => problem::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "A stored deduction is unreadable",
            code::DEDUCTION_UNREADABLE,
        ),
        DeductionError::GramsBeforeNotFinite | DeductionError::StatusOutOfRange => internal(),
        DeductionError::Writer(failure) => problem::writer_failure(failure),
    }
}

#[cfg(test)]
mod tests {
    use super::filter;
    use pp_storage::spoolman_deductions::State;
    use std::collections::HashMap;

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn filters_name_a_known_state_and_a_bounded_link() {
        let parsed = filter(&query(&[("state", "pending"), ("link_id", "link-1")])).unwrap();
        assert_eq!(
            (parsed.state, parsed.link_id.as_deref()),
            (Some(State::Pending), Some("link-1"))
        );
        assert_eq!(filter(&query(&[])).unwrap(), Default::default());
        // The bound counts characters: 200 three-byte characters pass.
        let wide = "€".repeat(200);
        assert!(filter(&query(&[("link_id", wide.as_str())])).is_some());
        let long = "x".repeat(201);
        for refused in [
            query(&[("state", "done")]),
            query(&[("link_id", "")]),
            query(&[("link_id", long.as_str())]),
            query(&[("spool", "7")]),
        ] {
            assert!(filter(&refused).is_none(), "{refused:?}");
        }
    }

    #[test]
    fn reported_grams_round_down_and_a_bad_report_is_unknown() {
        use super::{ReportedGrams, reported};
        assert_eq!(reported(Some(512.4)), ReportedGrams::Known { grams: 512 });
        assert_eq!(reported(Some(492.4)), ReportedGrams::Known { grams: 492 });
        assert_eq!(reported(Some(0.0)), ReportedGrams::Known { grams: 0 });
        for grams in [None, Some(-0.1), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_eq!(reported(grams), ReportedGrams::Unknown);
        }
    }
}
