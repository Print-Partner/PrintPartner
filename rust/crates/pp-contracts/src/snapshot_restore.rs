//! Wire type for `POST /plans/{id}/snapshots/{snapshot_id}/restore` (GRE-1094).
use crate::autosave::{PlanDraftBasis, PlanDraftIdentity};
use crate::reconciliation::required_nullable;
use serde::Deserialize;

/// How a snapshot is restored. `draft` is the only mode: the snapshot becomes
/// a draft, and only Accept publishes it.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    Draft,
}

/// The Accepted base and the open draft (`null` for none) the caller
/// observed. An observed open draft is abandoned for the restored one.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreSnapshotRequest {
    pub mode: RestoreMode,
    pub expected_base: PlanDraftBasis,
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<PlanDraftIdentity>")
    )]
    pub expected_draft: Option<PlanDraftIdentity>,
}
