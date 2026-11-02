import Fastify from "fastify";
import rateLimit from "@fastify/rate-limit";
import { afterEach, describe, expect, it, vi } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { PrinterStorageListing } from "@print-partner/contracts";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import type {
  IntegrationAdapter,
  PrinterCameraAccess,
  PrinterFileAccess,
} from "../integrations/store.js";
import { parsePrinterMachine, saveFleet } from "../services/printer-fleet.js";
import { registerPrinterRoutes } from "./printers.js";
import { getLogger } from "../services/logger.js";

/**
 * Route-level checks for the printer-host inspection endpoints: capability
 * advertisement, directory browsing, path-validated downloads, and cameras.
 *
 * The adapter registry is faked so these tests describe the route contract
 * rather than Moonraker or PrusaLink wire details.
 */

const registry = vi.hoisted(() => ({ adapter: undefined as IntegrationAdapter | undefined }));

vi.mock("../integrations/registry.js", () => ({
  getIntegrationAdapter: () => registry.adapter,
  listIntegrationTypes: () => ["moonraker"],
}));

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  registry.adapter = undefined;
  for (const fn of cleanup.splice(0)) await fn();
});

/** A minimal adapter; `testConnection` is never reached by these routes. */
function fakeAdapter(capabilities: {
  files?: PrinterFileAccess;
  cameras?: PrinterCameraAccess;
}): IntegrationAdapter {
  return {
    type: "moonraker",
    testConnection: async () => {
      throw new Error("testConnection is not part of the inspection routes");
    },
    ...capabilities,
  };
}

type SeededRoute = { method: string; url: string; config: unknown };

async function setup() {
  const dir = mkdtempSync(join(tmpdir(), "pp-printer-routes-"));
  const sqlite = new SqliteDatabase(dir);
  sqlite.connect();
  const repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
  const now = new Date().toISOString();
  repo.setSetting(
    "integrations",
    JSON.stringify([
      {
        id: "moonraker-1",
        type: "moonraker",
        name: "Voron",
        config: { base_url: "http://127.0.0.1:7125" },
        created_at: now,
        updated_at: now,
      },
    ]),
  );
  saveFleet(repo, [
    parsePrinterMachine({
      id: "hosted",
      name: "Voron 2.4",
      model: "Voron",
      bed_width_mm: 350,
      bed_depth_mm: 350,
      max_filament_slots: 1,
      loaded_filaments: [],
      integration_id: "moonraker-1",
    }),
    parsePrinterMachine({
      id: "manual",
      name: "Garage printer",
      model: "Custom",
      bed_width_mm: 250,
      bed_depth_mm: 210,
      max_filament_slots: 1,
      loaded_filaments: [],
    }),
  ]);

  const app = Fastify();
  const routes: SeededRoute[] = [];
  app.addHook("onRoute", (route) => {
    routes.push({ method: String(route.method), url: route.url, config: route.config });
  });
  await app.register(rateLimit, { global: false });
  await registerPrinterRoutes(app, { repo });
  await app.ready();
  cleanup.push(async () => {
    await app.close();
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });
  return { app, repo, routes };
}

describe("printer host route protection", () => {
  it("rate-limits every printer host route", async () => {
    const { routes } = await setup();
    const guarded = [
      "/printers/:id/capabilities",
      "/printers/:id/files",
      "/printers/:id/files/content",
      "/printers/:id/cameras",
      "/printers/:id/cameras/view",
    ];
    for (const url of guarded) {
      const route = routes.find((candidate) => candidate.url === url);
      expect(route, `${url} is registered`).toBeDefined();
      const config = route?.config;
      const rateLimited =
        typeof config === "object" &&
        config !== null &&
        "rateLimit" in config &&
        typeof config.rateLimit === "object";
      expect(rateLimited, `${url} carries a rateLimit config`).toBe(true);
    }
  });

  it("refuses a proxying route once its budget is spent", async () => {
    const { app } = await setup();
    const statuses: number[] = [];
    for (let attempt = 0; attempt < 21; attempt += 1) {
      const res = await app.inject({ method: "GET", url: "/printers/hosted/files/content" });
      statuses.push(res.statusCode);
    }
    expect(statuses.filter((status) => status === 400)).toHaveLength(20);
    expect(statuses.at(-1)).toBe(429);
  });
});

