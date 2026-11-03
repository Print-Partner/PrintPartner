pub use pp_domain::*;
const MIN_PLAN_DECISIONS: usize = 1;
const MAX_PLAN_DECISIONS: usize = 10_000;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{collections::HashSet, fmt};

pub(crate) fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PlanDraftBasisWire")]
pub struct PlanDraftBasis {
    revision_id: Option<PositiveId>,
    plan_version: Version,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanDraftBasisWire {
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(feature = "codegen", schemars(with = "RequiredNullable<PositiveId>"))]
    revision_id: Option<PositiveId>,
    plan_version: Version,
}

impl TryFrom<PlanDraftBasisWire> for PlanDraftBasis {
    type Error = &'static str;
    fn try_from(value: PlanDraftBasisWire) -> Result<Self, Self::Error> {
        if value.revision_id.is_none() != (value.plan_version.get() == 0) {
            return Err("basis_inconsistent");
        }
        Ok(Self {
            revision_id: value.revision_id,
            plan_version: value.plan_version,
        })
    }
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDraftIdentity {
    draft_id: PositiveId,
    state: DraftState,
    lifecycle_version: Version,
    snapshot_digest: Digest,
    base: PlanDraftBasis,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileTarget {
    part_key: WireText<1, 4096>,
    relative_path: WireText<1, 4096>,
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "RequiredNullable<WireText<0, 1000>>")
    )]
    source_layer: Option<WireText<0, 1000>>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanChoice {
    SetIncluded {
        target: FileTarget,
        value: bool,
    },
    SetQuantityOverride {
        target: FileTarget,
        #[serde(deserialize_with = "required_nullable")]
        #[cfg_attr(feature = "codegen", schemars(with = "RequiredNullable<Quantity>"))]
        value: Option<Quantity>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "SavePlanChoicesRequestWire")]
pub struct SavePlanChoicesRequest {
    expected_base: PlanDraftBasis,
    expected_draft: Option<PlanDraftIdentity>,
    remap_checkoff_links: bool,
    decisions: Vec<PlanChoice>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavePlanChoicesRequestWire {
    expected_base: PlanDraftBasis,
    #[serde(deserialize_with = "required_nullable")]
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "RequiredNullable<PlanDraftIdentity>")
    )]
    expected_draft: Option<PlanDraftIdentity>,
    remap_checkoff_links: bool,
    #[cfg_attr(
        feature = "codegen",
        schemars(length(min = "MIN_PLAN_DECISIONS", max = "MAX_PLAN_DECISIONS"))
    )]
    decisions: Vec<PlanChoice>,
}

