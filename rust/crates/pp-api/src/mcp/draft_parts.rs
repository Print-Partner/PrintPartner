//! `propose_edit_plan_draft_parts` and its `confirm_apply` (GRE-1298). Propose
//! reads the draft and writes nothing. Confirm runs `WorkingDraftClient::edit`,
//! the operation the Plan tab's Part edit calls, against the snapshot digest the
//! proposal carries, then logs the applied action. Refusals carry a stable
//! `code`; the message is for people and may change.
use super::*;
use pp_services::mcp_domain::ProposedAction;
use pp_storage::build_graph::{BuildCommand, BuildOutcome, DecisionOutcome};
use pp_storage::required_units::{Outcome as WorkspaceOutcome, PartView, Workspace};
use pp_storage::working_drafts::{
    EditRequest, Failure, Outcome as DraftOutcome, Request as DraftRequest,
};
use std::collections::BTreeSet;

pub(super) const PROPOSE: &str = "propose_edit_plan_draft_parts";
const LABEL: &str = "edit plan draft parts";
const SUMMARY: &str = "Apply the reviewed Part inclusion and quantity changes.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    ArgumentsRequired,
    NotFound,
    NotOpen,
    PartsRequired,
    InvalidEdit,
    PartNotFound,
    EditEmpty,
    InvalidDigest,
    DraftChanged,
    BaseChanged,
    AcceptedBaselineRequired,
    EditRefused,
    Unavailable,
    OutcomeUnknown,
}
impl Refusal {
    fn code(self) -> &'static str {
        match self {
            Self::ArgumentsRequired => "arguments_required",
            Self::NotFound => "not_found",
            Self::NotOpen => "not_open",
            Self::PartsRequired => "parts_required",
            Self::InvalidEdit => "invalid_edit",
            Self::PartNotFound => "part_not_found",
            Self::EditEmpty => "edit_empty",
            Self::InvalidDigest => "invalid_digest",
            Self::DraftChanged => "draft_changed",
            Self::BaseChanged => "base_changed",
            Self::AcceptedBaselineRequired => "accepted_baseline_required",
            Self::EditRefused => "edit_refused",
            Self::Unavailable => "unavailable",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::ArgumentsRequired => "plan_id and draft_id are required",
            Self::NotFound => "No such Build or draft",
            Self::NotOpen => "The draft is not open",
            Self::PartsRequired => "parts must list at least one edit",
            Self::InvalidEdit => {
                "Each edit needs a part_id, an optional boolean included and an optional quantity_override that is null or a whole number, with each Part listed once"
            }
            Self::PartNotFound => "A part_id is not in the draft",
            Self::EditEmpty => "Each edit must set included or quantity_override",
            Self::InvalidDigest => "expected_snapshot_digest must be 64 lowercase hex characters",
            Self::DraftChanged => "The draft changed since the proposal; propose again",
            Self::BaseChanged => "The Accepted Plan changed since the proposal; propose again",
            Self::AcceptedBaselineRequired => "The Build has no Accepted Plan to edit against",
            Self::EditRefused => "The draft edit was refused",
            Self::Unavailable => "Draft editing is unavailable",
            Self::OutcomeUnknown => {
                "The result of this edit is unknown. Check the draft before proposing again."
            }
        }
    }
    fn proposal(self) -> ToolResult {
        ToolResult::error(json!({ "error": self.message(), "code": self.code() }))
    }
    fn confirmation(self) -> ToolResult {
        ToolResult::error(json!({ "ok": false, "code": self.code(), "detail": self.message() }))
    }
}

/// One Part edit as the proposal stores it.
struct PartEdit {
    part_id: PositiveId,
    included: Option<bool>,
    quantity_override: Option<Option<u64>>,
}

fn part_edits(arguments: &Value) -> Result<Vec<PartEdit>, Refusal> {
    let rows = match arguments.get("parts") {
        Some(Value::Array(rows)) if !rows.is_empty() => rows,
        Some(Value::Array(_)) | None | Some(Value::Null) => return Err(Refusal::PartsRequired),
        Some(_) => return Err(Refusal::InvalidEdit),
    };
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let part_id = positive_id(row, "part_id").ok_or(Refusal::InvalidEdit)?;
            let included = match row.get("included") {
                None => None,
                Some(Value::Bool(value)) => Some(*value),
                Some(_) => return Err(Refusal::InvalidEdit),
            };
            let quantity_override = match row.get("quantity_override") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(value) => Some(Some(value.as_u64().ok_or(Refusal::InvalidEdit)?)),
            };
            if included.is_none() && quantity_override.is_none() {
                return Err(Refusal::EditEmpty);
            }
            if !seen.insert(part_id.get()) {
                return Err(Refusal::InvalidEdit);
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
            if text.len() == 64
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(Some(text.clone()))
        }
        Some(_) => Err(Refusal::InvalidDigest),
    }
}

