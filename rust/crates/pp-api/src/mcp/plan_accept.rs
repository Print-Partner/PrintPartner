//! `propose_apply_plan_draft` and its `confirm_apply` (GRE-1236). Both run
//! Node's guards: the Build planning brief must select the draft and its
//! source choices must resolve. Propose reads the same Accept preview as the
//! Plan tab, writes nothing, and proposes exactly the revision it read; confirm
//! accepts that revision through the same Rust Accept, so a draft header saved
//! in the Plan tab is copied to the Build once. A draft with no Required-unit
//! selection is proposed without a revision, and confirm selects the automatic
//! reconciliation first, as Node's `prepareForApply` does, then accepts the
//! revision it previews.

use super::*;
use pp_services::mcp_domain::ProposedAction;
use pp_storage::plan_publication::{
    ApplyRequest, Outcome as AcceptOutcome, PublicationCommand,
    preview::{AcceptPreview, PreviewOutcome},
};
use pp_storage::read_model::planning::{self, AcceptGuard};
use pp_storage::required_units::{Outcome as WorkspaceOutcome, ReconciliationCommand};
use pp_storage::working_drafts::{Outcome as DraftOutcome, Request as DraftRequest};

pub(super) const PROPOSE: &str = "propose_apply_plan_draft";
const SUMMARY: &str = "Apply the confirmed Build planning change.";
const NOT_FOUND: &str = "Selected Working Plan not found";

/// Test hook: a confirm signals `entered` once it has taken its proposal,
/// then waits for a `release` permit before Accept applies. `cancelled` fires
/// when rmcp cancels the confirm's request token.
#[cfg(test)]
pub(crate) struct ConfirmGate {
    pub entered: tokio::sync::Notify,
    pub release: tokio::sync::Semaphore,
    pub cancelled: tokio::sync::Notify,
}

#[cfg(test)]
impl ConfirmGate {
    pub fn closed() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            cancelled: tokio::sync::Notify::new(),
        })
    }

    /// Fires `cancelled` once `token_cancelled` (the request token's
    /// `cancelled_owned()`) resolves.
    pub fn watch(
        self: &Arc<Self>,
        token_cancelled: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let gate = Arc::clone(self);
        tokio::spawn(async move {
            token_cancelled.await;
            gate.cancelled.notify_one();
        });
    }
}

/// What the Accept preview allows.
enum Previewed {
    /// Accept would run: the preview's revision without `draft_id`, and the
    /// preview.
    Ready { revision: Value, preview: Value },
    /// The draft has no Required-unit selection, and none was made.
    Unprepared { reason: Value },
    /// A refusal: its message and the fields that explain it.
    Refused(String, Value),
}

impl ProductMcp {
    /// Runs the guards, reads the draft's Accept preview with the caller's key
    /// and, when Accept would run, proposes its revision. A refusal, running
    /// work or production Accept would have to remap proposes nothing.
    pub(super) async fn propose_accept(
        &self,
        arguments: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let (Some(plan), Some(draft)) = (
            positive_id(arguments, "plan_id"),
            positive_id(arguments, "draft_id"),
        ) else {
            return ToolResult::error(json!({ "error": "plan_id and draft_id required" }));
        };
        match self.guard(plan, draft, &presented).await {
            Ok(()) => {}
            Err(Refusal::NoBrief) => {
                return ToolResult::error(json!({ "error": "Build planning brief not found" }));
            }
            Err(refusal) => return ToolResult::error(json!({ "error": refusal.detail() })),
        }
        let (revision, read) = match self.preview(plan, draft, presented, false).await {
            Previewed::Ready { revision, preview } => (revision, json!({ "preview": preview })),
            Previewed::Unprepared { reason } => (
                Value::Null,
                json!({ "reconciliation": { "kind": "reconciliation_required", "reason": reason } }),
            ),
            Previewed::Refused(message, mut fields) => {
                fields["error"] = json!(message);
                return ToolResult::error(fields);
            }
        };
        let params =
            json!({ "plan_id": plan.get(), "draft_id": draft.get(), "revision": revision });
        let (Value::Object(params), Ok(plan_id)) = (params, i64::try_from(plan.get())) else {
            return ToolResult::error(json!({ "error": "plan_id required" }));
        };
        let action =
            ProposedAction::new(PROPOSE, plan_id, "apply plan draft".into(), SUMMARY, params);
        self.session.set_pending(action.clone());
        let mut content: Value =
            serde_json::from_str(&action.proposed_content()).unwrap_or_default();
        if let (Some(content), Value::Object(read)) = (content.as_object_mut(), read) {
            content.extend(read);
        }
        ToolResult::ok(content.to_string())
    }

