use super::{
    ErrorData, PresentedCredential, ProductMcp, ToolResult, plan_accept, plan_history_changes,
};
use pp_services::mcp_domain::{DomainPort, ProposedAction};
use pp_storage::{
    WriterFailure,
    pending_actions::{
        ActionError, ClaimToken, End, Listing, NewAction, PendingAction, PendingActionsClient,
        StagedBy, State,
    },
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "proposal_http.rs"]
pub(super) mod http;

pub(super) struct Decision {
    end: End,
    body: Value,
}

impl Decision {
    pub(super) fn confirmed(body: Value) -> Self {
        let end = match (body["ok"].as_bool(), body["outcome_unknown"].as_bool()) {
            (_, Some(true)) => End::Unknown,
            (Some(true), _) => End::Applied,
            (Some(false), _) => End::Released,
            _ => return Self::unknown(UNKNOWN),
        };
        Self { end, body }
    }

    pub(super) fn unknown(detail: &str) -> Self {
        Self::confirmed(json!({
            "ok": false, "code": "outcome_unknown", "outcome_unknown": true, "detail": detail,
        }))
    }

    fn tool_result(self) -> ToolResult {
        ToolResult {
            text: serde_json::to_string_pretty(&self.body)
                .unwrap_or_else(|_| self.body.to_string()),
            is_error: self.end != End::Applied,
        }
    }
}

enum DecisionRefusal {
    Queue(ActionError),
    Unavailable,
}

const UNKNOWN: &str =
    "The proposal's outcome is unknown. Check the affected work before proposing again.";

impl ProductMcp {
    pub(super) async fn product_tool(
        &self,
        name: String,
        arguments: Value,
        presented: PresentedCredential,
        staged_by: StagedBy,
    ) -> Result<ToolResult, ErrorData> {
        let (result, proposal) = self
            .in_tools(Some(presented.clone()), move |tools| {
                tools.invoke(&name, &arguments)
            })
            .await?;
        Ok(match proposal {
            Some(action) => {
                self.stage_proposal(action, presented, staged_by, result)
                    .await
            }
            None => result,
        })
    }

    pub(super) async fn stage_proposal(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
        staged_by: StagedBy,
        result: ToolResult,
    ) -> ToolResult {
        match self.stage(action, presented, staged_by).await {
            Ok(_) => result,
            Err(error) => refusal(error, ""),
        }
    }

    pub(super) async fn stage(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
        staged_by: StagedBy,
    ) -> Result<PendingAction, ActionError> {
        let queue = self.reads.accepted.pending_actions();
        let wait = self.reads.wait;
        let action = NewAction {
            id: action.id,
            action_type: action.action_type,
            plan_id: action.plan_id,
            label: action.label,
            summary: action.summary,
            params: action.params,
            depends_on: Vec::new(),
            staged_by,
        };
        tokio::task::spawn_blocking(move || {
            queue.stage(
                presented.into_credential(),
                action,
                &AtomicBool::new(false),
                wait,
            )
        })
        .await
        .map_err(worker_failure)?
    }

    async fn list(&self, presented: PresentedCredential) -> Result<Listing, ActionError> {
        let queue = self.reads.accepted.pending_actions();
        let wait = self.reads.wait;
        tokio::task::spawn_blocking(move || {
            queue.list(presented.into_credential(), &AtomicBool::new(false), wait)
        })
        .await
        .map_err(worker_failure)?
    }

