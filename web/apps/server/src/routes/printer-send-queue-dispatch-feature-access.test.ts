import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify, { type FastifyInstance } from "fastify";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { StatusExecution } from "../integrations/status-execution.js";
import { InProcessJobRunner } from "../services/job-runner.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import { registerPrinterSendQueueRoutes } from "./printer-send-queue.js";
import { createIntegrationPort, type IntegrationPort } from "../integrations/store.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import {
  enqueuePrinterSend,
  loadPrinterSendQueue,
  PRINTER_SEND_QUEUE_SETTINGS_KEY,
} from "../services/printer-send-queue-store.js";
import { registerTenantMiddleware } from "./auth.js";
import { loadConfig } from "../config.js";
import { buildApp } from "../app.js";
import { createSelfHostPorts } from "../adapters/self-host/index.js";

const disabled = {
  status: 403,
  title: "Forbidden",
  detail: "Feature bambu is disabled.",
  code: "FEATURE_DISABLED",
};
const invalid = {
  status: 500,
  title: "Internal Server Error",
  detail: "Stored feature toggles are invalid.",
  code: "FEATURE_TOGGLES_INVALID",
};
const urls = ["/printer-send-queue", "/api/v1/printer-send-queue"];
const bytes = Buffer.from("G1 X1 Y1\n");

function configureHost(repo: AppRepository, type = "bambu") {
  repo.setSetting(
    "integrations",
    JSON.stringify([{ id: "host", type, name: "Workshop", config: {} }]),
  );
  repo.setSetting("printer.fleet", JSON.stringify([{ id: "printer", integration_id: "host" }]));
}

function seed(repo: AppRepository, exportsDir: string, profileId?: number, printerId = "printer") {
  const uploads = join(exportsDir, "printer-uploads");
  mkdirSync(uploads, { recursive: true });
  const dir = mkdtempSync(join(uploads, "send-"));
  const artifact = join(dir, "plate.gcode");
  writeFileSync(artifact, bytes);
  const item = enqueuePrinterSend(repo, {
    filename: "plate.gcode",
    artifact_path: artifact,
    printer_id: printerId,
    profile_id: profileId,
    start: true,
    wait_for_idle: true,
    match: "pinned",
  });
  if (!item) throw new Error("Failed to seed stored queue item");
  return item;
}

