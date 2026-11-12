use crate::{
    Envelope, Shared, WriterOwner, auth,
    manifest_text::{DraftPart, JsText},
    read_model::Credential,
    required_units::{
        self, Outcome as WorkspaceOutcome, ReconciliationCommand, model,
        storage::{self, Draft, draft, num, one, rows, rows_checked, text},
        view::workspace,
    },
    writer_admission,
};
use anyhow::{Result, anyhow, ensure};
pub use pp_domain::{Digest, DraftState, PositiveId, Quantity, Version};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub mod autosave;
pub(crate) mod diff;
pub(crate) mod experiment;
pub mod header;
pub mod observation;
mod preparation;
#[cfg(test)]
mod preparation_tests;
mod rebase;
pub mod restore;
pub mod save;
pub use rebase::{
    Conflict as RebaseConflict, Field as RebaseField, RebaseArgs, RebaseOutcome, RebaseRequest,
};

#[cfg(test)]
#[path = "working_drafts/tests/draft_reads.rs"]
mod draft_reads;
mod manifest;
#[cfg(test)]
mod manifest_tests;
mod siblings;
#[cfg(test)]
mod siblings_tests;
pub mod source_reads;
mod yaml;
#[cfg(test)]
mod yaml_continuation_tests;

#[derive(Clone, Debug)]
pub enum Request {
    List,
    Read {
        draft_id: PositiveId,
    },
    Diff {
        draft_id: PositiveId,
    },
    Workspace {
        draft_id: PositiveId,
    },
    /// What accepting the draft would change, through Accept's own checks.
    AcceptPreview {
        draft_id: PositiveId,
    },
    /// The draft's proposed header beside the Build's current one (GRE-1197).
    Header {
        draft_id: PositiveId,
    },
    /// The Build's current header, read before any draft exists (GRE-1120).
    BuildHeader,
}

/// The JSON body of Node's draft Part edit route. It accepts what
/// `parseEditPlanDraftPartsRequest` accepts: one `decision` or a batch of 1 to
/// 10,000 `decisions`, each Part field edited at most once.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "EditWire")]
pub struct EditRequest {
    expected_snapshot_digest: Digest,
    decisions: Vec<PartDecision>,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PartDecision {
    SetIncluded {
        #[cfg_attr(feature = "codegen", schemars(length(min = 1)))]
        draft_part_ids: Vec<PositiveId>,
        value: bool,
    },
    SetQuantityOverride {
        #[cfg_attr(feature = "codegen", schemars(length(min = 1)))]
        draft_part_ids: Vec<PositiveId>,
        #[serde(deserialize_with = "Option::deserialize")]
        #[cfg_attr(
            feature = "codegen",
            schemars(with = "pp_contracts::RequiredNullable<Quantity>")
        )]
        value: Option<Quantity>,
    },
}
impl PartDecision {
    fn ids(&self) -> &[PositiveId] {
        match self {
            Self::SetIncluded { draft_part_ids, .. }
            | Self::SetQuantityOverride { draft_part_ids, .. } => draft_part_ids,
        }
    }
    fn apply(&self, part: &mut DraftPart) {
        match self {
            Self::SetIncluded { value, .. } => part["included"] = json!(value),
            Self::SetQuantityOverride { value, .. } => {
                part["quantityOverride"] = json!(value);
                part["quantityEffective"] = match value {
                    Some(value) => json!(value),
                    None => part["quantityInferred"].clone(),
                };
            }
        }
    }
}

#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(untagged)]
enum EditWire {
    One(OneEdit),
    Batch(BatchEdit),
}
#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct OneEdit {
    expected_snapshot_digest: Digest,
    decision: PartDecision,
}
#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct BatchEdit {
    expected_snapshot_digest: Digest,
    #[cfg_attr(feature = "codegen", schemars(length(min = 1, max = 10000)))]
    decisions: Vec<PartDecision>,
}
/// Where a rejected [`EditRequest`] body failed. The untagged wire reports
/// every member failure at the root, so the body's keys pick the shape first.
/// `None` when the body is not a JSON object or only a batch rule failed.
#[cfg(feature = "desktop_rebuild")]
pub fn edit_rejection(body: &[u8]) -> Option<serde_path_to_error::Error<serde_json::Error>> {
    let value = serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)?;
    if value.get("decisions").is_some() && value.get("decision").is_none() {
        serde_path_to_error::deserialize::<_, BatchEdit>(&value).err()
    } else {
        serde_path_to_error::deserialize::<_, OneEdit>(&value).err()
    }
}

impl TryFrom<EditWire> for EditRequest {
    type Error = &'static str;
    fn try_from(wire: EditWire) -> Result<Self, Self::Error> {
        let (expected_snapshot_digest, decisions) = match wire {
            EditWire::One(one) => (one.expected_snapshot_digest, vec![one.decision]),
            EditWire::Batch(batch) if (1..=10_000).contains(&batch.decisions.len()) => {
                (batch.expected_snapshot_digest, batch.decisions)
            }
            EditWire::Batch(_) => return Err("A Plan draft edit allows 1 to 10000 decisions"),
        };
        let mut targets = HashSet::new();
        for decision in &decisions {
            let field = std::mem::discriminant(decision);
            if decision.ids().is_empty()
                || !decision
                    .ids()
                    .iter()
                    .all(|id| targets.insert((field, id.get())))
            {
                return Err("A Plan draft Part field may be edited only once per batch");
            }
        }
        Ok(Self {
            expected_snapshot_digest,
            decisions,
        })
    }
}

/// The wire schema is [`EditWire`]'s. JSON Schema cannot express the
/// one-edit-per-Part-field rule; the server refuses it.
#[cfg(feature = "codegen")]
impl schemars::JsonSchema for EditRequest {
    fn schema_name() -> String {
        "EditRequest".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        <EditWire as schemars::JsonSchema>::json_schema(generator)
    }
}

/// The JSON body of Node's draft abandon route.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AbandonRequest {
    expected_lifecycle_version: Version,
}

/// Node's `PlanDraftWorkspaceService.abandon` result.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AbandonOutcome {
    Ready {
        draft: required_units::DraftIdentity,
    },
    ProfileNotFound,
    DraftNotFound,
    DraftChanged {
        workspace: Box<required_units::Workspace>,
    },
    NotOpen {
        workspace: Box<required_units::Workspace>,
    },
}

/// What Node's draft recompute route passes to `recomputePlanDraft`: the
/// trimmed `Idempotency-Key` header and the body's `apply_manifest` flag.
#[derive(Debug, Clone)]
pub struct RecomputeRequest {
    idempotency_key: String,
    apply_manifest: bool,
}
impl RecomputeRequest {
    pub fn new(idempotency_key: &str, apply_manifest: bool) -> Result<Self> {
        Ok(Self {
            idempotency_key: self::idempotency_key(idempotency_key)?,
            apply_manifest,
        })
    }
    fn options(&self) -> preparation::PreparationOptions {
        preparation::PreparationOptions {
            apply_manifest: self.apply_manifest,
            prefer_accepted: !self.apply_manifest,
            ..Default::default()
        }
    }
}

/// Node's trimmed `Idempotency-Key` header.
fn idempotency_key(value: &str) -> Result<String> {
    let key = JsText::scalar(value).trim_ecmascript();
    ensure!(!key.is_empty(), "Plan draft idempotency key is required");
    ensure!(key.units().len() <= 160, "Invalid idempotency key");
    key.as_scalar()
        .ok_or_else(|| anyhow!("Invalid idempotency key"))
}

/// Node's `recomputePlanDraft` result, after the service's Plan check.
#[derive(Debug)]
pub enum RecomputeOutcome {
    Created { draft: DraftDocument },
    Existing { draft: DraftDocument },
    ProfileNotFound,
    AcceptedBaselineRequired,
    BaseChanged,
    InputsChanged,
    IdempotencyConflict,
    NoLayers,
    NoStls,
    WouldWipe,
}

/// Node's `PlanDraftWorkspaceService.recompute` result.
#[derive(Debug)]
pub enum RecomputeWorkspaceOutcome {
    /// Node's `autoReconcile` over the created or existing draft.
    Selected(WorkspaceOutcome),
    /// Any other `recomputePlanDraft` result, which Node's `recomputeFailure`
    /// maps.
    Refused(RecomputeOutcome),
}

/// Node's `PlanDraftWorkspaceService.rebase` result.
#[derive(Debug)]
pub enum RebaseWorkspaceOutcome {
    /// Node's `autoReconcile` over the rebased or existing draft, or a
    /// refusal Node reports with a workspace: `draft_changed` with the source
    /// draft's for `source_conflict`, and `not_open` for `not_abandoned`.
    Workspace(WorkspaceOutcome),
    /// Any other `rebasePlanDraft` result, which Node's `rebaseFailure` maps.
    Refused(RebaseOutcome),
}

