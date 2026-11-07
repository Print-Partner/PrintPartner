//! `propose_apply_plan_draft` and its `confirm_apply` (GRE-1236). Both run
//! Node's guards: the Build planning brief must select the draft and its
//! source choices must resolve. MCP reads the same Accept preview as the Plan
//! tab, selecting the automatic Required-unit reconciliation first when the
//! draft has none, as Node's `prepareForApply` does. It proposes exactly the
//! revision it read, and confirm accepts that revision through the same Rust
//! Accept, so a draft header saved in the Plan tab is copied to the Build once.

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

impl ProductMcp {
    /// Reads the draft's Accept preview with the caller's key and, when Accept
    /// would run, proposes its revision. A refusal, running work or production
    /// Accept would have to remap proposes nothing.
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
        let preview = match self.preview(plan, draft, presented).await {
            Some(Err(prepared)) => {
                let kind = json!(prepared)["kind"]
                    .as_str()
                    .unwrap_or("refused")
                    .to_owned();
                return ToolResult::error(json!({
                    "error": format!("Working Plan acceptance preparation failed: {kind}"),
                    "preparation_result": prepared,
                }));
            }
            Some(Ok(DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Ready(preview),
            })) => preview,
            Some(Ok(DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Refused(AcceptOutcome::NotFound),
            }))
            | Some(Ok(DraftOutcome::DraftNotFound)) => {
                return ToolResult::error(json!({ "error": NOT_FOUND }));
            }
            Some(Ok(DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Refused(AcceptOutcome::ReconciliationRequired { reason }),
            })) => {
                return ToolResult::error(json!({
                    "error": "Working Plan acceptance requires required-unit decisions",
                    "reconciliation": { "kind": "reconciliation_required", "reason": reason },
                }));
            }
            Some(Ok(DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Refused(refused),
            })) => {
                return ToolResult::error(json!({
                    "error": "Working Plan cannot be accepted",
                    "preview_result": refused,
                }));
            }
            Some(Ok(DraftOutcome::ProfileNotFound)) => {
                return ToolResult::error(json!({ "error": "Build not found" }));
            }
            _ => {
                return ToolResult::error(
                    json!({ "error": "Working Plan preview is unavailable" }),
                );
            }
        };
        // Accept refuses running work, and production while this tool never
        // asks for the checkoff remap, so neither is proposed.
        if let AcceptPreview::Changed { changes, .. } = preview.as_ref()
            && (!changes.blocked_by.is_empty() || changes.production.is_some())
        {
            return ToolResult::error(json!({
                "error": "Working Plan cannot be accepted",
                "preview": preview,
            }));
        }
        let (AcceptPreview::NoChange { revision, .. } | AcceptPreview::Changed { revision, .. }) =
            preview.as_ref();
        let mut revision = json!(revision);
        if let Some(fields) = revision.as_object_mut() {
            fields.remove("draft_id");
        }
        let params =
            json!({ "plan_id": plan.get(), "draft_id": draft.get(), "revision": revision });
        let Value::Object(params) = params else {
            unreachable!("json! object literal")
        };
        let Ok(plan_id) = i64::try_from(plan.get()) else {
            return ToolResult::error(json!({ "error": "plan_id required" }));
        };
        let action =
            ProposedAction::new(PROPOSE, plan_id, "apply plan draft".into(), SUMMARY, params);
        self.session.set_pending(action.clone());
        let mut content: Value =
            serde_json::from_str(&action.proposed_content()).unwrap_or_default();
        content["preview"] = json!(preview);
        ToolResult::ok(content.to_string())
    }

    /// Accepts the revision `action` proposed, keyed by the action id. A
    /// refusal or failure leaves the action pending, as Node does.
    pub(super) async fn confirm_accept(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
    ) -> ToolResult {
        let Reads { drafts, wait, .. } = self.reads.clone();
        let params = Value::Object(action.params.clone());
        let parts = (
            positive_id(&params, "plan_id"),
            positive_id(&params, "draft_id"),
            serde_json::from_value::<ApplyRequest>(params["revision"].clone()),
        );
        let (Some(plan), Some(draft), Ok(request)) = parts else {
            self.session.set_pending(action);
            return ToolResult::error(json!({ "ok": false, "detail": NOT_FOUND }));
        };
        if let Err(refusal) = self.guard(plan, draft, &presented).await {
            self.session.set_pending(action);
            return ToolResult::error(json!({ "ok": false, "detail": refusal.detail() }));
        }
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
            Ok(Ok(
                outcome @ (AcceptOutcome::Applied { .. }
                | AcceptOutcome::Existing { .. }
                | AcceptOutcome::AlreadyApplied { .. }),
            )) => {
                let kind = json!(outcome)["kind"].clone();
                let (AcceptOutcome::Applied { receipt }
                | AcceptOutcome::Existing { receipt }
                | AcceptOutcome::AlreadyApplied { receipt }) = outcome
                else {
                    unreachable!("matched above")
                };
                let receipt = camel_case(json!(receipt));
                let planning_phase = json!({
                    "kind": "applied",
                    "draft_id": receipt["draftId"],
                    "revision_id": receipt["revisionId"],
                });
                let apply_result = json!({ "kind": kind, "receipt": receipt });
                let build_workflow = self.workflow(plan, credential).await;
                ToolResult::ok(pretty(&json!({
                    "ok": true,
                    "result": {
                        "apply_result": apply_result,
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
                self.session.set_pending(action);
                ToolResult::error(json!({
                    "ok": false,
                    "detail": format!("Working Plan acceptance failed: {kind}"),
                    "result": { "apply_result": refused },
                }))
            }
            _ => {
                self.session.set_pending(action);
                ToolResult::error(
                    json!({ "ok": false, "detail": "Working Plan acceptance is unavailable" }),
                )
            }
        }
    }

    /// The Accept preview. When it needs a Required-unit reconciliation and the
    /// draft has none selected, the automatic one is selected and the preview
    /// read again, as Node's `prepareForApply` does; `Err` is that selection's
    /// refusal and `None` a failed read.
    async fn preview(
        &self,
        plan: PositiveId,
        draft: PositiveId,
        presented: PresentedCredential,
    ) -> Option<Result<DraftOutcome, WorkspaceOutcome>> {
        let Reads { drafts, wait, .. } = self.reads.clone();
        let read = tokio::task::spawn_blocking(move || -> Result<Result<_, WorkspaceOutcome>> {
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
            if !matches!(
                first,
                DraftOutcome::AcceptPreview {
                    preview: PreviewOutcome::Refused(AcceptOutcome::ReconciliationRequired { .. }),
                }
            ) {
                return Ok(Ok(first));
            }
            let DraftOutcome::Read { draft: document } = drafts.execute(
                credential(),
                plan,
                DraftRequest::Read { draft_id: draft },
                flag(),
                wait,
            )?
            else {
                return Ok(Ok(first));
            };
            if document.has_required_unit_selection() {
                return Ok(Ok(first));
            }
            let command = ReconciliationCommand::automatic(
                plan,
                draft,
                document.identity().snapshot_digest(),
                credential(),
            )?;
            match drafts.reconciliation().reconcile(command, flag(), wait)? {
                WorkspaceOutcome::Ready { .. } => preview().map(Ok),
                refused => Ok(Err(refused)),
            }
        })
        .await;
        read.ok()?.ok()
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

    /// The Build workflow Node returns after Accept, or `null`.
    async fn workflow(&self, plan: PositiveId, presented: PresentedCredential) -> Value {
        let Reads { workflow, wait, .. } = self.reads.clone();
        let read = tokio::task::spawn_blocking(move || {
            workflow.read(
                presented.into_credential(),
                Request::Build(Some(plan)),
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        match read {
            Ok(Ok(Outcome::Workspace(workspace))) => json!(workspace),
            _ => Value::Null,
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