/// The stored proposal params: the edits and the digest confirm edits against.
fn params(plan: PositiveId, draft: PositiveId, digest: &str, edits: &[PartEdit]) -> Value {
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
    json!({
        "plan_id": plan.get(),
        "draft_id": draft.get(),
        "expected_snapshot_digest": digest,
        "parts": parts,
    })
}

/// The Plan tab's `EditRequest` for the stored params: one decision per field
/// value, listing the Parts it applies to.
fn edit_request(params: &Value) -> Result<EditRequest, Refusal> {
    let mut decisions = std::collections::BTreeMap::<String, Value>::new();
    for row in params["parts"].as_array().ok_or(Refusal::InvalidEdit)? {
        for (field, kind) in [
            ("included", "set_included"),
            ("quantity_override", "set_quantity_override"),
        ] {
            let Some(value) = row.get(field) else {
                continue;
            };
            decisions
                .entry(format!("{kind}:{value}"))
                .or_insert_with(|| json!({ "kind": kind, "draft_part_ids": [], "value": value }))
                ["draft_part_ids"]
                .as_array_mut()
                .ok_or(Refusal::InvalidEdit)?
                .push(row["part_id"].clone());
        }
    }
    serde_json::from_value(json!({
        "expected_snapshot_digest": params["expected_snapshot_digest"],
        "decisions": decisions.into_values().collect::<Vec<_>>(),
    }))
    .map_err(|_| Refusal::InvalidEdit)
}

fn part(view: &PartView) -> Value {
    json!({
        "part_id": view.draft_part_id,
        "included": view.included,
        "quantity_override": view.quantity_override,
        "quantity_effective": view.quantity_effective,
    })
}

impl ProductMcp {
    /// Checks the edits against the open draft and stores a pending proposal
    /// carrying the draft's snapshot digest. Writes nothing.
    pub(super) async fn propose_draft_parts(
        &self,
        arguments: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let (Some(plan), Some(draft)) = (
            positive_id(arguments, "plan_id"),
            positive_id(arguments, "draft_id"),
        ) else {
            return Refusal::ArgumentsRequired.proposal();
        };
        let (edits, expected) = match (
            part_edits(arguments),
            digest(arguments.get("expected_snapshot_digest")),
        ) {
            (Ok(edits), Ok(expected)) => (edits, expected),
            (Err(refusal), _) | (_, Err(refusal)) => return refusal.proposal(),
        };
        let workspace = match self.workspace(plan, draft, presented).await {
            Ok(workspace) => workspace,
            Err(refusal) => return refusal.proposal(),
        };
        if workspace.draft.state != "open" {
            return Refusal::NotOpen.proposal();
        }
        if edits.iter().any(|edit| {
            !workspace
                .parts
                .iter()
                .any(|part| part.draft_part_id == edit.part_id.get() as i64)
        }) {
            return Refusal::PartNotFound.proposal();
        }
        let digest = expected.unwrap_or(workspace.draft.snapshot_digest);
        let params = params(plan, draft, &digest, &edits);
        if let Err(refusal) = edit_request(&params) {
            return refusal.proposal();
        }
        let Value::Object(params) = params else {
            return Refusal::Unavailable.proposal();
        };
        let action = ProposedAction::new(PROPOSE, plan.get() as i64, LABEL.into(), SUMMARY, params);
        self.session.set_pending(action.clone());
        ToolResult::ok(action.proposed_content())
    }