#[derive(Debug)]
pub enum Outcome {
    Listed {
        drafts: DraftList,
    },
    Read {
        draft: DraftDocument,
    },
    Diff {
        diff: DraftDiff,
    },
    Workspace {
        workspace: Box<required_units::Workspace>,
    },
    AcceptPreview {
        preview: crate::plan_publication::preview::PreviewOutcome,
    },
    Header {
        header: header::HeaderView,
    },
    BuildHeader {
        header: header::BuildHeaderView,
    },
    ProfileNotFound,
    DraftNotFound,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftIdentity {
    #[serde(rename = "id")]
    draft_id: PositiveId,
    state: DraftState,
    lifecycle_version: u32,
    snapshot_digest: Digest,
    base_revision_id: Option<PositiveId>,
    base_plan_version: Version,
}
impl DraftIdentity {
    fn from_header(header: &Value) -> Result<Self> {
        let identity = Self {
            draft_id: serde_json::from_value(header["id"].clone())?,
            state: serde_json::from_value(header["state"].clone())?,
            lifecycle_version: serde_json::from_value(header["lifecycleVersion"].clone())?,
            snapshot_digest: serde_json::from_value(header["snapshotDigest"].clone())?,
            base_revision_id: serde_json::from_value(header["baseRevisionId"].clone())?,
            base_plan_version: serde_json::from_value(header["basePlanVersion"].clone())?,
        };
        ensure!(
            identity.lifecycle_version <= 2_147_483_647,
            "Invalid draft lifecycle version"
        );
        ensure!(
            identity.base_revision_id.is_some() == (identity.base_plan_version.get() > 0),
            "Invalid draft base"
        );
        Ok(identity)
    }
    pub fn draft_id(&self) -> PositiveId {
        self.draft_id
    }
    pub fn state(&self) -> DraftState {
        self.state
    }
    pub fn lifecycle_version(&self) -> u32 {
        self.lifecycle_version
    }
    pub fn snapshot_digest(&self) -> &Digest {
        &self.snapshot_digest
    }
    pub fn base_revision_id(&self) -> Option<PositiveId> {
        self.base_revision_id
    }
    pub fn base_plan_version(&self) -> Version {
        self.base_plan_version
    }
    /// The identity as the draft list and workspace bodies serialize it.
    pub fn workspace_identity(&self) -> Result<required_units::DraftIdentity> {
        Ok(required_units::DraftIdentity {
            draft_id: i64::try_from(self.draft_id.get())?,
            state: self.state.as_str().into(),
            lifecycle_version: self.lifecycle_version.into(),
            snapshot_digest: self.snapshot_digest.as_str().into(),
            base: required_units::DraftBase {
                revision_id: self
                    .base_revision_id
                    .map(|id| i64::try_from(id.get()))
                    .transpose()?,
                plan_version: i64::try_from(self.base_plan_version.get())?,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DraftDocument {
    identity: DraftIdentity,
    ordinary: Map<String, Value>,
    parts: Vec<DraftPart>,
}
impl DraftDocument {
    pub fn identity(&self) -> &DraftIdentity {
        &self.identity
    }
    /// Whether the draft has a selected Required-unit reconciliation.
    pub fn has_required_unit_selection(&self) -> bool {
        !self.ordinary["requiredUnitReconciliation"].is_null()
    }
    pub fn into_json_body(self) -> DraftJsonBody {
        let mut output = Value::Object(self.ordinary).to_string().into_bytes();
        output.pop();
        output.extend_from_slice(b",\"parts\":[");
        for (index, part) in self.parts.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            output.extend_from_slice(&part.json_fields(&snapshot_part_fields(), false));
        }
        output.extend_from_slice(b"]}");
        DraftJsonBody(output)
    }
}

#[derive(Debug)]
pub struct DraftList(Vec<DraftIdentity>);
impl DraftList {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &DraftIdentity> {
        self.0.iter()
    }
}
#[derive(Debug)]
pub struct DraftDiff(pub(crate) diff::DraftDiffModel);
impl DraftDiff {
    pub fn into_json_body(self) -> DraftDiffJsonBody {
        DraftDiffJsonBody(self.0.write_json())
    }
}
pub struct DraftJsonBody(Vec<u8>);
pub struct DraftDiffJsonBody(Vec<u8>);
pub struct DraftOutcomeJsonBody(Vec<u8>);
impl DraftJsonBody {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
impl DraftDiffJsonBody {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
impl DraftOutcomeJsonBody {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

fn snapshot_part_fields() -> Vec<&'static str> {
    [
        vec!["id", "draftId", "baseRevisionPartId"],
        model::PART_FIELDS.to_vec(),
    ]
    .concat()
}
fn draft_origin(header: &Value) -> Result<Value> {
    let origin_fields = [
        "rebasedFromDraftId",
        "rebasedFromLifecycleVersion",
        "rebasedFromSnapshotDigest",
    ];
    let origin = if origin_fields.iter().all(|key| header[*key].is_null()) {
        json!({"kind":"recompute"})
    } else if origin_fields.iter().all(|key| !header[*key].is_null()) {
        json!({"kind":"rebase", "sourceDraftId":header["rebasedFromDraftId"], "sourceLifecycleVersion":header["rebasedFromLifecycleVersion"], "sourceSnapshotDigest":header["rebasedFromSnapshotDigest"]})
    } else {
        return Err(Failure::CorruptOrigin.into());
    };
    Ok(origin)
}

fn snapshot(draft: &Draft) -> Result<DraftDocument> {
    let header = &draft.header;
    let origin = draft_origin(header)?;
    let mut ordinary = Map::new();
    for key in [
        "id",
        "profileId",
        "baseRevisionId",
        "basePlanVersion",
        "state",
        "lifecycleVersion",
        "consumedRevisionId",
        "consumedAt",
        "origin",
        "digestFormat",
        "snapshotDigest",
        "createdBy",
        "idempotencyKey",
        "createdAt",
    ] {
        ordinary.insert(
            key.into(),
            if key == "origin" {
                origin.clone()
            } else {
                header[key].clone()
            },
        );
    }
    ordinary.insert("requiredUnitReconciliation".into(), draft.selected.as_ref().map(|saved| json!({"id":saved.header["id"], "format":saved.header["format"], "digest":saved.header["reconciliationDigest"]})).unwrap_or(Value::Null));
    let fields = [vec!["id", "draftId"], model::INPUT_FIELDS.to_vec()].concat();
    ordinary.insert(
        "inputs".into(),
        Value::from_iter(
            draft
                .inputs
                .iter()
                .map(|input| model::project(input, &fields)),
        ),
    );
    Ok(DraftDocument {
        identity: DraftIdentity::from_header(header)?,
        ordinary,
        parts: draft
            .parts
            .iter()
            .map(|part| part.projected(&snapshot_part_fields()))
            .collect(),
    })
}

#[derive(Debug)]
pub enum Failure {
    Cancelled,
    QueueFull,
    Stopped,
    OutcomeUnknown,
    CorruptOrigin,
}
crate::impl_debug_error!(Failure);

#[derive(Clone)]
pub struct WorkingDraftClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
    observations: Option<Observations>,
}
#[derive(Clone)]
struct Observations {
    reads: Arc<dyn observation::DraftReads>,
    limits: observation::PreparationLimits,
    repos: PathBuf,
}
pub(super) struct Command {
    credential: Credential,
    profile: PositiveId,
    request: Request,
    policy: auth::AuthPolicy,
    cancelled: Arc<AtomicBool>,
}
pub(super) struct Write {
    credential: Credential,
    profile: PositiveId,
    policy: auth::AuthPolicy,
    cancelled: Arc<AtomicBool>,
    operation: Operation,
}
enum Operation {
    Save {
        request: pp_contracts::autosave::SavePlanChoicesRequest,
        key: String,
        observations: Observations,
        reply: mpsc::Sender<Result<save::Outcome>>,
    },
    Edit {
        draft_id: PositiveId,
        request: EditRequest,
        reply: mpsc::Sender<Result<WorkspaceOutcome>>,
    },
    Abandon {
        draft_id: PositiveId,
        request: AbandonRequest,
        reply: mpsc::Sender<Result<AbandonOutcome>>,
    },
    Recompute {
        request: RecomputeRequest,
        observations: Observations,
        reply: RecomputeReply,
    },
    Rebase {
        draft_id: PositiveId,
        idempotency_key: String,
        request: RebaseRequest,
        observations: Observations,
        reply: RebaseReply,
    },
    Autosave {
        request: pp_contracts::autosave::SavePlanChoicesRequest,
        key: String,
        observations: Observations,
        reply: mpsc::Sender<Result<autosave::AutosaveOutcome>>,
    },
    Header {
        request: header::SaveDraftHeaderRequest,
        key: String,
        observations: Observations,
        reply: mpsc::Sender<Result<header::HeaderOutcome>>,
    },
    Restore {
        snapshot: PositiveId,
        request: restore::RestoreSnapshotRequest,
        key: String,
        observations: Observations,
        reply: mpsc::Sender<Result<restore::RestoreOutcome>>,
    },
}
enum RecomputeReply {
    Draft(mpsc::Sender<Result<RecomputeOutcome>>),
    Workspace(mpsc::Sender<Result<RecomputeWorkspaceOutcome>>),
}
enum RebaseReply {
    Draft(mpsc::Sender<Result<RebaseOutcome>>),
    Workspace(mpsc::Sender<Result<RebaseWorkspaceOutcome>>),
}
impl WriterOwner {
    pub fn working_drafts_with_policy(
        &self,
        policy: auth::AuthPolicy,
    ) -> Result<WorkingDraftClient> {
        auth::validate_policy(policy)?;
        Ok(WorkingDraftClient {
            shared: self.client.shared.clone(),
            policy,
            observations: None,
        })
    }
    /// The production Source reader over this owner's data directory.
    pub fn source_reads(&self) -> Result<source_reads::SourceReads> {
        Ok(source_reads::SourceReads::new(
            self.lease
                .as_ref()
                .ok_or_else(|| anyhow!("Storage owner is closed"))?
                .data_dir(),
        ))
    }
    /// A client that can also recompute, reading Sources through `reads`.
    pub fn working_drafts_with_observations(
        &self,
        policy: auth::AuthPolicy,
        reads: Arc<dyn observation::DraftReads>,
        limits: observation::PreparationLimits,
    ) -> Result<WorkingDraftClient> {
        limits.validate()?;
        let mut client = self.working_drafts_with_policy(policy)?;
        client.observations = Some(Observations {
            reads,
            limits,
            repos: self
                .lease
                .as_ref()
                .ok_or_else(|| anyhow!("Storage owner is closed"))?
                .data_dir()
                .join("repos"),
        });
        Ok(client)
    }
}
impl WorkingDraftClient {
    pub fn execute(
        &self,
        credential: Credential,
        profile: PositiveId,
        request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<Outcome> {
        self.enqueue(cancelled, wait, |reply, cancelled| Envelope::WorkingDraft {
            command: Command {
                credential,
                profile,
                request,
                policy: self.policy,
                cancelled,
            },
            reply,
        })
    }
    /// Edits draft Parts as Node's `PlanDraftWorkspaceService.editParts` does,
    /// including its automatic Required-unit selection. The edit commits before
    /// the selection starts, so a selection error or cancellation keeps it.
    pub fn edit(
        &self,
        credential: Credential,
        profile: PositiveId,
        draft_id: PositiveId,
        request: EditRequest,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<WorkspaceOutcome> {
        self.enqueue(cancelled, wait, |reply, cancelled| {
            Envelope::WorkingDraftWrite {
                command: Write {
                    credential,
                    profile,
                    policy: self.policy,
                    cancelled,
                    operation: Operation::Edit {
                        draft_id,
                        request,
                        reply,
                    },
                },
            }
        })
    }
    /// The Required-unit reconciliation client on this client's writer and
    /// auth policy, for Node's `PUT .../drafts/:draftId/reconciliation`.
    pub fn reconciliation(&self) -> required_units::ReconciliationClient {
        required_units::ReconciliationClient::new(self.shared.clone(), self.policy)
    }
    /// The Accept (publication) client on this client's writer and auth
    /// policy, for Node's `POST .../drafts/:draftId/apply`.
    pub fn publication(&self) -> crate::plan_publication::PublicationClient {
        crate::plan_publication::PublicationClient::new(self.shared.clone(), self.policy)
    }
    /// Abandons an open draft as Node's `PlanDraftWorkspaceService.abandon`
    /// does.
    pub fn abandon(
        &self,
        credential: Credential,
        profile: PositiveId,
        draft_id: PositiveId,
        request: AbandonRequest,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<AbandonOutcome> {
        self.enqueue(cancelled, wait, |reply, cancelled| {
            Envelope::WorkingDraftWrite {
                command: Write {
                    credential,
                    profile,
                    policy: self.policy,
                    cancelled,
                    operation: Operation::Abandon {
                        draft_id,
                        request,
                        reply,
                    },
                },
            }
        })
    }
    /// Creates an open draft from a fresh preparation as Node's
    /// `recomputePlanDraft` does, abandoning every other open draft. Node's
    /// service then selects Required units; this method does not.
    pub fn recompute(
        &self,
        credential: Credential,
        profile: PositiveId,
        request: RecomputeRequest,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<RecomputeOutcome> {
        self.enqueue_recompute(
            credential,
            profile,
            request,
            cancelled,
            wait,
            RecomputeReply::Draft,
        )
    }
    /// Recomputes as Node's `PlanDraftWorkspaceService.recompute` does,
    /// including its automatic Required-unit selection. The recompute commits
    /// before the selection starts, so a selection error or cancellation keeps
    /// it.
    pub fn recompute_and_select(
        &self,
        credential: Credential,
        profile: PositiveId,
        request: RecomputeRequest,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<RecomputeWorkspaceOutcome> {
        self.enqueue_recompute(
            credential,
            profile,
            request,
            cancelled,
            wait,
            RecomputeReply::Workspace,
        )
    }
    fn enqueue_recompute<T>(
        &self,
        credential: Credential,
        profile: PositiveId,
        request: RecomputeRequest,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
        reply: fn(mpsc::Sender<Result<T>>) -> RecomputeReply,
    ) -> Result<T> {
        self.enqueue_observed(
            credential,
            profile,
            cancelled,
            wait,
            |observations, sender| Operation::Recompute {
                request,
                observations,
                reply: reply(sender),
            },
        )
    }
    /// Creates an open draft from a fresh preparation carrying the source
    /// draft's Part decisions, as Node's `rebasePlanDraft` does. An open
    /// source is abandoned in the same transaction. Node's service then selects
    /// Required units; this method does not.
    pub fn rebase(
        &self,
        credential: Credential,
        profile: PositiveId,
        args: RebaseArgs<'_>,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<RebaseOutcome> {
        self.enqueue_rebase(
            credential,
            profile,
            args,
            cancelled,
            wait,
            RebaseReply::Draft,
        )
    }
    /// Rebases as Node's `PlanDraftWorkspaceService.rebase` does, including
    /// its automatic Required-unit selection of the rebased draft. The rebase
    /// commits before the selection starts.
    pub fn rebase_and_select(
        &self,
        credential: Credential,
        profile: PositiveId,
        args: RebaseArgs<'_>,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<RebaseWorkspaceOutcome> {
        self.enqueue_rebase(
            credential,
            profile,
            args,
            cancelled,
            wait,
            RebaseReply::Workspace,
        )
    }
    fn enqueue_rebase<T>(
        &self,
        credential: Credential,
        profile: PositiveId,
        args: RebaseArgs<'_>,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
        reply: fn(mpsc::Sender<Result<T>>) -> RebaseReply,
    ) -> Result<T> {
        let RebaseArgs {
            draft_id,
            idempotency_key,
            request,
        } = args;
        let idempotency_key = self::idempotency_key(idempotency_key)?;
        self.enqueue_observed(
            credential,
            profile,
            cancelled,
            wait,
            |observations, sender| Operation::Rebase {
                draft_id,
                idempotency_key,
                request,
                observations,
                reply: reply(sender),
            },
        )
    }
    /// Queues a write that reads Plan sources through the configured
    /// observations.
    fn enqueue_observed<T>(
        &self,
        credential: Credential,
        profile: PositiveId,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
        operation: impl FnOnce(Observations, mpsc::Sender<Result<T>>) -> Operation,
    ) -> Result<T> {
        let observations = self
            .observations
            .clone()
            .ok_or_else(|| anyhow!("Working-draft observations are not configured"))?;
        self.enqueue(cancelled, wait, |reply, cancelled| {
            Envelope::WorkingDraftWrite {
                command: Write {
                    credential,
                    profile,
                    policy: self.policy,
                    cancelled,
                    operation: operation(observations, reply),
                },
            }
        })
    }
    fn enqueue<T>(
        &self,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
        envelope: impl FnOnce(mpsc::Sender<Result<T>>, Arc<AtomicBool>) -> Envelope,
    ) -> Result<T> {
        let deadline = Instant::now()
            .checked_add(wait)
            .ok_or_else(|| anyhow!("Invalid queue wait"))?;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!(Failure::Stopped))?;
        loop {
            ensure!(!queue.closed, Failure::Stopped);
            ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, Failure::QueueFull);
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!(Failure::Stopped))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(envelope(reply, cancelled));
        self.shared.changed.notify_all();
        drop(queue);
        receiver
            .recv()
            .map_err(|_| anyhow!(Failure::OutcomeUnknown))?
    }
}

pub(super) fn execute(connection: &mut Connection, command: Command) -> Result<Outcome> {
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let (tenant, _) = auth::authority::queries::observe_reconciliation_actor(
        &tx,
        &command.credential,
        command.policy,
    )?;
    let profile = i64::try_from(command.profile.get())?;
    let Some(profile_row) = one(
        &tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(Outcome::ProfileNotFound);
    };
    let load = |draft_id: PositiveId| -> Result<Option<Draft>> {
        draft(&tx, &tenant, profile, i64::try_from(draft_id.get())?)
    };
    let outcome = match command.request {
        Request::List => Outcome::Listed { drafts: DraftList(rows(&tx, "SELECT id,state,lifecycle_version,snapshot_digest,base_revision_id,base_plan_version FROM plan_drafts WHERE tenant_id=? AND profile_id=? ORDER BY created_at,id", &[&tenant, &profile])?.iter().map(DraftIdentity::from_header).collect::<Result<_>>()?) },
        Request::Read { draft_id } => {
            let Some(draft) = load(draft_id)? else { return Ok(Outcome::DraftNotFound); };
            Outcome::Read { draft: snapshot(&draft)? }
        }
        Request::Diff { draft_id } => {
            let Some(draft) = load(draft_id)? else { return Ok(Outcome::DraftNotFound); };
            Outcome::Diff { diff: diff::read(&tx, &tenant, &profile_row, &draft)? }
        }
        Request::Workspace { draft_id } => {
            let Some(draft) = load(draft_id)? else { return Ok(Outcome::DraftNotFound); };
            draft_origin(&draft.header)?;
            Outcome::Workspace { workspace: required_units::view::workspace(&tx, &tenant, &profile_row, &draft)? }
        }
        Request::Header { draft_id } => {
            let Some(draft) = load(draft_id)? else { return Ok(Outcome::DraftNotFound); };
            Outcome::Header { header: header::view(&tx, &tenant, profile, num(&draft.header, "id")?)? }
        }
        Request::BuildHeader => Outcome::BuildHeader { header: header::build_view(&tx, &tenant, profile)? },
        Request::AcceptPreview { draft_id } => Outcome::AcceptPreview {
            preview: crate::plan_publication::preview::preview(&tx, &tenant, profile, i64::try_from(draft_id.get())?)?,
        },
    };
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    tx.commit()?;
    Ok(outcome)
}

pub(super) fn write(
    connection: &mut Connection,
    command: Write,
    admission: writer_admission::Deadline,
    tokens: &mut dyn crate::plan_publication::RequiredUnitTokenAllocator,
) {
    let Write {
        credential,
        profile,
        policy,
        cancelled,
        operation,
    } = command;
    match operation {
        Operation::Save {
            request,
            key,
            observations,
            reply,
        } => {
            let _ = reply.send(save::execute(
                connection,
                save::Command {
                    input: save::SaveCommand {
                        credential,
                        profile,
                        request,
                        key,
                    },
                    policy,
                    observations,
                    cancelled,
                },
                tokens,
                admission,
            ));
        }
        Operation::Autosave {
            request,
            key,
            observations,
            reply,
        } => {
            let _ = reply.send(autosave::execute(
                connection,
                autosave::Args {
                    credential: &credential,
                    policy,
                    profile,
                    request: &request,
                    key: &key,
                    observations: &observations,
                    cancelled: &cancelled,
                },
                admission,
            ));
        }
        Operation::Header {
            request,
            key,
            observations,
            reply,
        } => {
            let _ = reply.send(header::execute(
                connection,
                header::Args {
                    credential: &credential,
                    policy,
                    profile,
                    request: &request,
                    key: &key,
                    observations: &observations,
                    cancelled: &cancelled,
                },
                admission,
            ));
        }
        Operation::Restore {
            snapshot,
            request,
            key,
            observations,
            reply,
        } => {
            let _ = reply.send(restore::execute(
                connection,
                restore::Args {
                    credential: &credential,
                    policy,
                    profile,
                    snapshot,
                    request: &request,
                    key: &key,
                    observations: &observations,
                    cancelled: &cancelled,
                },
                admission,
            ));
        }
        Operation::Edit {
            draft_id,
            request,
            reply,
        } => {
            let edited = transact(connection, admission, &cancelled, |tx| {
                edit(tx, &credential, policy, profile, draft_id, &request)
            });
            let _ = reply.send(edited.and_then(|edited| match edited {
                Edited::Done(outcome) => Ok(outcome),
                Edited::Unselected(digest) => auto_select(
                    connection, credential, policy, profile, draft_id, &digest, &cancelled,
                ),
            }));
        }
        Operation::Abandon {
            draft_id,
            request,
            reply,
        } => {
            let _ = reply.send(transact(connection, admission, &cancelled, |tx| {
                abandon(tx, &credential, policy, profile, draft_id, &request)
            }));
        }
        Operation::Recompute {
            request,
            observations,
            reply,
        } => {
            let recomputed = recompute(
                connection,
                &credential,
                policy,
                profile,
                &request,
                &observations,
                &cancelled,
            );
            match reply {
                RecomputeReply::Draft(reply) => {
                    let _ = reply.send(recomputed);
                }
                RecomputeReply::Workspace(reply) => {
                    let _ = reply.send(recomputed.and_then(|outcome| {
                        select_recomputed(
                            connection, credential, policy, profile, &cancelled, outcome,
                        )
                    }));
                }
            }
        }
        Operation::Rebase {
            draft_id,
            idempotency_key,
            request,
            observations,
            reply,
        } => {
            let rebased = rebase::rebase(
                connection,
                rebase::Args {
                    credential: &credential,
                    policy,
                    profile_id: profile,
                    draft_id,
                    key: &idempotency_key,
                    request: &request,
                    observations: &observations,
                    cancelled: &cancelled,
                },
            );
            match reply {
                RebaseReply::Draft(reply) => {
                    let _ = reply.send(rebased);
                }
                RebaseReply::Workspace(reply) => {
                    let _ = reply.send(rebased.and_then(|outcome| {
                        rebase_workspace(
                            connection, credential, policy, profile, draft_id, &cancelled, outcome,
                        )
                    }));
                }
            }
        }
    }
}
/// Node's `autoReconcile` after `recomputePlanDraft`. A draft that already has
/// a selection returns its workspace from a read transaction.
fn select_recomputed(
    connection: &mut Connection,
    credential: Credential,
    policy: auth::AuthPolicy,
    profile: PositiveId,
    cancelled: &Arc<AtomicBool>,
    outcome: RecomputeOutcome,
) -> Result<RecomputeWorkspaceOutcome> {
    let draft = match outcome {
        RecomputeOutcome::Created { draft } | RecomputeOutcome::Existing { draft } => draft,
        refused => return Ok(RecomputeWorkspaceOutcome::Refused(refused)),
    };
    select_draft(connection, credential, policy, profile, cancelled, draft)
        .map(RecomputeWorkspaceOutcome::Selected)
}
/// Node's `rebase` after `rebasePlanDraft`: `autoReconcile` for a rebased or
/// existing draft, and `rebaseFailure`'s workspace for a source conflict or a
/// source that is not abandoned.
fn rebase_workspace(
    connection: &mut Connection,
    credential: Credential,
    policy: auth::AuthPolicy,
    profile: PositiveId,
    source: PositiveId,
    cancelled: &Arc<AtomicBool>,
    outcome: RebaseOutcome,
) -> Result<RebaseWorkspaceOutcome> {
    let mut read = |credential, draft_id| {
        let read = Command {
            credential,
            profile,
            request: Request::Workspace { draft_id },
            policy,
            cancelled: cancelled.clone(),
        };
        Ok::<_, anyhow::Error>(match execute(connection, read)? {
            Outcome::Workspace { workspace } => Some(workspace),
            _ => None,
        })
    };
    Ok(match outcome {
        RebaseOutcome::Rebased { draft } | RebaseOutcome::Existing { draft } => {
            RebaseWorkspaceOutcome::Workspace(select_draft(
                connection, credential, policy, profile, cancelled, draft,
            )?)
        }
        RebaseOutcome::SourceConflict { draft } => {
            let workspace = read(credential, draft.identity.draft_id)?
                .ok_or_else(|| anyhow!("Plan draft rebase source is missing"))?;
            RebaseWorkspaceOutcome::Workspace(WorkspaceOutcome::DraftChanged {
                workspace: Some(workspace),
            })
        }
        RebaseOutcome::NotAbandoned { state } => match read(credential, source)? {
            Some(workspace) => {
                RebaseWorkspaceOutcome::Workspace(WorkspaceOutcome::NotOpen { workspace })
            }
            None => RebaseWorkspaceOutcome::Refused(RebaseOutcome::NotAbandoned { state }),
        },
        refused => RebaseWorkspaceOutcome::Refused(refused),
    })
}
/// Node's `autoReconcile`: a draft that already has a selection returns its
/// workspace from a read transaction; otherwise it is selected.
fn select_draft(
    connection: &mut Connection,
    credential: Credential,
    policy: auth::AuthPolicy,
    profile: PositiveId,
    cancelled: &Arc<AtomicBool>,
    draft: DraftDocument,
) -> Result<WorkspaceOutcome> {
    let draft_id = draft.identity.draft_id;
    if !draft.ordinary["requiredUnitReconciliation"].is_null() {
        return ready_workspace(connection, credential, policy, profile, cancelled, draft_id);
    }
    auto_select(
        connection,
        credential,
        policy,
        profile,
        draft_id,
        &draft.identity.snapshot_digest,
        cancelled,
    )
}
/// Abandons every open draft of Build `profile`, for recompute and restore,
/// and returns their IDs in order. Each stays rebasable.
fn abandon_open(tx: &Transaction<'_>, tenant: &str, profile: i64) -> Result<Vec<i64>> {
    let open: Vec<i64> = tx
        .prepare("SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND state='open' ORDER BY id")?
        .query_map(params![tenant, profile], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute(
        "UPDATE plan_drafts SET state='abandoned',lifecycle_version=lifecycle_version+1 WHERE tenant_id=? AND profile_id=? AND state='open'",
        params![tenant, profile],
    )?;
    for &draft in &open {
        crate::domain_events::draft_changed(tx, tenant, profile, draft)?;
    }
    Ok(open)
}
/// The draft's workspace as it stands, without selecting.
fn ready_workspace(
    connection: &mut Connection,
    credential: Credential,
    policy: auth::AuthPolicy,
    profile: PositiveId,
    cancelled: &Arc<AtomicBool>,
    draft_id: PositiveId,
) -> Result<WorkspaceOutcome> {
    let read = Command {
        credential,
        profile,
        request: Request::Workspace { draft_id },
        policy,
        cancelled: cancelled.clone(),
    };
    match execute(connection, read)? {
        Outcome::Workspace { workspace } => Ok(WorkspaceOutcome::Ready { workspace }),
        _ => Err(anyhow!("Selected Plan draft is missing")),
    }
}
/// Node's `autoReconcile` for an unselected draft: a selection with no
/// decisions, keyed by the snapshot digest.
fn auto_select(
    connection: &mut Connection,
    credential: Credential,
    policy: auth::AuthPolicy,
    profile: PositiveId,
    draft_id: PositiveId,
    digest: &Digest,
    cancelled: &AtomicBool,
) -> Result<WorkspaceOutcome> {
    let command = ReconciliationCommand::automatic(profile, draft_id, digest, credential)?;
    // Node's selection is a separate transaction with its own busy wait.
    let admission = writer_admission::Deadline::at_dequeue();
    transact(connection, admission, cancelled, |tx| {
        required_units::reconcile::run(tx, &command, policy, &mut || {
            ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
            Ok(())
        })
    })
}
fn transact<T>(
    connection: &mut Connection,
    admission: writer_admission::Deadline,
    cancelled: &AtomicBool,
    body: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
    let tx = writer_admission::begin_immediate(connection, admission)?;
    let outcome = body(&tx)?;
    ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
    tx.commit()?;
    Ok(outcome)
}

fn baseline_required(tx: &Transaction<'_>, tenant: &str, profile: &Value) -> Result<bool> {
    let version = num(profile, "acceptedPlanVersion")?;
    Ok(if profile["acceptedPlanRevisionId"].is_null() {
        version != 0
            || one(
                tx,
                "SELECT id FROM parts WHERE tenant_id=? AND profile_id=? LIMIT 1",
                &[&tenant, &num(profile, "id")?],
            )?
            .is_some()
    } else {
        version <= 0
    })
}

enum Edited {
    Done(WorkspaceOutcome),
    Unselected(Digest),
}

fn edit(
    tx: &Transaction<'_>,
    credential: &Credential,
    policy: auth::AuthPolicy,
    profile_id: PositiveId,
    draft_id: PositiveId,
    request: &EditRequest,
) -> Result<Edited> {
    let (tenant, _) = auth::authority::queries::reconciliation_actor_ref(tx, credential, policy)?;
    let profile = i64::try_from(profile_id.get())?;
    let Some(profile_row) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(Edited::Done(WorkspaceOutcome::ProfileNotFound));
    };
    let id = i64::try_from(draft_id.get())?;
    let Some(current) = draft(tx, &tenant, profile, id)? else {
        return Ok(Edited::Done(WorkspaceOutcome::DraftNotFound));
    };
    let current_view = || workspace(tx, &tenant, &profile_row, &current);
    if current.header["state"] != "open" {
        return Ok(Edited::Done(WorkspaceOutcome::NotOpen {
            workspace: current_view()?,
        }));
    }
    if baseline_required(tx, &tenant, &profile_row)? {
        return Ok(Edited::Done(WorkspaceOutcome::AcceptedBaselineRequired));
    }
    if profile_row["acceptedPlanRevisionId"] != current.header["baseRevisionId"]
        || profile_row["acceptedPlanVersion"] != current.header["basePlanVersion"]
    {
        return Ok(Edited::Done(WorkspaceOutcome::BaseChanged {
            workspace: current_view()?,
        }));
    }
    let mut parts = current.parts.clone();
    let mut by_id = HashMap::new();
    for part in &mut parts {
        by_id.insert(num(part, "id")?, part);
    }
    for decision in &request.decisions {
        for part_id in decision.ids() {
            let Some(part) = by_id.get_mut(&i64::try_from(part_id.get())?) else {
                return Ok(Edited::Done(WorkspaceOutcome::DraftNotFound));
            };
            decision.apply(part);
        }
    }
    let selected = if parts == current.parts {
        current
    } else {
        let expected = request.expected_snapshot_digest.as_str();
        if current.header["snapshotDigest"] != expected {
            return Ok(Edited::Done(WorkspaceOutcome::DraftChanged {
                workspace: Some(current_view()?),
            }));
        }
        let planning = model::planning(&current.header, &current.inputs, &parts)?;
        let next = if current.header["digestFormat"] == "plan-draft-v2" {
            model::selection(&planning, None)
        } else {
            planning
        };
        ensure!(
            tx.execute(
                "UPDATE plan_drafts SET current_required_unit_reconciliation_id=NULL,snapshot_digest=? WHERE tenant_id=? AND profile_id=? AND id=? AND state='open' AND snapshot_digest=?",
                params![next, tenant, profile, id, expected],
            )? == 1,
            "Plan draft edit lost its draft"
        );
        crate::domain_events::draft_changed(tx, &tenant, profile, id)?;
        for decision in &request.decisions {
            for part_id in decision.ids() {
                let part_id = i64::try_from(part_id.get())?;
                let changed = match decision {
                    PartDecision::SetIncluded { value, .. } => tx.execute(
                        "UPDATE plan_draft_parts SET included=? WHERE tenant_id=? AND draft_id=? AND id=?",
                        params![value, tenant, id, part_id],
                    )?,
                    PartDecision::SetQuantityOverride { value, .. } => tx.execute(
                        "UPDATE plan_draft_parts SET quantity_override=?1,quantity_effective=COALESCE(?1,quantity_inferred) WHERE tenant_id=?2 AND draft_id=?3 AND id=?4",
                        params![value.map(|value| i64::try_from(value.get())).transpose()?, tenant, id, part_id],
                    )?,
                };
                ensure!(
                    changed == 1,
                    "Plan draft Part edit did not update every selected row"
                );
            }
        }
        draft(tx, &tenant, profile, id)?
            .filter(|d| d.header["snapshotDigest"] == next.as_str() && d.selected.is_none())
            .ok_or_else(|| anyhow!("Edited Plan draft could not be verified"))?
    };
    if selected.selected.is_some() {
        return Ok(Edited::Done(WorkspaceOutcome::Ready {
            workspace: workspace(tx, &tenant, &profile_row, &selected)?,
        }));
    }
    Ok(Edited::Unselected(
        DraftIdentity::from_header(&selected.header)?.snapshot_digest,
    ))
}

fn abandon(
    tx: &Transaction<'_>,
    credential: &Credential,
    policy: auth::AuthPolicy,
    profile_id: PositiveId,
    draft_id: PositiveId,
    request: &AbandonRequest,
) -> Result<AbandonOutcome> {
    let (tenant, _) = auth::authority::queries::reconciliation_actor_ref(tx, credential, policy)?;
    let profile = i64::try_from(profile_id.get())?;
    let Some(profile_row) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(AbandonOutcome::ProfileNotFound);
    };
    let expected = u32::try_from(request.expected_lifecycle_version.get())
        .ok()
        .filter(|version| *version < 2_147_483_647)
        .ok_or_else(|| anyhow!("Expected Plan draft lifecycle version must be a nonnegative safe integer below 2147483647"))?;
    let id = i64::try_from(draft_id.get())?;
    let Some(current) = draft(tx, &tenant, profile, id)? else {
        return Ok(AbandonOutcome::DraftNotFound);
    };
    let identity = DraftIdentity::from_header(&current.header)?;
    let current_view = || workspace(tx, &tenant, &profile_row, &current);
    match identity.state {
        DraftState::Consumed => {
            return Ok(AbandonOutcome::NotOpen {
                workspace: current_view()?,
            });
        }
        DraftState::Abandoned if identity.lifecycle_version == expected + 1 => {
            return Ok(AbandonOutcome::Ready {
                draft: identity.workspace_identity()?,
            });
        }
        _ if identity.lifecycle_version != expected => {
            return Ok(AbandonOutcome::DraftChanged {
                workspace: current_view()?,
            });
        }
        DraftState::Abandoned => {
            return Ok(AbandonOutcome::NotOpen {
                workspace: current_view()?,
            });
        }
        DraftState::Open => {}
    }
    ensure!(
        tx.execute(
            "UPDATE plan_drafts SET state='abandoned',lifecycle_version=lifecycle_version+1 WHERE tenant_id=? AND profile_id=? AND id=? AND state='open' AND lifecycle_version=?",
            params![tenant, profile, id, expected],
        )? == 1,
        "Plan draft transition lost its draft"
    );
    crate::domain_events::draft_changed(tx, &tenant, profile, id)?;
    let after = draft(tx, &tenant, profile, id)?
        .ok_or_else(|| anyhow!("Transitioned Plan draft is missing"))?;
    let after = DraftIdentity::from_header(&after.header)?;
    ensure!(
        after.state == DraftState::Abandoned
            && after.lifecycle_version == expected + 1
            && after.snapshot_digest == identity.snapshot_digest,
        "Transitioned Plan draft is invalid"
    );
    Ok(AbandonOutcome::Ready {
        draft: after.workspace_identity()?,
    })
}

fn recompute(
    connection: &mut Connection,
    credential: &Credential,
    policy: auth::AuthPolicy,
    profile_id: PositiveId,
    request: &RecomputeRequest,
    observations: &Observations,
    cancelled: &AtomicBool,
) -> Result<RecomputeOutcome> {
    ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
    // The outcomes reached outside `transact` check cancellation as it does.
    let finished = |outcome| {
        ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
        Ok(outcome)
    };
    let key = request.idempotency_key.as_str();
    let profile = i64::try_from(profile_id.get())?;
    let tx = connection.transaction()?;
    let (tenant, actor) =
        auth::authority::queries::observe_reconciliation_actor(&tx, credential, policy)?;
    let Some(profile_row) = one(
        &tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return finished(RecomputeOutcome::ProfileNotFound);
    };
    if let Some(winner) = winner(&tx, &tenant, &actor, profile, key)? {
        return finished(winner);
    }
    if baseline_required(&tx, &tenant, &profile_row)? {
        return finished(RecomputeOutcome::AcceptedBaselineRequired);
    }
    let base = json!({"baseRevisionId":profile_row["acceptedPlanRevisionId"], "basePlanVersion":profile_row["acceptedPlanVersion"]});
    tx.commit()?;
    let mut budget = observation::PreparationBudget::new(cancelled, observations.limits)?;
    let options = request.options();
    let prepared = preparation::prepare(
        connection,
        &tenant,
        profile,
        base.clone(),
        preparation::PreparationContext {
            options: &options,
            repos: &observations.repos,
        },
        observations.reads.as_ref(),
        &mut budget,
    );
    let mut prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return match error.downcast_ref() {
                Some(preparation::Failure::NoLayers) => finished(RecomputeOutcome::NoLayers),
                Some(preparation::Failure::NoStls) => finished(RecomputeOutcome::NoStls),
                Some(preparation::Failure::WouldWipe) => finished(RecomputeOutcome::WouldWipe),
                None => Err(error),
            };
        }
    };
    // Node's write transaction waits for the writer lock on its own.
    let admission = writer_admission::Deadline::at_dequeue();
    transact(connection, admission, cancelled, |tx| {
        let principal = auth::authority::queries::reconciliation_actor_ref(tx, credential, policy)?;
        ensure!(
            principal == (tenant.clone(), actor.clone()),
            "Authentication principal changed"
        );
        if let Some(winner) = winner(tx, &tenant, &actor, profile, key)? {
            return Ok(winner);
        }
        let Some(profile_row) = one(
            tx,
            "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
            &[&tenant, &profile],
        )?
        else {
            return Ok(RecomputeOutcome::BaseChanged);
        };
        if baseline_required(tx, &tenant, &profile_row)? {
            return Ok(RecomputeOutcome::AcceptedBaselineRequired);
        }
        if profile_row["acceptedPlanRevisionId"] != base["baseRevisionId"]
            || profile_row["acceptedPlanVersion"] != base["basePlanVersion"]
        {
            return Ok(RecomputeOutcome::BaseChanged);
        }
        let current = preparation::capture(
            tx,
            &tenant,
            profile,
            observations.reads.as_ref(),
            &mut budget,
        )?;
        if current.fingerprint != prepared.capture.fingerprint {
            return Ok(RecomputeOutcome::InputsChanged);
        }
        if carry_open_edits(
            tx,
            &tenant,
            profile,
            &mut prepared.parts,
            !request.apply_manifest,
            &mut budget,
        )? {
            prepared.digest =
                model::planning(&prepared.base, &prepared.capture.inputs, &prepared.parts)?;
        }
        let open = abandon_open(tx, &tenant, profile)?;
        let id = insert_snapshot(
            tx,
            &tenant,
            &actor,
            profile,
            key,
            prepared.rows(),
            open.last().copied().map_or(Origin::New, Origin::Draft),
        )?;
        let created = draft(tx, &tenant, profile, id)?
            .ok_or_else(|| anyhow!("Created Plan draft is missing"))?;
        Ok(RecomputeOutcome::Created {
            draft: snapshot(&created)?,
        })
    })
}

fn winner(
    tx: &Transaction<'_>,
    tenant: &str,
    actor: &str,
    profile: i64,
    key: &str,
) -> Result<Option<RecomputeOutcome>> {
    let Some(row) = one(
        tx,
        "SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND created_by=? AND idempotency_key=?",
        &[&tenant, &profile, &actor, &key],
    )?
    else {
        return Ok(None);
    };
    let saved = draft(tx, tenant, profile, num(&row, "id")?)?
        .ok_or_else(|| anyhow!("Saved Plan draft is missing"))?;
    Ok(Some(
        if draft_origin(&saved.header)? == json!({"kind":"recompute"}) {
            RecomputeOutcome::Existing {
                draft: snapshot(&saved)?,
            }
        } else {
            RecomputeOutcome::IdempotencyConflict
        },
    ))
}

const ASSIGNMENT_ROW_WORK: usize = 1024;

fn charge_open_draft(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<()> {
    let tables = [
        ("plan_drafts", storage::DRAFT_COLUMNS, "id=o.id", 1, 0),
        (
            "plan_draft_inputs",
            storage::INPUT_COLUMNS,
            "draft_id=o.id",
            1,
            0,
        ),
        (
            "plan_draft_parts",
            storage::PART_COLUMNS,
            "draft_id=o.id",
            1,
            0,
        ),
        (
            "plan_draft_required_unit_reconciliations",
            storage::RECONCILIATION_CHECK_COLUMNS,
            "id=o.current_required_unit_reconciliation_id",
            1,
            0,
        ),
        (
            "plan_draft_required_unit_reconciliations",
            storage::RECONCILIATION_COLUMNS,
            "id=o.current_required_unit_reconciliation_id",
            1,
            0,
        ),
        (
            "plan_draft_required_unit_decisions",
            storage::DECISION_COLUMNS,
            "reconciliation_id=o.current_required_unit_reconciliation_id",
            1,
            0,
        ),
        (
            "plan_draft_required_unit_assignments",
            storage::ASSIGNMENT_COLUMNS,
            "reconciliation_id=o.current_required_unit_reconciliation_id",
            0,
            ASSIGNMENT_ROW_WORK,
        ),
    ];
    for (table, columns, predicate, visits, overhead) in tables {
        budget.check()?;
        let lengths = columns
            .split(',')
            .map(|column| format!("COALESCE(LENGTH(CAST(t.{column} AS BLOB)),0)"))
            .collect::<Vec<_>>()
            .join("+");
        let sql = format!(
            "SELECT {lengths} FROM plan_drafts o JOIN {table} t ON t.tenant_id=o.tenant_id AND t.{predicate} WHERE o.tenant_id=? AND o.id=?"
        );
        let mut statement = tx.prepare(&sql)?;
        let mut query = statement.query(params![tenant, id])?;
        let mut count = 0;
        while let Some(row) = query.next()? {
            budget.check()?;
            ensure!(
                count < storage::ROW_LIMIT,
                "Reconciliation row limit exceeded"
            );
            count += 1;
            for _ in 0..visits {
                budget.visit(0)?;
            }
            budget.comparison(usize::try_from(row.get::<_, i64>(0)?)? + overhead)?;
        }
    }
    Ok(())
}

/// Node's `carryOpenDraftEdits`. A Part edit can commit after preparation
/// read the open draft, so its quantity override, and its inclusion when the
/// caller keeps the current draft, replace the prepared values. Returns
/// whether any Part changed.
///
/// This runs under the writer lock. `charge_open_draft` charges what loading
/// each open draft reads to the recompute's budget first. A default-grant
/// carry or a budget refusal can occupy the lock for seconds; there is no
/// elapsed-time guarantee. GRE-706 callers must size wait and UX for that.
fn carry_open_edits(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    parts: &mut [DraftPart],
    keep_inclusion: bool,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<bool> {
    let mut edits = HashMap::new();
    for open in rows_checked(
        tx,
        "SELECT id FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND state='open' ORDER BY created_at,id",
        &[&tenant, &profile],
        &|| {
            budget.check()?;
            Ok(())
        },
    )? {
        charge_open_draft(tx, tenant, num(&open, "id")?, budget)?;
        let Some(open) = storage::draft_checked(tx, tenant, profile, num(&open, "id")?, &|| {
            Ok(budget.check()?)
        })?
        else {
            continue;
        };
        for part in open.parts {
            budget.check()?;
            let key = (
                text(&part, "sourceLayer")?.to_owned(),
                text(&part, "partKey")?.to_owned(),
            );
            edits.insert(
                key,
                (part["included"].clone(), part["quantityOverride"].clone()),
            );
        }
    }
    let mut changed = false;
    for part in parts {
        budget.check()?;
        let key = (
            text(part, "sourceLayer")?.to_owned(),
            text(part, "partKey")?.to_owned(),
        );
        let Some((included, quantity)) = edits.get(&key) else {
            continue;
        };
        let before = part.clone();
        if keep_inclusion {
            part["included"] = included.clone();
        }
        if !quantity.is_null() {
            part["quantityOverride"] = quantity.clone();
            part["quantityEffective"] = quantity.clone();
        }
        changed |= *part != before;
    }
    Ok(changed)
}

/// Where a new draft row comes from. `Draft` and `Rebased` carry that
/// draft's header row forward; `Snapshot` writes the header the Plan
/// snapshot saved.
#[derive(Clone, Copy)]
enum Origin<'a> {
    New,
    Draft(i64),
    Rebased(&'a rebase::Source<'a>),
    Snapshot(Option<&'a header::BuildHeader>),
}

/// The rows a new draft holds: its Accepted base, inputs and Parts, and their
/// planning digest.
struct DraftRows<'a> {
    base: &'a Value,
    inputs: &'a [Value],
    parts: &'a [DraftPart],
    digest: &'a str,
}

fn insert_snapshot(
    tx: &Transaction<'_>,
    tenant: &str,
    actor: &str,
    profile: i64,
    key: &str,
    prepared: DraftRows<'_>,
    origin: Origin<'_>,
) -> Result<i64> {
    let (rebased_from, header_from) = match origin {
        Origin::New | Origin::Snapshot(_) => (None, None),
        Origin::Draft(id) => (None, Some(id)),
        Origin::Rebased(source) => (Some(source), Some(source.id)),
    };
    tx.execute(
        "INSERT INTO plan_drafts (tenant_id,profile_id,base_revision_id,base_plan_version,state,digest_format,snapshot_digest,created_by,idempotency_key,created_at,rebased_from_draft_id,rebased_from_lifecycle_version,rebased_from_snapshot_digest) VALUES (?,?,?,?,'open','plan-draft-v1',?,?,?,?,?,?,?)",
        params![
            tenant,
            profile,
            sql_scalar(&prepared.base["baseRevisionId"])?,
            num(prepared.base, "basePlanVersion")?,
            prepared.digest,
            actor,
            key,
            auth::catalog_timestamp(),
            rebased_from.map(|source| source.id),
            rebased_from.map(|source| source.generation),
            rebased_from.map(|source| source.digest),
        ],
    )?;
    let id = tx.last_insert_rowid();
    crate::domain_events::draft_changed(tx, tenant, profile, id)?;
    if let Some(from) = header_from {
        header::carry(tx, tenant, from, id)?;
    }
    if let Origin::Snapshot(Some(saved)) = origin {
        header::write(tx, tenant, id, saved, 1)?;
    }
    for input in prepared.inputs {
        let mut values = vec![tenant.to_owned().into(), id.into()];
        for field in model::INPUT_FIELDS {
            values.push(sql_scalar(&input[*field])?);
        }
        tx.execute(
            "INSERT INTO plan_draft_inputs (tenant_id,draft_id,source_id,source_layer,layer_order,tracking_kind,source_revision_id,manifest_digest,effective_naming_digest) VALUES (?,?,?,?,?,?,?,?,?)",
            rusqlite::params_from_iter(values),
        )?;
    }
    let fields = [vec!["baseRevisionPartId"], model::PART_FIELDS.to_vec()].concat();
    for part in prepared.parts {
        let mut values = vec![tenant.to_owned().into(), id.into()];
        values.extend(
            part.sql_fields(&fields)?
                .into_iter()
                .map(|(_, value)| value),
        );
        tx.execute(
            "INSERT INTO plan_draft_parts (tenant_id,draft_id,base_revision_part_id,part_key,relative_path,filename,source_layer,status,role_inferred,role_override,filament_color_id,filament_custom_hex,spoolman_spool_id,quantity_inferred,quantity_override,quantity_effective,included,notes,github_blob_url,geometry_same,requirement,option_group_id,manifest_source,artifact_digest) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params_from_iter(values),
        )?;
    }
    Ok(id)
}

fn sql_scalar(value: &Value) -> Result<rusqlite::types::Value> {
    Ok(match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| anyhow!("Invalid Plan draft integer"))?
            .into(),
        Value::String(text) => text.clone().into(),
        _ => return Err(anyhow!("Invalid Plan draft scalar")),
    })
}

impl Outcome {
    pub fn into_json_body(self) -> Result<DraftOutcomeJsonBody> {
        let (kind, field, body) = match self {
            Self::Read { draft } => ("read", Some("draft"), draft.into_json_body().into_bytes()),
            Self::Diff { diff } => ("diff", Some("diff"), diff.into_json_body().into_bytes()),
            Self::Listed { drafts } => ("listed", Some("drafts"), serde_json::to_vec(&drafts.0)?),
            Self::Workspace { workspace } => (
                "workspace",
                Some("workspace"),
                serde_json::to_vec(&workspace)?,
            ),
            Self::AcceptPreview { preview } => match preview {
                crate::plan_publication::preview::PreviewOutcome::Ready(preview) => (
                    "accept_preview",
                    Some("preview"),
                    serde_json::to_vec(&preview)?,
                ),
                crate::plan_publication::preview::PreviewOutcome::Refused(refused) => (
                    "accept_refused",
                    Some("refused"),
                    serde_json::to_vec(&refused)?,
                ),
            },
            Self::Header { header } => ("header", Some("header"), serde_json::to_vec(&header)?),
            Self::BuildHeader { header } => {
                ("build_header", Some("header"), serde_json::to_vec(&header)?)
            }
            Self::ProfileNotFound => ("profile_not_found", None, Vec::new()),
            Self::DraftNotFound => ("draft_not_found", None, Vec::new()),
        };
        Ok(outcome_json_body(kind, field, &body))
    }
}

impl RecomputeOutcome {
    pub fn into_json_body(self) -> DraftOutcomeJsonBody {
        let kind = match self {
            Self::Created { draft } => {
                return outcome_json_body("created", Some("draft"), &draft.into_json_body().0);
            }
            Self::Existing { draft } => {
                return outcome_json_body("existing", Some("draft"), &draft.into_json_body().0);
            }
            Self::ProfileNotFound => "profile_not_found",
            Self::AcceptedBaselineRequired => "accepted_baseline_required",
            Self::BaseChanged => "base_changed",
            Self::InputsChanged => "inputs_changed",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::NoLayers => "no_layers",
            Self::NoStls => "no_stls",
            Self::WouldWipe => "would_wipe",
        };
        outcome_json_body(kind, None, &[])
    }
}

fn outcome_json_body(kind: &str, field: Option<&str>, body: &[u8]) -> DraftOutcomeJsonBody {
    let mut output = b"{\"kind\":".to_vec();
    JsText::scalar(kind).write_json(&mut output);
    if let Some(field) = field {
        output.push(b',');
        JsText::scalar(field).write_json(&mut output);
        output.push(b':');
        output.extend_from_slice(body);
    }
    output.push(b'}');
    DraftOutcomeJsonBody(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The required-units fixture after `WriterOwner::open` has migrated it.
    struct Database(PathBuf);
    impl Database {
        fn new() -> (Self, Connection) {
            let directory =
                std::env::temp_dir().join(format!("pp-recompute-{}", rand::random::<u64>()));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("print-partner.db");
            std::fs::write(
                &path,
                include_bytes!("../tests/fixtures/required-units/unchanged.db"),
            )
            .unwrap();
            WriterOwner::open(&directory, crate::Limits::default())
                .unwrap()
                .0
                .shutdown()
                .unwrap();
            let connection = Connection::open(&path).unwrap();
            (Self(directory), connection)
        }
    }
    impl Drop for Database {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct NoReads;
    impl observation::DraftReads for NoReads {
        fn resolve_stl_root(
            &self,
            _: &observation::OwnedStlReadRequest,
            _: &mut observation::PreparationBudget<'_>,
        ) -> observation::ReadResult<Box<dyn observation::StlRootRead>> {
            Err(observation::ReadFailure::Unavailable)
        }
        fn read_checkout_manifest(
            &self,
            _: &observation::OwnedCheckoutReadRequest,
            _: observation::CheckoutManifestUse,
            _: &mut observation::PreparationBudget<'_>,
        ) -> observation::ReadResult<observation::DocumentRead> {
            Err(observation::ReadFailure::Unavailable)
        }
        fn read_catalog_document(
            &self,
            _: &observation::OwnedCatalogReadRequest,
            _: &mut observation::PreparationBudget<'_>,
        ) -> observation::ReadResult<observation::DocumentRead> {
            Err(observation::ReadFailure::Unavailable)
        }
    }

    /// Recomputes `profile` with `key`, without Source reads.
    fn read_phase(
        connection: &mut Connection,
        profile: u64,
        key: &str,
        cancelled: &AtomicBool,
    ) -> Result<RecomputeOutcome> {
        recompute(
            connection,
            &Credential::Session(auth::Secret::new("required-unit-fixture-secret".into())),
            auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
                first_user: auth::FirstUserTenant::NewUser,
            },
            PositiveId::new(profile).unwrap(),
            &RecomputeRequest::new(key, true).unwrap(),
            &Observations {
                reads: Arc::new(NoReads),
                limits: observation::PreparationLimits::default(),
                repos: PathBuf::new(),
            },
            cancelled,
        )
    }

    /// Sets the returned token from SQLite's progress handler, so it trips
    /// during the next query.
    fn cancel_during_sql(connection: &mut Connection) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        connection
            .progress_handler(
                1,
                Some(move || {
                    flag.store(true, Ordering::Release);
                    false
                }),
            )
            .unwrap();
        cancelled
    }

    #[test]
    fn cancellation_mid_carry_scan_stops_before_the_remaining_assignments() {
        let (_database, mut connection) = Database::new();
        let assignments: Vec<_> = (0..10000)
            .map(|i| json!({"kind":"create","draftPartId":4,"unitIndex":i}))
            .collect();
        connection.execute_batch("DELETE FROM plan_draft_parts WHERE draft_id=2 AND id<>4; UPDATE plan_draft_parts SET quantity_effective=10000 WHERE id=4").unwrap();
        connection.execute(
            "INSERT INTO plan_draft_required_unit_reconciliations(tenant_id,profile_id,draft_id,format,planning_digest,base_revision_id,selection_basis_digest,selection_basis_json,decision_digest,result_kind,result_digest,result_json,reconciliation_digest,expected_assignment_count,actor_id,idempotency_key,payload_digest,created_at)
             SELECT tenant_id,profile_id,id,'required-unit-reconciliation-v1','',base_revision_id,'','[]','','ready','',?,'',10000,created_by,'scan','','2026-01-01T00:00:00.000Z' FROM plan_drafts WHERE id=2",
            [json!({"kind":"ready","assignments":assignments,"surplus":[]}).to_string()],
        ).unwrap();
        connection.execute_batch(
            "WITH RECURSIVE units(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM units WHERE i<9999)
             INSERT INTO plan_draft_required_unit_assignments(tenant_id,reconciliation_id,target_draft_part_id,unit_index,kind)
             SELECT 'default',2,4,i,'create' FROM units;
             UPDATE plan_draft_required_unit_reconciliations SET finalized_at=created_at WHERE id=2;
             UPDATE plan_drafts SET current_required_unit_reconciliation_id=2 WHERE id=2;"
        ).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let steps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = steps.clone();
        connection
            .progress_handler(
                1,
                Some(move || {
                    if count.fetch_add(1, Ordering::Relaxed) >= 1000 {
                        flag.store(true, Ordering::Release);
                    }
                    false
                }),
            )
            .unwrap();
        let before = connection.total_changes();
        let tx = connection.transaction().unwrap();
        let mut budget = observation::PreparationBudget::new(
            &cancelled,
            observation::PreparationLimits::default(),
        )
        .unwrap();
        let error = carry_open_edits(&tx, "default", 1, &mut [], true, &mut budget).unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(observation::ReadFailure::Cancelled)
        ));
        assert!(
            steps.load(Ordering::Relaxed) < 2000,
            "scanned {} VM steps",
            steps.load(Ordering::Relaxed)
        );
        tx.rollback().unwrap();
        assert_eq!(connection.total_changes(), before);
    }

    #[test]
    fn cancellation_while_loading_a_replay_is_not_existing() {
        let (_database, mut connection) = Database::new();
        assert!(matches!(
            read_phase(&mut connection, 1, "working", &AtomicBool::new(false)),
            Ok(RecomputeOutcome::Existing { .. })
        ));
        let cancelled = cancel_during_sql(&mut connection);
        let Err(error) = read_phase(&mut connection, 1, "working", &cancelled) else {
            panic!("a replay cancelled while loading returned an outcome");
        };
        assert!(
            matches!(error.downcast_ref(), Some(Failure::Cancelled)),
            "{error:?}"
        );
    }

    #[test]
    fn cancellation_during_the_read_phase_is_not_a_refusal() {
        let (_database, mut connection) = Database::new();
        assert!(matches!(
            read_phase(&mut connection, 999, "new", &AtomicBool::new(false)),
            Ok(RecomputeOutcome::ProfileNotFound)
        ));
        connection
            .execute_batch(
                "UPDATE build_profiles SET accepted_plan_revision_id=NULL,accepted_plan_version=1 WHERE id=1",
            )
            .unwrap();
        assert!(matches!(
            read_phase(&mut connection, 1, "new", &AtomicBool::new(false)),
            Ok(RecomputeOutcome::AcceptedBaselineRequired)
        ));
        for profile in [999, 1] {
            let cancelled = cancel_during_sql(&mut connection);
            let Err(error) = read_phase(&mut connection, profile, "new", &cancelled) else {
                panic!("Plan {profile} cancelled while reading returned an outcome");
            };
            assert!(
                matches!(error.downcast_ref(), Some(Failure::Cancelled)),
                "{error:?}"
            );
        }
    }

    #[test]
    fn cancellation_stops_carrying_open_draft_edits() {
        let (_database, mut connection) = Database::new();
        let tx = connection.transaction().unwrap();
        let carry = |cancel_after: Option<usize>| {
            let cancelled = AtomicBool::new(false);
            let mut budget = observation::PreparationBudget::new(
                &cancelled,
                observation::PreparationLimits::default(),
            )
            .unwrap();
            if let Some(checks) = cancel_after {
                budget.cancel_after_checks(checks);
            }
            carry_open_edits(&tx, "default", 1, &mut [], true, &mut budget)
        };
        assert!(!carry(None).unwrap());
        let error = carry(Some(1)).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref(),
                Some(observation::ReadFailure::Cancelled)
            ),
            "{error:?}"
        );
    }

