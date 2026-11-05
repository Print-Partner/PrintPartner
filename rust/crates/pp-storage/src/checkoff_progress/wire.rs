//! Typed views of the Checkoff burst bodies, for the generated client. The
//! routes parse and answer with JSON values (see [`super::burst`]); these types
//! describe the subset the replacement UI sends and reads. A receipt carries
//! more fields, such as each bed's stored link, which these types leave out.

use crate::read_model::farm::UnitResult;
use serde::{Deserialize, Serialize};

/// One unit decision in a burst bed, as `POST /printer-checkoff/verify` takes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UnitDecision {
    #[cfg_attr(feature = "codegen", schemars(range(min = 1)))]
    pub part_id: i64,
    pub unit_index: u32,
    pub result: UnitResult,
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
