import type {
  IntegrationSummary,
  IntegrationTestResult,
  PrinterHostStatus,
} from "@print-partner/contracts";
import { EngineHttpError, engineFetch } from "../engineTransport";

// Wire types belong to the contract. A hand-copied duplicate here drifts the
// moment the server adds a field, which is how the capability matrix ended up
// restated in the client in the first place.
export type { IntegrationSummary };

function v1Path(path: string): string {
  const normalized = path.startsWith("/") ? path : `/${path}`;
  return `/api/v1${normalized}`;
}

export async function fetchIntegrations(): Promise<IntegrationSummary[]> {
  const body = await engineFetch<{ integrations: IntegrationSummary[] }>(v1Path("/integrations"));
  return body.integrations;
}

export async function createIntegration(body: {
  type: string;
  name: string;
  config: Record<string, unknown>;
}): Promise<IntegrationSummary> {
  return engineFetch<IntegrationSummary>(v1Path("/integrations"), {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export async function updateIntegration(
  id: string,
  body: { name?: string; config?: Record<string, unknown> },
): Promise<IntegrationSummary> {
  return engineFetch<IntegrationSummary>(v1Path(`/integrations/${encodeURIComponent(id)}`), {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export async function deleteIntegration(id: string): Promise<void> {
  await engineFetch(v1Path(`/integrations/${encodeURIComponent(id)}`), { method: "DELETE" });
}

export async function testIntegration(id: string): Promise<IntegrationTestResult> {
  return engineFetch<IntegrationTestResult>(v1Path(`/integrations/${encodeURIComponent(id)}/test`), {
    method: "POST",
  });
}

export type TlsPin =
  | { pinned: false }
  | { pinned: true; sha256_prefix: string; pinned_at: string };

/** `null` when this runtime serves no pin route: Node answers 404 here. */
export async function fetchTlsPin(id: string): Promise<TlsPin | null> {
  try {
    return await engineFetch<TlsPin>(v1Path(`/integrations/${encodeURIComponent(id)}/tls-pin`));
  } catch (error) {
    if (error instanceof EngineHttpError && error.status === 404) return null;
    throw error;
  }
}

export async function resetTlsPin(id: string): Promise<void> {
  await engineFetch(v1Path(`/integrations/${encodeURIComponent(id)}/tls-pin/reset`), {
    method: "POST",
  });
}

export async function fetchIntegrationStatus(id: string): Promise<PrinterHostStatus> {
  return engineFetch<PrinterHostStatus>(v1Path(`/integrations/${encodeURIComponent(id)}/status`));
}