impl TryFrom<SavePlanChoicesRequestWire> for SavePlanChoicesRequest {
    type Error = &'static str;
    fn try_from(value: SavePlanChoicesRequestWire) -> Result<Self, Self::Error> {
        if !(MIN_PLAN_DECISIONS..=MAX_PLAN_DECISIONS).contains(&value.decisions.len()) {
            return Err("decision_count;field=decisions");
        }
        if let Some(draft) = &value.expected_draft
            && (draft.state != DraftState::Open || draft.base != value.expected_base)
        {
            return Err("observed_draft_mismatch;field=expected_draft");
        }
        let mut fields = HashSet::new();
        for decision in &value.decisions {
            let (kind, target) = match decision {
                PlanChoice::SetIncluded { target, .. } => ("set_included", target),
                PlanChoice::SetQuantityOverride { target, .. } => ("set_quantity_override", target),
            };
            if !fields.insert((kind, target)) {
                return Err("duplicate_target_field;field=decisions");
            }
        }
        Ok(Self {
            expected_base: value.expected_base,
            expected_draft: value.expected_draft,
            remap_checkoff_links: value.remap_checkoff_links,
            decisions: value.decisions,
        })
    }
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyPlanDraftReceipt {
    profile_id: PositiveId,
    draft_id: PositiveId,
    revision_id: PositiveId,
    plan_version: PositiveId,
    draft_lifecycle_version: PositiveId,
    revision_digest: Digest,
    required_unit_mapping_digest: Digest,
    applied_at: WireText<1, 100>,
}

#[derive(Debug, Serialize)]
pub struct BoundaryError {
    pub category: String,
    pub field: String,
    pub detail: String,
}

/// Match JSON.parse before DTO validation: repeated object keys keep the last
/// value. DTO unknown-field, required-field and domain checks still run after it.
pub(crate) fn parse<T: de::DeserializeOwned>(text: &str) -> Result<T, BoundaryError> {
    let input: serde_json::Value = deserialize(text)?;
    let normalized = serde_json::to_string(&input).map_err(|error| BoundaryError {
        category: "invalid_json".into(),
        field: "$".into(),
        detail: error.to_string(),
    })?;
    deserialize(&normalized)
}

fn deserialize<T: de::DeserializeOwned>(text: &str) -> Result<T, BoundaryError> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        let detail = error.inner().to_string();
        let mut field = error.path().to_string();
        let categories = [
            "basis_inconsistent",
            "observed_draft_mismatch",
            "duplicate_target_field",
            "decision_count",
            "integer_bounds",
            "integer_type",
            "text_codepoint_bounds",
            "digest_pattern",
        ];
        let category = categories
            .into_iter()
            .find(|category| detail.starts_with(category))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if detail.contains("surrogate") || detail.contains("hex escape") {
                    "invalid_unicode_scalar_text".into()
                } else if detail.starts_with("missing field") {
                    "missing_field".into()
                } else if detail.starts_with("unknown field") {
                    "unknown_field".into()
                } else if detail.starts_with("unknown variant") {
                    "invalid_variant".into()
                } else {
                    "invalid_shape".into()
                }
            });
        if let Some((_, suffix)) = detail.split_once(";field=") {
            field = suffix
                .split_whitespace()
                .next()
                .unwrap_or(suffix)
                .to_owned();
        } else if let Some(quoted) = detail.split('`').nth(1)
            && (field == "." || field.is_empty())
        {
            field = quoted.into();
        }
        BoundaryError {
            category,
            field,
            detail,
        }
    })?;
    deserializer.end().map_err(|error| BoundaryError {
        category: "invalid_json".into(),
        field: "$".into(),
        detail: error.to_string(),
    })?;
    Ok(value)
}

pub fn parse_request(text: &str) -> Result<SavePlanChoicesRequest, BoundaryError> {
    let value = parse(text)?;
    crate::semantics::request(text, &value)?;
    Ok(value)
}
pub fn parse_receipt(text: &str) -> Result<ApplyPlanDraftReceipt, BoundaryError> {
    let value = parse(text)?;
    crate::semantics::receipt(text, &value)?;
    Ok(value)
}
pub fn parse_identity(text: &str) -> Result<PlanDraftIdentity, BoundaryError> {
    let value = parse(text)?;
    crate::semantics::identity(text, &value)?;
    Ok(value)
}

impl PlanDraftBasis {
    pub fn new(
        revision_id: Option<PositiveId>,
        plan_version: Version,
    ) -> Result<Self, &'static str> {
        PlanDraftBasisWire {
            revision_id,
            plan_version,
        }
        .try_into()
    }
    pub fn revision_id(&self) -> Option<PositiveId> {
        self.revision_id
    }
    pub fn plan_version(&self) -> Version {
        self.plan_version
    }
}
impl PlanDraftIdentity {
    pub fn new(
        draft_id: PositiveId,
        state: DraftState,
        lifecycle_version: Version,
        snapshot_digest: Digest,
        base: PlanDraftBasis,
    ) -> Self {
        Self {
            draft_id,
            state,
            lifecycle_version,
            snapshot_digest,
            base,
        }
    }
    pub fn draft_id(&self) -> PositiveId {
        self.draft_id
    }
    pub fn state(&self) -> &DraftState {
        &self.state
    }
    pub fn lifecycle_version(&self) -> Version {
        self.lifecycle_version
    }
    pub fn snapshot_digest(&self) -> &Digest {
        &self.snapshot_digest
    }
    pub fn base(&self) -> &PlanDraftBasis {
        &self.base
    }
}
impl FileTarget {
    pub fn new(
        part_key: WireText<1, 4096>,
        relative_path: WireText<1, 4096>,
        source_layer: Option<WireText<0, 1000>>,
    ) -> Self {
        Self {
            part_key,
            relative_path,
            source_layer,
        }
    }
    pub fn part_key(&self) -> &str {
        self.part_key.as_str()
    }
    pub fn relative_path(&self) -> &str {
        self.relative_path.as_str()
    }
    pub fn source_layer(&self) -> Option<&str> {
        self.source_layer.as_ref().map(WireText::as_str)
    }
}
impl PlanChoice {
    pub fn target(&self) -> &FileTarget {
        match self {
            Self::SetIncluded { target, .. } | Self::SetQuantityOverride { target, .. } => target,
        }
    }
}
impl SavePlanChoicesRequest {
    pub fn new(
        expected_base: PlanDraftBasis,
        expected_draft: Option<PlanDraftIdentity>,
        remap_checkoff_links: bool,
        decisions: Vec<PlanChoice>,
    ) -> Result<Self, &'static str> {
        SavePlanChoicesRequestWire {
            expected_base,
            expected_draft,
            remap_checkoff_links,
            decisions,
        }
        .try_into()
    }
    pub fn expected_base(&self) -> &PlanDraftBasis {
        &self.expected_base
    }
    pub fn expected_draft(&self) -> Option<&PlanDraftIdentity> {
        self.expected_draft.as_ref()
    }
    pub fn remap_checkoff_links(&self) -> bool {
        self.remap_checkoff_links
    }
    pub fn decisions(&self) -> &[PlanChoice] {
        &self.decisions
    }
}