describe("stored Bambu send-queue dispatch/drain HTTP admission", () => {
  let root: string;
  let sqlite: SqliteDatabase;
  let repo: AppRepository;
  let jobs: InProcessJobRunner;
  let app: FastifyInstance;
  let execution: StatusExecution;
  let profileId: number;
  let integrations: IntegrationPort;

  beforeEach(async () => {
    vi.stubEnv("PP_DESKTOP_REBUILD", "1");
    vi.stubEnv("DEPLOY_MODE", "self-host");
    root = mkdtempSync(join(tmpdir(), "pp-send-queue-access-"));
    sqlite = new SqliteDatabase(root);
    sqlite.connect();
    repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
    profileId = repo.createProfile("Queue Plan").id;
    configureHost(repo);
    jobs = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: sqlite.reposDir,
      exportsDir: join(root, "exports"),
      dataDir: root,
    });
    vi.spyOn(jobs, "get").mockResolvedValue(null);
    vi.spyOn(jobs, "start").mockResolvedValue("upload-job");
    execution = new StatusExecution();
    vi.spyOn(execution, "probe").mockResolvedValue({ state: "idle", message: "Ready" });
    integrations = createIntegrationPort({
      repo,
      getAdapter: getIntegrationAdapter,
      statusExecution: execution,
    });
    vi.spyOn(integrations, "getStatus").mockResolvedValue({ state: "idle", message: "Ready" });
    app = Fastify();
    app.decorateRequest("tenantId", "default");
    app.addHook("onRequest", (request, _reply, done) => {
      const tenant = request.headers["x-test-tenant"];
      request.tenantId = typeof tenant === "string" ? tenant : "default";
      tenantStorage.run(request.tenantId, done);
    });
    for (const prefix of ["", "/api/v1"]) {
      await app.register(
        async (routes) => registerPrinterSendQueueRoutes(routes, { repo, jobs, integrations }),
        { prefix },
      );
    }
    await app.ready();
  });

  afterEach(async () => {
    await app.close();
    await execution.stop();
    sqlite.close();
    rmSync(root, { recursive: true, force: true });
    vi.restoreAllMocks();
    vi.unstubAllEnvs();
  });

  function request(
    action: string,
    id: string,
    url = urls[0],
    tenant = "default",
    payload: object = {},
  ) {
    return app.inject({
      method: "POST",
      url: action === "drain" ? `${url}/drain` : `${url}/${id}/dispatch`,
      headers: { "x-test-tenant": tenant },
      payload,
    });
  }

  async function refused(action: string, expected = disabled, url = urls[0], tenant = "default") {
    const item = tenantStorage.run(tenant, () => seed(repo, jobs.getExportsDir(tenant), profileId));
    const before = tenantStorage.run(tenant, () =>
      repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY),
    );
    const writes = vi.spyOn(repo, "setSetting");
    const response = await request(action, item.id, url, tenant, {
      force: true,
      profile_id: 999999,
    });
    expect(response.statusCode).toBe(expected.status);
    expect(response.json()).toEqual(expected);
    expect(tenantStorage.run(tenant, () => repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY))).toBe(
      before,
    );
    expect(writes.mock.calls.filter(([key]) => key === PRINTER_SEND_QUEUE_SETTINGS_KEY)).toEqual(
      [],
    );
    writes.mockRestore();
    expect(readFileSync(item.artifact_path)).toEqual(bytes);
    expect(readdirSync(join(item.artifact_path, ".."))).toEqual(["plate.gcode"]);
    expect(jobs.get).not.toHaveBeenCalled();
    expect(jobs.start).not.toHaveBeenCalled();
    expect(integrations.getStatus).not.toHaveBeenCalled();
    expect(execution.probe).not.toHaveBeenCalled();
  }

  for (const url of urls)
    for (const action of ["dispatch", "drain"]) {
      it.each([undefined, "{}", '{"bambu":false}', '{"farm":true,"spoolman":true}'])(
        `${url} ${action} refuses stored item for default/off %j`,
        async (stored) => {
          if (stored !== undefined) repo.setSetting("feature_toggles", stored);
          await refused(action, disabled, url);
        },
      );

      it.each([
        "",
        "{",
        "[]",
        "null",
        '{"bambu":null}',
        '{"bambu":"true"}',
        '{"bambu":true,"orders":0}',
      ])(`${url} ${action} fails closed for corrupt tenant/Plan %j`, async (stored) => {
        for (const key of ["feature_toggles", `feature_toggles:plan:${profileId}`]) {
          repo.setSetting("feature_toggles", '{"bambu":true}');
          repo.setSetting(key, stored);
          await refused(action, invalid, url);
        }
      });

      it(`${url} ${action} resolves live stored Plan overrides and preserves unsupported Bambu`, async () => {
        const other = repo.createProfile("Other Plan");
        repo.setSetting(`feature_toggles:plan:${other.id}`, '{"bambu":true}');
        const item = seed(repo, jobs.getExportsDir(), profileId);
        const before = repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY);
        const unsupported =
          action === "dispatch"
            ? { status: 400, title: "Bad Request", detail: "Upload is not supported for bambu" }
            : { results: [{ item_id: item.id, error: "Upload is not supported for bambu" }] };
        for (const [tenantChoice, planChoice, expected] of [
          ["{}", "{}", disabled],
          ["{}", '{"bambu":true}', unsupported],
          ['{"bambu":true}', '{"bambu":false}', disabled],
          ['{"bambu":true}', "{}", unsupported],
          ['{"bambu":false}', "{}", disabled],
          ["broken", '{"bambu":true}', invalid],
        ] satisfies Array<[string, string, object]>) {
          repo.setSetting("feature_toggles", tenantChoice);
          repo.setSetting(`feature_toggles:plan:${profileId}`, planChoice);
          const response = await request(action, item.id, url, "default", {
            profile_id: other.id,
            force: true,
          });
          expect(response.json()).toEqual(expected);
          expect(repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY)).toBe(before);
          expect(readFileSync(item.artifact_path)).toEqual(bytes);
        }
        expect(jobs.start).not.toHaveBeenCalled();
        expect(integrations.getStatus).not.toHaveBeenCalled();
      });
    }

  it.each(["dispatch", "drain"])(
    "%s validates stored Plan ownership without tenant fallback",
    async (action) => {
      const foreign = tenantStorage.run("other", () => repo.createProfile("Foreign"));
      repo.setSetting("feature_toggles", '{"bambu":true}');
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      for (const plan of [undefined, foreign.id, 999999]) {
        repo.setSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY, "[]");
        const item = seed(repo, jobs.getExportsDir(), plan);
        const before = repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY);
        const response = await request(action, item.id);
        expect(response.statusCode).toBe(plan === undefined ? 400 : 404);
        expect(response.json().detail).toBe(
          plan === undefined
            ? "Pick a plan to bind this send (profile_id required)"
            : "Profile not found",
        );
        expect(repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY)).toBe(before);
        expect(readFileSync(item.artifact_path)).toEqual(bytes);
      }
      expect(reads).not.toHaveBeenCalled();
      expect(jobs.get).not.toHaveBeenCalled();
    },
  );

  it.each(["dispatch", "drain"])("%s isolates tenant settings and stored items", async (action) => {
    const own = seed(repo, jobs.getExportsDir(), profileId);
    repo.setSetting("feature_toggles", '{"bambu":true}');
    const other = tenantStorage.run("other", () => {
      configureHost(repo);
      const plan = repo.createProfile("Other");
      return seed(repo, jobs.getExportsDir("other"), plan.id);
    });
    const denied = await request(action, other.id, urls[1], "other");
    expect(denied.json()).toEqual(disabled);
    const missing = await request("dispatch", own.id, urls[1], "other");
    expect(missing.statusCode).toBe(404);
    expect(readFileSync(own.artifact_path)).toEqual(bytes);
    expect(readFileSync(other.artifact_path)).toEqual(bytes);
    expect(jobs.start).not.toHaveBeenCalled();
    expect(jobs.get).not.toHaveBeenCalled();
  });

  it.each(["1", "true"])(
    "checks the complete drain batch before any side effect with flag %j",
    async (flag) => {
      vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      repo.setSetting("feature_toggles", '{"farm":true}');
      repo.setSetting(
        "integrations",
        JSON.stringify([
          { id: "moon", type: "moonraker", name: "Moon", config: {} },
          { id: "host", type: "bambu", name: "Bambu", config: {} },
        ]),
      );
      repo.setSetting(
        "printer.fleet",
        JSON.stringify([
          { id: "moon", integration_id: "moon" },
          { id: "printer", integration_id: "host" },
        ]),
      );
      const moon = seed(repo, jobs.getExportsDir(), profileId, "moon");
      const bambu = seed(repo, jobs.getExportsDir(), profileId);
      const sending = { ...moon, id: "in-flight", state: "sending", upload_job_id: "stale" };
      repo.setSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY, JSON.stringify([moon, bambu, sending]));
      const before = repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY);
      expect((await request("drain", "")).json()).toEqual(disabled);
      expect(repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY)).toBe(before);
      expect(jobs.get).not.toHaveBeenCalled();
      expect(jobs.start).not.toHaveBeenCalled();
      expect(integrations.getStatus).not.toHaveBeenCalled();
      expect(readFileSync(bambu.artifact_path)).toEqual(bytes);
      const filtered = await request("drain", "", urls[0], "default", { printer_id: "moon" });
      expect(filtered.statusCode).toBe(200);
      expect(filtered.json()).toEqual({ results: [{ item_id: moon.id, job_id: "upload-job" }] });
      expect(loadPrinterSendQueue(repo).find((item) => item.id === bambu.id)).toEqual(bambu);
    },
  );

  it.each(["dispatch", "drain"])(
    "%s rechecks access revoked during live-job reconciliation",
    async (action) => {
      repo.setSetting("feature_toggles", '{"bambu":true,"farm":true}');
      repo.setSetting(
        "integrations",
        JSON.stringify([
          { id: "moon", type: "moonraker", name: "Moon", config: {} },
          { id: "host", type: "bambu", name: "Bambu", config: {} },
        ]),
      );
      repo.setSetting(
        "printer.fleet",
        JSON.stringify([
          { id: "moon", integration_id: "moon" },
          { id: "printer", integration_id: "host" },
        ]),
      );
      const moon = seed(repo, jobs.getExportsDir(), profileId, "moon");
      const bambu = seed(repo, jobs.getExportsDir(), profileId);
      repo.setSetting(
        PRINTER_SEND_QUEUE_SETTINGS_KEY,
        JSON.stringify([
          moon,
          bambu,
          { ...moon, id: "in-flight", state: "sending", upload_job_id: "stale" },
        ]),
      );
      vi.mocked(jobs.get).mockImplementation(async () => {
        repo.setSetting(`feature_toggles:plan:${profileId}`, '{"bambu":false}');
        return null;
      });
      const response = await request(action, bambu.id);
      expect(response.json()).toEqual(disabled);
      expect(loadPrinterSendQueue(repo).filter((item) => item.state === "queued")).toEqual([
        moon,
        bambu,
      ]);
      expect(jobs.get).toHaveBeenCalled();
      expect(jobs.start).not.toHaveBeenCalled();
      expect(integrations.getStatus).not.toHaveBeenCalled();
      expect(readFileSync(moon.artifact_path)).toEqual(bytes);
      expect(readFileSync(bambu.artifact_path)).toEqual(bytes);
    },
  );

  it("preflights items added while live-job reconciliation awaits", async () => {
    configureHost(repo, "moonraker");
    repo.setSetting("feature_toggles", '{"farm":true}');
    const moon = seed(repo, jobs.getExportsDir(), profileId);
    repo.setSetting(
      PRINTER_SEND_QUEUE_SETTINGS_KEY,
      JSON.stringify([
        moon,
        { ...moon, id: "in-flight", state: "sending", upload_job_id: "stale" },
      ]),
    );
    vi.mocked(jobs.get).mockImplementation(async () => {
      repo.setSetting(
        "integrations",
        JSON.stringify([
          { id: "host", type: "moonraker", name: "Moon", config: {} },
          { id: "bambu", type: "bambu", name: "Bambu", config: {} },
        ]),
      );
      repo.setSetting(
        "printer.fleet",
        JSON.stringify([
          { id: "printer", integration_id: "host" },
          { id: "bambu", integration_id: "bambu" },
        ]),
      );
      seed(repo, jobs.getExportsDir(), profileId, "bambu");
      return null;
    });
    expect((await request("drain", "")).json()).toEqual(disabled);
    expect(loadPrinterSendQueue(repo).filter((item) => item.state === "queued")).toHaveLength(2);
    expect(jobs.start).not.toHaveBeenCalled();
    expect(integrations.getStatus).not.toHaveBeenCalled();
    expect(readFileSync(moon.artifact_path)).toEqual(bytes);
  });

  it("refuses retries before artifact validation and preserves unrelated in-flight items", async () => {
    const item = seed(repo, jobs.getExportsDir(), profileId);
    const retry = {
      ...item,
      state: "error",
      error: "Previous refusal",
      artifact_path: join(root, "missing.gcode"),
    };
    const sending = { ...item, id: "in-flight", state: "sending", upload_job_id: "stale" };
    repo.setSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY, JSON.stringify([retry, sending]));
    const before = repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY);
    expect((await request("dispatch", item.id)).json()).toEqual(disabled);
    expect(repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY)).toBe(before);
    expect(jobs.get).not.toHaveBeenCalled();
    expect(jobs.start).not.toHaveBeenCalled();
    expect(readFileSync(item.artifact_path)).toEqual(bytes);
  });

  it("limits drain admission to the capped queued batch and ignores terminal or retry items", async () => {
    configureHost(repo, "moonraker");
    repo.setSetting(
      "integrations",
      JSON.stringify([
        { id: "host", type: "moonraker", name: "Moon", config: {} },
        { id: "bambu", type: "bambu", name: "Bambu", config: {} },
      ]),
    );
    repo.setSetting(
      "printer.fleet",
      JSON.stringify([
        { id: "printer", integration_id: "host" },
        { id: "bambu", integration_id: "bambu" },
      ]),
    );
    const moon = Array.from({ length: 8 }, () => seed(repo, jobs.getExportsDir(), profileId));
    const bambu = seed(repo, jobs.getExportsDir(), profileId, "bambu");
    const terminal = ["done", "cancelled", "error"].map((state) => ({
      ...bambu,
      id: state,
      state,
    }));
    repo.setSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY, JSON.stringify([...terminal, ...moon, bambu]));
    repo.setSetting("feature_toggles", '{"farm":true}');
    const response = await request("drain", "");
    expect(response.json()).toEqual({ results: [{ item_id: moon[0]?.id, job_id: "upload-job" }] });
    expect(loadPrinterSendQueue(repo).find((item) => item.id === bambu.id)).toEqual(bambu);
    expect(readFileSync(bambu.artifact_path)).toEqual(bytes);
  });

  it.each([undefined, "", "0", "false", "TRUE", "true ", "yes", "2"])(
    "preserves flag-off behavior without feature reads for %j",
    async (flag) => {
      if (flag === undefined) delete process.env.PP_DESKTOP_REBUILD;
      else vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      repo.setSetting("feature_toggles", "broken");
      const item = seed(repo, jobs.getExportsDir());
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      expect((await request("dispatch", item.id)).statusCode).toBe(400);
      expect((await request("drain", item.id)).json()).toEqual({
        results: [{ item_id: item.id, error: "Upload is not supported for bambu" }],
      });
      expect(reads).not.toHaveBeenCalled();
      expect(readFileSync(item.artifact_path)).toEqual(bytes);
    },
  );

  it.each(["moonraker", "prusalink"])(
    "dispatches and drains %s when farm is enabled",
    async (type) => {
      configureHost(repo, type);
      repo.setSetting("feature_toggles", '{"farm":true}');
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      const item = seed(repo, jobs.getExportsDir(), profileId);
      const response = await request("dispatch", item.id);
      expect(response.statusCode).toBe(200);
      expect(response.json()).toMatchObject({
        job_id: "upload-job",
        item: { id: item.id, state: "sending" },
      });
      expect(jobs.start).toHaveBeenCalledWith(
        "printer-upload",
        expect.objectContaining({ artifact_path: item.artifact_path, profile_id: profileId }),
        "default",
      );
      expect(reads).toHaveBeenCalledWith("feature_toggles");
      expect(readFileSync(item.artifact_path)).toEqual(bytes);
      const next = seed(repo, jobs.getExportsDir(), profileId);
      const drained = await request("drain", "", urls[1]);
      expect(drained.json()).toEqual({ results: [{ item_id: next.id, job_id: "upload-job" }] });
      expect(readFileSync(next.artifact_path)).toEqual(bytes);
    },
  );

  it("runs auth before stored queue or feature reads for both aliases", async () => {
    const secured = Fastify();
    registerTenantMiddleware(
      secured,
      { ...loadConfig(), authRequired: true, multiUser: true },
      null,
    );
    for (const prefix of ["", "/api/v1"])
      await secured.register(
        async (routes) => registerPrinterSendQueueRoutes(routes, { repo, jobs, integrations }),
        { prefix },
      );
    const reads = vi.spyOn(repo, "getSetting");
    const features = vi.spyOn(repo, "getSettingSnapshot");
    try {
      for (const url of urls)
        for (const action of ["dispatch", "drain"]) {
          const response = await secured.inject({
            method: "POST",
            url: action === "drain" ? `${url}/drain` : `${url}/stored/dispatch`,
            payload: {},
          });
          expect(response.statusCode).toBe(401);
        }
      expect(reads).not.toHaveBeenCalled();
      expect(features).not.toHaveBeenCalled();
      expect(jobs.get).not.toHaveBeenCalled();
      expect(jobs.start).not.toHaveBeenCalled();
      expect(integrations.getStatus).not.toHaveBeenCalled();
    } finally {
      await secured.close();
    }
  });

  it.each(["1", "true"])(
    "guards both production aliases with flag %j and preserves real artifacts",
    async (flag) => {
      vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      const dataDir = join(root, "production");
      const ports = createSelfHostPorts(dataDir);
      await ports.db.connect();
      const production = await buildApp({ ...loadConfig(), dataDir, authRequired: false }, ports);
      const plan = ports.repository.createProfile("Production Plan");
      configureHost(ports.repository);
      const item = seed(ports.repository, join(dataDir, "exports", "tenant-default"), plan.id);
      const before = ports.repository.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY);
      try {
        for (const url of urls)
          for (const action of ["dispatch", "drain"])
            for (const [stored, expected] of [
              ['{"bambu":false}', disabled],
              ["broken", invalid],
            ] satisfies Array<[string, typeof disabled]>) {
              ports.repository.setSetting("feature_toggles", stored);
              const response = await production.inject({
                method: "POST",
                url: action === "drain" ? `${url}/drain` : `${url}/${item.id}/dispatch`,
                payload: {},
              });
              expect(response.json()).toEqual(expected);
              expect(ports.repository.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY)).toBe(before);
              expect(readFileSync(item.artifact_path)).toEqual(bytes);
            }
      } finally {
        await production.close();
        await ports.db.close();
      }
    },
  );
});
