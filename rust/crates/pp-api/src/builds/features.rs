//! Node's `GET` and `PATCH /plans/{id}/features` through the Build router
//! (GRE-1243). Rust alone reads and writes `feature_toggles:plan:<id>` for
//! them; `/settings/features` stays Node's.

use super::http::{App, Received, failure, refuse_after_auth, run, unreadable};
use axum::{
    extract::{Path, State},
    http::{Method, StatusCode},
    response::Response,
};
use pp_storage::{
    build_graph::{BuildCommand, PositiveId, variants::json_body_refused},
    features::{FeaturePatch, PlanFeatureOutcome, PlanFeatureRequest},
};

/// Node's `/^[1-9]\d*$/` id, as a positive safe integer.
fn plan_id(id: &str) -> Option<PositiveId> {
    const MAX_SAFE: u64 = 9_007_199_254_740_991;
    if !id.starts_with(|first: char| matches!(first, '1'..='9'))
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    id.parse()
        .ok()
        .filter(|id| *id <= MAX_SAFE)
        .and_then(|id| PositiveId::new(id).ok())
}

fn invalid() -> Response {
    failure(StatusCode::BAD_REQUEST, "Invalid plan feature input")
}

async fn answer(
    app: App,
    received: Received,
    method: Method,
    request: Option<PlanFeatureRequest>,
) -> Response {
    let Some(request) = request else {
        return refuse_after_auth(app, received.presented, invalid()).await;
    };
    run(
        app,
        received.presented,
        method,
        received.path,
        received.contract,
        BuildCommand::PlanFeatures(request),
    )
    .await
}

pub(super) async fn read(
    State(app): State<App>,
    Path(id): Path<String>,
    received: Received,
) -> Response {
    let request = plan_id(&id).map(|plan| PlanFeatureRequest { plan, patch: None });
    answer(app, received, Method::GET, request).await
}

/// Fastify refuses an empty or poisoned JSON body before the route runs (after
/// Node's `onRequest` hook has checked the credential), and
/// hands a `text/plain` body over as a string, which is never a valid patch;
/// the route then needs both a valid id and a valid patch before it looks up
/// the Plan.
pub(super) async fn patch(
    State(app): State<App>,
    Path(id): Path<String>,
    received: Received,
) -> Response {
    if received.json && (received.bytes.is_empty() || json_body_refused(&received.bytes)) {
        return refuse_after_auth(app, received.presented, unreadable()).await;
    }
    let patch = if received.text {
        None
    } else {
        FeaturePatch::from_json(&received.bytes)
    };
    let request = plan_id(&id)
        .zip(patch)
        .map(|(plan, patch)| PlanFeatureRequest {
            plan,
            patch: Some(patch),
        });
    answer(app, received, Method::PATCH, request).await
}

pub(super) fn present(outcome: PlanFeatureOutcome) -> Response {
    match outcome {
        PlanFeatureOutcome::View(body) => match serde_json::to_vec(&body) {
            Ok(body) => super::http::bytes_response(StatusCode::OK, body, false),
            Err(_) => failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Build service unavailable",
            ),
        },
        PlanFeatureOutcome::MissingPlan => failure(StatusCode::NOT_FOUND, "Plan not found"),
        PlanFeatureOutcome::InvalidStorage => crate::problem::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored feature toggles are invalid.",
            "FEATURE_TOGGLES_INVALID",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::plan_id;

    #[test]
    fn plan_feature_ids_follow_node_pattern() {
        for ok in ["1", "12", "9007199254740991"] {
            assert!(plan_id(ok).is_some(), "{ok}");
        }
        for bad in [
            "",
            "0",
            "01",
            "+1",
            "-1",
            "1.0",
            "0x1",
            " 1",
            "1e0",
            "9007199254740992",
        ] {
            assert!(plan_id(bad).is_none(), "{bad}");
        }
    }
}
