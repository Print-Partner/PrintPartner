//! One unauthenticated GET through the existing checked printer adapters.
pub struct PrinterProbeRequest {
    pub kind: String,
    pub address: String,
}

pub struct PrinterProbeResult {
    pub reachable: bool,
    pub kind: Option<String>,
    pub capabilities: Vec<String>,
    pub credentials_needed: bool,
    pub reason: String,
}
use pp_net::outbound::HttpGuard;
use serde_json::Value;
use std::time::Duration;

pub const DEADLINE: Duration = Duration::from_secs(8);

pub async fn probe(guard: HttpGuard, request: PrinterProbeRequest) -> PrinterProbeResult {
    let kind = request.kind.as_str();
    if !matches!(kind, "moonraker" | "prusalink") {
        return answer(
            false,
            None,
            false,
            "This printer kind is not supported by first-run probe.",
        );
    }
    let read = async {
        match kind {
            "moonraker" => {
                crate::moonraker_driver::Driver::new(guard, &request.address, None)
                    .probe_reply()
                    .await
            }
            _ => {
                crate::prusalink::Driver::new(guard, &request.address, "", None, None)
                    .probe_reply()
                    .await
            }
        }
    };
    let Ok(reply) = tokio::time::timeout(DEADLINE, read).await else {
        return answer(false, None, false, "Printer probe timed out.");
    };
    let Ok((status, body)) = reply else {
        return answer(
            false,
            None,
            false,
            "Could not reach the printer at an allowed HTTP address.",
        );
    };
    if matches!(status, 401 | 403) {
        return answer(true, Some(kind), true, "The printer requires credentials.");
    }
    let body = serde_json::from_slice::<Value>(&body).ok();
    let recognized = (200..300).contains(&status)
        && body.as_ref().is_some_and(|body| {
            if kind == "moonraker" {
                body.get("result")
                    .and_then(|v| v.get("klippy_state"))
                    .is_some_and(Value::is_string)
            } else {
                ["printer_model", "hostname", "name"]
                    .iter()
                    .any(|key| body.get(key).is_some_and(Value::is_string))
            }
        });
    if !recognized {
        return answer(
            true,
            None,
            false,
            "The address responded, but did not report the selected printer protocol.",
        );
    }
    let mut result = answer(true, Some(kind), false, "Printer responded successfully.");
    result.capabilities.push("status".into());
    if let Some(components) = body
        .as_ref()
        .and_then(|v| v.get("result"))
        .and_then(|v| v.get("components"))
        .and_then(Value::as_array)
    {
        for (component, capability) in [("file_manager", "files"), ("webcam", "cameras")] {
            if components.iter().any(|v| v.as_str() == Some(component)) {
                result.capabilities.push(capability.into());
            }
        }
    }
    result
}

fn answer(
    reachable: bool,
    kind: Option<&str>,
    credentials_needed: bool,
    reason: &str,
) -> PrinterProbeResult {
    PrinterProbeResult {
        reachable,
        kind: kind.map(str::to_owned),
        capabilities: vec![],
        credentials_needed,
        reason: reason.into(),
    }
}
