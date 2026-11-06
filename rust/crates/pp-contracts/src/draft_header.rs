//! Wire types for `PUT /plans/{id}/drafts/header` (GRE-1197).
use crate::autosave::{PlanDraftBasis, PlanDraftIdentity};
use crate::reconciliation::required_nullable;
use serde::{Deserialize, Serialize};

/// A Build header as a draft proposes it. `null` clears an optional field.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftHeader {
    pub name: String,
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub order_number: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub customer: Option<String>,
    /// A calendar date, `YYYY-MM-DD`.
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub due_date: Option<String>,
}

/// The Accepted base and open draft the caller observed, the header version
/// it observed on that draft (`null` for none), and the whole header.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveDraftHeaderRequest {
    pub expected_base: PlanDraftBasis,
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<PlanDraftIdentity>")
    )]
    pub expected_draft: Option<PlanDraftIdentity>,
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(feature = "codegen", schemars(with = "crate::RequiredNullable<u32>"))]
    pub expected_header_version: Option<u32>,
    pub header: DraftHeader,
}