describe("GET /printers/:id/capabilities", () => {
  it("reports what the linked adapter can serve", async () => {
    const { app } = await setup();
    registry.adapter = fakeAdapter({
      files: {
        browse: async () => ({ path: "", entries: [] }),
        open: async () => new Response("G1"),
      },
    });

    const res = await app.inject({ method: "GET", url: "/printers/hosted/capabilities" });

    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ files: true, cameras: false, status: false });
  });

  it("reports no capabilities for a printer without a linked host", async () => {
    const { app } = await setup();

    const res = await app.inject({ method: "GET", url: "/printers/manual/capabilities" });

    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ files: false, cameras: false, status: false });
  });

  it("still reports a missing printer as not found", async () => {
    const { app } = await setup();

    const res = await app.inject({ method: "GET", url: "/printers/ghost/capabilities" });

    expect(res.statusCode).toBe(404);
  });
});

describe("GET /printers/:id/files", () => {
  it("browses the storage root when no path is given", async () => {
    const { app } = await setup();
    const browsed: string[] = [];
    const listing: PrinterStorageListing = {
      path: "",
      entries: [
        { kind: "directory", path: "sub", name: "sub" },
        { kind: "file", path: "bracket.gcode", name: "bracket.gcode", size_bytes: 12 },
      ],
    };
    registry.adapter = fakeAdapter({
      files: {
        browse: async (_config, path) => {
          browsed.push(path);
          return listing;
        },
        open: async () => new Response("G1"),
      },
    });

    const res = await app.inject({ method: "GET", url: "/printers/hosted/files" });

    expect(browsed).toEqual([""]);
    expect(res.json()).toEqual(listing);
  });

  it("passes a requested subdirectory through to the adapter", async () => {
    const { app } = await setup();
    const browsed: string[] = [];
    registry.adapter = fakeAdapter({
      files: {
        browse: async (_config, path) => {
          browsed.push(path);
          return { path, entries: [] };
        },
        open: async () => new Response("G1"),
      },
    });

    const res = await app.inject({ method: "GET", url: "/printers/hosted/files?path=sub" });

    expect(res.statusCode).toBe(200);
    expect(browsed).toEqual(["sub"]);
  });

  it("rejects a traversal path before reaching the adapter", async () => {
    const { app } = await setup();
    let browseCalls = 0;
    registry.adapter = fakeAdapter({
      files: {
        browse: async (_config, path) => {
          browseCalls += 1;
          return { path, entries: [] };
        },
        open: async () => new Response("G1"),
      },
    });

    const res = await app.inject({ method: "GET", url: "/printers/hosted/files?path=../etc" });

    expect(res.statusCode).toBe(400);
    expect(browseCalls).toBe(0);
  });

  it("maps an adapter failure to 502 without leaking the host config", async () => {
    const { app } = await setup();
    registry.adapter = fakeAdapter({
      files: {
        browse: async () => {
          throw new Error("Moonraker refused the request");
        },
        open: async () => new Response("G1"),
      },
    });

    const res = await app.inject({ method: "GET", url: "/printers/hosted/files" });

    expect(res.statusCode).toBe(502);
    expect(res.json().detail).toBe("Moonraker refused the request");
  });

  it("answers 501 when the linked host cannot browse files", async () => {
    const { app } = await setup();
    registry.adapter = fakeAdapter({});

    const res = await app.inject({ method: "GET", url: "/printers/hosted/files" });

    expect(res.statusCode).toBe(501);
  });
});

