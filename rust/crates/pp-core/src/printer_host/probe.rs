//! First-run probe uses the gateway's checked drivers without a settings writer.
use pp_api::printer_host::probe::{PrinterProbePort, PrinterProbeRequest, PrinterProbeResult};
use pp_gateway::printer_host::{HostReads, PrinterProbeRequest as DriverRequest};
use std::{future::Future, pin::Pin, sync::Arc};

pub(crate) fn router(
    config: pp_api::admin::AdminHttpConfig,
    client: pp_storage::read_model::ReadClient,
) -> anyhow::Result<axum::Router> {
    Ok(pp_api::printer_host::probe::printer_probe_router(
        config,
        client,
        Arc::new(Probe(HostReads::new()?)),
    ))
}

struct Probe(HostReads);

impl PrinterProbePort for Probe {
    fn probe(
        &self,
        request: PrinterProbeRequest,
    ) -> Pin<Box<dyn Future<Output = PrinterProbeResult> + Send>> {
        let reads = self.0.clone();
        Box::pin(async move {
            let result = reads
                .probe(DriverRequest {
                    kind: request.kind,
                    address: request.address,
                })
                .await;
            PrinterProbeResult {
                kind: result.kind,
                reachable: result.reachable,
                credentials_needed: result.credentials_needed,
                reason: result.reason,
                capabilities: result.capabilities,
            }
        })
    }
}
