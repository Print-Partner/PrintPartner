use super::*;
use crate::printer_host::probe::{PrinterProbeRequest, PrinterProbeResult};

pub(super) fn schemas(dtos: &mut Map<String, Value>) -> Result<(), serde_json::Error> {
    dtos.insert(
        "PrinterProbeRequest".into(),
        serde_json::to_value(schemars::schema_for!(PrinterProbeRequest))?,
    );
    let mut result = schemars::schema_for!(PrinterProbeResult);
    // The response always serializes kind, including null.
    if let Some(object) = result.schema.object.as_mut() {
        object.required.insert("kind".into());
    }
    dtos.insert("PrinterProbeResult".into(), serde_json::to_value(result)?);
    Ok(())
}

pub(super) fn fixtures() -> Result<Vec<Value>, serde_json::Error> {
    let result = PrinterProbeResult {
        kind: Some("moonraker".into()),
        reachable: true,
        credentials_needed: false,
        reason: "Printer responded successfully.".into(),
        capabilities: vec!["status".into()],
    };
    Ok(vec![
        served(
            "PrinterProbeRequest",
            "request",
            PrinterProbeRequest {
                kind: "moonraker".into(),
                address: "http://fixture.invalid".into(),
            },
        )?,
        served("PrinterProbeResult", "identified", &result)?,
        served(
            "PrinterProbeResult",
            "unavailable",
            PrinterProbeResult {
                kind: None,
                reachable: false,
                reason: "Printer probe timed out.".into(),
                capabilities: vec![],
                ..result
            },
        )?,
    ])
}
