//! MCP `get_build_planning_state` and `get_plan_draft` on Rust storage
//! (GRE-1442): the reads that give an assistant the evidence, contribution
//! and Part ids the Build planning write tools take.
//!
//! Both read with the caller's API key through the clients the HTTP routes
//! use: `PlanningClient::read` (`GET /plans/{id}/build-planning`) and
//! `WorkingDraftClient::execute` (`GET /plans/{id}/drafts[/{draft_id}]`).
//! Like those routes, they run on the storage writer queue today. Neither
//! writes. The brief and the draft are read in two transactions, so a write
//! between them can pair a phase with a newer draft. Results are the typed
//! storage views, serialized here; a refusal carries a stable code from
//! `write_result.rs`.

use super::{
    ProductMcp, positive_id,
    write_result::{Code, Refusal, writer_failure},
};
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::ToolResult;
use pp_storage::{
    WriterFailure,
    build_graph::PositiveId,
    read_model::planning::{
        self, BuildPlanning, BuildPlanningBrief, BuildPlanningPhase, ContributionStatus,
        EvidenceInputKind,
    },
    required_units::{DraftIdentity, PartView, Workspace},
    working_drafts::{self, DraftState},
};
use serde::{Serialize, Serializer, ser::Error as _};
use serde_json::Value;
use std::{
    collections::HashSet,
    sync::{Arc, atomic::AtomicBool},
};

pub(super) const STATE: &str = "get_build_planning_state";
pub(super) const DRAFT: &str = "get_plan_draft";
pub(super) const TOOLS: [&str; 2] = [STATE, DRAFT];

const INVALID_PLAN_ID: Refusal = Refusal::new(
    Code::InvalidPlanId,
    "plan_id must be a positive whole number.",
);
const PLAN_NOT_FOUND: Refusal = Refusal::new(Code::PlanNotFound, "No Build has this plan_id.");
const NO_BRIEF: Refusal = Refusal::new(
    Code::NoPlanningBrief,
    "This Build has no planning brief. Builds created with propose_create_build have one.",
);
/// A read result this module does not expect. Its cause is never sent.
const INTERNAL: Refusal = Refusal::new(Code::Internal, "Storage failed. Nothing was written.");

/// A failed read: busy or stopped storage, a refused key, or stored data
/// that does not read, mapped as every MCP tool maps storage failures.
fn storage_failure(error: anyhow::Error) -> Refusal {
    writer_failure(&WriterFailure::from(error))
}

/// `get_build_planning_state`: the hydrated brief and what is derived from it.
#[derive(Serialize)]
struct PlanningState<'a> {
    #[serde(serialize_with = "brief_for_mcp")]
    brief: &'a BuildPlanningBrief,
    planning_phase: &'a BuildPlanningPhase,
    grouped_difference_count: usize,
    difference_count: usize,
    next_required_decisions: Vec<RequiredDecision<'a>>,
}

/// A decision the owner still has to make before the Build can be planned.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RequiredDecision<'a> {
    /// A model page whose files must be uploaded.
    Upload { evidence_id: &'a str },
    /// A Source contribution still proposed.
    Contribution { contribution_id: &'a str },
    /// A difference group with no resolution.
    DifferenceGroup { group_id: &'a str },
}

/// `get_plan_draft`: the draft and, when the Build has a brief, its phase.
#[derive(Serialize)]
struct PlanDraft<'a> {
    draft: Option<DraftView<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    planning_phase: Option<&'a BuildPlanningPhase>,
}

/// The draft's identity and Parts, as `GET /plans/{id}/drafts/{draft_id}`
/// serializes them.
#[derive(Serialize)]
struct DraftView<'a> {
    #[serde(flatten)]
    identity: &'a DraftIdentity,
    parts: &'a [PartView],
}

/// The stored brief without `draft_review_blockers`, a legacy field MCP
/// clients never see.
fn brief_for_mcp<S: Serializer>(brief: &&BuildPlanningBrief, out: S) -> Result<S::Ok, S::Error> {
    let mut value = serde_json::to_value(brief).map_err(S::Error::custom)?;
    if let Some(fields) = value.as_object_mut() {
        fields.remove("draft_review_blockers");
    }
    value.serialize(out)
}

/// Model pages still waiting for an upload, proposed contributions, then
/// difference groups without a resolution, each in brief order.
fn required_decisions(brief: &BuildPlanningBrief) -> Vec<RequiredDecision<'_>> {
    let uploaded: HashSet<&str> = brief
        .evidence
        .iter()
        .filter(|item| item.input_kind == Some(EvidenceInputKind::Upload))
        .filter_map(|item| item.derived_from_evidence_id.as_deref())
        .collect();
    let uploads = brief
        .evidence
        .iter()
        .filter(|item| {
            item.input_kind == Some(EvidenceInputKind::ModelPage)
                && item.upload_required == Some(true)
                && !uploaded.contains(item.id.as_str())
        })
        .map(|item| RequiredDecision::Upload {
            evidence_id: &item.id,
        });
    let contributions = brief
        .contributions
        .iter()
        .filter(|item| item.status == ContributionStatus::Proposed)
        .map(|item| RequiredDecision::Contribution {
            contribution_id: &item.id,
        });
    let mut seen = HashSet::new();
    let groups = brief
        .differences
        .iter()
        .map(|item| item.group_id.as_str())
        .filter(|group| seen.insert(*group) && !brief.resolutions.contains_key(*group))
        .map(|group_id| RequiredDecision::DifferenceGroup { group_id });
    uploads.chain(contributions).chain(groups).collect()
}

