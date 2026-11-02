import type { FastifyInstance } from "fastify";
import type { ServerConfig } from "../config.js";
import type { AppRepository } from "../db/repository.js";
import {
  DATE_FORMAT_DEFAULT,
  DATE_FORMAT_PRESETS,
  buildSourceCategoryTree,
  isExternalAccessMode,
  SourceNamingContractError,
  invalidSourceNaming,
  invalidSourceNamingState,
  parseSourceNamingPutInput,
  parseSourceNamingResponse,
  sourceNamingConflict,
  sourceNotFound,
  type DateFormatId,
  type SourceNamingPutInput,
  validateDiscordWebhookUrl,
} from "@print-partner/contracts";
import { checkAppUpdate, resetAppUpdateCheckCache } from "../services/app-update-check.js";
import {
  DEFAULT_NAMING_PROFILE,
  mergeNamingProfiles,
  namingProfileFromDict,
  previewParse,
} from "@print-partner/domain";
import { trimmedString } from "../lib/secure-path.js";
import { loadFilamentCatalog } from "../services/filament-catalog.js";
import {
  addCustomFilament,
  deleteCustomFilament,
  listCustomFilaments,
} from "../services/custom-filaments.js";
import { getIntegrationConfig } from "../integrations/store.js";
import {
  listSpoolmanFilaments,
  spoolmanFilamentToCatalogColor,
} from "../integrations/spoolman-client.js";
import { workflowGuideForExternalAccess } from "./workflow-guide.js";
import { sendDiscordNotification } from "../services/discord-notify.js";
import { sourceIdFromParams } from "./settings-route-model.js";
import { readExternalAccessSettings, saveExternalAccessMode } from "../services/external-access.js";
import {
  parseSourceMonitoringUpdate,
  readStoredSourceUpdateIntervalHours,
} from "../services/source-monitoring-settings.js";
import { hostedPlanningPolicy } from "../lib/hosted-planning.js";
import { isRecord } from "../lib/guards.js";
import { readSpoolmanQueryAccess, spoolmanAccessProblem } from "../lib/spoolman-access.js";
import { registerTenantFeatureRoutes } from "./tenant-features.js";

type RouteDeps = { repo: AppRepository; dataDir: string; config?: ServerConfig };