    /// Applies the proposal through the Plan tab's edit. A refusal keeps the
    /// action pending; an edit whose outcome is unknown drops it, because it
    /// may have committed.
    pub(super) async fn confirm_draft_parts(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
    ) -> ToolResult {
        let params = Value::Object(action.params.clone());
        let (Some(plan), Some(draft)) = (
            positive_id(&params, "plan_id"),
            positive_id(&params, "draft_id"),
        ) else {
            self.session.set_pending(action);
            return Refusal::ArgumentsRequired.confirmation();
        };
        let request = match edit_request(&params) {
            Ok(request) => request,
            Err(refusal) => {
                self.session.set_pending(action);
                return refusal.confirmation();
            }
        };
        let Reads {
            drafts,
            builds,
            wait,
            ..
        } = self.reads.clone();
        let session = self.session.clone();
        // RPC cancellation must not lose a reserved proposal or interrupt its commit.
        tokio::spawn(async move {
            let credential = presented.clone();
            let edited = tokio::task::spawn_blocking(move || {
                drafts.edit(
                    credential.into_credential(),
                    plan,
                    draft,
                    request,
                    Arc::new(AtomicBool::new(false)),
                    wait,
                )
            })
            .await;
            let refusal = match edited {
                Ok(Ok(WorkspaceOutcome::Ready { workspace })) => {
                    let result = applied(&workspace, &params);
                    log(&builds, presented, &action, &result, wait).await;
                    return ToolResult::ok(pretty(&json!({ "ok": true, "result": result })));
                }
                Ok(Ok(WorkspaceOutcome::ProfileNotFound | WorkspaceOutcome::DraftNotFound)) => {
                    Refusal::NotFound
                }
                Ok(Ok(WorkspaceOutcome::DraftChanged { .. })) => Refusal::DraftChanged,
                Ok(Ok(WorkspaceOutcome::BaseChanged { .. })) => Refusal::BaseChanged,
                Ok(Ok(WorkspaceOutcome::NotOpen { .. })) => Refusal::NotOpen,
                Ok(Ok(WorkspaceOutcome::AcceptedBaselineRequired)) => {
                    Refusal::AcceptedBaselineRequired
                }
                Ok(Ok(_)) => Refusal::EditRefused,
                Ok(Err(error)) if matches!(error.downcast_ref(), Some(Failure::OutcomeUnknown)) => {
                    return Refusal::OutcomeUnknown.confirmation();
                }
                _ => Refusal::Unavailable,
            };
            session.set_pending(action);
            refusal.confirmation()
        })
        .await
        .unwrap_or_else(|_| Refusal::OutcomeUnknown.confirmation())
    }

    async fn workspace(
        &self,
        plan: PositiveId,
        draft: PositiveId,
        presented: PresentedCredential,
    ) -> Result<Workspace, Refusal> {
        let Reads { drafts, wait, .. } = self.reads.clone();
        let read = tokio::task::spawn_blocking(move || {
            drafts.execute(
                presented.into_credential(),
                plan,
                DraftRequest::Workspace { draft_id: draft },
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        match read {
            Ok(Ok(DraftOutcome::Workspace { workspace })) => Ok(*workspace),
            Ok(Ok(DraftOutcome::ProfileNotFound | DraftOutcome::DraftNotFound)) => {
                Err(Refusal::NotFound)
            }
            _ => Err(Refusal::Unavailable),
        }
    }
}

/// The edited draft's id, its new snapshot digest and the Parts the proposal named.
fn applied(workspace: &Workspace, params: &Value) -> Value {
    let named: BTreeSet<i64> = params["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["part_id"].as_i64())
        .collect();
    json!({
        "draft_id": workspace.draft.draft_id,
        "snapshot_digest": workspace.draft.snapshot_digest,
        "parts": workspace
            .parts
            .iter()
            .filter(|view| named.contains(&view.draft_part_id))
            .map(part)
            .collect::<Vec<_>>(),
    })
}

/// Records the applied action in the Build's decision log, as
/// `POST /plans/{id}/decisions` does. The edit has committed by now, so a
/// logging failure does not change the answer.
async fn log(
    builds: &BuildGraphClient,
    presented: PresentedCredential,
    action: &ProposedAction,
    result: &Value,
    wait: Duration,
) {
    let Ok(build) = PositiveId::new(action.plan_id as u64) else {
        return;
    };
    let body = json!({
        "kind": "applied_action",
        "actor": "assistant",
        "action_type": action.action_type,
        "params": action.params,
        "label": action.label,
        "summary": action.summary,
        "result": result,
    })
    .to_string()
    .into_bytes();
    let builds = builds.clone();
    let logged = tokio::task::spawn_blocking(move || {
        builds.execute(
            presented.into_credential(),
            BuildCommand::AppendDecision { build, body },
            Arc::new(AtomicBool::new(false)),
            wait,
        )
    })
    .await;
    if !matches!(
        logged,
        Ok(Ok(BuildOutcome::Decision(DecisionOutcome::Created(_))))
    ) {
        log::warn!(
            "draft Part edit {} applied; its decision log failed",
            action.id
        );
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