    /// Accepts the revision `action` proposed, or, for a draft proposed
    /// without one, selects the automatic reconciliation and accepts the
    /// revision it then previews. The action id is the idempotency key. A
    /// refusal or failure leaves the action pending, as Node does.
    pub(super) async fn confirm_accept(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
    ) -> ToolResult {
        let params = Value::Object(action.params.clone());
        let refuse = |action: ProposedAction, detail: &str, result: Value| {
            self.session.set_pending(action);
            let mut body = json!({ "ok": false, "detail": detail });
            if result.as_object().is_some_and(|fields| !fields.is_empty()) {
                body["result"] = result;
            }
            ToolResult::error(body)
        };
        let (Some(plan), Some(draft)) = (
            positive_id(&params, "plan_id"),
            positive_id(&params, "draft_id"),
        ) else {
            return refuse(action, NOT_FOUND, Value::Null);
        };
        if let Err(refusal) = self.guard(plan, draft, &presented).await {
            return refuse(action, &refusal.detail(), Value::Null);
        }
        let revision = match params["revision"].clone() {
            Value::Null => match self.preview(plan, draft, presented.clone(), true).await {
                Previewed::Ready { revision, .. } => revision,
                Previewed::Refused(message, fields) => return refuse(action, &message, fields),
                Previewed::Unprepared { .. } => {
                    return refuse(action, "Working Plan preview is unavailable", Value::Null);
                }
            },
            revision => revision,
        };
        let Ok(request) = serde_json::from_value::<ApplyRequest>(revision) else {
            return refuse(action, NOT_FOUND, Value::Null);
        };
        #[cfg(test)]
        if let Some(gate) = &self.reads.confirm_gate {
            gate.entered.notify_one();
            drop(gate.release.acquire().await);
        }
        let Reads { drafts, wait, .. } = self.reads.clone();
        let key = action.id.clone();
        let credential = presented.clone();
        let applied = tokio::task::spawn_blocking(move || {
            let command =
                PublicationCommand::new(plan, draft, request, key, presented.into_credential())?;
            drafts
                .publication()
                .apply(command, &AtomicBool::new(false), wait)
        })
        .await;
        match applied {
            // Node's `applyResult` answers `applied` for a replay too.
            Ok(Ok(
                AcceptOutcome::Applied { receipt }
                | AcceptOutcome::Existing { receipt }
                | AcceptOutcome::AlreadyApplied { receipt },
            )) => {
                let receipt = camel_case(json!(receipt));
                let planning_phase = json!({
                    "kind": "applied",
                    "draft_id": receipt["draftId"],
                    "revision_id": receipt["revisionId"],
                });
                let build_workflow = match self.workflow(plan, credential).await {
                    Some(Outcome::Workspace(workspace)) => json!(workspace),
                    _ => Value::Null,
                };
                ToolResult::ok(pretty(&json!({
                    "ok": true,
                    "result": {
                        "apply_result": { "kind": "applied", "receipt": receipt },
                        "build_workflow": build_workflow,
                        "planning_phase": planning_phase,
                    },
                })))
            }
            Ok(Ok(refused)) => {
                let kind = json!(refused)["kind"]
                    .as_str()
                    .unwrap_or("refused")
                    .to_owned();
                refuse(
                    action,
                    &format!("Working Plan acceptance failed: {kind}"),
                    json!({ "apply_result": refused }),
                )
            }
            _ => refuse(
                action,
                "Working Plan acceptance is unavailable",
                Value::Null,
            ),
        }
    }