export async function registerSettingsRoutes(app: FastifyInstance, deps: RouteDeps): Promise<void> {
  await registerTenantFeatureRoutes(app, deps.repo);
  app.get("/settings/update-check", async (request) => {
    if (!deps.config) {
      return {
        enabled: false,
        update_available: false,
        current_version: "unknown",
        latest_version: null,
        release_url: null,
        release_notes_url: null,
        deploy_mode: "self-host" as const,
        checked_at: null,
      };
    }
    const query = request.query as { refresh?: string };
    if (query.refresh === "1") {
      resetAppUpdateCheckCache();
    }
    return checkAppUpdate(deps.config);
  });

  // `categories` is the wire format: a flat, ordered list of "/"-separated
  // paths. `tree` is the same data nested, so clients need not rebuild it.
  app.get("/settings/source-categories", async () => ({
    categories: deps.repo.getSourceCategories(),
    tree: deps.repo.getSourceCategoryTree(),
  }));

  app.put("/settings/source-categories", async (request, reply) => {
    try {
      const body = request.body as {
        categories?: string[];
        replacements?: Record<string, string | null>;
      };
      const categories = deps.repo.saveSourceCategories(
        body.categories ?? [],
        body.replacements ?? {},
      );
      return { categories, tree: buildSourceCategoryTree(categories) };
    } catch (e) {
      return reply.status(400).send({ detail: e instanceof Error ? e.message : String(e) });
    }
  });

  app.get("/settings/date-format", async () => ({
    format: (deps.repo.getSetting("date_format") as DateFormatId | null) ?? DATE_FORMAT_DEFAULT,
  }));

  app.put("/settings/date-format", async (request, reply) => {
    const body = request.body as { format?: string };
    const format = body.format ?? "";
    if (!DATE_FORMAT_PRESETS.some((p) => p.id === format)) {
      return reply.status(400).send({ detail: "format must be one of the supported date formats" });
    }
    deps.repo.setSetting("date_format", format);
    return { format: format as DateFormatId };
  });

  app.get("/settings/stl-naming", async () => ({
    profile: deps.repo.getGlobalNaming(),
  }));

  app.put("/settings/stl-naming", async (request, reply) => {
    try {
      const body = request.body as { profile?: typeof DEFAULT_NAMING_PROFILE };
      const profile = deps.repo.saveGlobalNaming(body.profile ?? DEFAULT_NAMING_PROFILE);
      return { profile };
    } catch (e) {
      return reply.status(400).send({ detail: e instanceof Error ? e.message : String(e) });
    }
  });

  app.post("/settings/stl-naming/preview", async (request, reply) => {
    try {
      const body = request.body as {
        relative_path?: string;
        profile?: Partial<typeof DEFAULT_NAMING_PROFILE>;
      };
      const globalProfile = deps.repo.getGlobalNaming();
      const merged = body.profile
        ? mergeNamingProfiles(globalProfile, body.profile)
        : globalProfile;
      const profile = namingProfileFromDict(merged);
      return previewParse(String(body.relative_path ?? ""), profile);
    } catch (e) {
      return reply.status(400).send({ detail: e instanceof Error ? e.message : String(e) });
    }
  });

  app.get("/settings/github-pat", async () => ({
    configured: Boolean(deps.repo.getSetting("github_pat")),
    masked: null,
  }));

  app.put("/settings/github-pat", async (request, reply) => {
    const body = request.body;
    if (!isRecord(body) || typeof body.token !== "string") {
      return reply.status(400).send({ detail: "Token must be a string" });
    }
    const token = body.token.trim();
    deps.repo.setSetting("github_pat", token);
    return {
      configured: Boolean(token),
      masked: token ? "****" : null,
    };
  });

  app.get("/settings/source-update-check", async () => ({
    interval_hours: readStoredSourceUpdateIntervalHours(
      deps.repo.getSetting("source_update_check_hours"),
    ),
    auto_sync_updates: deps.repo.getSetting("discord_auto_sync_updates", "1") !== "0",
    last_checked_at: deps.repo.getSetting("source_update_check_last_run_at") || null,
  }));

  app.get("/settings/build-tracking", async () => ({
    assembly_tracking: deps.repo.getSetting("build_tracking_assembly", "0") !== "0",
  }));

  app.put("/settings/build-tracking", async (request) => {
    const body = request.body as { assembly_tracking?: boolean };
    deps.repo.setSetting("build_tracking_assembly", body.assembly_tracking ? "1" : "0");
    return { assembly_tracking: body.assembly_tracking === true };
  });

  app.get("/settings/external-access", async () => {
    if (deps.config && hostedPlanningPolicy(deps.config.deployMode).hostedPlanning) {
      return { mode: "off" as const };
    }
    return readExternalAccessSettings(deps.repo);
  });

  app.put("/settings/external-access", async (request, reply) => {
    const body: unknown = request.body;
    if (!body || typeof body !== "object" || !("mode" in body)) {
      return reply.status(400).send({ detail: "mode is required" });
    }
    if (!isExternalAccessMode(body.mode)) {
      return reply.status(400).send({
        detail: "mode must be off, api, or api_and_mcp",
      });
    }
    return saveExternalAccessMode(deps.repo, body.mode);
  });

  app.get("/settings/discord-notify", async () => ({
    webhook_url: deps.repo.getSetting("discord_notify_webhook_url") || null,
    notify_on_update: deps.repo.getSetting("discord_notify_on_update", "1") !== "0",
    notify_on_sync: deps.repo.getSetting("discord_notify_on_sync", "0") !== "0",
    auto_sync_updates: deps.repo.getSetting("discord_auto_sync_updates", "1") !== "0",
  }));

  app.put("/settings/discord-notify", async (request, reply) => {
    const body = request.body as {
      webhook_url?: string | null;
      notify_on_update?: boolean;
      notify_on_sync?: boolean;
      auto_sync_updates?: boolean;
    };
    if (body.webhook_url !== undefined) {
      const trimmed = (body.webhook_url ?? "").trim();
      if (trimmed.length > 0) {
        const error = validateDiscordWebhookUrl(trimmed);
        if (error) {
          return reply.status(400).send({ detail: error });
        }
      }
      deps.repo.setSetting("discord_notify_webhook_url", trimmed);
    }
    if (body.notify_on_update !== undefined) {
      deps.repo.setSetting("discord_notify_on_update", body.notify_on_update ? "1" : "0");
    }
    if (body.notify_on_sync !== undefined) {
      deps.repo.setSetting("discord_notify_on_sync", body.notify_on_sync ? "1" : "0");
    }
    if (body.auto_sync_updates !== undefined) {
      deps.repo.setSetting("discord_auto_sync_updates", body.auto_sync_updates ? "1" : "0");
    }
    return {
      webhook_url: deps.repo.getSetting("discord_notify_webhook_url") || null,
      notify_on_update: deps.repo.getSetting("discord_notify_on_update", "1") !== "0",
      notify_on_sync: deps.repo.getSetting("discord_notify_on_sync", "0") !== "0",
      auto_sync_updates: deps.repo.getSetting("discord_auto_sync_updates", "1") !== "0",
    };
  });

  app.post("/settings/discord-notify/test", async (_, reply) => {
    const webhookUrl = deps.repo.getSetting("discord_notify_webhook_url") || null;
    if (!webhookUrl) {
      return reply.status(400).send({ ok: false, error: "No Discord webhook URL configured" });
    }
    try {
      // Report the ACTUAL delivery outcome. This endpoint previously returned
      // {ok:true} as long as no exception escaped, so a webhook that Discord
      // rejected (401/404/429) still looked healthy from the UI — which is how
      // the broken #print-partner webhook went unnoticed.
      const result = await sendDiscordNotification(webhookUrl, "source.synced", {
        sourceName: "Test Source",
        sourceUrl: "https://github.com/example/test-repo",
        branch: "main",
        commitSha: "abc1234",
        stlCount: 42,
      });
      if (!result.ok) {
        // 502: we reached Discord but it refused to deliver.
        return reply.status(502).send({
          ok: false,
          error: result.error ?? "Discord webhook delivery failed",
          status: result.status,
          permanent: result.permanent ?? false,
          attempts: result.attempts,
        });
      }
      return { ok: true, status: result.status, attempts: result.attempts };
    } catch (err) {
      return reply
        .status(502)
        .send({ ok: false, error: err instanceof Error ? err.message : String(err) });
    }
  });

  app.put("/settings/source-update-check", async (request, reply) => {
    const parsed = parseSourceMonitoringUpdate(request.body);
    if (parsed.kind === "invalid") {
      return reply.status(400).send({ detail: parsed.detail });
    }
    if (parsed.update.intervalHours !== undefined) {
      deps.repo.setSetting("source_update_check_hours", String(parsed.update.intervalHours));
    }
    if (parsed.update.autoSyncUpdates !== undefined) {
      deps.repo.setSetting("discord_auto_sync_updates", parsed.update.autoSyncUpdates ? "1" : "0");
    }
    return {
      interval_hours: readStoredSourceUpdateIntervalHours(
        deps.repo.getSetting("source_update_check_hours"),
      ),
      auto_sync_updates: deps.repo.getSetting("discord_auto_sync_updates", "1") !== "0",
      last_checked_at: deps.repo.getSetting("source_update_check_last_run_at") || null,
    };
  });

  // --- Printer → Plan default bindings ---

  type PlanBinding = { integration_id: string; profile_id: number | null; updated_at: string };

  function loadPlanBindings(): PlanBinding[] {
    const raw = deps.repo.getSetting("printer.plan_bindings");
    return raw ? (JSON.parse(raw) as PlanBinding[]) : [];
  }

  app.get("/settings/printer-plan-bindings", async () => {
    return { bindings: loadPlanBindings() };
  });

  app.put("/settings/printer-plan-bindings", async (request) => {
    const body = request.body as { integration_id?: string; profile_id?: number | null };
    const integrationId = String(body.integration_id ?? "").trim();
    const profileId = body.profile_id == null ? null : Number(body.profile_id);
    const bindings = loadPlanBindings();
    const idx = bindings.findIndex((b) => b.integration_id === integrationId);
    const entry: PlanBinding = {
      integration_id: integrationId,
      profile_id: profileId,
      updated_at: new Date().toISOString(),
    };
    if (idx >= 0) bindings[idx] = entry;
    else bindings.push(entry);
    deps.repo.setSetting("printer.plan_bindings", JSON.stringify(bindings));
    return { bindings };
  });

  app.delete("/settings/printer-plan-bindings/:integration_id", async (request) => {
    const { integration_id } = request.params as { integration_id: string };
    const bindings = loadPlanBindings().filter((b) => b.integration_id !== integration_id);
    deps.repo.setSetting("printer.plan_bindings", JSON.stringify(bindings));
    return { ok: true };
  });
}