fn json(value: &impl Serialize) -> ToolResult {
    match serde_json::to_string(value) {
        Ok(text) => ToolResult::ok(text),
        Err(_) => refused(INTERNAL),
    }
}

fn refused(refusal: Refusal) -> ToolResult {
    ToolResult {
        text: refusal.proposal(),
        is_error: true,
    }
}

impl ProductMcp {
    /// One of [`TOOLS`], read with the caller's key.
    pub(super) async fn planning_read(
        &self,
        name: &str,
        arguments: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let Some(build) = positive_id(arguments, "plan_id") else {
            return refused(INVALID_PLAN_ID);
        };
        let planning = match self.build_planning(build, presented.clone()).await {
            Ok(planning) => planning,
            Err(refusal) => return refused(refusal),
        };
        if name == STATE {
            let Some(planning) = planning else {
                return refused(NO_BRIEF);
            };
            return json(&PlanningState {
                brief: &planning.brief,
                planning_phase: &planning.planning_phase,
                grouped_difference_count: planning.grouped_difference_count,
                difference_count: planning.difference_count,
                next_required_decisions: required_decisions(&planning.brief),
            });
        }
        let selected = planning.as_ref().and_then(|planning| {
            planning
                .brief
                .draft_id
                .and_then(|id| u64::try_from(id).ok())
                .and_then(|id| PositiveId::new(id).ok())
        });
        let draft = match selected {
            Some(draft) => Some(draft),
            None => match self.open_draft(build, presented.clone()).await {
                Ok(draft) => draft,
                Err(refusal) => return refused(refusal),
            },
        };
        let workspace = match draft {
            Some(draft) => match self.draft_workspace(build, draft, presented).await {
                Ok(workspace) => workspace,
                Err(refusal) => return refused(refusal),
            },
            None => None,
        };
        json(&PlanDraft {
            draft: workspace.as_deref().map(|workspace| DraftView {
                identity: &workspace.draft,
                parts: &workspace.parts,
            }),
            planning_phase: planning.as_ref().map(|planning| &planning.planning_phase),
        })
    }

    /// The Build's hydrated brief, or `None` when it has none.
    async fn build_planning(
        &self,
        build: PositiveId,
        presented: PresentedCredential,
    ) -> Result<Option<BuildPlanning>, Refusal> {
        let client = self.reads.planning.clone();
        let wait = self.reads.wait;
        let read = tokio::task::spawn_blocking(move || {
            client.read(
                presented.into_credential(),
                planning::Request {
                    build: Some(build),
                    query: planning::Query::BuildPlanning {
                        draft_id_valid: true,
                    },
                },
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        match read.map_err(|_| INTERNAL)?.map_err(storage_failure)? {
            planning::Outcome::BuildPlanning(response) => Ok(response.planning),
            planning::Outcome::MissingBuild => Err(PLAN_NOT_FOUND),
            _ => Err(INTERNAL),
        }
    }

    /// One draft request on the Build, read with the caller's key.
    async fn drafts(
        &self,
        build: PositiveId,
        request: working_drafts::Request,
        presented: PresentedCredential,
    ) -> Result<working_drafts::Outcome, Refusal> {
        let client = self.reads.drafts.clone();
        let wait = self.reads.wait;
        let read = tokio::task::spawn_blocking(move || {
            client.execute(
                presented.into_credential(),
                build,
                request,
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        match read.map_err(|_| INTERNAL)?.map_err(storage_failure)? {
            working_drafts::Outcome::ProfileNotFound => Err(PLAN_NOT_FOUND),
            outcome => Ok(outcome),
        }
    }

    /// The Build's open draft, the last in storage's list order (by creation)
    /// when there are several.
    async fn open_draft(
        &self,
        build: PositiveId,
        presented: PresentedCredential,
    ) -> Result<Option<PositiveId>, Refusal> {
        match self
            .drafts(build, working_drafts::Request::List, presented)
            .await?
        {
            working_drafts::Outcome::Listed { drafts } => Ok(drafts
                .iter()
                .filter(|draft| draft.state() == DraftState::Open)
                .map(|draft| draft.draft_id())
                .last()),
            _ => Err(INTERNAL),
        }
    }

    /// The draft's workspace, or `None` when the draft is gone.
    async fn draft_workspace(
        &self,
        build: PositiveId,
        draft_id: PositiveId,
        presented: PresentedCredential,
    ) -> Result<Option<Box<Workspace>>, Refusal> {
        let request = working_drafts::Request::Workspace { draft_id };
        match self.drafts(build, request, presented).await? {
            working_drafts::Outcome::Workspace { workspace } => Ok(Some(workspace)),
            working_drafts::Outcome::DraftNotFound => Ok(None),
            _ => Err(INTERNAL),
        }
    }
}
