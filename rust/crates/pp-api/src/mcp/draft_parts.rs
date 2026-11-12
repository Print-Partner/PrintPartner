//! MCP `propose_edit_plan_draft_parts` and its `confirm_apply` (GRE-1298).
//!
//! Propose reads the draft through the Plan tab's workspace read and writes
//! nothing. Confirm runs `WorkingDraftClient::edit`, the operation the Plan
//! tab's Part edit calls, against the snapshot digest the proposal carries,
//! then adds an `applied_action` entry to the Build's decision log. Both use
//! the request's API key. Refusals carry a [`Code`]; the message is for people.

use super::{
    checkoff::CheckoffDomain,
    positive_id,
    write_result::{Code, Refusal, writer_failure},
};
use pp_services::mcp_domain::{ProposedAction, ToolInvocation};
use pp_storage::WriterFailure;
use pp_storage::auth::AuthFailure;
use pp_storage::build_graph::{BuildCommand, BuildOutcome, DecisionOutcome, PositiveId};
use pp_storage::required_units::{Outcome as EditOutcome, Workspace};
use pp_storage::working_drafts::{
    EditRequest, Failure, Outcome as DraftOutcome, Request as DraftRequest,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, atomic::AtomicBool};

pub(super) const PROPOSE: &str = "propose_edit_plan_draft_parts";
const LABEL: &str = "edit plan draft parts";
const SUMMARY: &str = "Apply the reviewed Part inclusion and quantity changes.";

const ARGUMENTS_REQUIRED: Refusal = Refusal::new(
    Code::ArgumentsRequired,
    "plan_id and draft_id must be positive whole numbers.",
);
const NOT_FOUND: Refusal = Refusal::new(Code::NotFound, "No such Build or draft.");
const NOT_OPEN: Refusal = Refusal::new(Code::NotOpen, "The draft is not open.");
const PARTS_REQUIRED: Refusal =
    Refusal::new(Code::PartsRequired, "parts must list at least one edit.");
const INVALID_EDIT: Refusal = Refusal::new(
    Code::InvalidEdit,
    "Each edit needs a part_id, an optional boolean included, and an optional quantity_override that is null or a whole number. List each Part once.",
);
const PART_NOT_FOUND: Refusal = Refusal::new(Code::PartNotFound, "A part_id is not in the draft.");
const EDIT_EMPTY: Refusal = Refusal::new(
    Code::EditEmpty,
    "Each edit must set included or quantity_override.",
);
const INVALID_DIGEST: Refusal = Refusal::new(
    Code::InvalidDigest,
    "expected_snapshot_digest must be 64 lowercase hex characters.",
);
const OUTCOME_UNKNOWN: Refusal = Refusal::new(
    Code::OutcomeUnknown,
    "The result is unknown. Check the draft before proposing again.",
);
const UNAVAILABLE: Refusal = Refusal::new(
    Code::Unavailable,
    "Storage is busy. Nothing was written; try again.",
);

/// One checked Part edit.
struct PartEdit {
    part_id: PositiveId,
    included: Option<bool>,
    /// `Some(None)` clears the override.
    quantity_override: Option<Option<u64>>,
}

