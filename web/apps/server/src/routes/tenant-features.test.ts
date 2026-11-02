import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify, { type FastifyInstance } from "fastify";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TENANT_FEATURE_TOGGLES_CAPABILITY } from "@print-partner/contracts";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import { readFeatureAccess } from "../lib/feature-access.js";
import { readPlanFeatures } from "../services/plan-features.js";
import { registerTenantFeatureRoutes } from "./tenant-features.js";

const defaults = {
  farm: { enabled: false, source: "default" },
  orders: { enabled: false, source: "default" },
  bambu: { enabled: false, source: "default" },
  spoolman: { enabled: false, source: "default" },
};

describe("tenant feature settings", () => {
  let dir: string;
  let sqlite: SqliteDatabase;
  let repo: AppRepository;
  let app: FastifyInstance;
  beforeEach(async () => {
    dir = mkdtempSync(join(tmpdir(), "pp-tenant-feature-ui-"));
    vi.stubEnv("PP_DESKTOP_REBUILD", "1");
    vi.stubEnv("PRINT_PARTNER_DATA_DIR", dir);
    sqlite = new SqliteDatabase(dir);
    sqlite.connect();
    repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
    app = Fastify();
    app.addHook("onRequest", (request, _reply, done) => {
      const tenant = request.headers["x-test-tenant"];
      tenantStorage.run(typeof tenant === "string" ? tenant : "default", done);
    });
    for (const prefix of ["", "/api/v1"]) {
      await app.register(async (routes) => registerTenantFeatureRoutes(routes, repo), { prefix });
    }
    await app.ready();
  });
  afterEach(async () => {
    await app.close();
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
    vi.restoreAllMocks();
    vi.unstubAllEnvs();
  });
  const patch = (payload: unknown, prefix = "", tenant = "default") =>
    app.inject({
      method: "PATCH",
      url: `${prefix}/settings/features`,
      payload: JSON.stringify(payload),
      headers: { "content-type": "application/json", "x-test-tenant": tenant },
    });

  it("defaults every feature off and saves choices through all aliases", async () => {
    for (const prefix of ["", "/api/v1"]) {
      expect((await app.inject(`${prefix}/settings/features`)).json()).toEqual({
        choices: {},
        effective: defaults,
      });
      const saved = await patch({ farm: true, orders: false }, prefix);
      expect(saved.statusCode).toBe(200);
      expect(saved.json()).toEqual({
        choices: { farm: true, orders: false },
        effective: {
          ...defaults,
          farm: { enabled: true, source: "user" },
          orders: { enabled: false, source: "user" },
        },
      });
      expect((await patch({ farm: null, orders: null }, prefix)).json()).toEqual({
        choices: {},
        effective: defaults,
      });
    }
  });

  it.each([undefined, "", "0", "false", "TRUE", "yes", " 1"])(
    "never reads or writes choices with flag %s",
    async (flag) => {
      if (flag === undefined) delete process.env.PP_DESKTOP_REBUILD;
      else vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      const read = vi.spyOn(repo, "getSettingSnapshot");
      const write = vi.spyOn(repo, "compareAndSetSetting");
      for (const prefix of ["", "/api/v1"]) {
        expect((await app.inject(`${prefix}/settings/features`)).statusCode).toBe(404);
        expect((await patch({ farm: true }, prefix)).statusCode).toBe(404);
      }
      expect(read).not.toHaveBeenCalled();
      expect(write).not.toHaveBeenCalled();
    },
  );

  it("accepts true, preserves unknown values and exact numeric tokens, and clears to defaults", async () => {
    vi.stubEnv("PP_DESKTOP_REBUILD", "true");
    repo.setSetting(
      "feature_toggles",
      '{"future":{"large":9007199254740993123456789},"bambu":true}',
    );
    expect((await patch({ spoolman: true })).json().choices).toEqual({
      bambu: true,
      spoolman: true,
    });
    expect(repo.getSettingSnapshot("feature_toggles")).toEqual({
      kind: "stored",
      value: '{"future":{"large":9007199254740993123456789},"bambu":true,"spoolman":true}',
    });
    expect((await patch({ bambu: null })).json().effective.bambu).toEqual(defaults.bambu);
    expect((await app.inject("/settings/features")).json().effective.spoolman).toEqual({
      enabled: true,
      source: "user",
    });
  });

  it("uses current tenant choices for admission and preserves Plan precedence", async () => {
    const plan = repo.createProfile("Owned plan");
    repo.setSetting(`feature_toggles:plan:${plan.id}`, '{"farm":false}');
    await patch({ farm: true, bambu: true });
    expect(readFeatureAccess(() => repo, "bambu")).toEqual({ kind: "allowed" });
    expect(readPlanFeatures(repo, { id: String(plan.id) })).toMatchObject({
      kind: "ready",
      view: {
        effective: {
          farm: { enabled: false, source: "plan" },
          bambu: { enabled: true, source: "user" },
        },
      },
    });
    await patch({ bambu: false });
    expect(readFeatureAccess(() => repo, "bambu")).toMatchObject({
      kind: "disabled",
      code: "FEATURE_DISABLED",
    });
    await patch({ spoolman: true }, "", "other");
    expect((await app.inject("/settings/features")).json().choices).toEqual({
      farm: true,
      bambu: false,
    });
    expect(
      (
        await app.inject({ url: "/settings/features", headers: { "x-test-tenant": "other" } })
      ).json().choices,
    ).toEqual({ spoolman: true });
  });

  it.each(['{"farm":"yes"}', "[]", "null", "", '{"future":1,"orders":null}'])(
    "rejects corrupt choices %s without overwrite",
    async (stored) => {
      repo.setSetting("feature_toggles", stored);
      expect((await app.inject("/settings/features")).statusCode).toBe(500);
      expect((await patch({ farm: true })).json()).toEqual({
        detail: "Stored feature toggles are invalid.",
        code: "FEATURE_TOGGLES_INVALID",
      });
      expect(repo.getSettingSnapshot("feature_toggles")).toEqual({ kind: "stored", value: stored });
    },
  );

  it.each([{ unknown: true }, { farm: "true" }, null, [], { farm: 1 }])(
    "rejects invalid patch %j",
    async (payload) => {
      expect((await patch(payload)).statusCode).toBe(400);
      expect(repo.getSettingSnapshot("feature_toggles")).toEqual({ kind: "missing" });
    },
  );

  it("retries a lost update against the current snapshot and reports persistent conflicts", async () => {
    const original = repo.compareAndSetSetting.bind(repo);
    const cas = vi.spyOn(repo, "compareAndSetSetting").mockImplementationOnce((input) => {
      repo.setSetting("feature_toggles", '{"orders":true}');
      return original(input);
    });
    expect((await patch({ farm: true })).json().choices).toEqual({ orders: true, farm: true });
    cas.mockReturnValue(false);
    expect((await patch({ farm: false })).statusCode).toBe(409);
    expect(cas).toHaveBeenCalledTimes(7);
    expect((await app.inject("/settings/features")).json().choices).toEqual({
      orders: true,
      farm: true,
    });
  });

  it("inherits production authentication and advertises only exact opt-in in health", async () => {
    const { buildApp } = await import("../app.js");
    const { createSelfHostPorts } = await import("../adapters/self-host/index.js");
    const { loadConfig } = await import("../config.js");
    const isolated = mkdtempSync(join(tmpdir(), "pp-tenant-features-auth-"));
    const ports = createSelfHostPorts(isolated);
    await ports.db.connect();
    const live = await buildApp(
      {
        ...loadConfig(),
        dataDir: isolated,
        authRequired: true,
        singleUserAuth: true,
        multiUser: false,
        sessionSecret: "isolated-tenant-features-test",
        staticDir: null,
      },
      ports,
    );
    try {
      for (const prefix of ["", "/api/v1"]) {
        expect((await live.inject(`${prefix}/settings/features`)).statusCode).toBe(401);
        expect(
          (
            await live.inject({
              method: "PATCH",
              url: `${prefix}/settings/features`,
              payload: { farm: true },
            })
          ).statusCode,
        ).toBe(401);
      }
      for (const flag of ["1", "true", "0", "false", "TRUE", ""]) {
        vi.stubEnv("PP_DESKTOP_REBUILD", flag);
        const health = (await live.inject("/health")).json();
        expect(health.capabilities.includes(TENANT_FEATURE_TOGGLES_CAPABILITY)).toBe(
          flag === "1" || flag === "true",
        );
      }
      vi.stubEnv("PP_DESKTOP_REBUILD", "1");
      const registration = await live.inject({
        method: "POST",
        url: "/auth/register",
        payload: {
          email: "tenant-features@example.com",
          password: "isolated-feature-password",
          display_name: "Owner",
        },
      });
      expect(registration.statusCode).toBe(200);
      const session = registration.cookies.find((cookie) => cookie.name === "pp_session");
      if (!session) throw new Error("No authenticated session");
      for (const prefix of ["", "/api/v1"]) {
        const response = await live.inject({
          method: "PATCH",
          url: `${prefix}/settings/features`,
          cookies: { pp_session: session.value },
          payload: { farm: true },
        });
        expect(response.statusCode).toBe(200);
        expect(response.json().effective.farm).toEqual({ enabled: true, source: "user" });
      }
    } finally {
      await live.close();
      await ports.db.close();
      rmSync(isolated, { recursive: true, force: true });
    }
  });
});
