import type { FastifyInstance } from "fastify";
import type { PrinterSendQueueItem } from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import type { IntegrationPort } from "../integrations/store.js";
import { getIntegrationConfig } from "../integrations/store.js";
import { selectFarmSuggestionItems } from "../services/plan-features.js";
import { sendProblem } from "../lib/api-error.js";
import { readFeatureAccess } from "../lib/feature-access.js";
import { desktopFeaturesEnabled } from "../lib/feature-toggles.js";
import { parseCheckoffUnits } from "../services/printer-checkoff.js";
import { loadFleet } from "../services/printer-fleet.js";
import {
  assertPrinterUploadArtifactPath,
  cancelPrinterSendQueueItem,
  enqueuePrinterSend,
  getPrinterSendQueueItem,
  listActivePrinterSendQueue,
  loadPrinterSendQueue,
} from "../services/printer-send-queue-store.js";
import {
  dispatchPrinterSendQueueItem,
  drainPrinterSendQueue,
  reconcilePrinterSendsAfterProcessStart,
  reconcilePrinterSendsAgainstLiveJobs,
  selectPrinterSendQueueDrainItems,
} from "../services/printer-send-queue.js";
import { computePrinterQueueSuggestions } from "../services/printer-queue-suggestions.js";
import { parsePrinterUploadMultipart } from "../services/printer-upload-multipart.js";
import { cleanupPrinterUploadArtifactDir } from "../services/printer-upload-job.js";
import type { InProcessJobRunner } from "../services/job-runner.js";

type RouteDeps = {
  repo: AppRepository;
  integrations: IntegrationPort;
  jobs: InProcessJobRunner;
};

const QUEUE_HOSTS = new Set(["moonraker", "prusalink"]);

function deniedFeature(access: {
  kind: "disabled" | "invalid";
  error: string;
  code: "FEATURE_DISABLED" | "FEATURE_TOGGLES_INVALID";
}) {
  const disabled = access.kind === "disabled";
  return {
    status: disabled ? 403 : 500,
    title: disabled ? "Forbidden" : "Internal Server Error",
    detail: access.error,
    code: access.code,
  };
}

function storedQueueAdmissionProblem(repo: AppRepository, items: PrinterSendQueueItem[]) {
  if (!desktopFeaturesEnabled()) return null;
  const fleet = loadFleet(repo);
  for (const item of items) {
    const integrationId = fleet
      .find((printer) => printer.id === item.printer_id)
      ?.integration_id?.trim();
    const type = integrationId ? getIntegrationConfig(repo, integrationId)?.type : undefined;
    if (type != null && QUEUE_HOSTS.has(type)) {
      const farm = readFeatureAccess(() => repo, "farm", item.profile_id);
      if (farm.kind !== "allowed") return deniedFeature(farm);
    }
    if (type !== "bambu") continue;
    if (item.profile_id == null) {
      return {
        status: 400,
        title: "Bad Request",
        detail: "Pick a plan to bind this send (profile_id required)",
      };
    }
    if (!repo.getOwnedProfileIdentity(item.profile_id)) {
      return { status: 404, title: "Not Found", detail: "Profile not found" };
    }
    const access = readFeatureAccess(() => repo, "bambu", item.profile_id);
    if (access.kind !== "allowed") return deniedFeature(access);
  }
  return null;
}