impl CheckoffDomain {
    pub(super) fn draft_parts_propose(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let refused = |refusal: Refusal| {
            Ok(ToolInvocation {
                content: refusal.proposal(),
                proposed_action: None,
            })
        };
        let args = Value::Object(args.clone());
        let (Some(plan), Some(draft)) = (
            positive_id(&args, "plan_id"),
            positive_id(&args, "draft_id"),
        ) else {
            return refused(ARGUMENTS_REQUIRED);
        };
        let edits = match part_edits(&args) {
            Ok(edits) => edits,
            Err(refusal) => return refused(refusal),
        };
        let expected = match digest(args.get("expected_snapshot_digest")) {
            Ok(expected) => expected,
            Err(refusal) => return refused(refusal),
        };
        let workspace = match self.workspace(plan, draft)? {
            Ok(workspace) => workspace,
            Err(refusal) => return refused(refusal),
        };
        if workspace.draft.state != "open" {
            return refused(NOT_OPEN);
        }
        let known: BTreeSet<i64> = workspace
            .parts
            .iter()
            .map(|part| part.draft_part_id)
            .collect();
        if edits
            .iter()
            .any(|edit| !known.contains(&(edit.part_id.get() as i64)))
        {
            return refused(PART_NOT_FOUND);
        }
        let digest = expected.unwrap_or(workspace.draft.snapshot_digest);
        let params = params(plan, draft, &digest, &edits);
        if let Err(refusal) = edit_request(&params) {
            return refused(refusal);
        }
        let action = ProposedAction::new(PROPOSE, plan.get() as i64, LABEL.into(), SUMMARY, params);
        Ok(ToolInvocation {
            content: action.proposed_content(),
            proposed_action: Some(action),
        })
    }

    pub(super) fn draft_parts_apply(&self, action: &ProposedAction) -> Result<Value, String> {
        let params = Value::Object(action.params.clone());
        let (Some(plan), Some(draft)) = (
            positive_id(&params, "plan_id"),
            positive_id(&params, "draft_id"),
        ) else {
            return Ok(ARGUMENTS_REQUIRED.confirmation());
        };
        let request = match edit_request(&action.params) {
            Ok(request) => request,
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        let edited = self.drafts.edit(
            self.caller()?.into_credential(),
            plan,
            draft,
            request,
            Arc::new(AtomicBool::new(false)),
            self.wait,
        );
        let workspace = match edited {
            Ok(EditOutcome::Ready { workspace }) => workspace,
            Ok(outcome) => return Ok(edit_refusal(&outcome).confirmation()),
            Err(error) => {
                return Ok(self
                    .failed_edit(plan, draft, action, &error)?
                    .confirmation());
            }
        };
        let result = applied(&workspace, &action.params);
        self.log(plan, action, &result)?;
        Ok(json!({ "ok": true, "result": result }))
    }

    /// The refusal for an edit that returned an error. The edit commits the
    /// Part changes before it selects Required units in a second transaction,
    /// so an error can follow a committed edit. Only a draft whose digest still
    /// matches the proposal is known to be unwritten; anything else is
    /// `outcome_unknown`, which drops the proposal.
    fn failed_edit(
        &self,
        plan: PositiveId,
        draft: PositiveId,
        action: &ProposedAction,
        error: &anyhow::Error,
    ) -> Result<Refusal, String> {
        let refusal = storage_refusal(error);
        if refusal.code == Code::OutcomeUnknown {
            return Ok(refusal);
        }
        let expected = action
            .params
            .get("expected_snapshot_digest")
            .and_then(Value::as_str);
        Ok(match self.workspace(plan, draft)? {
            Ok(workspace) if Some(workspace.draft.snapshot_digest.as_str()) == expected => refusal,
            _ => OUTCOME_UNKNOWN,
        })
    }

    /// The draft as the Plan tab reads it, or the refusal for its absence.
    fn workspace(
        &self,
        plan: PositiveId,
        draft: PositiveId,
    ) -> Result<Result<Workspace, Refusal>, String> {
        let read = self.drafts.execute(
            self.caller()?.into_credential(),
            plan,
            DraftRequest::Workspace { draft_id: draft },
            Arc::new(AtomicBool::new(false)),
            self.wait,
        );
        Ok(match read {
            Ok(DraftOutcome::Workspace { workspace }) => Ok(*workspace),
            Ok(DraftOutcome::ProfileNotFound | DraftOutcome::DraftNotFound) => Err(NOT_FOUND),
            Ok(_) => Err(UNAVAILABLE),
            Err(error) => Err(storage_refusal(&error)),
        })
    }

    /// Adds the applied action to the Build's decision log, as
    /// `POST /plans/{id}/decisions` does. The edit has committed, so a failed
    /// log entry is reported in the server log and does not change the answer.
    fn log(&self, plan: PositiveId, action: &ProposedAction, result: &Value) -> Result<(), String> {
        let body = json!({
            "kind": "applied_action",
            "actor": "assistant",
            "action_type": action.action_type,
            "params": action.params,
            "label": action.label,
            "summary": action.summary,
            "result": result,
        });
        let logged = self.builds.execute(
            self.caller()?.into_credential(),
            BuildCommand::AppendDecision {
                build: plan,
                body: body.to_string().into_bytes(),
            },
            Arc::new(AtomicBool::new(false)),
            self.wait,
        );
        if !matches!(
            logged,
            Ok(BuildOutcome::Decision(DecisionOutcome::Created(_)))
        ) {
            log::warn!(
                "draft Part edit {} applied; its decision log entry failed",
                action.id
            );
        }
        Ok(())
    }
}

fn part_edits(args: &Value) -> Result<Vec<PartEdit>, Refusal> {
    let rows = match args.get("parts") {
        Some(Value::Array(rows)) if !rows.is_empty() => rows,
        None | Some(Value::Null | Value::Array(_)) => return Err(PARTS_REQUIRED),
        Some(_) => return Err(INVALID_EDIT),
    };
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let part_id = positive_id(row, "part_id").ok_or(INVALID_EDIT)?;
            let included = match row.get("included") {
                None => None,
                Some(Value::Bool(value)) => Some(*value),
                Some(_) => return Err(INVALID_EDIT),
            };
            let quantity_override = match row.get("quantity_override") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(value) => Some(Some(value.as_u64().ok_or(INVALID_EDIT)?)),
            };
            if included.is_none() && quantity_override.is_none() {
                return Err(EDIT_EMPTY);
            }
            if !seen.insert(part_id.get()) {
                return Err(INVALID_EDIT);
            }
            Ok(PartEdit {
                part_id,
                included,
                quantity_override,
            })
        })
        .collect()
}

