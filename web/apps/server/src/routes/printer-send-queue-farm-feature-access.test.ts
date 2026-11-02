import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify, { type FastifyInstance } from "fastify";
import multipart from "@fastify/multipart";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { StatusExecution } from "../integrations/status-execution.js";
import { createIntegrationPort, type IntegrationPort } from "../integrations/store.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import { InProcessJobRunner } from "../services/job-runner.js";
import {
  enqueuePrinterSend,
  loadPrinterSendQueue,
  PRINTER_SEND_QUEUE_SETTINGS_KEY,
} from "../services/printer-send-queue-store.js";
import { registerPrinterSendQueueRoutes } from "./printer-send-queue.js";

const farmOff = {
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
const urls = ["/printer-send-queue", "/api/v1/printer-send-queue"];
const bytes = Buffer.from("G1 X1 Y1\n");
type Fields = Array<[string, string]>;

function upload(fields: Fields) {
  const boundary = "----pp-send-queue-farm";
  return {
    headers: { "content-type": `multipart/form-data; boundary=${boundary}` },
    payload: Buffer.concat([
      Buffer.from(
        `--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="plate.gcode"\r\nContent-Type: application/octet-stream\r\n\r\n`,
      ),
      bytes,
      Buffer.from("\r\n"),
      ...fields.map(([name, value]) =>
        Buffer.from(
          `--${boundary}\r\nContent-Disposition: form-data; name="${name}"\r\n\r\n${value}\r\n`,
        ),
      ),
      Buffer.from(`--${boundary}--\r\n`),
    ]),
  };
}

function configureHost(repo: AppRepository, type = "moonraker") {
  repo.setSetting(
    "integrations",
    JSON.stringify([{ id: "host", type, name: "Workshop", config: {} }]),
  );
  repo.setSetting("printer.fleet", JSON.stringify([{ id: "printer", integration_id: "host" }]));
}

function cleaned(exportsDir: string) {
  const staged = join(exportsDir, "printer-uploads");
  expect(existsSync(staged)).toBe(true);
  expect(readdirSync(staged)).toEqual([]);
}

describe("printer send queue farm admission", () => {
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
    root = mkdtempSync(join(tmpdir(), "pp-send-queue-farm-"));
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
    await app.register(multipart);
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

  function fields(plan = profileId): Fields {
    return [
      ["printer_id", "printer"],
      ["profile_id", String(plan)],
      ["start", "true"],
    ];
  }

  function enqueue(url = urls[0], tenant = "default", metadata = fields()) {
    const body = upload(metadata);
    return app.inject({
      method: "POST",
      url,
      ...body,
      headers: { ...body.headers, "x-test-tenant": tenant },
    });
  }

  function act(action: "dispatch" | "drain", id: string, url = urls[0], tenant = "default") {
    return app.inject({
      method: "POST",
      url: action === "drain" ? `${url}/drain` : `${url}/${id}/dispatch`,
      headers: { "x-test-tenant": tenant },
      payload: { force: true },
    });
  }

  function seed(tenant = "default", plan = profileId, filename = `kept-${plan}.gcode`) {
    return tenantStorage.run(tenant, () => {
      const dir = jobs.getExportsDir(tenant);
      const artifact = join(dir, "printer-uploads", filename);
      mkdirSync(join(dir, "printer-uploads"), { recursive: true });
      writeFileSync(artifact, bytes);
      const item = enqueuePrinterSend(repo, {
        filename: "kept.gcode",
        artifact_path: artifact,
        printer_id: "printer",
        profile_id: plan,
        start: true,
        wait_for_idle: true,
        match: "pinned",
      });
      if (!item) throw new Error("seed failed");
      return item;
    });
  }

  for (const url of urls) {
    it.each([undefined, "{}", '{"farm":false}', '{"orders":true,"bambu":true}'])(
      `${url} enqueue denies default or explicit farm off %j before writing the queue`,
      async (stored) => {
        if (stored !== undefined) repo.setSetting("feature_toggles", stored);
        const before = repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY);
        const response = await enqueue(url);
        expect(response.statusCode).toBe(403);
        expect(response.json()).toEqual(farmOff);
        expect(repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY)).toBe(before);
        expect(loadPrinterSendQueue(repo)).toEqual([]);
        cleaned(jobs.getExportsDir());
        expect(jobs.start).not.toHaveBeenCalled();
        expect(integrations.getStatus).not.toHaveBeenCalled();
      },
    );
  }

  it("enqueues when the selected Plan enables farm over an off account", async () => {
    repo.setSetting("feature_toggles", '{"farm":false}');
    repo.setSetting(`feature_toggles:plan:${profileId}`, '{"farm":true}');
    const response = await enqueue();
    expect(response.statusCode).toBe(200);
    expect(loadPrinterSendQueue(repo)).toHaveLength(1);
    expect(response.json().item).toMatchObject({
      state: "queued",
      profile_id: profileId,
      printer_id: "printer",
    });
    expect(jobs.start).not.toHaveBeenCalled();
  });

  it("denies enqueue when the selected Plan disables farm over an on account", async () => {
    repo.setSetting("feature_toggles", '{"farm":true}');
    repo.setSetting(`feature_toggles:plan:${profileId}`, '{"farm":false}');
    const response = await enqueue();
    expect(response.json()).toEqual(farmOff);
    expect(loadPrinterSendQueue(repo)).toEqual([]);
    cleaned(jobs.getExportsDir());
  });

  it.each(["", "{", "[]", "null", '{"farm":"yes"}', '{"farm":true,"orders":0}'])(
    "enqueue fails closed for corrupt tenant or Plan storage %j",
    async (stored) => {
      for (const key of ["feature_toggles", `feature_toggles:plan:${profileId}`]) {
        repo.setSetting("feature_toggles", '{"farm":true}');
        repo.setSetting(key, stored);
        const response = await enqueue();
        expect(response.statusCode).toBe(500);
        expect(response.json()).toEqual(invalid);
        expect(loadPrinterSendQueue(repo)).toEqual([]);
        cleaned(jobs.getExportsDir());
      }
    },
  );

  it.each(["dispatch", "drain"] as const)(
    "%s denies a stored Moonraker item when farm is off and does not start a job",
    async (action) => {
      const item = seed();
      const before = repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY);
      const response = await act(action, item.id);
      expect(response.json()).toEqual(farmOff);
      expect(repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY)).toBe(before);
      expect(jobs.get).not.toHaveBeenCalled();
      expect(jobs.start).not.toHaveBeenCalled();
      expect(integrations.getStatus).not.toHaveBeenCalled();
    },
  );

  it("drain denies the batch when any selected Plan has farm off", async () => {
    repo.setSetting("feature_toggles", '{"farm":true}');
    const allowed = repo.createProfile("Allowed").id;
    repo.setSetting(`feature_toggles:plan:${allowed}`, '{"farm":true}');
    repo.setSetting(`feature_toggles:plan:${profileId}`, '{"farm":false}');
    configureHost(repo);
    const blocked = seed();
    const open = seed("default", allowed);
    repo.setSetting(
      "printer.fleet",
      JSON.stringify([
        { id: "printer", integration_id: "host" },
        { id: "other", integration_id: "host" },
      ]),
    );
    const queued = loadPrinterSendQueue(repo).map((item) =>
      item.id === open.id ? { ...item, printer_id: "other" } : item,
    );
    repo.setSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY, JSON.stringify(queued));
    const response = await act("drain", blocked.id);
    expect(response.json()).toEqual(farmOff);
    expect(jobs.start).not.toHaveBeenCalled();
    expect(loadPrinterSendQueue(repo).every((item) => item.state === "queued")).toBe(true);
  });

  it("rechecks farm revoked while live-job reconciliation is in flight", async () => {
    repo.setSetting("feature_toggles", '{"farm":true}');
    const item = seed();
    const sending = { ...item, id: "in-flight", state: "sending", upload_job_id: "stale" };
    repo.setSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY, JSON.stringify([item, sending]));
    vi.mocked(jobs.get).mockImplementation(async () => {
      repo.setSetting("feature_toggles", '{"farm":false}');
      return null;
    });
    const response = await act("dispatch", item.id);
    expect(response.json()).toEqual(farmOff);
    expect(jobs.get).toHaveBeenCalled();
    expect(jobs.start).not.toHaveBeenCalled();
    expect(loadPrinterSendQueue(repo).find((row) => row.id === item.id)?.state).toBe("queued");
  });

  it("keeps queue listing and cancellation available when farm is off", async () => {
    const item = seed();
    const listed = await app.inject({ method: "GET", url: "/printer-send-queue" });
    expect(listed.statusCode).toBe(200);
    expect(listed.json().items.map((row: { id: string }) => row.id)).toEqual([item.id]);
    const cancelled = await app.inject({ method: "DELETE", url: `/printer-send-queue/${item.id}` });
    expect(cancelled.statusCode).toBe(200);
    expect(cancelled.json().item.state).toBe("cancelled");
    expect(jobs.start).not.toHaveBeenCalled();
  });

  it.each([undefined, "", "0", "false", "TRUE", "true ", "yes"])(
    "flag %j leaves Moonraker enqueue on the legacy path without feature reads",
    async (flag) => {
      if (flag === undefined) delete process.env.PP_DESKTOP_REBUILD;
      else vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      repo.setSetting("feature_toggles", "broken");
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      const response = await enqueue();
      expect(response.statusCode).toBe(200);
      expect(loadPrinterSendQueue(repo)).toHaveLength(1);
      expect(reads).not.toHaveBeenCalled();
    },
  );

  it.each(["1", "true"])(
    "exact flag %j enforces farm on both PrusaLink enqueue and dispatch",
    async (flag) => {
      vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      configureHost(repo, "prusalink");
      const denied = await enqueue(urls[1]);
      expect(denied.json()).toEqual(farmOff);
      expect(loadPrinterSendQueue(repo)).toEqual([]);
      repo.setSetting("feature_toggles", '{"farm":true}');
      const accepted = await enqueue(urls[1]);
      expect(accepted.statusCode).toBe(200);
      const item = loadPrinterSendQueue(repo)[0];
      if (!item) throw new Error("missing queued item");
      const dispatched = await act("dispatch", item.id, urls[1]);
      expect(dispatched.statusCode).toBe(200);
      expect(dispatched.json()).toMatchObject({ job_id: "upload-job" });
    },
  );

  it("isolates tenant farm choices", async () => {
    repo.setSetting("feature_toggles", '{"farm":true}');
    const otherPlan = tenantStorage.run("other", () => {
      configureHost(repo);
      const plan = repo.createProfile("Other").id;
      repo.setSetting("feature_toggles", '{"farm":false}');
      return plan;
    });
    const denied = await enqueue(urls[0], "other", [
      ["printer_id", "printer"],
      ["profile_id", String(otherPlan)],
      ["start", "true"],
    ]);
    expect(denied.json()).toEqual(farmOff);
    const accepted = await enqueue();
    expect(accepted.statusCode).toBe(200);
    expect(tenantStorage.run("other", () => loadPrinterSendQueue(repo))).toEqual([]);
    expect(loadPrinterSendQueue(repo)).toHaveLength(1);
  });
});
