//! The replacement UI's view of the same proposals MCP stages and decides.

use super::{DecisionRefusal, End, PendingAction, State as StoredState, state_name};
use crate::accepted::{AcceptedHttpConfig, Gate, build_id, failure, ok_json};
use crate::loopback::PresentedCredential;
use crate::mcp::{
    McpClients, ProductMcp,
    plan_accept::{Refusal, StageRefusal},
};
use crate::problem::{self, ProblemCode};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use pp_storage::{
    pending_actions::{ActionError, StagedBy},
    plan_publication::ApplyRequest,
};
use serde::Serialize;
use serde_json::{Map, Value, json};

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
enum OpenState {
    Pending,
    Applying,
    Expired,
    Unknown,
    InvalidatedByRestore,
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct Proposal {
    id: String,
    plan_id: i64,
    label: String,
    summary: String,
    params: Map<String, Value>,
    state: OpenState,
    staged_by: String,
    created_at: String,
    expires_at: String,
    depends_on: Vec<String>,
}

impl From<PendingAction> for Proposal {
    fn from(row: PendingAction) -> Self {
        let state = match row.state {
            StoredState::Pending => OpenState::Pending,
            StoredState::Applying => OpenState::Applying,
            StoredState::Expired => OpenState::Expired,
            StoredState::Unknown => OpenState::Unknown,
            StoredState::InvalidatedByRestore => OpenState::InvalidatedByRestore,
            StoredState::Applied | StoredState::Dismissed => unreachable!("only open rows"),
        };
        Self {
            id: row.id,
            plan_id: row.plan_id,
            label: row.label,
            summary: row.summary,
            params: row.params,
            state,
            staged_by: row.staged_by,
            created_at: row.created_at,
            expires_at: row.expires_at,
            depends_on: row.depends_on,
        }
    }
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct ProposalList {
    actions: Vec<Proposal>,
    omitted: u64,
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProposalDecision {
    Applied,
    Dismissed,
}

#[cfg(feature = "codegen")]
pub(crate) fn proposal_fixtures() -> Result<Vec<Value>, serde_json::Error> {
    use crate::client_contract::codegen::served;

    let proposal = |state| Proposal {
        id: "proposal-2".into(),
        plan_id: 7,
        label: "Apply reviewed draft".into(),
        summary: "Publish the reviewed Draft as the Accepted Plan.".into(),
        params: Map::from_iter([("draft_id".into(), json!(11))]),
        state,
        staged_by: "UI".into(),
        created_at: "2026-11-07T05:00:00.000Z".into(),
        expires_at: "2026-11-07T13:00:00.000Z".into(),
        depends_on: vec!["proposal-1".into()],
    };
    Ok(vec![
        served("Proposal", "staged", proposal(OpenState::Pending))?,
        served(
            "ProposalList",
            "expired",
            ProposalList {
                actions: vec![proposal(OpenState::Expired)],
                omitted: 1,
            },
        )?,
        served("ProposalDecision", "applied", ProposalDecision::Applied)?,
        served("ProposalDecision", "dismissed", ProposalDecision::Dismissed)?,
    ])
}

/// Mounts proposal review on the Rust API only, with rebuild HTTP admission.
pub fn proposal_router(config: AcceptedHttpConfig, clients: McpClients) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    let owner = ProductMcp::new(clients.reads(gate.wait()));
    gate.guard(
        Router::new()
            .route("/api/v2/proposals", get(list))
            .route("/api/v2/proposals/{id}/apply", post(apply))
            .route("/api/v2/proposals/{id}/dismiss", post(dismiss))
            .route("/api/v2/plans/{id}/drafts/{draft_id}/propose", post(stage))
            .layer(DefaultBodyLimit::max(16 * 1024)),
    )
    .with_state(owner)
}

async fn list(
    State(owner): State<ProductMcp>,
    Extension(credential): Extension<PresentedCredential>,
) -> Response {
    match owner.list(credential).await {
        Ok(list) => ok_json(&ProposalList {
            actions: list.actions.into_iter().map(Proposal::from).collect(),
            omitted: list.omitted,
        }),
        Err(error) => queue_refused(error),
    }
}

async fn apply(
    State(owner): State<ProductMcp>,
    Extension(credential): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    match owner.confirm(id, Value::Null, credential).await {
        Ok(decision) if decision.end == End::Applied => ok_json(&ProposalDecision::Applied),
        Ok(decision) if decision.end == End::Unknown => problem::coded(
            StatusCode::CONFLICT,
            "The proposal may have applied. Check the affected work before proposing again.",
            ProblemCode::OutcomeUnknown.as_str(),
        ),
        Ok(decision) if decision.body["result"]["apply_result"]["kind"] == "draft_changed" => {
            problem::coded(
                StatusCode::CONFLICT,
                "The draft changed. Review it again and stage a new proposal.",
                ProblemCode::DraftChanged.as_str(),
            )
        }
        Ok(_) => failure(
            StatusCode::CONFLICT,
            "The operation refused this proposal. Review the affected work.",
        ),
        Err(DecisionRefusal::Queue(error)) => queue_refused(error),
        Err(DecisionRefusal::Unavailable) => unavailable(),
    }
}

async fn dismiss(
    State(owner): State<ProductMcp>,
    Extension(credential): Extension<PresentedCredential>,
    Path(id): Path<String>,
) -> Response {
    match owner.dismiss(id, credential).await {
        Ok(()) => ok_json(&ProposalDecision::Dismissed),
        Err(error) => queue_refused(error),
    }
}

async fn stage(
    State(owner): State<ProductMcp>,
    Extension(credential): Extension<PresentedCredential>,
    Path((id, draft_id)): Path<(String, String)>,
    Json(reviewed): Json<ApplyRequest>,
) -> Response {
    let (Some(id), Some(draft_id)) = (build_id(&id), build_id(&draft_id)) else {
        return failure(StatusCode::NOT_FOUND, "Draft not found");
    };
    let (action, _) = match owner
        .plan_proposal(id, draft_id, &credential, Some(reviewed))
        .await
    {
        Ok(proposal) => proposal,
        Err(StageRefusal::Changed) => {
            return problem::coded(
                StatusCode::CONFLICT,
                "The draft changed. Review it again before staging.",
                ProblemCode::DraftChanged.as_str(),
            );
        }
        Err(StageRefusal::Guard(Refusal::Auth(error))) => return problem::writer_failure(&error),
        Err(StageRefusal::Guard(Refusal::Unavailable)) => return unavailable(),
        Err(StageRefusal::Guard(error)) => return failure(StatusCode::CONFLICT, &error.detail()),
        Err(StageRefusal::Preview(message, _)) => return failure(StatusCode::CONFLICT, &message),
    };
    match owner.stage(action, credential, StagedBy::label("UI")).await {
        Ok(action) => ok_json(&Proposal::from(action)),
        Err(error) => queue_refused(error),
    }
}

fn queue_refused(error: ActionError) -> Response {
    let code = match &error {
        ActionError::NotFound => return failure(StatusCode::NOT_FOUND, "Proposal not found"),
        ActionError::Expired { .. } => ProblemCode::ProposalExpired,
        ActionError::Blocked(blocker) => {
            let label: String = blocker.label.chars().take(120).collect();
            let state = state_name(blocker.state);
            let detail = format!("Apply {label} first (currently {state}).");
            let extensions = Map::from_iter([(
                "blocked_by".into(),
                json!({
                    "id": blocker.id, "label": label, "state": state,
                }),
            )]);
            return problem::build(
                problem::problem(
                    StatusCode::CONFLICT,
                    &detail,
                    Some(ProblemCode::ProposalDependencyBlocked.as_str()),
                ),
                extensions,
            );
        }
        ActionError::InFlight => ProblemCode::ProposalApplying,
        ActionError::AlreadyApplied { .. } => ProblemCode::AlreadyApplied,
        ActionError::AlreadyDismissed { .. } => ProblemCode::ProposalAlreadyDismissed,
        ActionError::OutcomeUnknown => ProblemCode::OutcomeUnknown,
        ActionError::InvalidatedByRestore { .. } => ProblemCode::ProposalInvalidated,
        ActionError::Writer(error) => return problem::writer_failure(error),
        _ => return unavailable(),
    };
    problem::coded(StatusCode::CONFLICT, &error.to_string(), code.as_str())
}

fn unavailable() -> Response {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "The proposal queue is unavailable",
    )
}