fn digest(value: Option<&Value>) -> Result<Option<String>, Refusal> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text))
            if text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            Ok(Some(text.clone()))
        }
        Some(_) => Err(INVALID_DIGEST),
    }
}

/// The stored proposal: the edits and the digest confirm edits against.
fn params(
    plan: PositiveId,
    draft: PositiveId,
    digest: &str,
    edits: &[PartEdit],
) -> Map<String, Value> {
    let parts: Vec<Value> = edits
        .iter()
        .map(|edit| {
            let mut row = json!({ "part_id": edit.part_id.get() });
            if let Some(included) = edit.included {
                row["included"] = json!(included);
            }
            if let Some(quantity) = edit.quantity_override {
                row["quantity_override"] = json!(quantity);
            }
            row
        })
        .collect();
    let mut params = Map::new();
    params.insert("plan_id".into(), plan.get().into());
    params.insert("draft_id".into(), draft.get().into());
    params.insert("expected_snapshot_digest".into(), digest.into());
    params.insert("parts".into(), parts.into());
    params
}

/// The Plan tab's `EditRequest` for the stored proposal: one decision per
/// field value, listing the Parts it applies to.
fn edit_request(params: &Map<String, Value>) -> Result<EditRequest, Refusal> {
    let mut decisions = BTreeMap::<String, Value>::new();
    let rows = params
        .get("parts")
        .and_then(Value::as_array)
        .ok_or(INVALID_EDIT)?;
    for row in rows {
        for (field, kind) in [
            ("included", "set_included"),
            ("quantity_override", "set_quantity_override"),
        ] {
            let Some(value) = row.get(field) else {
                continue;
            };
            let decision = decisions
                .entry(format!("{kind}:{value}"))
                .or_insert_with(|| json!({ "kind": kind, "draft_part_ids": [], "value": value }));
            if let Some(ids) = decision["draft_part_ids"].as_array_mut() {
                ids.push(row["part_id"].clone());
            }
        }
    }
    serde_json::from_value(json!({
        "expected_snapshot_digest": params.get("expected_snapshot_digest"),
        "decisions": decisions.into_values().collect::<Vec<_>>(),
    }))
    .map_err(|_| INVALID_EDIT)
}

