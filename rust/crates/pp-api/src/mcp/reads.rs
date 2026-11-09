//! MCP read tools on Rust storage (GRE-441): `list_plans`,
//! `get_plan_decisions` and `list_plan_snapshots`.
//!
//! Each keeps Node's name, schema and output text (`assistant/tools.ts`).
//! Every read runs with the caller's API key, so storage resolves the tenant
//! from that key: a Build of another tenant reads as missing. Like Node with
//! no `PRINT_PARTNER_MCP_PLAN_ID`, a `plan_id` that names no owned Build
//! answers `plan_id required`.

use super::ProductMcp;
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::ToolResult;
use pp_storage::build_graph::{BuildCommand, BuildOutcome, PositiveId};
use pp_storage::read_model::planning::{Outcome, Query, Request};
use pp_storage::read_model::views::js_trim;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

/// The read tools this module answers.
pub(super) const TOOLS: [&str; 3] = ["list_plans", "get_plan_decisions", "list_plan_snapshots"];
/// Node's `get_plan_decisions` default `limit`.
const DECISION_LIMIT: i64 = 40;
const PLAN_REQUIRED: &str = "plan_id required";

/// Node's `asInt`: a finite number, or a non-blank string `Number()` reads
/// as finite, truncated toward zero. Saturates outside the `i64` range.
fn as_int(raw: Option<&Value>) -> Option<i64> {
    let value = match raw? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => {
            let text = js_trim(text);
            // Digits only (no sign), summed as a double like `Number()`.
            let radix = |digits: &str, radix: u32| {
                (!digits.is_empty()).then_some(())?;
                digits.chars().try_fold(0.0, |sum: f64, digit| {
                    Some(sum * f64::from(radix) + f64::from(digit.to_digit(radix)?))
                })
            };
            match text.get(..2) {
                _ if text.is_empty() => return None,
                Some("0x" | "0X") => radix(&text[2..], 16)?,
                Some("0o" | "0O") => radix(&text[2..], 8)?,
                Some("0b" | "0B") => radix(&text[2..], 2)?,
                // Rust also reads `inf` and `nan`; the finite check drops both.
                _ => text.parse().ok()?,
            }
        }
        _ => return None,
    };
    value.is_finite().then(|| value.trunc() as i64)
}

fn error(message: &str) -> ToolResult {
    ToolResult::error(json!({ "error": message }))
}

impl ProductMcp {
    /// One of [`TOOLS`], read with the caller's key.
    pub(super) async fn read_tool(
        &self,
        name: &str,
        arguments: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        if name == "list_plans" {
            return self.list_plans(presented).await;
        }
        let Some(build) = as_int(arguments.get("plan_id"))
            .and_then(|id| u64::try_from(id).ok())
            .and_then(|id| PositiveId::new(id).ok())
        else {
            return error(PLAN_REQUIRED);
        };
        let query = if name == "get_plan_decisions" {
            Query::LatestDecisions {
                limit: as_int(arguments.get("limit")).unwrap_or(DECISION_LIMIT),
            }
        } else {
            Query::Snapshots
        };
        let planning = self.reads.planning.clone();
        let wait = self.reads.wait;
        let read = tokio::task::spawn_blocking(move || {
            planning.read(
                presented.into_credential(),
                Request {
                    build: Some(build),
                    query,
                },
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        let plan_id = build.get();
        match read {
            Ok(Ok(Outcome::Decisions(list))) => ToolResult::ok(
                json!({ "plan_id": plan_id, "decisions": list.decisions }).to_string(),
            ),
            Ok(Ok(Outcome::Snapshots(snapshots))) => {
                ToolResult::ok(json!({ "plan_id": plan_id, "snapshots": snapshots }).to_string())
            }
            Ok(Ok(Outcome::MissingBuild)) => error(PLAN_REQUIRED),
            _ if name == "get_plan_decisions" => error("Plan decisions are unavailable"),
            _ => error("Plan snapshots are unavailable"),
        }
    }

    /// `list_plans`: every Build of the caller's tenant, by name, as
    /// `{id, name, part_count, build_stale}`.
    async fn list_plans(&self, presented: PresentedCredential) -> ToolResult {
        let builds = self.reads.builds.clone();
        let wait = self.reads.wait;
        let read = tokio::task::spawn_blocking(move || {
            builds.execute(
                presented.into_credential(),
                BuildCommand::List,
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        let Ok(Ok(BuildOutcome::Listed { profiles })) = read else {
            return error("Plans are unavailable");
        };
        let plans: Vec<Value> = profiles
            .iter()
            .map(|plan| {
                json!({
                    "id": plan.id,
                    "name": plan.name,
                    "part_count": plan.part_count,
                    "build_stale": plan.build_stale,
                })
            })
            .collect();
        ToolResult::ok(json!({ "plans": plans }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::as_int;
    use serde_json::json;

    #[test]
    fn as_int_follows_node() {
        for (raw, want) in [
            (json!(3), Some(3)),
            (json!(3.9), Some(3)),
            (json!(-2.5), Some(-2)),
            (json!("4"), Some(4)),
            (json!(" 5 "), Some(5)),
            (json!("0x10"), Some(16)),
            (json!("0x+1"), None),
            (json!("0x"), None),
            (json!("0b102"), None),
            (json!("0x10000000000000000"), Some(i64::MAX)),
            (json!("1e2"), Some(100)),
            (json!("2.7"), Some(2)),
            (json!(""), None),
            (json!("  "), None),
            (json!("x"), None),
            (json!("inf"), None),
            (json!("Infinity"), None),
            (json!(true), None),
            (json!(null), None),
            (json!([1]), None),
        ] {
            assert_eq!(as_int(Some(&raw)), want, "{raw}");
        }
        assert_eq!(as_int(None), None);
    }
}
