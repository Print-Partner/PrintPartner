//! MCP `propose_rebuild_plan` and its `confirm_apply` on Rust storage
//! (GRE-1518, GRE-1434 PR 1).
//!
//! Propose checks `plan_id` and `idempotency_key`, then proves the API key
//! and finds the Build's planning brief on the read seam. It writes nothing.
//! Confirm runs `WorkingDraftClient::rebuild_from_brief` with the brief as it
//! is at confirm time: it sets the base and addon layers from the Source
//! roles, prepares and stores a draft, and selects it in the brief, in one
//! transaction. It never accepts or publishes a Plan.

use super::{
    checkoff::CheckoffDomain,
    write_result::{Code, Refusal, writer_failure},
};
use pp_services::mcp_domain::{ProposedAction, ToolInvocation};
use pp_storage::{
    build_graph::SourceRolesError,
    working_drafts::rebuild::{RebuildError, RebuildRequest, Rebuilt},
};
use serde_json::{Map, Value, json};
use std::sync::atomic::AtomicBool;

pub(super) const PROPOSE: &str = "propose_rebuild_plan";

const PLAN_ID: &str = "plan_id";
const IDEMPOTENCY_KEY: &str = "idempotency_key";
const INVALID_PLAN: Refusal = Refusal::new(
    Code::InvalidPlanId,
    "plan_id must name a Build by its positive number.",
);
const INVALID_KEY: Refusal = Refusal::new(
    Code::InvalidIdempotencyKey,
    "idempotency_key must be a non-blank string of at most 143 characters.",
);
const NO_BRIEF: Refusal = Refusal::new(
    Code::NoPlanningBrief,
    "This Build has no planning brief. Create the Build with propose_create_build.",
);
const BRIEF_INVALID: Refusal = Refusal::new(
    Code::PlanningBriefInvalid,
    "This Build's stored planning brief is invalid.",
);

impl CheckoffDomain {
    pub(super) fn rebuild_plan_propose(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let Some(build) = super::plan_id(args) else {
            return Ok(INVALID_PLAN.invocation());
        };
        let key = match key(args) {
            Ok(key) => key,
            Err(refusal) => return Ok(refusal.invocation()),
        };
        let credential = self.caller()?.into_credential();
        if let Err(error) = self.builds.check_source_roles(credential, build, self.wait) {
            return Ok(match error {
                SourceRolesError::NoBrief => NO_BRIEF,
                SourceRolesError::Writer(failure) => writer_failure(&failure),
                _ => BRIEF_INVALID,
            }
            .invocation());
        }
        let mut params = Map::new();
        params.insert(PLAN_ID.into(), build.get().into());
        if let Some(key) = key {
            params.insert(IDEMPOTENCY_KEY.into(), key.into());
        }
        let plan_id = i64::try_from(build.get()).map_err(|error| error.to_string())?;
        let action = ProposedAction::new(
            PROPOSE,
            plan_id,
            format!("Rebuild the Working Plan of Build {plan_id}"),
            "Set the base and addon layers from the Source roles and prepare a draft. Nothing is accepted.",
            params,
        );
        Ok(ToolInvocation {
            content: action.proposed_content(),
            proposed_action: Some(action),
        })
    }

    pub(super) fn rebuild_plan_apply(&self, action: &ProposedAction) -> Result<Value, String> {
        let Some(build) = super::plan_id(&action.params) else {
            return Ok(INVALID_PLAN.confirmation());
        };
        let key = match key(&action.params) {
            Ok(key) => key.unwrap_or_else(|| action.id.clone()),
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        let Ok(request) = RebuildRequest::new(&key) else {
            return Ok(INVALID_KEY.confirmation());
        };
        let credential = self.caller()?.into_credential();
        Ok(
            match self.drafts.rebuild_from_brief(
                credential,
                build,
                request,
                &AtomicBool::new(false),
                self.wait,
            ) {
                Ok(rebuilt) => json!({ "ok": true, "result": result(&rebuilt) }),
                Err(error) => {
                    let mut refused = storage_refusal(&error).confirmation();
                    if let RebuildError::SourceNotFound(id) | RebuildError::LayerConflict(id) =
                        error
                    {
                        refused["source_id"] = id.into();
                    }
                    refused
                }
            },
        )
    }
}

/// The optional key, trimmed. Absent and `null` mean "use the action id".
fn key(args: &Map<String, Value>) -> Result<Option<String>, Refusal> {
    match args.get(IDEMPOTENCY_KEY) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(key)) => RebuildRequest::new(key)
            .map(|_| Some(key.trim().to_owned()))
            .map_err(|_| INVALID_KEY),
        Some(_) => Err(INVALID_KEY),
    }
}

/// Provisional (GRE-1518): no Part list. `get_plan_draft` returns the Parts.
fn result(rebuilt: &Rebuilt) -> Value {
    let sources: Vec<Value> = rebuilt
        .sources
        .iter()
        .map(|source| {
            json!({
                "source_id": source.source_id,
                "role": source.role.as_str(),
                "revision": source.revision,
            })
        })
        .collect();
    json!({
        "draft_id": rebuilt.draft_id.get(),
        "snapshot_digest": rebuilt.snapshot_digest,
        "recompute": if rebuilt.created { "created" } else { "existing" },
        "sources": sources,
    })
}

fn storage_refusal(error: &RebuildError) -> Refusal {
    match error {
        RebuildError::InvalidIdempotencyKey => INVALID_KEY,
        RebuildError::NoBrief => NO_BRIEF,
        RebuildError::BriefInvalid => BRIEF_INVALID,
        RebuildError::NoStructuralBase => Refusal::new(
            Code::NoStructuralBase,
            "No Source has the structural_base role. Set one with propose_set_build_source_roles.",
        ),
        RebuildError::MultipleStructuralBases => Refusal::new(
            Code::MultipleStructuralBases,
            "A Build can have only one structural base.",
        ),
        RebuildError::SourceChoicesUnsupported => Refusal::new(
            Code::SourceChoicesUnsupported,
            "The brief resolves differences or scopes contributions to paths, which the rebuild does not apply yet.",
        ),
        RebuildError::SourceNotFound(_) => Refusal::new(
            Code::SourceNotFound,
            "A Source the brief names does not exist.",
        ),
        RebuildError::LayerConflict(_) => Refusal::new(
            Code::LayerConflict,
            "The structural base Source is already attached as another layer. Detach it first.",
        ),
        RebuildError::AcceptedBaselineRequired => Refusal::new(
            Code::AcceptedBaselineRequired,
            "The Build needs an Accepted Plan before it can be rebuilt.",
        ),
        RebuildError::NoStls => Refusal::new(
            Code::NoStls,
            "The Sources have no printable files. Sync them first.",
        ),
        RebuildError::WouldWipe => Refusal::new(
            Code::WouldWipe,
            "The rebuild would remove every Part of the Working Plan.",
        ),
        RebuildError::IdempotencyConflict => Refusal::new(
            Code::IdempotencyConflict,
            "This idempotency_key already made a different draft. Use another key.",
        ),
        RebuildError::Writer(failure) => writer_failure(failure),
    }
}
