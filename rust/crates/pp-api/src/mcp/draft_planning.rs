//! Reviewed draft edits, planning rebuilds, and verified filament assignments.
use super::*;
use pp_services::mcp_domain::ProposedAction;
use pp_storage::build_graph::{BuildCommand, BuildOutcome, mcp::Request};

pub(super) const TOOLS: [&str; 3] = [
    "propose_edit_plan_draft_parts",
    "propose_rebuild_plan",
    "propose_assign_role_filament",
];

async fn execute(reads: &Reads, presented: PresentedCredential, request: Request) -> Value {
    let Reads { builds, wait, .. } = reads.clone();
    match tokio::task::spawn_blocking(move || {
        builds.execute(
            presented.into_credential(),
            BuildCommand::Mcp(request),
            Arc::new(AtomicBool::new(false)),
            wait,
        )
    })
    .await
    {
        Ok(Ok(BuildOutcome::Mcp(value))) => value,
        _ => json!({"error":"Build planning is unavailable"}),
    }
}
fn returned(value: Value) -> ToolResult {
    let failed = value["ok"] == false || value.get("error").is_some();
    ToolResult {
        text: if value.get("error").is_some() {
            value.to_string()
        } else {
            serde_json::to_string_pretty(&value).unwrap_or_default()
        },
        is_error: failed,
    }
}
impl ProductMcp {
    pub(super) async fn propose_draft_planning(
        &self,
        tool: &str,
        args: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let edit = tool == TOOLS[0];
        let build = reads::as_int(args.get("plan_id"));
        let draft = reads::as_int(args.get("draft_id"));
        let Some(build) = build.filter(|_| !edit || draft.is_some()) else {
            return ToolResult::error(
                json!({"error":if edit {"plan_id and draft_id required"} else {"plan_id required"}}),
            );
        };
        let mut params = args.clone();
        if edit {
            params["draft_id"] = json!(draft);
        }
        let result = execute(
            &self.reads,
            presented,
            Request::new(build, tool.into(), params, None),
        )
        .await;
        if result.get("error").is_some() {
            return returned(result);
        }
        let params = if edit {
            match super::draft_parts::proposal(
                build,
                draft.unwrap_or_default(),
                &args.as_object().cloned().unwrap_or_default(),
                &result["draft"],
            ) {
                Ok(params) => params,
                Err(detail) => return ToolResult::error(json!({"error":detail})),
            }
        } else {
            args.clone()
        };
        let label = if edit {
            "edit plan draft parts".into()
        } else {
            tool.trim_start_matches("propose_").replace('_', " ")
        };
        let summary = if edit {
            "Apply the reviewed part inclusion and quantity changes."
        } else {
            "Apply the confirmed Build planning change."
        };
        let mut action = ProposedAction::new(
            tool,
            build,
            label,
            summary,
            params.as_object().cloned().unwrap_or_default(),
        );
        action.confirmation_guard = result.get("guard").cloned();
        self.session.set_pending(action.clone());
        ToolResult::ok(action.proposed_content())
    }

    pub(super) async fn confirm_draft_planning(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
    ) -> ToolResult {
        let reads = self.reads.clone();
        let session = self.session.clone();
        let backup = action.clone();
        let recovery = session.clone();
        // RPC cancellation must not lose a reserved proposal or interrupt its commit.
        tokio::spawn(async move {
            let mut request = Request::new(
                action.plan_id,
                action.action_type.clone(),
                Value::Object(action.params.clone()),
                Some(action.to_json()),
            );
            request.confirmation_guard = action.confirmation_guard.clone();
            if action.action_type == TOOLS[2] {
                request.verify_only = true;
                let checked = execute(&reads, presented.clone(), request.clone()).await;
                if checked["ok"] == false || checked.get("error").is_some() {
                    session.set_pending(action);
                    return returned(checked);
                }
                request.inventory_proof = if checked["catalog"].is_object() {
                    Some(checked["catalog"].clone())
                } else if let (Some(reader), Some(id)) =
                    (&reads.filament_reader, checked["filament_id"].as_i64())
                {
                    reader.resolve(checked["integration"].clone(), id).await
                } else {
                    None
                };
                request.verify_only = false;
            }
            let result = execute(&reads, presented, request).await;
            if result["ok"] != true {
                session.set_pending(action)
            }
            returned(result)
        })
        .await
        .unwrap_or_else(|_| {
            recovery.set_pending(backup);
            ToolResult::error(json!({"error":"Build planning is unavailable"}))
        })
    }
}