    pub(super) async fn list_proposals(&self, presented: PresentedCredential) -> ToolResult {
        match self.list(presented).await {
            Ok(list) => {
                let actions: Vec<Value> = list.actions.iter().map(listed_action).collect();
                let mut body = json!({ "actions": actions });
                if list.omitted > 0 {
                    body["omitted"] = json!(list.omitted);
                }
                ToolResult::ok(
                    serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()),
                )
            }
            Err(error) => refusal(error, ""),
        }
    }

    pub(super) async fn dismiss_proposal(
        &self,
        arguments: Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let id = action_id(&arguments);
        match self.dismiss(id.clone(), presented).await {
            Ok(()) => ToolResult::ok(json!({ "ok": true, "action_id": id }).to_string()),
            Err(error) => refusal(error, &id),
        }
    }

    async fn dismiss(&self, id: String, presented: PresentedCredential) -> Result<(), ActionError> {
        let queue = self.reads.accepted.pending_actions();
        let wait = self.reads.wait;
        tokio::task::spawn_blocking(move || {
            queue.dismiss(
                presented.into_credential(),
                &id,
                &AtomicBool::new(false),
                wait,
            )
        })
        .await
        .map_err(worker_failure)?
    }

    pub(super) async fn confirm_proposal(
        &self,
        arguments: Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let id = action_id(&arguments);
        match self.confirm(id.clone(), arguments, presented.clone()).await {
            Ok(decision) => decision.tool_result(),
            Err(DecisionRefusal::Queue(
                error @ (ActionError::NotFound
                | ActionError::AlreadyApplied { .. }
                | ActionError::OutcomeUnknown),
            )) => self.reconcile_proposal(presented, id, error).await,
            Err(DecisionRefusal::Queue(error)) => refusal(error, &id),
            Err(DecisionRefusal::Unavailable) => unavailable(),
        }
    }

    async fn confirm(
        &self,
        id: String,
        arguments: Value,
        presented: PresentedCredential,
    ) -> Result<Decision, DecisionRefusal> {
        let owner = self.clone();
        // A dropped request must not interrupt claim, apply, or settlement.
        tokio::spawn(async move { owner.decide(id, arguments, presented).await })
            .await
            .unwrap_or_else(|_| Ok(decision_unavailable()))
    }

    async fn decide(
        &self,
        id: String,
        arguments: Value,
        presented: PresentedCredential,
    ) -> Result<Decision, DecisionRefusal> {
        let queue = self.reads.accepted.pending_actions();
        let wait = self.reads.wait;
        let claim = {
            let (queue, presented, id) = (queue.clone(), presented.clone(), id.clone());
            tokio::task::spawn_blocking(move || {
                queue.claim(
                    presented.into_credential(),
                    &id,
                    &AtomicBool::new(false),
                    wait,
                )
            })
            .await
        };
        let (action, token) = match claim {
            Ok(Ok(claim)) => claim.into_parts(),
            Ok(Err(error)) => return Err(DecisionRefusal::Queue(error)),
            Err(_) => return Err(DecisionRefusal::Unavailable),
        };
        let action = ProposedAction {
            id: action.id,
            action_type: action.action_type,
            plan_id: action.plan_id,
            label: action.label,
            summary: action.summary,
            params: action.params,
        };
        let decision = if matches!(
            action.action_type.as_str(),
            plan_accept::PROPOSE | plan_accept::REVIEWED
        ) {
            self.confirm_accept(action, presented).await
        } else if plan_history_changes::TOOLS.contains(&action.action_type.as_str()) {
            self.confirm_history_change(action, presented).await
        } else {
            match self
                .in_tools(Some(presented), move |tools| {
                    tools.apply(&action, &arguments)
                })
                .await
            {
                Ok(Ok(body)) => Decision::confirmed(body),
                _ => Decision::unknown(UNKNOWN),
            }
        };
        Ok(if record_decision(queue, token, decision.end, wait).await {
            decision
        } else {
            decision_unavailable()
        })
    }

    async fn reconcile_proposal(
        &self,
        presented: PresentedCredential,
        id: String,
        error: ActionError,
    ) -> ToolResult {
        let lookup = id.clone();
        // Existing Checkoff receipts remain readable, including ones written
        // before the queue was introduced. This lookup never applies work.
        match self
            .in_tools(Some(presented), move |tools| {
                tools.port().reconcile(&lookup)
            })
            .await
        {
            Ok(Ok(Some(body))) => Decision::confirmed(body).tool_result(),
            _ => refusal(error, &id),
        }
    }
}