export async function registerSourceNamingRoutes(
  app: FastifyInstance,
  deps: RouteDeps,
): Promise<void> {
  app.get("/sources/:id/naming", async (request, reply) => {
    const sourceId = sourceIdFromParams(request.params);
    if (sourceId === null) {
      return reply.status(400).send(invalidSourceNaming("Source id must be a positive integer"));
    }
    const result = deps.repo.getSourceNaming(sourceId);
    switch (result.kind) {
      case "found":
        try {
          return parseSourceNamingResponse(result.settings);
        } catch {
          return reply.status(500).send(invalidSourceNamingState());
        }
      case "source_not_found":
        return reply.status(404).send(sourceNotFound());
      case "invalid_state":
        return reply.status(500).send(invalidSourceNamingState());
      default: {
        const exhaustive: never = result;
        return exhaustive;
      }
    }
  });

  app.put("/sources/:id/naming", async (request, reply) => {
    const sourceId = sourceIdFromParams(request.params);
    if (sourceId === null) {
      return reply.status(400).send(invalidSourceNaming("Source id must be a positive integer"));
    }
    let input: SourceNamingPutInput;
    try {
      input = parseSourceNamingPutInput(request.body);
    } catch (error) {
      if (!(error instanceof SourceNamingContractError)) throw error;
      return reply.status(400).send(invalidSourceNaming(error.message));
    }
    const result = deps.repo.saveSourceNaming(
      sourceId,
      input.use_defaults ? { kind: "use_defaults" } : { kind: "override", profile: input.override },
    );
    switch (result.kind) {
      case "saved":
        try {
          return parseSourceNamingResponse(result.settings);
        } catch {
          return reply.status(500).send(invalidSourceNamingState());
        }
      case "source_not_found":
        return reply.status(404).send(sourceNotFound());
      case "conflict":
        return reply.status(409).send(sourceNamingConflict());
      default: {
        const exhaustive: never = result;
        return exhaustive;
      }
    }
  });
}

