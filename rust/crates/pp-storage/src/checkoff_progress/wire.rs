//! Typed views of the Checkoff burst bodies, for the generated client. The
//! routes parse and answer with JSON values (see [`super::burst`]); these types
//! describe the subset the replacement UI sends and reads. A receipt carries
//! more fields, such as each bed's stored link, which these types leave out.

use super::Basis;
use crate::read_model::farm::{CheckoffResultState, UnitResult};
use serde::{Deserialize, Serialize};

/// Why a unit was rejected; the route refuses a rejection without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum RejectReason {
    BedAdhesion,
    LayerShift,
    Warping,
    Stringing,
    UnderExtrusion,
    OverExtrusion,
    Dimensional,
    Collision,
    WrongFilament,
    Other,
}

/// One unit decision in a burst bed, as `POST /printer-checkoff/verify` takes it.
/// A rejected unit needs `reason`; `note` keeps its first 500 characters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UnitDecision {
    #[cfg_attr(feature = "codegen", schemars(range(min = 1)))]
    pub part_id: i64,
    pub unit_index: u32,
    pub result: UnitResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<RejectReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One bed of a burst: a printer Checkoff result and its unit decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BurstBed {
    pub link_id: String,
    pub decisions: Vec<UnitDecision>,
}

/// `POST /printer-checkoff/verify-batch`: repeating it with the same
/// `idempotency_key` returns the first receipt and writes nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct VerifyBatchRequest {
    /// 1 to 200 characters after trimming; the route checks it.
    pub idempotency_key: String,
    #[cfg_attr(feature = "codegen", schemars(length(min = 1, max = 50)))]
    pub beds: Vec<BurstBed>,
}

/// One bed's counts in a burst receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct BurstBedReceipt {
    pub units_confirmed: u64,
    pub units_rejected: u64,
}

/// The burst receipt: its undo token and what each bed recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct VerifyBatchReceipt {
    pub idempotency_key: String,
    pub undo_token: String,
    pub units_confirmed: u64,
    pub units_rejected: u64,
    pub beds: Vec<BurstBedReceipt>,
}

/// `POST /printer-checkoff/verify-batch/undo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UndoBatchRequest {
    pub undo_token: String,
}

/// The undo receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct UndoBatchReceipt {
    pub undo_token: String,
    pub undone_at: String,
}

/// One Required unit on a Build's Checkoff tab.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct CheckoffUnitState {
    pub token: String,
    pub unit_index: u32,
    pub object_name: String,
    pub printed: bool,
    pub assembled: bool,
}

/// One included Part of the Accepted Plan with its Required units.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct CheckoffPart {
    pub part_id: i64,
    pub filename: String,
    pub relative_path: String,
    pub source_layer: String,
    pub role: String,
    pub quantity: u64,
    pub printed_count: u64,
    pub assembled_count: u64,
    pub units: Vec<CheckoffUnitState>,
}

/// `GET /api/v2/plans/{id}/checkoff/units`: the Accepted Plan's included Parts
/// with each Required unit's token and durable printed and assembled state.
/// `basis` is what a manual burst sends as `expected`; it is `null` when the
/// Build has no Accepted Plan yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct CheckoffUnits {
    pub profile_id: i64,
    pub basis: Option<Basis>,
    pub parts: Vec<CheckoffPart>,
}

/// One unit of a manual burst: the state it sets. Printing a unit prints
/// every lower unit of its Part; clearing one clears every higher unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ManualBurstUnit {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembled: Option<bool>,
}

/// `POST /api/v2/plans/{id}/checkoff/bursts`: repeating it with the same
/// `idempotency_key` returns the first receipt and writes nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ManualBurstRequest {
    /// 1 to 200 characters after trimming; the route checks it.
    pub idempotency_key: String,
    pub expected: Basis,
    #[cfg_attr(feature = "codegen", schemars(length(min = 1, max = 1000)))]
    pub units: Vec<ManualBurstUnit>,
}

/// A touched Part's counts after a manual burst.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ManualBurstPart {
    pub part_id: i64,
    pub quantity: u64,
    pub printed_count: u64,
    pub assembled_count: u64,
}

/// The manual burst receipt: its undo token and the units it changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ManualBurstReceipt {
    pub idempotency_key: String,
    pub undo_token: String,
    pub profile_id: i64,
    pub printed_added: u64,
    pub printed_removed: u64,
    pub assembled_added: u64,
    pub assembled_removed: u64,
    pub parts: Vec<ManualBurstPart>,
}

/// `POST /printer-checkoff/unattributed/{id}/claim`: attributes an
/// unattributed print to a Build. With `selected_stl_basenames`, only the
/// objects of those plate files are attributed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ClaimUnattributedRequest {
    #[cfg_attr(feature = "codegen", schemars(range(min = 1)))]
    pub profile_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_stl_basenames: Option<Vec<String>>,
}

/// The printer Checkoff link a claim created. The stored link carries more
/// fields, such as its units, which this type leaves out.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ClaimedLink {
    pub id: String,
    pub profile_id: i64,
    pub printer_id: String,
    pub filename: String,
    pub state: CheckoffResultState,
}

/// The claim receipt.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ClaimUnattributedReceipt {
    pub ok: bool,
    pub link: ClaimedLink,
}

/// `POST /printer-checkoff/unattributed/{id}/dismiss` takes an empty object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DismissUnattributedRequest {}

/// The dismiss receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DismissUnattributedReceipt {
    pub ok: bool,
}

#[cfg(test)]
mod tests {
    use super::RejectReason::*;

    #[test]
    fn every_reject_reason_is_one_the_route_accepts() {
        let names: Vec<serde_json::Value> = [
            BedAdhesion,
            LayerShift,
            Warping,
            Stringing,
            UnderExtrusion,
            OverExtrusion,
            Dimensional,
            Collision,
            WrongFilament,
            Other,
        ]
        .iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect();
        assert_eq!(
            names,
            super::super::links::REASONS.map(serde_json::Value::from)
        );
    }
}
