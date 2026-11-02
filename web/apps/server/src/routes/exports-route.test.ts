import { afterEach, describe, expect, it } from "vitest";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify from "fastify";
import { buildApp } from "../app.js";
import { loadConfig } from "../config.js";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { enqueuePrinterSend, loadPrinterSendQueue } from "../services/printer-send-queue-store.js";
import { tenantExportDirectory } from "../lib/secure-path.js";
import { registerExportRoutes } from "./exports.js";

let cleanup: Array<() => void> = [];

afterEach(() => {
  for (const fn of cleanup) fn();
  cleanup = [];
});

async function makeApp() {
  const dir = mkdtempSync(join(tmpdir(), "pp-exports-route-"));
  process.env.PRINT_PARTNER_DATA_DIR = dir;
  delete process.env.PRINT_PARTNER_API_KEY;
  const config = loadConfig();
  const ports = createSelfHostPorts(dir);
  await ports.db.connect();
  const app = await buildApp(config, ports);
  cleanup.push(() => {
    void app.close();
    void ports.db.close();
    rmSync(dir, { recursive: true, force: true });
  });
  return { app, dir };
}

/** 1x1 transparent PNG. */
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
  "base64",
);

describe("GET /exports/*", () => {
  it("migrates legacy self-host exports and queued artifact paths without overwriting", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-exports-legacy-migration-"));
    process.env.PRINT_PARTNER_DATA_DIR = dir;
    delete process.env.PRINT_PARTNER_API_KEY;
    const ports = createSelfHostPorts(dir);
    await ports.db.connect();
    const legacyRoot = join(dir, "exports");
    const legacyArtifact = join(legacyRoot, "printer-uploads", "queued", "plate.gcode");
    const collidingArtifact = join(legacyRoot, "printer-uploads", "colliding", "plate.gcode");
    const tenantRoot = join(legacyRoot, "tenant-default");
    const targetArtifact = join(tenantRoot, "printer-uploads", "queued", "plate.gcode");
    const collidingTarget = join(tenantRoot, "printer-uploads", "colliding", "plate.gcode");
    mkdirSync(join(legacyRoot, "printer-uploads", "queued"), { recursive: true });
    mkdirSync(join(legacyRoot, "printer-uploads", "colliding"), { recursive: true });
    mkdirSync(join(tenantRoot, "printer-uploads", "colliding"), { recursive: true });
    mkdirSync(join(legacyRoot, "tenant-other"), { recursive: true });
    writeFileSync(legacyArtifact, "G28\n");
    writeFileSync(collidingArtifact, "legacy queue artifact");
    writeFileSync(collidingTarget, "current queue artifact");
    writeFileSync(join(legacyRoot, "collision.txt"), "legacy");
    writeFileSync(join(tenantRoot, "collision.txt"), "current");
    writeFileSync(join(legacyRoot, "tenant-other", "secret.txt"), "other");
    enqueuePrinterSend(ports.repository, {
      filename: "plate.gcode",
      artifact_path: legacyArtifact,
      printer_id: "printer-1",
      start: false,
    });
    enqueuePrinterSend(ports.repository, {
      filename: "plate.gcode",
      artifact_path: collidingArtifact,
      printer_id: "printer-1",
      start: false,
    });

    const app = await buildApp(loadConfig(), ports);
    cleanup.push(() => {
      void app.close();
      void ports.db.close();
      rmSync(dir, { recursive: true, force: true });
    });

    expect(readFileSync(targetArtifact, "utf8")).toBe("G28\n");
    expect(existsSync(legacyArtifact)).toBe(false);
    expect(loadPrinterSendQueue(ports.repository)[0]?.artifact_path).toBe(targetArtifact);
    expect(loadPrinterSendQueue(ports.repository)[1]?.artifact_path).toBe(collidingArtifact);
    expect(readFileSync(collidingArtifact, "utf8")).toBe("legacy queue artifact");
    expect(readFileSync(collidingTarget, "utf8")).toBe("current queue artifact");
    expect(readFileSync(join(tenantRoot, "collision.txt"), "utf8")).toBe("current");
    expect(readFileSync(join(legacyRoot, "collision.txt"), "utf8")).toBe("legacy");
    expect(readFileSync(join(legacyRoot, "tenant-other", "secret.txt"), "utf8")).toBe("other");
  });

  it("serves same-named artifacts only from the authenticated tenant directory", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-exports-tenant-route-"));
    const app = Fastify();
    app.decorateRequest("tenantId", "default");
    app.addHook("onRequest", async (request) => {
      request.tenantId = String(request.headers["x-test-tenant"] ?? "default");
    });
    await registerExportRoutes(app, { dataDir: dir });
    cleanup.push(() => {
      void app.close();
      rmSync(dir, { recursive: true, force: true });
    });
    const tenantA = join(
      tenantExportDirectory(join(dir, "exports"), "tenant-a"),
      "accepted-plates",
      "profile-1",
      "revision-2",
      "plates",
    );
    const tenantB = join(
      tenantExportDirectory(join(dir, "exports"), "tenant-b"),
      "accepted-plates",
      "profile-1",
      "revision-2",
      "plates",
    );
    mkdirSync(tenantA, { recursive: true });
    mkdirSync(tenantB, { recursive: true });
    writeFileSync(join(tenantA, "0001.3mf"), "TENANT-A\n");
    writeFileSync(join(tenantB, "0001.3mf"), "TENANT-B\n");

    const a = await app.inject({
      method: "GET",
      url: "/exports/accepted-plates/profile-1/revision-2/plates/0001.3mf",
      headers: { "x-test-tenant": "tenant-a" },
    });
    const b = await app.inject({
      method: "GET",
      url: "/exports/accepted-plates/profile-1/revision-2/plates/0001.3mf",
      headers: { "x-test-tenant": "tenant-b" },
    });
    const crossTenant = await app.inject({
      method: "GET",
      url: "/exports/tenant-tenant-a/accepted-plates/profile-1/revision-2/plates/0001.3mf",
      headers: { "x-test-tenant": "tenant-b" },
    });

    expect(a.statusCode).toBe(200);
    expect(a.body).toBe("TENANT-A\n");
    expect(b.statusCode).toBe(200);
    expect(b.body).toBe("TENANT-B\n");
    expect(crossTenant.statusCode).toBe(404);
  });

  it("serves a PNG export inline as image/png", async () => {
    const { app, dir } = await makeApp();
    const thumbDir = join(dir, "exports", "tenant-default", "previews");
    mkdirSync(thumbDir, { recursive: true });
    writeFileSync(join(thumbDir, "plate_01.png"), PNG);

    const res = await app.inject({
      method: "GET",
      url: "/exports/previews/plate_01.png",
    });

    expect(res.statusCode).toBe(200);
    expect(res.headers["content-type"]).toBe("image/png");
    expect(res.headers["content-disposition"]).toContain("inline");
    expect(res.rawPayload.subarray(0, 8)).toEqual(PNG.subarray(0, 8));
  });

  it("serves ZIP attachments with a safe Unicode filename and explicit type", async () => {
    const { app, dir } = await makeApp();
    const exportDir = join(dir, "exports", "tenant-default", "bundles");
    mkdirSync(exportDir, { recursive: true });
    const name = 'Milo "pièce".zip';
    const bytes = Buffer.from([0x50, 0x4b, 3, 4]);
    writeFileSync(join(exportDir, name), bytes);
    const response = await app.inject({
      method: "GET",
      url: `/exports/bundles/${encodeURIComponent(name)}`,
    });
    expect(response.statusCode).toBe(200);
    expect(response.headers["content-type"]).toBe("application/zip");
    expect(response.headers["x-content-type-options"]).toBe("nosniff");
    expect(response.headers["content-disposition"]).toBe(
      `attachment; filename="Milo _pi_ce_.zip"; filename*=UTF-8''${encodeURIComponent(name)}`,
    );
    expect(response.rawPayload).toEqual(bytes);
  });

  it("serves accepted Plate files with the 3MF media type", async () => {
    const { app, dir } = await makeApp();
    const exportDir = join(dir, "exports", "tenant-default", "accepted-plates");
    mkdirSync(exportDir, { recursive: true });
    writeFileSync(join(exportDir, "0001.3mf"), "3mf-bytes");

    const response = await app.inject({
      method: "GET",
      url: "/exports/accepted-plates/0001.3mf",
    });

    expect(response.statusCode).toBe(200);
    expect(response.headers["content-type"]).toBe("model/3mf");
    expect(response.headers["content-disposition"]).toContain('attachment; filename="0001.3mf"');
  });

  it("still serves gcode as a downloadable attachment", async () => {
    const { app, dir } = await makeApp();
    const gcodeDir = join(dir, "exports", "tenant-default", "my_plan", "gcode");
    mkdirSync(gcodeDir, { recursive: true });
    writeFileSync(join(gcodeDir, "my_plan_voron_plate_01.gcode"), "G28\n");

    const res = await app.inject({
      method: "GET",
      url: "/exports/my_plan/gcode/my_plan_voron_plate_01.gcode",
    });

    expect(res.statusCode).toBe(200);
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["content-disposition"]).toContain("attachment");
    expect(res.body).toContain("G28");
  });

  it("404s a missing export and rejects traversal", async () => {
    const { app } = await makeApp();
    const missing = await app.inject({ method: "GET", url: "/exports/nope/plate_01.png" });
    expect(missing.statusCode).toBe(404);

    const traversal = await app.inject({
      method: "GET",
      url: "/exports/../../etc/passwd",
    });
    expect(traversal.statusCode).toBeGreaterThanOrEqual(400);
  });
});
