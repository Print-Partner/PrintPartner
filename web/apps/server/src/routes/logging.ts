import type { FastifyInstance } from "fastify";
import { z } from "zod";
import { getLogger, type LogSeverity, type LoggerConfig } from "../services/logger.js";

const loggingUpdateSchema = z
  .object({
    minSeverity: z.enum(["debug", "info", "warn", "error"]).optional(),
    enableWorkflowTracking: z.boolean().optional(),
  })
  .strict();

export async function registerLoggingRoutes(app: FastifyInstance): Promise<void> {
  const logger = getLogger();

  /**
   * GET /settings/logging/config
   * Get current logging configuration.
   */
  app.get<{ Reply: LoggerConfig }>("/settings/logging/config", async (_request, reply) => {
    const config = logger.getConfig();
    reply.send(config);
  });

  /**
   * POST /settings/logging/config
   * Update logging configuration.
   */
  app.post<{
    Body: unknown;
    Reply: LoggerConfig | { detail: string };
  }>("/settings/logging/config", async (request, reply) => {
    try {
      const updates = loggingUpdateSchema.parse(request.body);

      if (updates.minSeverity) {
        logger.setMinSeverity(updates.minSeverity);
      }

      if (updates.enableWorkflowTracking !== undefined) {
        logger.setWorkflowTracking(updates.enableWorkflowTracking);
      }

      const config = logger.getConfig();
      reply.send(config);
    } catch (error) {
      reply.code(400);
      reply.send({
        detail: error instanceof Error ? error.message : "Failed to update logging config",
      });
    }
  });

  /**
   * GET /settings/logging/logs
   * Get stored workflow logs with optional filtering.
   * Query params:
   *   - severity: filter by severity level
   *   - method: filter by HTTP method
   *   - limit: max logs to return (default 100)
   */
  app.get<{
    Querystring: {
      severity?: LogSeverity;
      method?: string;
      limit?: string;
    };
    Reply: object[];
  }>("/settings/logging/logs", { compress: false }, async (request, reply) => {
    const logs = logger.getLogs({
      severity: request.query.severity,
      method: request.query.method,
      limit: request.query.limit ? parseInt(request.query.limit, 10) : 100,
    });

    reply.send(logs as object[]);
  });

  /**
   * GET /settings/logging/stats
   * Get logging statistics (summary).
   */
  app.get("/settings/logging/stats", async (_request, reply) => {
    const stats = logger.getStats();
    reply.send(stats);
  });

  /**
   * GET /settings/logging/export
   * Export all logs as JSON for sharing with agents.
   * Query param:
   *   - format: "json" (default) or "jsonl" (newline-delimited)
   */
  app.get<{
    Querystring: {
      format?: "json" | "jsonl";
    };
    Reply: string;
  }>("/settings/logging/export", { compress: false }, async (request, reply) => {
    const format = request.query.format ?? "json";

    if (format === "jsonl") {
      reply.header("Content-Type", "application/x-ndjson");
      reply.send(logger.exportAsJsonl());
    } else {
      reply.header("Content-Type", "application/json");
      reply.send(logger.exportAsJson());
    }
  });

  /**
   * DELETE /settings/logging/logs
   * Clear all stored logs.
   */
  app.delete<{ Reply: { success: boolean } }>("/settings/logging/logs", async (_request, reply) => {
    logger.clear();
    reply.send({ success: true });
  });
}