async fn record_decision(
    queue: PendingActionsClient,
    token: ClaimToken,
    end: End,
    wait: Duration,
) -> bool {
    let (reply, recorded) = tokio::sync::oneshot::channel();
    let token = Arc::new(token);
    // Reply after the first attempt, but keep the claim until storage can
    // record the decision. Restore refuses while an `applying` row remains.
    tokio::spawn(async move {
        let mut reply = Some(reply);
        loop {
            let (queue, token) = (queue.clone(), token.clone());
            let result = tokio::task::spawn_blocking(move || {
                queue.settle(&token, end, &AtomicBool::new(false), wait)
            })
            .await;
            if let Some(reply) = reply.take() {
                let _ = reply.send(matches!(result, Ok(Ok(true))));
            }
            if !matches!(
                result,
                Ok(Err(ActionError::Writer(
                    WriterFailure::Busy | WriterFailure::QueueFull | WriterFailure::Internal(_)
                )))
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    recorded.await.unwrap_or(false)
}

fn action_id(arguments: &Value) -> String {
    arguments["action_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn listed_action(action: &PendingAction) -> Value {
    json!({
        "id": action.id, "type": action.action_type, "plan_id": action.plan_id,
        "label": action.label, "summary": action.summary, "params": action.params,
        "state": state_name(action.state), "staged_by": action.staged_by,
        "created_at": action.created_at, "expires_at": action.expires_at,
        "depends_on": action.depends_on,
    })
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Pending => "pending",
        State::Applying => "applying",
        State::Applied => "applied",
        State::Dismissed => "dismissed",
        State::Expired => "expired",
        State::Unknown => "unknown",
        State::InvalidatedByRestore => "invalidated_by_restore",
    }
}

fn unavailable() -> ToolResult {
    ToolResult::error(
        json!({ "ok": false, "code": "unavailable", "error": "The proposal queue is unavailable." }),
    )
}

fn decision_unavailable() -> Decision {
    Decision::unknown(
        "The proposal decision could not be recorded. Check the affected work before proposing again.",
    )
}

fn worker_failure(error: tokio::task::JoinError) -> ActionError {
    WriterFailure::Internal(error.into()).into()
}

fn refusal(error: ActionError, id: &str) -> ToolResult {
    let code = match &error {
        ActionError::NotFound => "not_found",
        ActionError::Expired { .. } => "expired",
        ActionError::Blocked(_) => "dependency_blocked",
        ActionError::InFlight => "applying",
        ActionError::AlreadyApplied { .. } => "already_applied",
        ActionError::AlreadyDismissed { .. } => "already_dismissed",
        ActionError::OutcomeUnknown => "outcome_unknown",
        ActionError::InvalidatedByRestore { .. } => "invalidated_by_restore",
        ActionError::Writer(WriterFailure::Auth(_)) => {
            return ToolResult::error(
                json!({ "ok": false, "code": "unauthorized", "error": "The API key was not accepted." }),
            );
        }
        _ => return unavailable(),
    };
    let detail = match &error {
        ActionError::NotFound => {
            "Unknown, in-flight, or already applied/dismissed action_id".into()
        }
        _ => error.to_string(),
    };
    let mut body = json!({ "ok": false, "code": code, "error": detail, "action_id": id });
    match error {
        ActionError::Blocked(first) => {
            body["blocked_by"] =
                json!({ "id": first.id, "label": first.label, "state": state_name(first.state) });
        }
        ActionError::Expired { expires_at } => body["expires_at"] = json!(expires_at),
        ActionError::OutcomeUnknown => body["outcome_unknown"] = json!(true),
        _ => {}
    }
    ToolResult::error(body)
}