    /// Cancels on the first budget check inside the open-draft ID query, before
    /// `charge_open_draft` runs.
    #[test]
    fn cancellation_during_the_open_draft_id_query_writes_nothing() {
        let (_database, mut connection) = Database::new();
        let before = connection.total_changes();
        let tx = connection.transaction().unwrap();
        let cancelled = AtomicBool::new(false);
        let mut budget = observation::PreparationBudget::new(
            &cancelled,
            observation::PreparationLimits::default(),
        )
        .unwrap();
        budget.cancel_after_checks(0);
        let error = carry_open_edits(&tx, "default", 1, &mut [], true, &mut budget).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref(),
                Some(observation::ReadFailure::Cancelled)
            ),
            "{error:?}"
        );
        tx.rollback().unwrap();
        assert_eq!(connection.total_changes(), before);
    }

    /// Draft 2 trimmed to Part 4 at `quantity`, with a matching snapshot digest.
    fn single_part_draft(quantity: i64) -> (Database, Connection, String) {
        let (database, mut connection) = Database::new();
        let mut draft = {
            let tx = connection.transaction().unwrap();
            storage::draft(&tx, "default", 1, 2).unwrap().unwrap()
        };
        draft.parts.retain(|part| part["id"] == 4);
        draft.parts[0]["quantityOverride"] = json!(quantity);
        draft.parts[0]["quantityEffective"] = json!(quantity);
        let planning = model::planning(&draft.header, &draft.inputs, &draft.parts).unwrap();
        connection
            .execute(
                "DELETE FROM plan_draft_parts WHERE draft_id=2 AND id<>4",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE plan_draft_parts SET quantity_override=?1,quantity_effective=?1 WHERE id=4",
                [quantity],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE plan_drafts SET snapshot_digest=? WHERE id=2",
                [&planning],
            )
            .unwrap();
        (database, connection, planning)
    }

    fn select(
        connection: &mut Connection,
        planning: &str,
        cancelled: &AtomicBool,
    ) -> Result<WorkspaceOutcome> {
        auto_select(
            connection,
            Credential::Session(auth::Secret::new("required-unit-fixture-secret".into())),
            auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
                first_user: auth::FirstUserTenant::NewUser,
            },
            PositiveId::new(1).unwrap(),
            PositiveId::new(2).unwrap(),
            &serde_json::from_value(json!(planning)).unwrap(),
            cancelled,
        )
    }

    fn count(connection: &Connection, sql: &str) -> i64 {
        connection.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Part 4's quantity, the draft digest, and its selection pointer.
    fn part_state(connection: &Connection) -> (i64, String, Option<i64>) {
        connection
            .query_row(
                "SELECT quantity_override,snapshot_digest,current_required_unit_reconciliation_id
                 FROM plan_draft_parts p JOIN plan_drafts d ON d.id=p.draft_id WHERE p.id=4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    /// Cancellation is checked per assignment insert, so a cancelled selection
    /// stops inserting and rolls back instead of finishing every row.
    #[test]
    fn cancellation_during_assignment_insertion_rolls_back_selection() {
        let cap = storage::ASSIGNMENT_CAP;
        let (_database, mut connection, planning) = single_part_draft(cap);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let inserted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = inserted.clone();
        connection
            .update_hook(Some(move |action, _: &str, table: &str, _: i64| {
                if action == rusqlite::hooks::Action::SQLITE_INSERT
                    && table == "plan_draft_required_unit_assignments"
                    && seen.fetch_add(1, Ordering::Relaxed) + 1 == 64
                {
                    flag.store(true, Ordering::Release);
                }
            }))
            .unwrap();
        let error = select(&mut connection, &planning, &cancelled)
            .expect_err("cancelled selection must fail");
        assert!(
            matches!(error.downcast_ref(), Some(Failure::Cancelled)),
            "{error:?}"
        );
        // A hook counts attempted writes even when the transaction rolls back.
        assert_eq!(inserted.load(Ordering::Relaxed), 64);
        assert_eq!(part_state(&connection), (cap, planning, None));
        assert_eq!(
            count(
                &connection,
                "SELECT count(*) FROM plan_draft_required_unit_reconciliations WHERE draft_id=2"
            ),
            0
        );
        assert_eq!(
            count(
                &connection,
                "SELECT count(*) FROM plan_draft_required_unit_assignments WHERE target_draft_part_id=4"
            ),
            0
        );
    }

    /// A draft whose quantities sum past the assignment cap is refused before
    /// any reconciliation or assignment row is written.
    #[test]
    fn over_cap_selection_fails_before_any_row_is_inserted() {
        let cap = storage::ASSIGNMENT_CAP;
        let (_database, mut connection, planning) = single_part_draft(cap + 1);
        let inserted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = inserted.clone();
        connection
            .update_hook(Some(move |action, _: &str, table: &str, _: i64| {
                if action == rusqlite::hooks::Action::SQLITE_INSERT
                    && table.starts_with("plan_draft_required_unit_")
                {
                    seen.fetch_add(1, Ordering::Relaxed);
                }
            }))
            .unwrap();
        let error = select(&mut connection, &planning, &AtomicBool::new(false))
            .expect_err("over-cap selection must fail");
        assert_eq!(error.to_string(), "Reconciliation row limit exceeded");
        assert_eq!(inserted.load(Ordering::Relaxed), 0);
        assert_eq!(part_state(&connection), (cap + 1, planning, None));
    }

    /// At exactly the cap, the selection still succeeds and inserts every row.
    #[test]
    fn selection_at_the_assignment_cap_inserts_every_assignment() {
        let cap = storage::ASSIGNMENT_CAP;
        let (_database, mut connection, planning) = single_part_draft(cap);
        let outcome = select(&mut connection, &planning, &AtomicBool::new(false)).unwrap();
        assert!(
            matches!(outcome, WorkspaceOutcome::Ready { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            count(
                &connection,
                "SELECT count(*) FROM plan_draft_required_unit_assignments WHERE target_draft_part_id=4"
            ),
            cap
        );
    }
}
