/**
 * POST /api/discord-digest
 *
 * Trigger the morning farm digest and post it to the configured Discord webhook.
 * Called by an external morning-digest cron. Requires no body — all data is
 * sourced from the local DB and live printer integrations.
 *
 * Auth: requires a valid API key or session (same as /metrics).
 */

import type { FastifyInstance } from "fastify";
import type { ServerConfig } from "../config.js";
import type { AppRepository } from "../db/repository.js";
import type { IntegrationPort } from "../integrations/store.js";
import { readFarmAccess } from "../lib/farm-access.js";
import { loadFleet } from "../services/printer-fleet.js";
import { probeFleetHostStatus } from "../services/printer-fleet-status.js";
import {
  sendFarmDigest,
  type FarmDigestData,
  type FarmDigestPlanCollection,
} from "../services/discord-notify.js";
import { getLogger } from "../services/logger.js";

type DigestDeps = {
  repo: AppRepository;
  integrations?: IntegrationPort;
};

export async function registerDiscordDigestRoute(
  app: FastifyInstance,
  deps: DigestDeps,
): Promise<void> {
  app.post("/api/discord-digest", async (request, reply) => {
    const logger = getLogger();

    // Auth check: session or API key (same pattern as /metrics)
    const sessionUser = request.sessionUser;
    const authHeader = request.headers["authorization"];
    const apiKey = authHeader?.startsWith("Bearer ") ? authHeader.slice(7) : null;
    if (!sessionUser && !apiKey) {
      const { config } = app as FastifyInstance & { readonly config?: ServerConfig };
      if (config?.authRequired) {
        return reply.status(401).send({ detail: "Authentication required" });
      }
    }

    const access = readFarmAccess(() => deps.repo);
    if (access.kind !== "allowed") {
      const status = access.kind === "disabled" ? 403 : 500;
      return reply.status(status).send({
        status,
        title: access.kind === "disabled" ? "Forbidden" : "Internal Server Error",
        detail: access.error,
        code: access.code,
      });
    }

    const webhookUrl = deps.repo.getSetting("discord_notify_webhook_url") || null;
    if (!webhookUrl) {
      return reply.status(422).send({ detail: "discord_notify_webhook_url not configured" });
    }

    // Collect print stats for overnight window (last 8 hours)
    const windowHours = 8;
    const since = new Date(Date.now() - windowHours * 3600 * 1000).toISOString();
    let platesOvernight = 0;
    try {
      platesOvernight = deps.repo.recentPrintJobs(since, 1000).length;
    } catch {
      // table may not exist yet — proceed with 0
    }

    // Collect farm status
    const fleet = loadFleet(deps.repo);
    const statuses = await probeFleetHostStatus(deps.repo, deps.integrations, fleet);
    const printers = fleet.map((m, i) => ({
      name: m.name,
      state: statuses[i]?.state ?? "unknown",
      active_job: statuses[i]?.filename ?? null,
    }));

    let activePlans: FarmDigestPlanCollection;
    try {
      activePlans = {
        kind: "available",
        plans: deps.repo
          .listAcceptedProfileSummaries()
          .filter(({ header }) => !header.archived_at)
          .map(({ header, progress }) => ({ plan_name: header.name, progress })),
      };
    } catch {
      activePlans = { kind: "unavailable" };
      logger.log("error", "[discord-digest] Plan progress collection unavailable", {
        failure: "unexpected",
        operation: "discord_digest_plan_progress",
      });
    }

    const digestData: FarmDigestData = {
      platesOvernight,
      windowHours,
      printers,
      activePlans,
    };

    try {
      await sendFarmDigest(webhookUrl, digestData);
      logger.log("info", "[discord-digest] Morning digest sent", {
        plates: platesOvernight,
        printers: printers.length,
      });
      return reply.send({ ok: true, plates_overnight: platesOvernight });
    } catch (err) {
      logger.log(
        "error",
        `[discord-digest] Failed: ${err instanceof Error ? err.message : String(err)}`,
        {},
      );
      return reply.status(500).send({ detail: "Failed to send digest" });
    }
  });
}