export async function registerPrinterSendQueueRoutes(
  app: FastifyInstance,
  deps: RouteDeps,
): Promise<void> {
  // Rust reconciles its own queue rows after cutover.
  if (!deps.jobs.rustOwns("reconcile.reconcilePrinterSendsAfterProcessStart")) {
    reconcilePrinterSendsAfterProcessStart(deps.repo);
  }

  const reconcileLiveSends = (tenantId: string) =>
    reconcilePrinterSendsAgainstLiveJobs(deps.repo, async (jobId) => {
      const snap = await deps.jobs.get(jobId, tenantId);
      return snap != null;
    });

  const makeStartJob =
    (tenantId: string) =>
    async (payload: {
      printer_id: string;
      artifact_path: string;
      filename: string;
      start: boolean;
      host_name?: string;
      profile_id?: number;
      checkoff_units?: ReturnType<typeof parseCheckoffUnits>;
    }) =>
      deps.jobs.start(
        "printer-upload",
        {
          ...payload,
          upload_job_id: undefined,
        },
        tenantId,
      );

  const getStatus = (integrationId: string) => deps.integrations.getStatus(integrationId);

  app.get("/printer-send-queue", async (request) => {
    await reconcileLiveSends(request.tenantId);
    const query = request.query as { active?: string };
    if (query.active === "1" || query.active === "true") {
      return { items: listActivePrinterSendQueue(deps.repo) };
    }
    return { items: loadPrinterSendQueue(deps.repo) };
  });

  /**
   * Smart queue routing suggestions.
   * Caller passes idle integration ids (from their live status poll) and
   * receives a suggestion per idle printer that has matching queued items.
   *
   * Query params:
   *   idle_integration_ids  comma-separated integration ids currently idle/complete
   */
  app.get(
    "/printer-send-queue/suggestions",
    { config: { rateLimit: { max: 60, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const access = selectFarmSuggestionItems(deps.repo);
      if (access.kind !== "allowed") {
        const status = access.kind === "disabled" ? 403 : 500;
        return reply.status(status).send({
          status,
          title: access.kind === "disabled" ? "Forbidden" : "Internal Server Error",
          detail:
            access.kind === "disabled"
              ? "Feature farm is disabled."
              : "Stored feature toggles are invalid.",
          code: access.kind === "disabled" ? "FEATURE_DISABLED" : "FEATURE_TOGGLES_INVALID",
        });
      }
      const query = request.query as { idle_integration_ids?: string };
      const rawIds = (query.idle_integration_ids ?? "").trim();
      const idleIds = new Set(
        rawIds
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
      );
      const fleet = loadFleet(deps.repo);
      const suggestions = computePrinterQueueSuggestions(deps.repo, fleet, idleIds, access.items);
      return { suggestions };
    },
  );

  app.post(
    "/printer-send-queue",
    { config: { rateLimit: { max: 10, timeWindow: "1 minute" } } },
    async (request, reply) => {
      let artifactPath: string | null = null;

      try {
        const parsed = await parsePrinterUploadMultipart(request, {
          exportsDir: deps.jobs.getExportsDir(request.tenantId),
          allowQueueFields: true,
        });
        if (!parsed.ok) {
          return sendProblem(reply, parsed.error.status, parsed.error.title, parsed.error.detail);
        }

        const {
          printer_id: printerId,
          start,
          filename: baseName,
          artifact_path,
          profile_id: profileId,
          checkoff_units_raw: checkoffUnitsRaw,
          wait_for_idle: waitForIdle = true,
          match = "pinned",
        } = parsed.value;
        artifactPath = artifact_path;

        const checkoff_units = parseCheckoffUnits(checkoffUnitsRaw);
        if (profileId == null) {
          return sendProblem(
            reply,
            400,
            "Bad Request",
            "Pick a plan to bind this send (profile_id required)",
          );
        }
        if (!deps.repo.getOwnedProfileIdentity(profileId)) {
          return sendProblem(reply, 404, "Not Found", "Profile not found");
        }

        const machine = loadFleet(deps.repo).find((m) => m.id === printerId);
        if (!machine) {
          return sendProblem(reply, 404, "Not Found", "Fleet printer not found");
        }
        const integrationId = machine.integration_id?.trim();
        if (!integrationId) {
          return sendProblem(
            reply,
            400,
            "Bad Request",
            "Printer is not linked to a host. Link a Moonraker or PrusaLink host in Settings.",
          );
        }
        const integration = getIntegrationConfig(deps.repo, integrationId);
        if (!integration) {
          return sendProblem(reply, 400, "Bad Request", "Linked printer host was not found");
        }
        if (integration.type === "bambu") {
          const access = readFeatureAccess(() => deps.repo, "bambu", profileId);
          if (access.kind !== "allowed") {
            const status = access.kind === "disabled" ? 403 : 500;
            return reply.status(status).send({
              status,
              title: access.kind === "disabled" ? "Forbidden" : "Internal Server Error",
              detail: access.error,
              code: access.code,
            });
          }
        }
        if (!QUEUE_HOSTS.has(integration.type)) {
          return sendProblem(
            reply,
            400,
            "Bad Request",
            `Queue is not supported for ${integration.type}`,
          );
        }
        const farm = readFeatureAccess(() => deps.repo, "farm", profileId);
        if (farm.kind !== "allowed") {
          const problem = deniedFeature(farm);
          return reply.status(problem.status).send(problem);
        }

        const item = enqueuePrinterSend(deps.repo, {
          filename: baseName,
          artifact_path: artifactPath,
          printer_id: printerId,
          start,
          wait_for_idle: waitForIdle,
          match,
          profile_id: profileId,
          checkoff_units,
          host_name: integration.name,
        });
        if (!item) {
          return sendProblem(reply, 400, "Bad Request", "Could not enqueue item");
        }
        artifactPath = null;
        return { item };
      } finally {
        if (artifactPath) {
          cleanupPrinterUploadArtifactDir(artifactPath);
        }
      }
    },
  );

  app.post(
    "/printer-send-queue/:id/dispatch",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const id = String((request.params as { id: string }).id ?? "").trim();
      const body = (request.body ?? {}) as { force?: boolean };
      if (!id) return sendProblem(reply, 400, "Bad Request", "id is required");
      const item = getPrinterSendQueueItem(deps.repo, id);
      const problem = storedQueueAdmissionProblem(deps.repo, item ? [item] : []);
      if (problem) return reply.status(problem.status).send(problem);
      await reconcileLiveSends(request.tenantId);
      const currentItem = getPrinterSendQueueItem(deps.repo, id);
      const currentProblem = storedQueueAdmissionProblem(
        deps.repo,
        currentItem ? [currentItem] : [],
      );
      if (currentProblem) return reply.status(currentProblem.status).send(currentProblem);
      const result = await dispatchPrinterSendQueueItem(
        deps.repo,
        deps.jobs.getExportsDir(request.tenantId),
        id,
        {
          startJob: makeStartJob(request.tenantId ?? "default"),
          getStatus,
          force: Boolean(body.force),
        },
      );
      if ("error" in result) {
        return sendProblem(
          reply,
          result.status,
          result.status === 404
            ? "Not Found"
            : result.status === 409
              ? "Conflict"
              : result.status >= 500
                ? "Internal Server Error"
                : "Bad Request",
          result.error,
        );
      }
      return result;
    },
  );

  app.post(
    "/printer-send-queue/drain",
    { config: { rateLimit: { max: 30, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const body = (request.body ?? {}) as { printer_id?: unknown };
      const rawPrinterId = body.printer_id;
      if (
        rawPrinterId !== undefined &&
        (typeof rawPrinterId !== "string" || !rawPrinterId.trim())
      ) {
        return sendProblem(reply, 400, "Bad Request", "printer_id must be a non-empty string");
      }
      const printerId = typeof rawPrinterId === "string" ? rawPrinterId.trim() : undefined;
      const problem = storedQueueAdmissionProblem(
        deps.repo,
        selectPrinterSendQueueDrainItems(deps.repo, printerId),
      );
      if (problem) return reply.status(problem.status).send(problem);
      await reconcileLiveSends(request.tenantId);
      const currentProblem = storedQueueAdmissionProblem(
        deps.repo,
        selectPrinterSendQueueDrainItems(deps.repo, printerId),
      );
      if (currentProblem) return reply.status(currentProblem.status).send(currentProblem);
      const results = await drainPrinterSendQueue(
        deps.repo,
        deps.jobs.getExportsDir(request.tenantId),
        {
          startJob: makeStartJob(request.tenantId ?? "default"),
          getStatus,
        },
        { printerId },
      );
      return { results };
    },
  );

  app.delete("/printer-send-queue/:id", async (request, reply) => {
    const id = String((request.params as { id: string }).id ?? "").trim();
    if (!id) return sendProblem(reply, 400, "Bad Request", "id is required");
    const existing = getPrinterSendQueueItem(deps.repo, id);
    if (!existing) {
      return sendProblem(reply, 404, "Not Found", "Queue item not found");
    }
    const item = cancelPrinterSendQueueItem(deps.repo, id);
    if (!item) {
      return sendProblem(reply, 409, "Conflict", "Item not cancellable");
    }
    try {
      const artifactPath = assertPrinterUploadArtifactPath(
        deps.jobs.getExportsDir(request.tenantId),
        item.artifact_path,
      );
      cleanupPrinterUploadArtifactDir(artifactPath);
    } catch {
      request.log.warn(
        { queueItemId: item.id },
        "Skipped cleanup of invalid cancelled upload path",
      );
    }
    return { item };
  });
}
