//! `propose_apply_plan_draft` and its `confirm_apply` (GRE-1236). MCP reads
//! the same Accept preview as the Plan tab, proposes exactly the revision it
//! read, and confirm accepts that revision through the same Rust Accept, so a
//! draft header saved in the Plan tab is copied to the Build once.

use super::*;
use pp_services::mcp_domain::ProposedAction;
use pp_storage::plan_publication::{
    ApplyRequest, Outcome as AcceptOutcome, PublicationCommand,
    preview::{AcceptPreview, PreviewOutcome},
};
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
        let Reads { drafts, wait, .. } = self.reads.clone();
        let read = tokio::task::spawn_blocking(move || {
            drafts.execute(
                presented.into_credential(),
                plan,
                DraftRequest::AcceptPreview { draft_id: draft },
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        let preview = match read {
            Ok(Ok(DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Ready(preview),
            })) => preview,
            Ok(Ok(DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Refused(AcceptOutcome::NotFound),
            }))
            | Ok(Ok(DraftOutcome::DraftNotFound)) => {
                return ToolResult::error(json!({ "error": NOT_FOUND }));
            }
            Ok(Ok(DraftOutcome::AcceptPreview {
                preview: PreviewOutcome::Refused(refused),
            })) => {
                return ToolResult::error(json!({
                    "error": "Working Plan cannot be accepted",
                    "preview_result": refused,
                }));
            }
            Ok(Ok(DraftOutcome::ProfileNotFound)) => {
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
        let key = action.id.clone();
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
                AcceptOutcome::Applied { receipt }
                | AcceptOutcome::Existing { receipt }
                | AcceptOutcome::AlreadyApplied { receipt },
            )) => ToolResult::ok(pretty(
                &json!({ "ok": true, "result": { "apply_result": receipt } }),
            )),
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
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