    /// Reads the Accept preview. When it needs a Required-unit reconciliation
    /// and the draft has none selected, `prepare` selects the automatic one and
    /// reads the preview again, as Node's `prepareForApply` does; without
    /// `prepare` nothing is written and the answer is `Unprepared`.
    async fn preview(
        &self,
        plan: PositiveId,
        draft: PositiveId,
        presented: PresentedCredential,
        prepare: bool,
    ) -> Previewed {
        let Reads { drafts, wait, .. } = self.reads.clone();
        let read = tokio::task::spawn_blocking(move || -> Result<Previewed> {
            let credential = || presented.clone().into_credential();
            let flag = || Arc::new(AtomicBool::new(false));
            let preview = || {
                drafts.execute(
                    credential(),
                    plan,
                    DraftRequest::AcceptPreview { draft_id: draft },
                    flag(),
                    wait,
                )
            };
            let first = preview()?;
            let DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Refused(AcceptOutcome::ReconciliationRequired { reason }),
            } = &first
            else {
                return Ok(classify(first));
            };
            let reason = json!(reason);
            let request = DraftRequest::Read { draft_id: draft };
            let document = match drafts.execute(credential(), plan, request, flag(), wait)? {
                DraftOutcome::Read { draft } if !draft.has_required_unit_selection() => draft,
                _ => return Ok(classify(first)),
            };
            if !prepare {
                return Ok(Previewed::Unprepared { reason });
            }
            let snapshot = document.identity().snapshot_digest();
            let command = ReconciliationCommand::automatic(plan, draft, snapshot, credential())?;
            Ok(
                match drafts.reconciliation().reconcile(command, flag(), wait)? {
                    WorkspaceOutcome::Ready { .. } => classify(preview()?),
                    refused => {
                        let kind = json!(refused)["kind"]
                            .as_str()
                            .unwrap_or("refused")
                            .to_owned();
                        Previewed::Refused(
                            format!("Working Plan acceptance preparation failed: {kind}"),
                            json!({ "preparation_result": refused }),
                        )
                    }
                },
            )
        })
        .await;
        match read {
            Ok(Ok(previewed)) => previewed,
            _ => unavailable(),
        }
    }

    /// Node's guards, read with the caller's key in one storage transaction.
    async fn guard(
        &self,
        plan: PositiveId,
        draft: PositiveId,
        presented: &PresentedCredential,
    ) -> Result<(), Refusal> {
        let Reads { planning, wait, .. } = self.reads.clone();
        let credential = presented.clone().into_credential();
        let read = tokio::task::spawn_blocking(move || {
            planning.read(
                credential,
                planning::Request {
                    build: Some(plan),
                    query: planning::Query::AcceptGuard { draft_id: draft },
                },
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        match read {
            Ok(Ok(planning::Outcome::AcceptGuard(AcceptGuard::Clear))) => Ok(()),
            Ok(Ok(planning::Outcome::AcceptGuard(AcceptGuard::NoBrief))) => Err(Refusal::NoBrief),
            Ok(Ok(planning::Outcome::AcceptGuard(AcceptGuard::SourceChoices(blockers)))) => {
                Err(Refusal::SourceChoices(blockers))
            }
            Ok(Ok(planning::Outcome::AcceptGuard(AcceptGuard::NotSelected))) => {
                Err(Refusal::NotSelected)
            }
            Ok(Ok(planning::Outcome::MissingBuild)) => Err(Refusal::MissingBuild),
            _ => Err(Refusal::Unavailable),
        }
    }
}

enum Refusal {
    NoBrief,
    NotSelected,
    SourceChoices(Vec<String>),
    MissingBuild,
    Unavailable,
}

impl Refusal {
    /// Node's `confirm_apply` detail; a missing brief is a missing selection.
    fn detail(&self) -> String {
        match self {
            Self::NoBrief | Self::NotSelected => NOT_FOUND.into(),
            Self::SourceChoices(blockers) => format!(
                "Working Plan cannot apply source choices: {}",
                blockers.join("; ")
            ),
            Self::MissingBuild => "Build not found".into(),
            Self::Unavailable => "Working Plan preview is unavailable".into(),
        }
    }
}

fn unavailable() -> Previewed {
    Previewed::Refused("Working Plan preview is unavailable".into(), json!({}))
}

/// Whether Accept would run on a read preview. Accept refuses running work,
/// and production while this tool never asks for the checkoff remap.
fn classify(outcome: DraftOutcome) -> Previewed {
    let refused = |message: &str, fields: Value| Previewed::Refused(message.into(), fields);
    let preview = match outcome {
        DraftOutcome::AcceptPreview {
            preview: PreviewOutcome::Ready(preview),
        } => preview,
        DraftOutcome::AcceptPreview {
            preview: PreviewOutcome::Refused(AcceptOutcome::NotFound),
        }
        | DraftOutcome::DraftNotFound => return refused(NOT_FOUND, json!({})),
        DraftOutcome::AcceptPreview {
            preview: PreviewOutcome::Refused(AcceptOutcome::ReconciliationRequired { reason }),
        } => {
            return refused(
                "Working Plan acceptance requires required-unit decisions",
                json!({ "reconciliation": { "kind": "reconciliation_required", "reason": reason } }),
            );
        }
        DraftOutcome::AcceptPreview {
            preview: PreviewOutcome::Refused(outcome),
        } => {
            return refused(
                "Working Plan cannot be accepted",
                json!({ "preview_result": outcome }),
            );
        }
        DraftOutcome::ProfileNotFound => return refused("Build not found", json!({})),
        _ => return unavailable(),
    };
    if let AcceptPreview::Changed { changes, .. } = preview.as_ref()
        && (!changes.blocked_by.is_empty() || changes.production.is_some())
    {
        return refused(
            "Working Plan cannot be accepted",
            json!({ "preview": preview }),
        );
    }
    let (AcceptPreview::NoChange { revision, .. } | AcceptPreview::Changed { revision, .. }) =
        preview.as_ref();
    let mut revision = json!(revision);
    if let Some(fields) = revision.as_object_mut() {
        fields.remove("draft_id");
    }
    Previewed::Ready {
        revision,
        preview: json!(preview),
    }
}

/// Node's `AppliedPlanReceipt` keys: the Rust receipt is snake_case.
fn camel_case(value: Value) -> Value {
    let Value::Object(fields) = value else {
        return value;
    };
    let fields = fields.into_iter().map(|(key, value)| {
        let mut words = key.split('_');
        let mut camel = words.next().unwrap_or_default().to_owned();
        for word in words {
            let mut chars = word.chars();
            camel.extend(chars.next().map(|c| c.to_ascii_uppercase()));
            camel.push_str(chars.as_str());
        }
        (camel, value)
    });
    Value::Object(fields.collect())
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
