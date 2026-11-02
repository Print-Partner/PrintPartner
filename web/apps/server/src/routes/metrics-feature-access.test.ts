import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify, { type FastifyInstance } from "fastify";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { StatusExecution } from "../integrations/status-execution.js";
import { createIntegrationPort, type IntegrationAdapter } from "../integrations/store.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import * as printerFleet from "../services/printer-fleet.js";
import { registerMetricsRoutes } from "./metrics.js";
import { buildApp } from "../app.js";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { loadConfig } from "../config.js";

const invalid = {
  status: 500,
  title: "Internal Server Error",
  detail: "Stored feature toggles are invalid.",
  code: "FEATURE_TOGGLES_INVALID",
};
const disabledMarker = "# FEATURE_DISABLED: Feature farm is disabled.";

describe("metrics farm feature admission", () => {
  let root: string;
  let database: SqliteDatabase;
  let repo: AppRepository;
  let app: FastifyInstance;
  let execution: StatusExecution;
  const probe = vi.fn<NonNullable<IntegrationAdapter["getStatus"]>>(async () => ({
    state: "idle",
  }));

  beforeEach(async () => {
    vi.stubEnv("PP_DESKTOP_REBUILD", "1");
    vi.stubEnv("DEPLOY_MODE", "self-host");
    root = mkdtempSync(join(tmpdir(), "pp-metrics-features-"));
    database = new SqliteDatabase(root);
    database.connect();
    repo = new AppRepository(getDb(database), undefined, database.reposDir);
    execution = new StatusExecution();
    const integrations = createIntegrationPort({
      repo,
      statusExecution: execution,
      getAdapter: (type) => ({
        type,
        getStatus: probe,
        testConnection: async () => ({ ok: true }),
      }),
    });
    const connector = integrations.create({
      type: "moonraker",
      name: "Workshop",
      config: { host: "printer.invalid" },
    });
    repo.setSetting(
      "printer.fleet",
      JSON.stringify([{ id: "printer", name: "Workshop", integration_id: connector.id }]),
    );
    app = Fastify();
    app.decorateRequest("tenantId", "default");
    app.decorateRequest("sessionUser", null);
    app.addHook("onRequest", (request, _reply, done) => {
      const tenant = request.headers["x-test-tenant"];
      request.tenantId = typeof tenant === "string" ? tenant : "default";
      if (request.headers["x-test-session"] === "1") {
        request.sessionUser = {
          user_id: "user",
          tenant_id: request.tenantId,
          login: "user",
          display_name: "User",
          email: null,
          provider: "basic",
          is_admin: false,
        };
      }
      tenantStorage.run(request.tenantId, done);
    });
    await registerMetricsRoutes(app, {
      repo,
      integrations,
      validateApiKey: (key) => key === "valid",
      authRequired: true,
      version: "test",
    });
    await app.ready();
    vi.clearAllMocks();
    vi.spyOn(printerFleet, "loadFleet");
    vi.spyOn(execution, "probe");
  });

  afterEach(async () => {
    await app.close();
    await execution.stop();
    database.close();
    rmSync(root, { recursive: true, force: true });
    vi.restoreAllMocks();
    vi.unstubAllEnvs();
  });

  function get(tenant = "default") {
    return app.inject({
      method: "GET",
      url: "/metrics",
      headers: { authorization: "Bearer valid", "x-test-tenant": tenant },
    });
  }

  function noFarmWork() {
    expect(printerFleet.loadFleet).not.toHaveBeenCalled();
    expect(execution.probe).not.toHaveBeenCalled();
    expect(probe).not.toHaveBeenCalled();
    expect(execution.snapshot()).toEqual({ state: "running", tasks: [] });
  }

  it.each([undefined, "{}", '{"farm":false}'])(
    "omits farm metrics for %j while retaining core metrics",
    async (value) => {
      if (value !== undefined) repo.setSetting("feature_toggles", value);
      const response = await get();
      expect(response.statusCode).toBe(200);
      expect(response.headers["content-type"]).toContain("text/plain");
      expect(response.body).toContain(disabledMarker);
      expect(response.body).not.toContain("printers_idle");
      expect(response.body).not.toContain("printers_printing");
      expect(response.body).toContain("http_requests_total 0");
      expect(response.body).toContain('app_info{version="test"');
      expect(response.body).toContain("plates_sent_total 0");
      expect(response.body).toContain("plan_progress_collection_available 1");
      noFarmWork();
    },
  );

  it.each([
    "",
    "{",
    "[]",
    "null",
    '{"farm":null}',
    '{"farm":0}',
    '{"farm":"true"}',
    '{"farm":true,"orders":0}',
  ])("fails closed for malformed settings %j before collection", async (value) => {
    repo.setSetting("feature_toggles", value);
    const jobs = vi.spyOn(repo, "printJobMetrics");
    const plans = vi.spyOn(repo, "listAcceptedProfileSummaries");
    const response = await get();
    expect(response.statusCode).toBe(500);
    expect(response.json()).toEqual(invalid);
    expect(jobs).not.toHaveBeenCalled();
    expect(plans).not.toHaveBeenCalled();
    noFarmWork();
  });

  it.each(["1", "true"])(
    "supports live opt-in, revoke and re-enable with exact flag %j",
    async (flag) => {
      vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      for (const enabled of [true, false, true]) {
        repo.setSetting(
          "feature_toggles",
          JSON.stringify({ farm: enabled, future: { nested: [null] } }),
        );
        vi.clearAllMocks();
        const response = await get();
        expect(response.statusCode).toBe(200);
        if (enabled) {
          expect(response.body).toContain("printers_idle 1\n");
          expect(response.body).toContain("printers_printing 0\n");
          expect(response.body).not.toContain("FEATURE_DISABLED");
          expect(printerFleet.loadFleet).toHaveBeenCalledTimes(1);
          expect(execution.probe).toHaveBeenCalledTimes(1);
          expect(probe).toHaveBeenCalledTimes(1);
        } else {
          expect(response.body).toContain(disabledMarker);
          expect(response.body).not.toContain("printers_idle");
          noFarmWork();
        }
      }
    },
  );

  it("uses the request tenant and ignores Plan overrides for global metrics", async () => {
    const plan = repo.createProfile("Metrics");
    repo.setSetting(`feature_toggles:plan:${plan.id}`, '{"farm":true}');
    expect((await get()).body).toContain(disabledMarker);
    noFarmWork();
    repo.setSetting("feature_toggles", '{"farm":true}');
    repo.setSetting(`feature_toggles:plan:${plan.id}`, '{"farm":false}');
    expect((await get()).body).toContain("printers_idle 1\n");
    vi.clearAllMocks();
    tenantStorage.run("other", () => repo.setSetting("feature_toggles", '{"farm":false}'));
    expect((await get("other")).body).toContain(disabledMarker);
    noFarmWork();
    tenantStorage.run("other", () => repo.setSetting("feature_toggles", "broken"));
    expect((await get("other")).json()).toEqual(invalid);
    noFarmWork();
    expect((await get()).body).toContain("printers_idle 1\n");
  });

  it.each([undefined, "0", "false", "TRUE", "true ", "yes", "2", ""])(
    "keeps legacy behavior without feature reads for flag %j",
    async (flag) => {
      if (flag === undefined) delete process.env.PP_DESKTOP_REBUILD;
      else vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      repo.setSetting("feature_toggles", "broken");
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      const response = await get();
      expect(response.statusCode).toBe(200);
      expect(response.body).toContain("printers_idle 1\n");
      expect(response.body).not.toContain("FEATURE_DISABLED");
      expect(reads).not.toHaveBeenCalled();
      expect(probe).toHaveBeenCalledTimes(1);
    },
  );

  it("authenticates before reading toggles or collecting metrics", async () => {
    repo.setSetting("feature_toggles", "broken");
    const reads = vi.spyOn(repo, "getSettingSnapshot");
    for (const headers of [{}, { authorization: "Bearer invalid" }]) {
      const response = await app.inject({ method: "GET", url: "/metrics", headers });
      expect(response.statusCode).toBe(401);
      expect(response.json()).toEqual({
        detail: "authorization" in headers ? "Valid API key required" : "Authentication required",
      });
    }
    expect(reads).not.toHaveBeenCalled();
    noFarmWork();
  });

  it("applies the same guard to authenticated sessions", async () => {
    const getSession = () =>
      app.inject({ method: "GET", url: "/metrics", headers: { "x-test-session": "1" } });
    expect((await getSession()).body).toContain(disabledMarker);
    noFarmWork();
    repo.setSetting("feature_toggles", '{"farm":true}');
    expect((await getSession()).body).toContain("printers_idle 1\n");
  });

  it.each(["1", "true"])(
    "guards the production registration with optional auth and flag %j",
    async (flag) => {
      vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      const dataDir = join(root, "production");
      const ports = createSelfHostPorts(dataDir);
      await ports.db.connect();
      const production = await buildApp({ ...loadConfig(), dataDir, authRequired: false }, ports);
      try {
        const request = () => production.inject({ method: "GET", url: "/metrics" });
        const disabled = await request();
        expect(disabled.statusCode).toBe(200);
        expect(disabled.body).toContain(disabledMarker);
        expect(disabled.body).not.toContain("printers_idle");
        noFarmWork();
        ports.repository.setSetting("feature_toggles", '{"farm":true}');
        expect((await request()).body).toContain("printers_idle 0\n");
        ports.repository.setSetting("feature_toggles", "broken");
        const corrupt = await request();
        expect(corrupt.statusCode).toBe(500);
        expect(corrupt.json()).toEqual(invalid);
      } finally {
        await production.close();
        await ports.db.close();
      }
    },
  );
});
