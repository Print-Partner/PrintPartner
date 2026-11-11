//! MCP progress read tools on Rust storage (GRE-1297): `get_remaining` and
//! `get_printer_checkoff`.
//!
//! Each keeps Node's name, schema and output text (`assistant/tools.ts`) and
//! reads with the caller's API key, so storage resolves the tenant from that
//! key. A Build of another tenant reads as `Plan not found`, and another
//! tenant's links and prints are never read.

use super::ProductMcp;
use super::reads::as_int;
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::ToolResult;
use pp_storage::checkoff_progress::{Body, PrinterCheckoffFilter, Request, Response};
use pp_storage::read_model::{AcceptedRead, Profile, views::js_trim};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

/// The tools this module answers.
pub(super) const TOOLS: [&str; 2] = ["get_remaining", "get_printer_checkoff"];
const STATES: [&str; 5] = [
    "watching",
    "awaiting_verify",
    "host_failed",
    "dismissed",
    "verified",
];
const MAX_SAFE_ID: i64 = 9_007_199_254_740_991;

fn error(message: &str) -> ToolResult {
    ToolResult::error(json!({ "error": message }))
}

/// `typeof value === "string" ? value.trim() : ""`.
fn trimmed(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(|text| js_trim(text).to_owned())
        .unwrap_or_default()
}

impl ProductMcp {
    /// One of [`TOOLS`], read with the caller's key.
    pub(super) async fn progress_tool(
        &self,
        name: &str,
        arguments: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        if name == "get_remaining" {
            return self.remaining(arguments, presented).await;
        }
        let state = trimmed(arguments.get("state"));
        if !state.is_empty() && !STATES.contains(&state.as_str()) {
            return error("invalid checkoff state");
        }
        let filter = PrinterCheckoffFilter {
            plan_id: as_int(arguments.get("plan_id")),
            state,
            integration_id: trimmed(arguments.get("integration_id")),
        };
        let progress = self.reads.progress.clone();
        let wait = self.reads.wait;
        let read = tokio::task::spawn_blocking(move || {
            progress.apply(
                presented.into_credential(),
                Request::PrinterCheckoff(filter),
                &AtomicBool::new(false),
                wait,
            )
        })
        .await;
        match read {
            Ok(Ok(Response {
                status: 200,
                body: Body::PrinterCheckoff(body),
            })) => ToolResult::ok(body.to_string()),
            _ => error("Printer Checkoff is unavailable"),
        }
    }

    /// `get_remaining`: Accepted Plan progress for one Build, as Node's tool
    /// summarizes it. Node does not check ownership before the read, so any
    /// number that names no Build of the key's tenant reads as missing.
    async fn remaining(&self, arguments: &Value, presented: PresentedCredential) -> ToolResult {
        let Some(plan_id) = as_int(arguments.get("plan_id")) else {
            return error("plan_id required");
        };
        if !(1..=MAX_SAFE_ID).contains(&plan_id) {
            return error("Plan not found");
        }
        let accepted = self.reads.accepted.clone();
        let wait = self.reads.wait;
        let read = tokio::task::spawn_blocking(move || {
            accepted.read(
                presented.into_credential(),
                &[plan_id],
                &AtomicBool::new(false),
                wait,
            )
        })
        .await;
        let Ok(Ok(mut batch)) = read else {
            return error("Internal Server Error");
        };
        let accepted = match batch.builds.pop().map(|build| build.accepted) {
            None | Some(AcceptedRead::Missing) => return error("Plan not found"),
            Some(AcceptedRead::IntegrityFailure { .. }) => {
                return error("Accepted Plan data is inconsistent");
            }
            Some(AcceptedRead::CompatibilityDirty) => {
                return error("Accepted Plan requires compatibility repair");
            }
            Some(AcceptedRead::Uninitialized) => {
                return error("Accepted Plan operational state is not initialized");
            }
            Some(accepted) => accepted,
        };
        let (profile, parts): (&Profile, Vec<_>) = match &accepted {
            AcceptedRead::Ready { snapshot } => (
                &snapshot.profile,
                snapshot.parts.iter().filter(|part| part.included).collect(),
            ),
            AcceptedRead::Empty { profile } => (profile, Vec::new()),
            _ => return error("Internal Server Error"),
        };
        let units = parts.iter().flat_map(|part| &part.units);
        let total = units.clone().count();
        let remaining = units.filter(|unit| !unit.completed).count();
        let printed = total - remaining;
        let fully = parts
            .iter()
            .filter(|part| part.units.iter().all(|unit| unit.completed))
            .count();
        let percent = if total == 0 {
            0.0
        } else {
            ((printed as f64 / total as f64) * 100.0)
                .floor()
                .clamp(0.0, 100.0)
        };
        let archived = profile
            .archived_at
            .as_deref()
            .is_some_and(|at| !at.is_empty());
        ToolResult::ok(
            json!({
                "plan_id": plan_id,
                "plan_name": profile.name,
                "archived_at": profile.archived_at,
                "summary": format!(
                    "{fully}/{} parts fully printed \u{b7} {printed}/{total} units",
                    parts.len()
                ),
                "printed_units": printed,
                "total_units": total,
                "remaining_units": remaining,
                "percent": percent as u64,
                "can_archive": total > 0 && remaining == 0 && !archived,
                "part_count": parts.len(),
            })
            .to_string(),
        )
    }
}
