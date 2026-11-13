//! `GET /api/v2/spoolman/deductions`: the Spoolman deductions Checkoff bursts
//! recorded, newest first, at most 100. Filters: `state` and `link_id`.
//! Rust-only runtime: the route mounts only when `PP_DESKTOP_REBUILD` is on.
//!
//! Storage owns the rows and answers `DeductionError`; this module maps it to
//! a status code and a problem body, once.

use crate::accepted::{AcceptedHttpConfig, Gate, internal, ok_json};
use crate::loopback::PresentedCredential;
use crate::problem;
use axum::{
    Extension, Router,
    extract::{Query, State as AxumState},
    http::StatusCode,
    response::Response,
    routing::get,
};
use pp_storage::spoolman_deductions::{
    Deduction, DeductionError, Filter, GramsBasis, SkipReason, SpoolmanDeductionsClient, State,
};
use serde::Serialize;
use std::collections::HashMap;

pub mod code {
    pub const INVALID_FILTER: &str = "invalid_deduction_filter";
    pub const DEDUCTION_UNREADABLE: &str = "deduction_unreadable";
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
/// set exactly then.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DeductionView {
    pub id: i64,
    /// `pending`, `sending`, `applied`, `unknown`, `failed`, `skipped` or
    /// `dismissed`.
    pub state: &'static str,
    /// `no_grams`, `no_pinned_spool`, `integration_missing`,
    /// `integration_disabled` or `integrations_unreadable`.
    pub reason: Option<&'static str>,
    pub link_id: String,
    /// The Build.
    pub profile_id: i64,
    /// The burst's confirmed units this row covers.
    pub units: u32,
    pub integration_id: Option<String>,
    pub spool_id: Option<i64>,
    /// Grams to deduct, rounded to the nearest gram, as every grams field
    /// of this API is. The deduction itself uses the exact value.
    pub grams: Option<u64>,
    /// `estimated` (slicer) or `measured` (at print finish).
    pub grams_basis: Option<&'static str>,
    pub created_at: String,
    pub updated_at: String,
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
            grams: target.as_ref().map(|t| t.grams.round() as u64),
            grams_basis: target.as_ref().map(|t| GramsBasis::as_str(t.basis)),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: SpoolmanDeductionsClient,
}

/// Mounts the list route under the accepted gate when `PP_DESKTOP_REBUILD`
/// was exactly `1` or `true`; otherwise nothing.
pub fn spoolman_deductions_router(
    config: AcceptedHttpConfig,
    client: SpoolmanDeductionsClient,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let routes = Router::new().route("/api/v2/spoolman/deductions", get(list));
    gate.guard(routes).with_state(App { gate, client })
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
        Some(id) if id.is_empty() || id.len() > MAX_LINK_ID => return None,
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

fn refused(error: &DeductionError) -> Response {
    match error {
        DeductionError::StoredRowInvalid { .. } => problem::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "A stored deduction is unreadable",
            code::DEDUCTION_UNREADABLE,
        ),
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
}