export async function registerStubRoutes(app: FastifyInstance, deps: RouteDeps): Promise<void> {
  app.get("/settings/spoolman-default", async () => ({
    integration_id: deps.repo.getSetting("default_spoolman_integration_id") || null,
  }));

  app.put("/settings/spoolman-default", async (request, reply) => {
    const body = request.body as { integration_id?: unknown };
    const raw = trimmedString(body.integration_id);
    if (!raw) {
      deps.repo.setSetting("default_spoolman_integration_id", "");
      return { integration_id: null };
    }
    const item = getIntegrationConfig(deps.repo, raw);
    if (!item || item.type !== "spoolman") {
      return reply.status(400).send({ detail: "integration_id must be a Spoolman connector" });
    }
    deps.repo.setSetting("default_spoolman_integration_id", raw);
    return { integration_id: raw };
  });

  app.get("/filaments/catalog", async (request, reply) => {
    const access = readSpoolmanQueryAccess(deps.repo, request.query);
    if (access.kind !== "allowed" && access.kind !== "disabled") {
      const problem = spoolmanAccessProblem(access);
      return reply.status(problem.status).send(problem);
    }
    const catalog = loadFilamentCatalog();
    const custom = listCustomFilaments(deps.dataDir).map((f) => ({
      id: f.id,
      display_name: f.display_name,
      product_line: f.product_line,
      hex: f.hex,
      combo_label: f.combo_label,
      swatch_url: "",
    }));

    const query = request.query as { spoolman_integration_id?: unknown };
    const defaultId = deps.repo.getSetting("default_spoolman_integration_id") || null;
    const integrationId = trimmedString(query.spoolman_integration_id) || defaultId || "";
    let spoolman_colors: ReturnType<typeof spoolmanFilamentToCatalogColor>[] = [];
    let spoolman_status: "ok" | "empty" | "error" | "disabled" | "not_found" | undefined;
    let spoolman_error: string | null = null;

    if (access.kind === "disabled") {
      spoolman_status = "disabled";
      spoolman_error = access.error;
    } else if (integrationId) {
      const integration = getIntegrationConfig(deps.repo, integrationId);
      if (!integration) {
        spoolman_status = "not_found";
        spoolman_error = "Spoolman integration not found";
      } else if (integration.type !== "spoolman") {
        spoolman_status = "error";
        spoolman_error = "Selected integration is not a Spoolman connector";
      } else if (integration.config.enabled === false) {
        spoolman_status = "disabled";
        spoolman_error = "Spoolman integration is disabled in Settings";
      } else {
        try {
          const filaments = await listSpoolmanFilaments(integration.config);
          spoolman_colors = filaments.map((f) => spoolmanFilamentToCatalogColor(integration.id, f));
          if (filaments.length === 0) {
            spoolman_status = "empty";
            spoolman_error =
              "Spoolman returned no filament types — add filaments in Spoolman (spools alone are not listed here)";
          } else {
            spoolman_status = "ok";
          }
        } catch (e) {
          spoolman_status = "error";
          spoolman_error = e instanceof Error ? e.message : String(e);
          request.log.warn({ err: e, integrationId }, "Spoolman catalog fetch failed");
          spoolman_colors = [];
        }
      }
    }

    const currentAccess = readSpoolmanQueryAccess(deps.repo, request.query);
    if (currentAccess.kind === "disabled") {
      spoolman_colors = [];
      spoolman_status = "disabled";
      spoolman_error = currentAccess.error;
    } else if (currentAccess.kind !== "allowed") {
      const problem = spoolmanAccessProblem(currentAccess);
      return reply.status(problem.status).send(problem);
    }
    return {
      ...catalog,
      custom_colors: custom,
      spoolman_colors,
      ...(currentAccess.kind === "allowed" ? { default_spoolman_integration_id: defaultId } : {}),
      ...(spoolman_status ? { spoolman_status } : {}),
      ...(spoolman_error ? { spoolman_error } : {}),
    };
  });

  app.get("/filaments/custom", async () => ({ filaments: listCustomFilaments(deps.dataDir) }));

  app.post("/filaments/custom", async (request, reply) => {
    try {
      const body = request.body as { display_name?: string; hex?: string; product_line?: string };
      return addCustomFilament(deps.dataDir, {
        display_name: String(body.display_name ?? ""),
        hex: String(body.hex ?? ""),
        product_line: body.product_line,
      });
    } catch (e) {
      return reply.status(400).send({ detail: e instanceof Error ? e.message : String(e) });
    }
  });

  app.delete("/filaments/custom/:id", async (request, reply) => {
    try {
      deleteCustomFilament(deps.dataDir, decodeURIComponent((request.params as { id: string }).id));
      return reply.status(204).send();
    } catch (e) {
      return reply.status(404).send({ detail: e instanceof Error ? e.message : String(e) });
    }
  });

  app.get("/help/workflow", async () =>
    workflowGuideForExternalAccess(readExternalAccessSettings(deps.repo).mode),
  );
}
