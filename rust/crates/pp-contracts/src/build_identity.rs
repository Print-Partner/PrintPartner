pub use pp_api_types::build_identity::BuildName;
pub(crate) use pp_api_types::build_identity::trim_js;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBuildRequest {
    pub name: BuildName,
    pub base_project_id: Option<crate::autosave::PositiveId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAttachmentRequest {
    pub project_id: crate::autosave::PositiveId,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct ProfileSummary {
    pub id: i64,
    pub name: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub order_number: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub special_request: Option<String>,
    pub part_count: i64,
    pub accepted_progress: AcceptedProgressSummary,
    pub build_stale: bool,
    pub freshness: PlanFreshness,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub archived_at: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub last_used_at: Option<String>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptedProgressSummary {
    Ready {
        total_units: i64,
        remaining_units: i64,
    },
    Empty,
    Unavailable {
        reason: AcceptedProgressUnavailableReason,
    },
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptedProgressUnavailableReason {
    CompatibilityDirty,
    Uninitialized,
    Integrity,
    ConcurrentUpdate,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlanFreshness {
    /// Integrity failure while evaluating freshness; `code` names the broken input.
    Unavailable { code: String },
    Current {
        accepted_input_set_id: i64,
        accepted_at: String,
    },
    Stale {
        accepted_input_set_id: i64,
        accepted_at: String,
        reasons: FreshnessReasons<PlanStaleReason>,
        untracked_sources: Vec<PlanUntrackedReason>,
    },
    Untracked {
        #[cfg_attr(feature = "codegen", schemars(with = "crate::RequiredNullable<i64>"))]
        accepted_input_set_id: Option<i64>,
        #[cfg_attr(
            feature = "codegen",
            schemars(with = "crate::RequiredNullable<String>")
        )]
        accepted_at: Option<String>,
        reasons: FreshnessReasons<PlanUntrackedReason>,
    },
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanStaleReason {
    SourceRevisionChanged {
        source_id: i64,
        source_name: String,
        accepted_revision_id: i64,
        current_revision_id: i64,
    },
    SourceRevisionUnavailable {
        source_id: i64,
        source_name: String,
        accepted_revision_id: i64,
    },
    NamingRulesChanged {
        source_id: i64,
        source_name: String,
        accepted_digest: String,
        current_digest: String,
    },
    PlanInputsInvalid,
    PlanConfigurationChanged,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanUntrackedReason {
    NoAcceptedInputs,
    SourceRevisionUntracked { source_id: i64, source_name: String },
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct ProfileLayer {
    pub id: i64,
    pub layer_order: i64,
    pub layer_type: String,
    #[cfg_attr(feature = "codegen", schemars(with = "crate::RequiredNullable<i64>"))]
    pub project_id: Option<i64>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "crate::RequiredNullable<String>")
    )]
    pub project_name: Option<String>,
}

/// `GET /plans` body. `T` is [`ProfileSummary`] on `/` and `/api/v2`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ProfileList<T> {
    pub profiles: Vec<T>,
}

/// `GET /plans/{id}/layers` body.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ProfileLayers {
    pub profile_id: i64,
    pub layers: Vec<ProfileLayer>,
}

#[derive(Debug, Clone)]
pub struct FreshnessReasons<T> {
    first: T,
    rest: Vec<T>,
}

impl<T> FreshnessReasons<T> {
    pub fn new(first: T, rest: Vec<T>) -> Self {
        Self { first, rest }
    }
}

impl<T> TryFrom<Vec<T>> for FreshnessReasons<T> {
    type Error = &'static str;

    fn try_from(reasons: Vec<T>) -> Result<Self, Self::Error> {
        let mut reasons = reasons.into_iter();
        let first = reasons.next().ok_or("freshness requires a reason")?;
        Ok(Self::new(first, reasons.collect()))
    }
}

/// A nonempty JSON array of reasons.
#[cfg(feature = "codegen")]
impl<T: schemars::JsonSchema> schemars::JsonSchema for FreshnessReasons<T> {
    fn schema_name() -> String {
        format!("FreshnessReasons_{}", T::schema_name())
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::Array.into()),
            array: Some(Box::new(schemars::schema::ArrayValidation {
                items: Some(generator.subschema_for::<T>().into()),
                min_items: Some(1),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

impl<T: Serialize> Serialize for FreshnessReasons<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(1 + self.rest.len()))?;
        for reason in std::iter::once(&self.first).chain(&self.rest) {
            sequence.serialize_element(reason)?;
        }
        sequence.end()
    }
}