/// The edited draft's id, its new snapshot digest and the Parts the proposal named.
fn applied(workspace: &Workspace, params: &Map<String, Value>) -> Value {
    let named: BTreeSet<i64> = params
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row["part_id"].as_i64())
        .collect();
    let parts: Vec<Value> = workspace
        .parts
        .iter()
        .filter(|part| named.contains(&part.draft_part_id))
        .map(|part| {
            json!({
                "part_id": part.draft_part_id,
                "included": part.included,
                "quantity_override": part.quantity_override,
                "quantity_effective": part.quantity_effective,
            })
        })
        .collect();
    json!({
        "draft_id": workspace.draft.draft_id,
        "snapshot_digest": workspace.draft.snapshot_digest,
        "parts": parts,
    })
}

fn edit_refusal(outcome: &EditOutcome) -> Refusal {
    match outcome {
        EditOutcome::ProfileNotFound | EditOutcome::DraftNotFound => NOT_FOUND,
        EditOutcome::DraftChanged { .. } => Refusal::new(
            Code::DraftChanged,
            "The draft changed after the proposal. Propose the edit again.",
        ),
        EditOutcome::BaseChanged { .. } => Refusal::new(
            Code::BaseChanged,
            "The Accepted Plan changed after the proposal. Propose the edit again.",
        ),
        EditOutcome::NotOpen { .. } => NOT_OPEN,
        EditOutcome::AcceptedBaselineRequired => Refusal::new(
            Code::AcceptedBaselineRequired,
            "The Build needs an Accepted Plan before its draft can be edited.",
        ),
        EditOutcome::Ready { .. }
        | EditOutcome::IdempotencyConflict
        | EditOutcome::DomainError { .. } => {
            Refusal::new(Code::EditRefused, "The draft edit was refused.")
        }
    }
}

/// The refusal for a storage error, mapped as the Plan tab's HTTP handler
/// maps it: credential failures first, then the draft client's own failures.
fn storage_refusal(error: &anyhow::Error) -> Refusal {
    if let Some(auth) = error.downcast_ref::<AuthFailure>() {
        return writer_failure(&WriterFailure::Auth(*auth));
    }
    match error.downcast_ref::<Failure>() {
        Some(Failure::OutcomeUnknown) => OUTCOME_UNKNOWN,
        Some(Failure::Cancelled | Failure::QueueFull | Failure::Stopped) => UNAVAILABLE,
        Some(Failure::CorruptOrigin) | None => {
            Refusal::new(Code::Internal, "Storage failed. Nothing was written.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_errors_map_to_the_shared_write_codes() {
        let code = |error: anyhow::Error| storage_refusal(&error).code;
        for (failure, expected) in [
            (AuthFailure::InvalidCredentials, Code::Unauthorized),
            (AuthFailure::CredentialChanged, Code::Unauthorized),
            (AuthFailure::QueueFull, Code::Unavailable),
            (AuthFailure::Stopped, Code::Unavailable),
            (AuthFailure::CommitUnknown, Code::OutcomeUnknown),
        ] {
            assert_eq!(code(anyhow::Error::new(failure)), expected, "{failure:?}");
        }
        for (failure, expected) in [
            (Failure::OutcomeUnknown, Code::OutcomeUnknown),
            (Failure::QueueFull, Code::Unavailable),
            (Failure::Cancelled, Code::Unavailable),
            (Failure::CorruptOrigin, Code::Internal),
        ] {
            let name = format!("{failure:?}");
            assert_eq!(code(anyhow::Error::new(failure)), expected, "{name}");
        }
    }
}
