import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify, { type FastifyInstance } from "fastify";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { StatusExecution } from "../integrations/status-execution.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import { createIntegrationPort } from "../integrations/store.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import * as outbound from "../lib/outbound-url.js";
import * as discordNotify from "../services/discord-notify.js";
import * as printerFleet from "../services/printer-fleet.js";
import { registerDiscordDigestRoute } from "./discord-digest.js";

const disabled = {
  status: 403,
  title: "Forbidden",
  detail: "Feature farm is disabled.",
  code: "FEATURE_DISABLED",
};
const invalid = {
  status: 500,
  title: "Internal Server Error",
  detail: "Stored feature toggles are invalid.",
  code: "FEATURE_TOGGLES_INVALID",
};
const webhook = "https://discord.example/webhook/digest";

describe("Discord digest farm feature admission", () => {
  let dataDir: string;
  let sqlite: SqliteDatabase;
  let repo: AppRepository;
  let app: FastifyInstance;
  let statusExecution: StatusExecution;
  let integrations: ReturnType<typeof createIntegrationPort>;
  const network = vi.fn(async () => new Response(null, { status: 204 }));

  beforeEach(async () => {
    dataDir = mkdtempSync(join(tmpdir(), "pp-discord-digest-features-"));
    vi.stubEnv("PP_DESKTOP_REBUILD", "1");
    vi.stubEnv("DEPLOY_MODE", "self-host");
    vi.spyOn(outbound, "safeOutboundFetch").mockImplementation(network);
    network.mockClear();
    sqlite = new SqliteDatabase(dataDir);
    sqlite.connect();
    repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
    statusExecution = new StatusExecution();
    integrations = createIntegrationPort({
      repo,
      getAdapter: getIntegrationAdapter,
      statusExecution,
    });
    repo.setSetting("discord_notify_webhook_url", webhook);
    repo.setSetting(
      "printer.fleet",
      JSON.stringify([{ id: "printer", name: "Workshop", integration_id: "connector" }]),
    );
    app = Fastify();
    app.decorate("config", { authRequired: true });
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
    for (const prefix of ["", "/api/v1"]) {
      await app.register(
        async (routes) => registerDiscordDigestRoute(routes, { repo, integrations }),
        { prefix },
      );
    }
    await app.ready();
  });

  afterEach(async () => {
    await app.close();
    await statusExecution.stop();
    sqlite.close();
    rmSync(dataDir, { recursive: true, force: true });
    vi.restoreAllMocks();
    vi.unstubAllEnvs();
  });

  function post(url: string, tenant = "default") {
    return app.inject({
      method: "POST",
      url,
      headers: { authorization: "Bearer test-key", "x-test-tenant": tenant },
    });
  }

  function observeOptionalWork() {
    return [
      vi.spyOn(repo, "getSetting"),
      vi.spyOn(repo, "recentPrintJobs"),
      vi.spyOn(printerFleet, "loadFleet"),
      vi.spyOn(repo, "listAcceptedProfileSummaries"),
      vi.spyOn(integrations, "getStatus"),
      vi.spyOn(discordNotify, "sendFarmDigest"),
      network,
    ];
  }

  for (const url of ["/api/discord-digest", "/api/v1/api/discord-digest"]) {
    it(`${url} denies defaults and explicit off before optional reads or delivery`, async () => {
      const work = observeOptionalWork();
      for (const value of [undefined, "{}", '{"farm":false}']) {
        if (value !== undefined) repo.setSetting("feature_toggles", value);
        const response = await post(url);
        expect(response.statusCode).toBe(403);
        expect(response.json()).toEqual(disabled);
      }
      for (const call of work) expect(call).not.toHaveBeenCalled();
    });

    it.each([
      "{",
      "[]",
      "null",
      '{"farm":"yes"}',
      '{"farm":null}',
      '{"farm":0}',
      '{"farm":true,"spoolman":0}',
    ])(`${url} denies malformed stored %j before optional work`, async (value) => {
      repo.setSetting("feature_toggles", value);
      const work = observeOptionalWork();
      const response = await post(url);
      expect(response.statusCode).toBe(500);
      expect(response.json()).toEqual(invalid);
      for (const call of work) expect(call).not.toHaveBeenCalled();
    });

    it(`${url} sends the expected digest and supports live revoke and re-enable`, async () => {
      const plan = repo.createProfile("Overnight Run");
      const now = new Date().toISOString();
      repo.insertPrintJob({
        id: randomUUID(),
        profileId: plan.id,
        printerId: "printer",
        material: "PLA",
        status: "completed",
        at: now,
        completedAt: now,
      });
      const status = vi.spyOn(integrations, "getStatus").mockResolvedValue({ state: "idle" });
      const send = vi.spyOn(discordNotify, "sendFarmDigest");
      for (const enabled of [true, false, true]) {
        repo.setSetting("feature_toggles", JSON.stringify({ farm: enabled }));
        status.mockClear();
        send.mockClear();
        network.mockClear();
        const response = await post(url);
        expect(response.statusCode).toBe(enabled ? 200 : 403);
        expect(response.json()).toEqual(enabled ? { ok: true, plates_overnight: 1 } : disabled);
        expect(status).toHaveBeenCalledTimes(enabled ? 1 : 0);
        expect(send).toHaveBeenCalledTimes(enabled ? 1 : 0);
        expect(network).toHaveBeenCalledTimes(enabled ? 1 : 0);
        if (enabled) {
          expect(status).toHaveBeenCalledWith("connector");
          expect(send).toHaveBeenCalledWith(webhook, {
            platesOvernight: 1,
            windowHours: 8,
            printers: [{ name: "Workshop", state: "idle", active_job: null }],
            activePlans: {
              kind: "available",
              plans: [{ plan_name: "Overnight Run", progress: { kind: "empty" } }],
            },
          });
          expect(network).toHaveBeenCalledWith(
            webhook,
            expect.objectContaining({
              method: "POST",
              body: expect.stringContaining("**1** plate(s) sent to printers overnight."),
            }),
            { allowPrivate: false },
          );
        }
      }
    });

    it(`${url} preserves auth precedence before reading feature settings`, async () => {
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      const work = observeOptionalWork();
      const response = await app.inject({ method: "POST", url });
      expect(response.statusCode).toBe(401);
      expect(response.json()).toEqual({ detail: "Authentication required" });
      expect(reads).not.toHaveBeenCalled();
      for (const call of work) expect(call).not.toHaveBeenCalled();
    });

    it(`${url} admits sessions and optional-auth requests through the same feature guard`, async () => {
      const session = () => app.inject({ method: "POST", url, headers: { "x-test-session": "1" } });
      expect((await session()).json()).toEqual(disabled);
      repo.setSetting("feature_toggles", '{"farm":true}');
      expect((await session()).json()).toEqual({ ok: true, plates_overnight: 0 });
      app.getDecorator<{ authRequired: boolean }>("config").authRequired = false;
      repo.setSetting("feature_toggles", '{"farm":false}');
      expect((await app.inject({ method: "POST", url })).json()).toEqual(disabled);
      repo.setSetting("feature_toggles", '{"farm":true}');
      expect((await app.inject({ method: "POST", url })).json()).toEqual({
        ok: true,
        plates_overnight: 0,
      });
    });
  }

  it("uses current tenant choices and ignores Plan overrides", async () => {
    const plan = repo.createProfile("Tenant-global digest");
    const url = "/api/v1/api/discord-digest";
    repo.setSetting(`feature_toggles:plan:${plan.id}`, '{"farm":true}');
    expect((await post(url)).json()).toEqual(disabled);
    repo.setSetting("feature_toggles", '{"farm":true}');
    repo.setSetting(`feature_toggles:plan:${plan.id}`, '{"farm":false}');
    tenantStorage.run("other", () => {
      repo.setSetting("feature_toggles", '{"farm":false}');
      repo.setSetting("discord_notify_webhook_url", webhook);
    });
    expect((await post(url)).statusCode).toBe(200);
    network.mockClear();
    expect((await post(url, "other")).json()).toEqual(disabled);
    expect(network).not.toHaveBeenCalled();
    expect((await post(url)).statusCode).toBe(200);
  });

  it.each([undefined, "0", "false", "TRUE", "true ", "yes", "2", ""])(
    "preserves legacy behavior without feature reads for flag %j",
    async (flag) => {
      if (flag === undefined) delete process.env.PP_DESKTOP_REBUILD;
      else vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      repo.setSetting("feature_toggles", "invalid JSON");
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      for (const url of ["/api/discord-digest", "/api/v1/api/discord-digest"]) {
        expect((await post(url)).json()).toEqual({ ok: true, plates_overnight: 0 });
      }
      expect(reads).not.toHaveBeenCalled();
      expect(network).toHaveBeenCalledTimes(2);
    },
  );

  it("accepts exact true and unknown future settings while keeping default-off admission", async () => {
    vi.stubEnv("PP_DESKTOP_REBUILD", "true");
    expect((await post("/api/discord-digest")).json()).toEqual(disabled);
    repo.setSetting("feature_toggles", JSON.stringify({ farm: true, future: { nested: [null] } }));
    expect((await post("/api/discord-digest")).statusCode).toBe(200);
  });
});