describe("GET /printers/:id/files/content", () => {
  it.each([200, 404])(
    "rejects an unusable HTTP %s file response and cancels its body",
    async (status) => {
      const { app } = await setup();
      const cancel = vi.fn();
      registry.adapter = fakeAdapter({
        files: {
          browse: async () => ({
            path: "",
            entries: [{ kind: "file", path: "bracket.gcode", name: "bracket.gcode" }],
          }),
          open: async () =>
            new Response(status === 200 ? null : new ReadableStream<Uint8Array>({ cancel }), {
              status,
            }),
        },
      });

      const res = await app.inject({
        method: "GET",
        url: "/printers/hosted/files/content?path=bracket.gcode",
      });

      expect(res.statusCode).toBe(status === 404 ? 404 : 502);
      expect(cancel).toHaveBeenCalledTimes(status === 404 ? 1 : 0);
    },
  );

  it("records a download stream failure without leaking the file path or upstream error", async () => {
    const { app } = await setup();
    const log = vi.spyOn(getLogger(), "logWorkflow");
    const privatePath = "private-customer-part.bgcode";
    registry.adapter = fakeAdapter({
      files: {
        browse: async () => ({
          path: "",
          entries: [{ kind: "file", path: privatePath, name: privatePath }],
        }),
        open: async () =>
          new Response(
            new ReadableStream<Uint8Array>({
              start(controller) {
                controller.enqueue(new TextEncoder().encode("partial"));
                setTimeout(() => controller.error(new Error("secret-password upstream-host")), 10);
              },
            }),
          ),
      },
    });

    try {
      await expect(
        app.inject({
          method: "GET",
          url: `/printers/hosted/files/content?path=${privatePath}`,
        }),
      ).rejects.toThrow("response destroyed before completion");
      expect(log).toHaveBeenCalledTimes(1);
      expect(log).toHaveBeenCalledWith(
        expect.objectContaining({
          severity: "error",
          url: "/printers/:id/files/content",
          message: "Printer file download interrupted",
          context: { printerId: "hosted", failure: "stream_interrupted" },
        }),
      );
      expect(JSON.stringify(log.mock.calls)).not.toMatch(
        /private-customer|secret-password|upstream-host/,
      );
    } finally {
      log.mockRestore();
    }
  });

  function filesWithEntries(entries: PrinterStorageListing["entries"]) {
    const browsed: string[] = [];
    const opened: string[] = [];
    const files: PrinterFileAccess = {
      browse: async (_config, path) => {
        browsed.push(path);
        return { path, entries };
      },
      open: async (_config, path) => {
        opened.push(path);
        return new Response("G1 X0 Y0", { headers: { "content-type": "text/plain" } });
      },
    };
    return { files, browsed, opened };
  }

  it("opens a file listed in its own directory", async () => {
    const { app } = await setup();
    const { files, browsed, opened } = filesWithEntries([
      { kind: "file", path: "sub/bracket.gcode", name: "bracket.gcode" },
    ]);
    registry.adapter = fakeAdapter({ files });

    const res = await app.inject({
      method: "GET",
      url: "/printers/hosted/files/content?path=sub/bracket.gcode",
    });

    expect(res.statusCode).toBe(200);
    expect(res.body).toBe("G1 X0 Y0");
    expect(browsed).toEqual(["sub"]);
    expect(opened).toEqual(["sub/bracket.gcode"]);
    expect(res.headers["cache-control"]).toBe("private, no-store");
  });

  it("rejects a path that the directory listing does not contain", async () => {
    const { app } = await setup();
    const { files, opened } = filesWithEntries([
      { kind: "file", path: "sub/bracket.gcode", name: "bracket.gcode" },
    ]);
    registry.adapter = fakeAdapter({ files });

    const res = await app.inject({
      method: "GET",
      url: "/printers/hosted/files/content?path=sub/secrets.gcode",
    });

    expect(res.statusCode).toBe(404);
    expect(opened).toEqual([]);
  });

  it("refuses to open a directory entry", async () => {
    const { app } = await setup();
    const { files, opened } = filesWithEntries([{ kind: "directory", path: "sub", name: "sub" }]);
    registry.adapter = fakeAdapter({ files });

    const res = await app.inject({ method: "GET", url: "/printers/hosted/files/content?path=sub" });

    expect(res.statusCode).toBe(404);
    expect(opened).toEqual([]);
  });

  it("requires a path", async () => {
    const { app } = await setup();
    registry.adapter = fakeAdapter({ files: filesWithEntries([]).files });

    const res = await app.inject({ method: "GET", url: "/printers/hosted/files/content" });

    expect(res.statusCode).toBe(400);
  });
});

describe("printer camera routes", () => {
  it("reports no cameras when the adapter cannot serve any", async () => {
    const { app } = await setup();
    registry.adapter = fakeAdapter({});

    const res = await app.inject({ method: "GET", url: "/printers/hosted/cameras" });

    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ cameras: [] });
  });

  it("only opens a camera the adapter discovered", async () => {
    const { app } = await setup();
    const opened: string[] = [];
    registry.adapter = fakeAdapter({
      cameras: {
        list: async () => [{ id: "cam-1", name: "Nozzle", view: "snapshot" }],
        open: async (_config, cameraId) => {
          opened.push(cameraId);
          return new Response("jpeg-bytes", { headers: { "content-type": "image/jpeg" } });
        },
      },
    });

    const missing = await app.inject({
      method: "GET",
      url: "/printers/hosted/cameras/view?id=cam-9",
    });
    const found = await app.inject({
      method: "GET",
      url: "/printers/hosted/cameras/view?id=cam-1",
    });

    expect(missing.statusCode).toBe(404);
    expect(found.statusCode).toBe(200);
    expect(found.headers["content-type"]).toBe("image/jpeg");
    expect(opened).toEqual(["cam-1"]);
  });

  it("answers 409 for a printer that is not linked to a host", async () => {
    const { app } = await setup();

    const res = await app.inject({ method: "GET", url: "/printers/manual/cameras" });

    expect(res.statusCode).toBe(409);
  });
});
