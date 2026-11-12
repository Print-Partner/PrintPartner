//! Confirmation-gated Build inputs and planning brief updates on Rust storage.
use super::{checkoff::CheckoffDomain, reads::as_int};
use pp_services::mcp_domain::{ProposedAction, ToolInvocation};
use pp_storage::build_graph::{BuildCommand, BuildOutcome, mcp::Request};
use serde_json::{Map, Value, json};
use std::sync::{Arc, atomic::AtomicBool};
pub(super) const TOOLS: [&str; 2] = ["add_addon", "propose_update_build_brief"];
impl CheckoffDomain {
    pub(super) fn planning_run(&self, request: Request) -> Result<Value, String> {
        match self.builds.execute(
            self.caller()?.into_credential(),
            BuildCommand::Mcp(request),
            Arc::new(AtomicBool::new(false)),
            self.wait,
        ) {
            Ok(BuildOutcome::Mcp(value)) => Ok(value),
            _ => Err("Build planning is unavailable".into()),
        }
    }
    pub(super) fn planning_propose(
        &self,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let error = |message: &str| {
            Ok(ToolInvocation {
                content: json!({"error":message}).to_string(),
                proposed_action: None,
            })
        };
        let build = as_int(args.get("plan_id"));
        let addon = name == "add_addon";
        let source = args
            .get("source_name")
            .and_then(Value::as_str)
            .map(pp_storage::read_model::views::js_trim)
            .unwrap_or_default();
        let Some(build) = build else {
            return error(if addon {
                "plan_id and source_name required"
            } else {
                "plan_id required"
            });
        };
        if addon && source.is_empty() {
            return error("plan_id and source_name required");
        }
        let params = Value::Object(args.clone());
        let result = self.planning_run(Request::new(build, name.into(), params.clone(), None))?;
        if result.get("error").is_some() {
            return Ok(ToolInvocation {
                content: result.to_string(),
                proposed_action: None,
            });
        }
        let check = if addon {
            let names = result["layers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["project_name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            pp_services::mcp_domain::addon::check(
                result["source_name"].as_str().unwrap_or(source),
                &names,
                &self.builds.mcp_addon_documents(),
            )
        } else {
            Value::Null
        };
        let (label, summary, params) = if addon {
            let canonical = result["source_name"].as_str().unwrap_or(source);
            let (summary, params) = addon_fields(canonical, &check);
            if result["dismissed"] == true {
                return Ok(ToolInvocation { content:json!({"error":"user_dismissed","detail":"User dismissed this action fingerprint on this plan. Ask before re-proposing the same change.","fingerprint":format!("add_addon|source_name={canonical}"),"action_type":"add_addon"}).to_string(),proposed_action:None});
            }
            (format!("Add addon {canonical}"), summary, params)
        } else {
            (
                "update build brief".into(),
                "Apply the confirmed Build planning change.".into(),
                params,
            )
        };
        let action = ProposedAction::new(
            name,
            build,
            label,
            &summary,
            params.as_object().cloned().unwrap_or_default(),
        );
        let mut content: Value =
            serde_json::from_str(&action.proposed_content()).map_err(|e| e.to_string())?;
        if addon && let (Some(content), Some(check)) = (content.as_object_mut(), check.as_object())
        {
            content.extend(check.clone());
        }
        Ok(ToolInvocation {
            content: content.to_string(),
            proposed_action: Some(action),
        })
    }
    pub(super) fn planning_apply(&self, action: &ProposedAction) -> Result<Value, String> {
        let params = Value::Object(action.params.clone());
        self.planning_run(Request::new(
            action.plan_id,
            action.action_type.clone(),
            params,
            Some(action.to_json()),
        ))
    }
}
fn addon_fields(canonical: &str, check: &Value) -> (String, Value) {
    let warnings = check["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["severity"] == "warning")
        .filter_map(|w| w["message"].as_str())
        .take(4)
        .collect::<Vec<_>>();
    let mut summary = format!("Add “{canonical}” as an addon layer.");
    if !warnings.is_empty() {
        summary.push_str(&format!(" Warnings: {}", warnings.join(" ")));
    }
    let excludes = check["suggested_excludes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut params = json!({"source_name":canonical});
    if !excludes.is_empty() {
        summary.push_str(&format!(
            " Suggested excludes: {}.",
            excludes
                .iter()
                .filter_map(Value::as_str)
                .take(6)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        params["suggested_excludes"] = json!(excludes);
    }
    (summary, params)
}
