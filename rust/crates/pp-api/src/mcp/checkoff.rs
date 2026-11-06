//! MCP `propose_verify_printer_checkoff` on Rust storage (GRE-1170).
//!
//! The tool keeps Node's name, schema and propose step. `confirm_apply` runs
//! the proposal as a one-bed Checkoff burst through
//! `pp_storage::checkoff_progress`, the same writer transaction as
//! `POST /printer-checkoff/verify-batch`, under the idempotency key
//! `mcp:<action_id>`. The action id is the operation id: confirming an action
//! this session no longer holds (already applied, or proposed before a
//! restart) answers from the stored burst instead of applying again.

use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::{DomainPort, FarmAccess, ProposedAction, ToolInvocation};
use pp_storage::checkoff_progress::{Body, ProgressClient, Request, Response};
use serde_json::{Map, Value, json};
use std::{
    sync::{Mutex, PoisonError, atomic::AtomicBool},
    time::Duration,
};

pub(super) const VERIFY: &str = "propose_verify_printer_checkoff";
/// Prefix of the burst idempotency key a confirmed proposal applies under.
pub(super) const KEY_PREFIX: &str = "mcp:";
const UNAVAILABLE: &str = "Checkoff is unavailable";

/// Product tools on storage; every other tool still has no Rust port.
pub(super) struct CheckoffDomain {
    progress: ProgressClient,
    wait: Duration,
    /// The API key of the tool call in progress. `ProductMcp` sets it around
    /// each call and runs one call per session at a time.
    caller: Mutex<Option<PresentedCredential>>,
}

impl CheckoffDomain {
    pub(super) fn new(progress: ProgressClient, wait: Duration) -> Self {
        Self {
            progress,
            wait,
            caller: Mutex::new(None),
        }
    }

    pub(super) fn set_caller(&self, caller: Option<PresentedCredential>) {
        *self.caller.lock().unwrap_or_else(PoisonError::into_inner) = caller;
    }

    fn run(&self, request: Request) -> Result<Response, String> {
        let caller = self
            .caller
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or("MCP request has no resolved API key")?;
        self.progress
            .apply(
                caller.into_credential(),
                request,
                &AtomicBool::new(false),
                self.wait,
            )
            .map_err(|_| UNAVAILABLE.to_owned())
    }

    /// Node's propose step: the link must exist and await verification.
    fn propose(&self, args: &Map<String, Value>) -> Result<ToolInvocation, String> {
        let error = |message: &str| {
            Ok(ToolInvocation {
                content: json!({ "error": message }).to_string(),
                proposed_action: None,
            })
        };
        let link_id = match args.get("link_id") {
            Some(Value::String(id)) => {
                id.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            }
            _ => "",
        };
        let decisions = args
            .get("decisions")
            .filter(|d| d.as_array().is_some_and(|d| !d.is_empty()));
        let (false, Some(decisions)) = (link_id.is_empty(), decisions) else {
            return error("link_id and decisions are required");
        };
        let link = match self.run(Request::CheckoffLink {
            id: link_id.to_owned(),
        })? {
            Response {
                status: 200,
                body: Body::Link { link },
            } => link,
            Response { status: 404, .. } => return error("Checkoff link not found"),
            _ => return Err(UNAVAILABLE.into()),
        };
        if link["state"] != "awaiting_verify" {
            return error("Checkoff link is not awaiting verification");
        }
        let mut params = Map::new();
        params.insert("link_id".into(), link_id.into());
        params.insert("decisions".into(), decisions.clone());
        let action = ProposedAction::new(
            VERIFY,
            link["profile_id"].as_i64().unwrap_or(0),
            format!(
                "verify printer checkoff {}",
                link["filename"].as_str().unwrap_or_default()
            ),
            "Apply the reviewed confirmed or rejected unit decisions.",
            params,
        );
        Ok(ToolInvocation {
            content: action.proposed_content(),
            proposed_action: Some(action),
        })
    }
}

impl DomainPort for CheckoffDomain {
    fn farm_access(&self) -> FarmAccess {
        FarmAccess::Allowed
    }

    fn invoke_tool(&self, name: &str, args: &Map<String, Value>) -> Result<ToolInvocation, String> {
        if name == VERIFY {
            return self.propose(args);
        }
        Err(format!("{name} has no Rust port yet"))
    }

    fn apply_action(&self, action: &ProposedAction) -> Result<Value, String> {
        if action.action_type != VERIFY {
            return Ok(
                json!({ "ok": false, "detail": format!("Unknown action type: {}", action.action_type) }),
            );
        }
        let body = json!({
            "idempotency_key": format!("{KEY_PREFIX}{}", action.id),
            "beds": [{ "link_id": action.params.get("link_id"), "decisions": action.params.get("decisions") }],
        });
        Ok(match self.run(Request::VerifyBatch { body })? {
            Response {
                status: 200,
                body: Body::Receipt(receipt),
            } => json!({ "ok": true, "result": receipt }),
            Response {
                status,
                body: Body::Failure { detail, .. },
            } => json!({ "ok": false, "status": status, "detail": detail }),
            _ => return Err(UNAVAILABLE.into()),
        })
    }

    fn reconcile(&self, action_id: &str) -> Option<Value> {
        let Response {
            status: 200,
            body: Body::Receipt(burst),
        } = self
            .run(Request::BurstStatus {
                key: format!("{KEY_PREFIX}{action_id}"),
            })
            .ok()?
        else {
            return None;
        };
        let (ok, detail) = match burst["state"].as_str() {
            Some("applied") => (true, "This action was already applied"),
            Some("undone") => (false, "This action was applied and later undone"),
            _ => (
                false,
                "This action was applied; its receipt is too old to return",
            ),
        };
        Some(json!({
            "ok": ok,
            "reconciled": true,
            "action_id": action_id,
            "detail": detail,
            "result": burst["receipt"],
            "undone_at": burst["undone_at"],
        }))
    }
}
