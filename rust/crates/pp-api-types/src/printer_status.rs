//! Existing PrinterHostStatus JSON contract (absent optional fields stay absent).
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrinterHostState {
    Idle,
    Printing,
    Paused,
    Complete,
    Error,
    Offline,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrinterHostStatus {
    pub state: PrinterHostState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nozzle_temperature_c: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nozzle_target_c: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bed_temperature_c: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bed_target_c: Option<f64>,
    /// The host reported the print stopped on request (Moonraker `cancelled`,
    /// PrusaLink `STOPPED`). `state` still reads `idle`, as in Node; this
    /// keeps a cancel apart from a host back at rest. Never serialized.
    #[serde(skip)]
    pub cancelled: bool,
}
impl PrinterHostStatus {
    pub fn offline(message: &str) -> Self {
        Self::with_state(PrinterHostState::Offline, message)
    }
    pub fn unknown(message: &str) -> Self {
        Self::with_state(PrinterHostState::Unknown, message)
    }
    fn with_state(state: PrinterHostState, message: &str) -> Self {
        Self {
            state,
            message: Some(message.into()),
            progress: None,
            filename: None,
            eta_seconds: None,
            ip_address: None,
            uptime_seconds: None,
            nozzle_temperature_c: None,
            nozzle_target_c: None,
            bed_temperature_c: None,
            bed_target_c: None,
            cancelled: false,
        }
    }
}
