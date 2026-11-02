import type { PrinterHostStatus } from "@print-partner/contracts";
import type { PrinterMachine } from "@print-partner/domain";
import type { AppRepository } from "../db/repository.js";
import { getIntegrationConfig, type IntegrationPort } from "../integrations/store.js";
import { readFeatureAccess } from "../lib/feature-access.js";
import { desktopFeaturesEnabled } from "../lib/feature-toggles.js";

/**
 * Live host status per fleet printer, in fleet order; null when the printer has no host.
 * Bambu printers are not probed while the tenant-global Bambu feature is not admitted, and
 * access is re-read after every probe settles so a mid-probe revocation withholds the telemetry.
 */
export async function probeFleetHostStatus(
  repo: AppRepository,
  integrations: IntegrationPort | null | undefined,
  fleet: readonly PrinterMachine[],
): Promise<(PrinterHostStatus | null)[]> {
  const isBambu = fleet.map(
    (m) =>
      desktopFeaturesEnabled() &&
      !!m.integration_id &&
      getIntegrationConfig(repo, m.integration_id)?.type === "bambu",
  );
  const bambuDenial = (): PrinterHostStatus | null => {
    const access = readFeatureAccess(() => repo, "bambu");
    return access.kind === "allowed" ? null : { state: "unknown", message: access.error };
  };
  const probed = await Promise.all(
    fleet.map(async (m, i): Promise<PrinterHostStatus | null> => {
      const denial = isBambu[i] ? bambuDenial() : null;
      if (denial) return denial;
      if (!m.integration_id || !integrations) return null;
      try {
        return await integrations.getStatus(m.integration_id);
      } catch {
        return { state: "offline" };
      }
    }),
  );
  const denial = isBambu.some(Boolean) ? bambuDenial() : null;
  return probed.map((status, i) => (isBambu[i] && denial ? denial : status));
}
