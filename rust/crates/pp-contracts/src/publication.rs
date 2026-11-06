use crate::autosave::{ApplyPlanDraftReceipt, Digest, PlanDraftBasis, WireInteger};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyRequest {
    pub expected_snapshot_digest: Digest,
    pub expected_lifecycle_version: WireInteger<0, 2_147_483_646>,
    pub expected_base: PlanDraftBasis,
    #[serde(default)]
    pub remap_checkoff_links: bool,
    /// The draft header version the preview showed; `None` when it showed no
    /// header. Accept answers `draft_changed` when the header moved since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_header_version: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationReason {
    Missing,
    Unresolved,
    Stale,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmappableLink {
    #[serde(rename = "linkId")]
    pub link_id: String,
    pub filename: String,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConflict {
    pub operation_id: String,
    pub kind: String,
    pub state: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosedDraftState {
    Abandoned,
    Consumed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Applied {
        receipt: ApplyPlanDraftReceipt,
    },
    Existing {
        receipt: ApplyPlanDraftReceipt,
    },
    AlreadyApplied {
        receipt: ApplyPlanDraftReceipt,
    },
    NotFound,
    BuildArchived,
    NotOpen {
        state: ClosedDraftState,
    },
    DraftChanged,
    AcceptedBaselineRequired,
    BaseChanged,
    InputsChanged,
    ReconciliationRequired {
        reason: ReconciliationReason,
    },
    ProductionActive {
        checkoff_link_count: usize,
        send_queue_item_count: usize,
    },
    CheckoffRemapUnsafe {
        unmappable: Vec<UnmappableLink>,
    },
    ExecutionConflict {
        operations: Vec<ExecutionConflict>,
    },
    TransactionUnavailable,
    TokenAllocationFailed,
    IdempotencyConflict,
    /// The draft renames the Build to a name another Build already has.
    DuplicateBuildName {
        name: String,
    },
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn request() -> serde_json::Value {
        json!({"expected_snapshot_digest":"a".repeat(64),"expected_lifecycle_version":0,"expected_base":{"revision_id":null,"plan_version":0}})
    }
    #[test]
    fn apply_request_has_no_caller_tenant_or_actor_authority() {
        for field in ["tenant_id", "actor_id", "connection"] {
            let mut value = request();
            value[field] = json!("untrusted");
            assert!(serde_json::from_value::<ApplyRequest>(value).is_err());
        }
    }
    #[test]
    fn apply_request_rejects_inconsistent_base_and_lifecycle_overflow() {
        let mut value = request();
        value["expected_base"]["plan_version"] = json!(1);
        assert!(serde_json::from_value::<ApplyRequest>(value).is_err());
        let mut value = request();
        value["expected_lifecycle_version"] = json!(2147483647u64);
        assert!(serde_json::from_value::<ApplyRequest>(value).is_err());
    }
    #[test]
    fn omitted_remap_is_false_and_explicit_remap_is_preserved() {
        let value: ApplyRequest = serde_json::from_value(request()).unwrap();
        assert!(!value.remap_checkoff_links);
        let mut value = request();
        value["remap_checkoff_links"] = json!(true);
        let parsed: ApplyRequest = serde_json::from_value(value).unwrap();
        assert!(parsed.remap_checkoff_links);
    }
}
