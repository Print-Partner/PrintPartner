//! MCP `propose_set_build_source_roles` and its `confirm_apply` on Rust
//! storage (GRE-1300 PR 1).
//!
//! Propose checks `plan_id` and that `roles` is a list, then proves the API
//! key and finds the Build's planning brief on the read seam. It writes
//! nothing. Confirm checks each `{ evidence_id, role }` and runs
//! `BuildGraphClient::set_source_roles`, which sets the roles in the brief
//! and clears its selected draft in one transaction. A Source's Library
//! role and the Build's layers stay as they are.

use super::{
    checkoff::CheckoffDomain,
    write_result::{Code, Refusal, writer_failure},
};
use pp_services::mcp_domain::{ProposedAction, ToolInvocation};
use pp_storage::{
    build_graph::{PositiveId, RoleChange, RoleSet, SourceRolesError},
    read_model::planning::SourceRole,
};
use serde_json::{Map, Value, json};
use std::sync::atomic::AtomicBool;

pub(super) const PROPOSE: &str = "propose_set_build_source_roles";

const PLAN_ID: &str = "plan_id";
const ROLES: &str = "roles";
const INVALID_PLAN: Refusal = Refusal::new(
    Code::InvalidPlanId,
    "plan_id must name a Build by its positive number.",
);
/// Confirm adds the unknown id as `evidence_id`.
const EVIDENCE_NOT_FOUND: Refusal = Refusal::new(
    Code::EvidenceNotFound,
    "An evidence_id names no evidence in this Build's planning brief.",
);
const INVALID_ROLES: Refusal = Refusal::new(
    Code::InvalidRoles,
    "roles must be a list of { evidence_id, role }.",
);

impl CheckoffDomain {
    pub(super) fn source_roles_propose(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let refused = |refusal: Refusal| ToolInvocation {
            content: refusal.proposal(),
            proposed_action: None,
        };
        let Some(build) = plan(args) else {
            return Ok(refused(INVALID_PLAN));
        };
        let Some(roles) = args.get(ROLES).and_then(Value::as_array) else {
            return Ok(refused(INVALID_ROLES));
        };
        let credential = self.caller()?.into_credential();
        if let Err(error) = self.builds.check_source_roles(credential, build, self.wait) {
            return Ok(refused(storage_refusal(&error)));
        }
        let mut params = Map::new();
        params.insert(PLAN_ID.into(), build.get().into());
        params.insert(ROLES.into(), Value::Array(roles.clone()));
        let plan_id = i64::try_from(build.get()).map_err(|error| error.to_string())?;
        let action = ProposedAction::new(
            PROPOSE,
            plan_id,
            format!("Set Source roles on Build {plan_id}"),
            "Set the Build's own Source roles and clear its selected draft. Library roles do not change.",
            params,
        );
        Ok(ToolInvocation {
            content: action.proposed_content(),
            proposed_action: Some(action),
        })
    }

    pub(super) fn source_roles_apply(&self, action: &ProposedAction) -> Result<Value, String> {
        let Some(build) = plan(&action.params) else {
            return Ok(INVALID_PLAN.confirmation());
        };
        let changes = match changes(&action.params) {
            Ok(changes) => changes,
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        let credential = self.caller()?.into_credential();
        Ok(
            match self.builds.set_source_roles(
                credential,
                build,
                changes,
                &AtomicBool::new(false),
                self.wait,
            ) {
                Ok(set) => json!({
                    "ok": true,
                    "result": { "updated": set.len(), "evidence": set.iter().map(item).collect::<Vec<_>>() },
                }),
                Err(SourceRolesError::EvidenceNotFound(id)) => {
                    let mut refused = EVIDENCE_NOT_FOUND.confirmation();
                    refused["evidence_id"] = id.into();
                    refused
                }
                Err(error) => storage_refusal(&error).confirmation(),
            },
        )
    }
}

/// `plan_id` as every MCP planning tool reads it: a JSON whole number from 1
/// up, never a numeric string.
fn plan(args: &Map<String, Value>) -> Option<PositiveId> {
    super::positive_id(&json!({ PLAN_ID: args.get(PLAN_ID)? }), PLAN_ID)
}

/// The checked assignments, or the first refusal. A missing or non-string
/// `evidence_id` checks as blank, which no evidence item has.
fn changes(args: &Map<String, Value>) -> Result<Vec<RoleChange>, Refusal> {
    let roles = args
        .get(ROLES)
        .and_then(Value::as_array)
        .ok_or(INVALID_ROLES)?;
    roles
        .iter()
        .map(|row| {
            let row = row.as_object().ok_or(Refusal::new(
                Code::InvalidRoleAssignment,
                "Each roles item must be an object { evidence_id, role }.",
            ))?;
            let text = |field: &str| row.get(field).and_then(Value::as_str).unwrap_or_default();
            let role = SourceRole::from_wire(pp_storage::read_model::views::js_trim(text("role")))
                .ok_or(Refusal::new(
                    Code::InvalidSourceRole,
                    "role must be structural_base, overlay, addon or evidence.",
                ))?;
            Ok(RoleChange::new(text("evidence_id"), role))
        })
        .collect()
}

fn item(set: &RoleSet) -> Value {
    json!({ "id": set.evidence_id, "source_id": set.source_id, "source_role": set.role.as_str() })
}

fn storage_refusal(error: &SourceRolesError) -> Refusal {
    match error {
        SourceRolesError::NoBrief => Refusal::new(
            Code::NoPlanningBrief,
            "This Build has no planning brief. Create the Build with propose_create_build.",
        ),
        SourceRolesError::BriefInvalid => Refusal::new(
            Code::PlanningBriefInvalid,
            "This Build's stored planning brief is invalid.",
        ),
        SourceRolesError::EvidenceNotFound(_) => EVIDENCE_NOT_FOUND,
        SourceRolesError::MultipleStructuralBases => Refusal::new(
            Code::MultipleStructuralBases,
            "A Build can have only one structural base.",
        ),
        SourceRolesError::Writer(failure) => writer_failure(failure),
    }
}
