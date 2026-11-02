import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify, { type FastifyInstance } from "fastify";
import multipart from "@fastify/multipart";
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
const unsupported = {
  status: 400,
  title: "Bad Request",
  detail: "Queue is not supported for bambu",
};
const urls = ["/printer-send-queue", "/api/v1/printer-send-queue"];
const bytes = Buffer.from("G1 X1 Y1\n");
type Fields = Array<[string, string]>;

function upload(fields: Fields) {
  const boundary = "----pp-send-queue-feature-access";
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

function configureHost(repo: AppRepository, type = "bambu") {
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

describe("printer-send-queue Bambu HTTP feature admission", () => {
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

  function fields(): Fields {
    return [
      ["printer_id", "printer"],
      ["profile_id", String(profileId)],
      ["start", "true"],
    ];
  }
  function request(url = urls[0], tenant = "default", metadata = fields()) {
    const body = upload(metadata);
    return app.inject({
      method: "POST",
      url,
      ...body,
      headers: { ...body.headers, "x-test-tenant": tenant },
    });
  }
  async function refused(
    expected: typeof disabled | typeof invalid | typeof unsupported,
    url = urls[0],
    tenant = "default",
  ) {
    const before = tenantStorage.run(tenant, () =>
      repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY),
    );
    const writes = vi.spyOn(repo, "setSetting");
    const response = await request(url, tenant);
    expect(response.statusCode).toBe(expected.status);
    expect(response.json()).toEqual(expected);
    expect(tenantStorage.run(tenant, () => repo.getSetting(PRINTER_SEND_QUEUE_SETTINGS_KEY))).toBe(
      before,
    );
    expect(writes.mock.calls.filter(([key]) => key === PRINTER_SEND_QUEUE_SETTINGS_KEY)).toEqual(
      [],
    );
    writes.mockRestore();
    expect(integrations.getStatus).not.toHaveBeenCalled();
    cleaned(jobs.getExportsDir(tenant));
    expect(jobs.start).not.toHaveBeenCalled();
    expect(execution.probe).not.toHaveBeenCalled();
  }

  for (const url of urls) {
    it.each([undefined, "{}", '{"bambu":false}', '{"farm":true,"spoolman":true}'])(
      `${url} denies default or explicit off %j and cleans the staged file`,
      async (stored) => {
        if (stored !== undefined) repo.setSetting("feature_toggles", stored);
        await refused(disabled, url);
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
    ])(`${url} fails closed on corrupt tenant or selected Plan %j`, async (stored) => {
      for (const key of ["feature_toggles", `feature_toggles:plan:${profileId}`]) {
        repo.setSetting("feature_toggles", '{"bambu":true}');
        repo.setSetting(key, stored);
        await refused(invalid, url);
      }
    });

    it(`${url} reads live tenant and selected Plan choices while retaining unsupported Bambu`, async () => {
      const other = repo.createProfile("Unrelated Plan");
      repo.setSetting(`feature_toggles:plan:${other.id}`, '{"bambu":true}');
      await refused(disabled, url);
      repo.setSetting(`feature_toggles:plan:${profileId}`, '{"bambu":true,"future":{"option":1}}');
      await refused(unsupported, url);
      repo.setSetting("feature_toggles", '{"bambu":true}');
      repo.setSetting(`feature_toggles:plan:${profileId}`, '{"bambu":false}');
      await refused(disabled, url);
      repo.setSetting(`feature_toggles:plan:${profileId}`, "{}");
      await refused(unsupported, url);
      repo.setSetting("feature_toggles", '{"bambu":false}');
      await refused(disabled, url);
      repo.setSetting("feature_toggles", '{"bambu":true}');
      await refused(unsupported, url);
      repo.setSetting(`feature_toggles:plan:${other.id}`, "broken");
      await refused(unsupported, url);
      repo.setSetting(`feature_toggles:plan:${profileId}`, '{"bambu":true}');
      repo.setSetting("feature_toggles", "broken");
      await refused(invalid, url);
    });
  }

  it.each(["1", "true"])("enforces exact enabled flag %j", async (flag) => {
    vi.stubEnv("PP_DESKTOP_REBUILD", flag);
    await refused(disabled);
  });

  it.each([undefined, "", "0", "false", "TRUE", "true ", "yes", "2"])(
    "preserves legacy refusal without feature reads for flag %j",
    async (flag) => {
      if (flag === undefined) delete process.env.PP_DESKTOP_REBUILD;
      else vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      repo.setSetting("feature_toggles", "broken");
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      await refused(unsupported);
      expect(reads).not.toHaveBeenCalled();
    },
  );

  it("isolates tenant choices and checks Plan ownership before feature reads", async () => {
    repo.setSetting("feature_toggles", '{"bambu":true}');
    const otherPlan = tenantStorage.run("other", () => {
      configureHost(repo);
      const plan = repo.createProfile("Other tenant Plan");
      repo.setSetting(`feature_toggles:plan:${plan.id}`, '{"bambu":false}');
      return plan;
    });
    await refused(unsupported);
    tenantStorage.run("other", () => repo.setSetting("feature_toggles", '{"bambu":true}'));
    const denied = await request(urls[0], "other", [
      ["printer_id", "printer"],
      ["plan_id", String(otherPlan.id)],
    ]);
    expect(denied.statusCode).toBe(403);
    expect(denied.json()).toEqual(disabled);
    cleaned(jobs.getExportsDir("other"));
    tenantStorage.run("other", () =>
      repo.setSetting(`feature_toggles:plan:${otherPlan.id}`, '{"bambu":true}'),
    );
    const allowed = await request(urls[0], "other", [
      ["printer_id", "printer"],
      ["plan_id", String(otherPlan.id)],
    ]);
    expect(allowed.statusCode).toBe(400);
    expect(allowed.json()).toEqual(unsupported);
    cleaned(jobs.getExportsDir("other"));
    repo.setSetting("feature_toggles", '{"bambu":false}');
    await refused(disabled);
    const reads = vi.spyOn(repo, "getSettingSnapshot");
    for (const metadata of [
      [["printer_id", "printer"]],
      [
        ["printer_id", "printer"],
        ["profile_id", String(profileId)],
      ],
      [
        ["printer_id", "printer"],
        ["profile_id", "999999"],
      ],
    ] satisfies Fields[]) {
      reads.mockClear();
      const response = await request(urls[0], "other", metadata);
      expect(response.statusCode).toBe(metadata.length === 1 ? 400 : 404);
      expect(response.json().detail).toBe(
        metadata.length === 1
          ? "Pick a plan to bind this send (profile_id required)"
          : "Profile not found",
      );
      expect(reads).not.toHaveBeenCalled();
      cleaned(jobs.getExportsDir("other"));
    }
    expect(jobs.start).not.toHaveBeenCalled();
    expect(execution.probe).not.toHaveBeenCalled();
    expect(integrations.getStatus).not.toHaveBeenCalled();
    expect(tenantStorage.run("other", () => loadPrinterSendQueue(repo))).toEqual([]);
  });

  it.each(["moonraker", "prusalink"])(
    "preserves %s queue ownership and cancellation cleanup when farm is enabled",
    async (type) => {
      configureHost(repo, type);
      repo.setSetting("feature_toggles", '{"farm":true}');
      const reads = vi.spyOn(repo, "getSettingSnapshot");
      const response = await request(urls[1], "default", [
        ...fields(),
        ["wait_for_idle", "false"],
        ["match", "compatible"],
      ]);
      expect(response.statusCode).toBe(200);
      const items = loadPrinterSendQueue(repo);
      expect(items).toHaveLength(1);
      const item = items[0];
      if (!item) throw new Error("Missing persisted queue item");
      expect(response.json()).toEqual({ item: { ...item, checkoff_units: [] } });
      expect(item).toMatchObject({
        filename: "plate.gcode",
        printer_id: "printer",
        start: true,
        wait_for_idle: false,
        match: "compatible",
        profile_id: profileId,
        host_name: "Workshop",
        state: "queued",
      });
      expect(readFileSync(item.artifact_path)).toEqual(bytes);
      expect(readdirSync(join(jobs.getExportsDir(), "printer-uploads"))).toHaveLength(1);
      expect(jobs.start).not.toHaveBeenCalled();
      expect(integrations.getStatus).not.toHaveBeenCalled();
      expect(execution.probe).not.toHaveBeenCalled();
      expect(reads).toHaveBeenCalledWith("feature_toggles");
      const cancel = await app.inject({ method: "DELETE", url: `${urls[1]}/${item.id}` });
      expect(cancel.statusCode).toBe(200);
      expect(cancel.json().item.state).toBe("cancelled");
      expect(loadPrinterSendQueue(repo)[0]?.state).toBe("cancelled");
      expect(existsSync(item.artifact_path)).toBe(false);
      cleaned(jobs.getExportsDir());
    },
  );

  it.each([
    ["parser", 400, "printer_id is required"],
    ["plan", 400, "Pick a plan to bind this send (profile_id required)"],
    ["fleet", 404, "Fleet printer not found"],
    [
      "unlinked",
      400,
      "Printer is not linked to a host. Link a Moonraker or PrusaLink host in Settings.",
    ],
    ["host", 400, "Linked printer host was not found"],
    ["unsupported", 400, "Queue is not supported for octoprint"],
    ["enqueue", 500, "queue write failed"],
  ])("cleans staged artifacts after %s refusal", async (failure, status, detail) => {
    configureHost(repo, failure === "unsupported" ? "octoprint" : "moonraker");
    let metadata = fields();
    if (failure === "parser") metadata = [["profile_id", String(profileId)]];
    if (failure === "plan") metadata = [["printer_id", "printer"]];
    if (failure === "fleet") repo.setSetting("printer.fleet", "[]");
    if (failure === "unlinked") repo.setSetting("printer.fleet", '[{"id":"printer"}]');
    if (failure === "host") repo.setSetting("integrations", "[]");
    repo.setSetting("feature_toggles", failure === "enqueue" ? '{"farm":true}' : "broken");
    const reads = vi.spyOn(repo, "getSettingSnapshot");
    const writes = vi.spyOn(repo, "setSetting");
    if (failure === "enqueue")
      writes.mockImplementation((key) => {
        if (key === PRINTER_SEND_QUEUE_SETTINGS_KEY) throw new Error("queue write failed");
        throw new Error(`Unexpected write: ${key}`);
      });
    const response = await request(urls[0], "default", metadata);
    expect(response.statusCode).toBe(status);
    expect(response.json().detail ?? response.json().message).toBe(detail);
    cleaned(jobs.getExportsDir());
    expect(loadPrinterSendQueue(repo)).toEqual([]);
    if (failure !== "enqueue") expect(reads).not.toHaveBeenCalled();
    expect(jobs.start).not.toHaveBeenCalled();
    expect(integrations.getStatus).not.toHaveBeenCalled();
    expect(execution.probe).not.toHaveBeenCalled();
  });

  it("runs authentication before multipart staging and feature reads", async () => {
    const secured = Fastify();
    registerTenantMiddleware(
      secured,
      { ...loadConfig(), authRequired: true, multiUser: true },
      null,
    );
    await secured.register(multipart);
    await registerPrinterSendQueueRoutes(secured, { repo, jobs, integrations });
    const reads = vi.spyOn(repo, "getSettingSnapshot");
    try {
      const response = await secured.inject({ method: "POST", url: urls[0], ...upload(fields()) });
      expect(response.statusCode).toBe(401);
      expect(response.json()).toEqual({ detail: "Authentication required" });
      expect(existsSync(jobs.getExportsDir())).toBe(false);
      expect(reads).not.toHaveBeenCalled();
      expect(jobs.start).not.toHaveBeenCalled();
      expect(execution.probe).not.toHaveBeenCalled();
      expect(integrations.getStatus).not.toHaveBeenCalled();
      expect(loadPrinterSendQueue(repo)).toEqual([]);
    } finally {
      await secured.close();
    }
  });

  it.each(["1", "true"])("guards both production HTTP aliases with flag %j", async (flag) => {
    vi.stubEnv("PP_DESKTOP_REBUILD", flag);
    const dataDir = join(root, "production");
    const ports = createSelfHostPorts(dataDir);
    await ports.db.connect();
    const production = await buildApp({ ...loadConfig(), dataDir, authRequired: false }, ports);
    const plan = ports.repository.createProfile("Production Plan");
    configureHost(ports.repository);
    try {
      for (const url of urls) {
        for (const [stored, expected] of [
          ['{"bambu":false}', disabled],
          ['{"bambu":true}', unsupported],
          ["broken", invalid],
        ] satisfies Array<[string, typeof disabled | typeof unsupported | typeof invalid]>) {
          ports.repository.setSetting("feature_toggles", stored);
          const response = await production.inject({
            method: "POST",
            url,
            ...upload([
              ["printer_id", "printer"],
              ["profile_id", String(plan.id)],
              ["start", "true"],
            ]),
          });
          expect(response.statusCode).toBe(expected.status);
          expect(response.json()).toEqual(expected);
          cleaned(join(dataDir, "exports", "tenant-default"));
        }
      }
    } finally {
      await production.close();
      await ports.db.close();
    }
  });
});
