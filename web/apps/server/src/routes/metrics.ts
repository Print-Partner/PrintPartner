import type { FastifyInstance, FastifyRequest } from "fastify";
import type { AcceptedProfileProgress, AppRepository } from "../db/repository.js";
import { loadFleet } from "../services/printer-fleet.js";
import { probeFleetHostStatus } from "../services/printer-fleet-status.js";
import type { IntegrationPort } from "../integrations/store.js";
import { extractApiKey, type ApiKeyValidator } from "../middleware/api-key.js";
import { getLogger } from "../services/logger.js";
import { readFarmAccess } from "../lib/farm-access.js";

export type MetricsDeps = {
  repo: AppRepository;
  integrations?: IntegrationPort;
  validateApiKey: ApiKeyValidator;
  authRequired?: boolean;
  version: string;
};

/**
 * Prometheus metrics in Prometheus text format (exposition format v0.0.4).
 *
 * Counters (labeled by printer_id and material):
 *   plates_sent_total
 *   plates_completed_total
 *   plates_failed_total
 *
 * Gauges:
 *   parts_remaining{plan_id, plan_name}
 *   printers_idle
 *   printers_printing
 *   filament_consumed_g
 *
 * Plus existing HTTP-level counters.
 */
export async function registerMetricsRoutes(
  app: FastifyInstance,
  deps: MetricsDeps,
): Promise<void> {
  // In-memory HTTP metrics store
  const httpMetrics = {
    httpRequestsTotal: 0,
    httpRequestDurationMs: [] as number[],
    httpErrorsTotal: 0,
    httpSuccessTotal: 0,
  };

  // Hook to collect metrics
  app.addHook("onResponse", async (request, reply) => {
    if (request.url === "/metrics") return;

    httpMetrics.httpRequestsTotal++;
    if (reply.statusCode >= 400) {
      httpMetrics.httpErrorsTotal++;
    } else {
      httpMetrics.httpSuccessTotal++;
    }
    const startTime = (request as FastifyRequest & { readonly startTime?: number }).startTime;
    const duration = Date.now() - (startTime ?? Date.now());
    httpMetrics.httpRequestDurationMs.push(duration);
    if (httpMetrics.httpRequestDurationMs.length > 1000) {
      httpMetrics.httpRequestDurationMs.shift();
    }
  });

  /**
   * GET /metrics
   * Return Prometheus-compatible metrics.
   * In multi-user mode, requires a valid session or API key.
   */
  app.get("/metrics", async (request, reply) => {
    const apiKey = extractApiKey(request);
    if (apiKey && !deps.validateApiKey(apiKey)) {
      return reply.status(401).send({ detail: "Valid API key required" });
    }
    if (!request.sessionUser && !apiKey && deps.authRequired) {
      return reply.status(401).send({ detail: "Authentication required" });
    }

    const farmAccess = readFarmAccess(() => deps.repo);
    if (farmAccess.kind === "invalid") {
      return reply.status(500).send({
        status: 500,
        title: "Internal Server Error",
        detail: farmAccess.error,
        code: farmAccess.code,
      });
    }

    reply.header("Content-Type", "text/plain; version=0.0.4; charset=utf-8");

    const lines: string[] = [];

    // ─── HTTP counters ────────────────────────────────────────────────────────
    const sorted = [...httpMetrics.httpRequestDurationMs].sort((a, b) => a - b);
    const p50 = sorted.length > 0 ? (sorted[Math.floor(sorted.length * 0.5)] ?? 0) : 0;
    const p95 = sorted.length > 0 ? (sorted[Math.floor(sorted.length * 0.95)] ?? 0) : 0;
    const p99 = sorted.length > 0 ? (sorted[Math.floor(sorted.length * 0.99)] ?? 0) : 0;

    lines.push(
      "# HELP http_requests_total Total HTTP requests",
      "# TYPE http_requests_total counter",
      `http_requests_total ${httpMetrics.httpRequestsTotal}`,
      "",
      "# HELP http_request_duration_ms HTTP request duration in milliseconds",
      "# TYPE http_request_duration_ms summary",
      `http_request_duration_ms_sum ${httpMetrics.httpRequestDurationMs.reduce((a, b) => a + b, 0)}`,
      `http_request_duration_ms_count ${httpMetrics.httpRequestDurationMs.length}`,
      `http_request_duration_ms{quantile="0.5"} ${p50}`,
      `http_request_duration_ms{quantile="0.95"} ${p95}`,
      `http_request_duration_ms{quantile="0.99"} ${p99}`,
      "",
      "# HELP http_errors_total Total HTTP errors (4xx, 5xx)",
      "# TYPE http_errors_total counter",
      `http_errors_total ${httpMetrics.httpErrorsTotal}`,
      "",
      "# HELP http_success_total Total successful HTTP requests (2xx, 3xx)",
      "# TYPE http_success_total counter",
      `http_success_total ${httpMetrics.httpSuccessTotal}`,
      "",
      "# HELP http_error_rate Error rate (errors / total)",
      "# TYPE http_error_rate gauge",
      `http_error_rate ${httpMetrics.httpRequestsTotal > 0 ? (httpMetrics.httpErrorsTotal / httpMetrics.httpRequestsTotal).toFixed(4) : 0}`,
      "",
      "# HELP app_info Application version and build info",
      "# TYPE app_info gauge",
      `app_info{version="${escape(deps.version)}",node="${escape(process.version)}"} 1`,
      "",
    );

    // ─── Print-job counters and gauges ────────────────────────────────────────
    if (deps.repo) {
      const repo = deps.repo;

      // Query print_jobs table for counter metrics (grouped by printer_id, material, status)
      type JobRow = {
        printer_id: string;
        material: string;
        status: string;
        cnt: number;
        filament_sum: number | null;
      };
      let jobRows: JobRow[] = [];
      try {
        jobRows = repo.printJobMetrics();
      } catch {
        // Table may not exist yet on old DBs; skip silently
      }

      // Aggregate by (printer_id, material) across statuses
      type LabelKey = string;
      const sentMap = new Map<LabelKey, number>();
      const completedMap = new Map<LabelKey, number>();
      const failedMap = new Map<LabelKey, number>();
      let filamentConsumedTotal = 0;

      for (const row of jobRows) {
        const key = `printer_id="${escape(row.printer_id)}",material="${escape(row.material)}"`;
        if (row.status === "sent") sentMap.set(key, (sentMap.get(key) ?? 0) + row.cnt);
        if (row.status === "completed")
          completedMap.set(key, (completedMap.get(key) ?? 0) + row.cnt);
        if (row.status === "failed") failedMap.set(key, (failedMap.get(key) ?? 0) + row.cnt);
        filamentConsumedTotal += row.filament_sum ?? 0;
      }

      lines.push("# HELP plates_sent_total Total print plates sent to printers");
      lines.push("# TYPE plates_sent_total counter");
      if (sentMap.size === 0) {
        lines.push("plates_sent_total 0");
      } else {
        for (const [labels, val] of sentMap) {
          lines.push(`plates_sent_total{${labels}} ${val}`);
        }
      }
      lines.push("");

      lines.push("# HELP plates_completed_total Total print plates completed successfully");
      lines.push("# TYPE plates_completed_total counter");
      if (completedMap.size === 0) {
        lines.push("plates_completed_total 0");
      } else {
        for (const [labels, val] of completedMap) {
          lines.push(`plates_completed_total{${labels}} ${val}`);
        }
      }
      lines.push("");

      lines.push("# HELP plates_failed_total Total print plates that failed");
      lines.push("# TYPE plates_failed_total counter");
      if (failedMap.size === 0) {
        lines.push("plates_failed_total 0");
      } else {
        for (const [labels, val] of failedMap) {
          lines.push(`plates_failed_total{${labels}} ${val}`);
        }
      }
      lines.push("");

      lines.push(
        "# HELP filament_consumed_g Total filament consumed in grams (from recorded print jobs)",
      );
      lines.push("# TYPE filament_consumed_g gauge");
      lines.push(`filament_consumed_g ${filamentConsumedTotal}`);
      lines.push("");

      lines.push(
        "# HELP plan_progress_collection_available Whether accepted Plan progress was collected",
        "# TYPE plan_progress_collection_available gauge",
      );
      try {
        const profiles = repo
          .listAcceptedProfileSummaries()
          .filter(({ header }) => !header.archived_at);
        lines.push("plan_progress_collection_available 1", "");
        lines.push("# HELP plan_progress_state Accepted Progress state per active plan");
        lines.push("# TYPE plan_progress_state gauge");
        for (const { header, progress } of profiles) {
          const planLabel = `plan_id="${header.id}",plan_name="${escapeLabel(header.name)}"`;
          lines.push(
            `plan_progress_state{${planLabel},state="${acceptedProgressMetricState(progress)}"} 1`,
          );
        }
        lines.push("");
        lines.push("# HELP parts_remaining Remaining print units per active plan");
        lines.push("# TYPE parts_remaining gauge");
        for (const { header, progress } of profiles) {
          if (progress.kind !== "ready") continue;
          const planLabel = `plan_id="${header.id}",plan_name="${escapeLabel(header.name)}"`;
          lines.push(`parts_remaining{${planLabel}} ${progress.remainingUnits}`);
        }
        lines.push("");
      } catch {
        lines.push("plan_progress_collection_available 0", "");
        getLogger().log("error", "[metrics] Accepted Plan progress collection unavailable", {
          failure: "unexpected",
          operation: "metrics_plan_progress",
        });
      }

      // printers_idle / printers_printing from fleet + live integration status
      if (farmAccess.kind === "disabled") {
        lines.push(`# ${farmAccess.code}: ${farmAccess.error}`, "");
      } else {
        try {
          const fleet = loadFleet(repo);
          let idleCount = 0;
          let printingCount = 0;

          for (const status of await probeFleetHostStatus(repo, deps.integrations, fleet)) {
            if (status?.state === "idle" || status?.state === "complete") idleCount++;
            else if (status?.state === "printing" || status?.state === "paused") printingCount++;
          }

          lines.push("# HELP printers_idle Number of printers currently idle");
          lines.push("# TYPE printers_idle gauge");
          lines.push(`printers_idle ${idleCount}`);
          lines.push("");

          lines.push("# HELP printers_printing Number of printers currently printing or paused");
          lines.push("# TYPE printers_printing gauge");
          lines.push(`printers_printing ${printingCount}`);
          lines.push("");
        } catch {
          // skip
        }
      }
    }

    return reply.send(lines.join("\n") + "\n");
  });
}

function escape(s: string): string {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
}

function escapeLabel(s: string): string {
  return escape(s);
}

type AcceptedProgressMetricState =
  "ready" | "empty" | "compatibility_dirty" | "uninitialized" | "integrity" | "concurrent_update";

function acceptedProgressMetricState(
  progress: AcceptedProfileProgress,
): AcceptedProgressMetricState {
  switch (progress.kind) {
    case "ready":
    case "empty":
      return progress.kind;
    case "unavailable":
      return progress.reason;
    case "integrity_failure":
      return "integrity";
    case "concurrent_update":
      return "concurrent_update";
  }
}