pub struct ReceiptFields {
    pub profile_id: PositiveId,
    pub draft_id: PositiveId,
    pub revision_id: PositiveId,
    pub plan_version: PositiveId,
    pub draft_lifecycle_version: PositiveId,
    pub revision_digest: Digest,
    pub required_unit_mapping_digest: Digest,
    pub applied_at: WireText<1, 100>,
}
impl ApplyPlanDraftReceipt {
    pub fn new(fields: ReceiptFields) -> Self {
        Self {
            profile_id: fields.profile_id,
            draft_id: fields.draft_id,
            revision_id: fields.revision_id,
            plan_version: fields.plan_version,
            draft_lifecycle_version: fields.draft_lifecycle_version,
            revision_digest: fields.revision_digest,
            required_unit_mapping_digest: fields.required_unit_mapping_digest,
            applied_at: fields.applied_at,
        }
    }
    pub fn profile_id(&self) -> PositiveId {
        self.profile_id
    }
    pub fn draft_id(&self) -> PositiveId {
        self.draft_id
    }
    pub fn revision_id(&self) -> PositiveId {
        self.revision_id
    }
    pub fn plan_version(&self) -> PositiveId {
        self.plan_version
    }
    pub fn draft_lifecycle_version(&self) -> PositiveId {
        self.draft_lifecycle_version
    }
    pub fn revision_digest(&self) -> &Digest {
        &self.revision_digest
    }
    pub fn required_unit_mapping_digest(&self) -> &Digest {
        &self.required_unit_mapping_digest
    }
    pub fn applied_at(&self) -> &str {
        self.applied_at.as_str()
    }
}
impl fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} at {}: {}", self.category, self.field, self.detail)
    }
}
impl std::error::Error for BoundaryError {}

#[cfg(feature = "codegen")]
/// Schema-only marker for a required field that accepts null. A plain
/// `Option<T>` field would let the schema accept an omitted key.
pub struct RequiredNullable<T>(std::marker::PhantomData<T>);
#[cfg(feature = "codegen")]
impl<T: schemars::JsonSchema> schemars::JsonSchema for RequiredNullable<T> {
    fn schema_name() -> String {
        format!("RequiredNullable_{}", T::schema_name())
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        <Option<T> as schemars::JsonSchema>::json_schema(generator)
    }
}
#[cfg(feature = "codegen")]
impl schemars::JsonSchema for PlanDraftBasis {
    fn schema_name() -> String {
        "PlanDraftBasis".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        <PlanDraftBasisWire as schemars::JsonSchema>::json_schema(generator)
    }
}
#[cfg(feature = "codegen")]
impl schemars::JsonSchema for SavePlanChoicesRequest {
    fn schema_name() -> String {
        "SavePlanChoicesRequest".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        <SavePlanChoicesRequestWire as schemars::JsonSchema>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture(name: &str) -> Value {
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../../web/packages/contracts/test-fixtures/desktop/autosave-v1.json"
        ))
        .unwrap();
        corpus["normalized"]["schema_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["input"]
            .clone()
    }
    fn reject(request: &Value, category: &str) -> BoundaryError {
        let error = parse_request(&request.to_string()).unwrap_err();
        assert_eq!(error.category, category);
        assert!(!error.field.is_empty());
        error
    }

    #[test]
    fn required_nullable_fields_cannot_become_optional() {
        for (pointer, field) in [
            ("", "expected_draft"),
            ("/expected_base", "revision_id"),
            ("/decisions/0", "value"),
            ("/decisions/0/target", "source_layer"),
        ] {
            let mut request = fixture("quantity-null-reset");
            assert!(parse_request(&request.to_string()).is_ok());
            request
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            let error = reject(&request, "missing_field");
            assert!(error.detail.contains(field));
        }
    }
    #[test]
    fn integer_bounds_and_no_coercion_cannot_relax() {
        for value in [json!(0), json!(10_001), json!(1.5), json!("1"), json!(true)] {
            let mut request = fixture("quantity-null-reset");
            request["decisions"][0]["value"] = value;
            assert!(parse_request(&request.to_string()).is_err());
        }
        let mut request = fixture("quantity-null-reset");
        request["decisions"][0]["value"] = json!(10_000);
        assert!(parse_request(&request.to_string()).is_ok());
        let integral = request.to_string().replace("10000", "1.0");
        let parsed = parse_request(&integral).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap()["decisions"][0]["value"],
            1
        );
    }
    #[test]
    fn basis_consistency_cannot_relax() {
        let mut request = fixture("quantity-null-reset");
        request["expected_base"] = json!({ "revision_id": null, "plan_version": 1 });
        reject(&request, "basis_inconsistent");
        request["expected_base"] = json!({ "revision_id": 1, "plan_version": 0 });
        reject(&request, "basis_inconsistent");
    }
    #[test]
    fn observed_draft_state_and_base_linkage_cannot_relax() {
        for (pointer, value) in [
            ("/expected_draft/state", json!("consumed")),
            ("/expected_draft/base/revision_id", json!(999)),
        ] {
            let mut request = fixture("observed-open-draft");
            *request.pointer_mut(pointer).unwrap() = value;
            let error = reject(&request, "observed_draft_mismatch");
            assert_eq!(error.field, "expected_draft");
        }
    }
    #[test]
    fn duplicate_target_field_rejects_but_distinct_fields_remain_valid() {
        let mut request = fixture("different-fields-same-target");
        assert!(parse_request(&request.to_string()).is_ok());
        let duplicate = request["decisions"][0].clone();
        request["decisions"].as_array_mut().unwrap().push(duplicate);
        assert_eq!(
            reject(&request, "duplicate_target_field").field,
            "decisions"
        );
    }
    #[test]
    fn strict_fields_and_discriminator_cannot_relax() {
        for pointer in ["", "/expected_base", "/decisions/0", "/decisions/0/target"] {
            let mut request = fixture("quantity-null-reset");
            request
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("extra".into(), json!(true));
            reject(&request, "unknown_field");
        }
        let mut request = fixture("quantity-null-reset");
        request["decisions"][0]["kind"] = json!("set_filament");
        reject(&request, "invalid_variant");
    }
    #[test]
    fn codepoint_length_does_not_apply_legacy_utf16_bounds() {
        let mut request = fixture("quantity-null-reset");
        request["decisions"][0]["target"]["source_layer"] = json!("🙂".repeat(1000));
        assert!(parse_request(&request.to_string()).is_ok());
        request["decisions"][0]["target"]["source_layer"] = json!("🙂".repeat(1001));
        reject(&request, "text_codepoint_bounds");
    }
    #[test]
    fn receipt_identity_and_digest_preserve_exact_values() {
        let receipt = fixture("receipt-valid");
        assert_eq!(
            serde_json::to_value(parse_receipt(&receipt.to_string()).unwrap()).unwrap(),
            receipt
        );
        let mut invalid = receipt.clone();
        invalid["revision_digest"] = json!("B".repeat(64));
        assert_eq!(
            parse_receipt(&invalid.to_string()).unwrap_err().category,
            "digest_pattern"
        );
        invalid = receipt.clone();
        invalid["profile_id"] = json!(MAX_SAFE_INTEGER + 1);
        assert_eq!(
            parse_receipt(&invalid.to_string()).unwrap_err().category,
            "integer_bounds"
        );
        invalid = receipt;
        invalid["profile_id"] = json!(MAX_SAFE_INTEGER);
        assert!(parse_receipt(&invalid.to_string()).is_ok());
    }
    #[test]
    fn decision_count_bounds_cannot_relax() {
        let mut request = fixture("quantity-null-reset");
        request["decisions"] = json!([]);
        reject(&request, "decision_count");
        let decision = fixture("included-false")["decisions"][0].clone();
        request["decisions"] = Value::Array(vec![decision; 10_001]);
        reject(&request, "decision_count");
    }
}
