import { StatusExecution } from "../integrations/status-execution.js";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify from "fastify";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { moonrakerAdapter } from "../integrations/adapters/moonraker.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import { createIntegrationPort } from "../integrations/store.js";
import { registerPrinterSendQueueRoutes } from "../routes/printer-send-queue.js";
import { createJobRunner } from "./job-runner.js";
import { saveFleet } from "./printer-fleet.js";
import { dispatchPrinterSendQueueItem, drainPrinterSendQueue } from "./printer-send-queue.js";
import { enqueuePrinterSend, getPrinterSendQueueItem } from "./printer-send-queue-store.js";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  for (const close of cleanup.splice(0)) await close();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function fixture() {
  const dir = mkdtempSync(join(tmpdir(), "pp-upload-retry-"));
  const ports = createSelfHostPorts(dir);
  await ports.db.connect();
  const repo = ports.repository;
  const integrations = createIntegrationPort({
    repo,
    getAdapter: getIntegrationAdapter,
    statusExecution: new StatusExecution(),
  });
  const integration = integrations.create({
    type: "moonraker",
    name: "Fixture host",
    config: { base_url: "http://127.0.0.1:1", enabled: true },
  });
  saveFleet(repo, [
    {
      id: "fixture-printer",
      name: "Fixture",
      model: "Fixture",
      bed_width_mm: 250,
      bed_depth_mm: 250,
      bed_height_mm: 250,
      margin_mm: 5,
      max_filament_slots: 1,
      loaded_filaments: [],
      integration_id: integration.id,
    },
  ]);
  const jobs = createJobRunner(() => repo, dir);
  const exportsDir = jobs.getExportsDir("default");
  const app = Fastify();
  await registerPrinterSendQueueRoutes(app, { repo, integrations, jobs });
  cleanup.push(async () => {
    await app.close();
    await ports.db.close();
    rmSync(dir, { recursive: true, force: true });
  });
  const fetch = vi.fn(() => Promise.reject(new Error("Unexpected network request")));
  vi.stubGlobal("fetch", fetch);
  const upload = vi.spyOn(moonrakerAdapter, "uploadFile");
  upload.mockResolvedValue({ ok: true, started: false, remote_path: "plate.gcode" });

  function stage(id: string) {
    const artifactDir = join(exportsDir, "printer-uploads", id);
    mkdirSync(artifactDir, { recursive: true });
    const path = join(artifactDir, "plate.gcode");
    writeFileSync(path, "; isolated upload fixture\n");
    return path;
  }

  function enqueue(id: string, start = false) {
    const path = stage(id);
    const item = enqueuePrinterSend(repo, {
      filename: "plate.gcode",
      artifact_path: path,
      printer_id: "fixture-printer",
      start,
      wait_for_idle: true,
      match: "pinned",
    });
    if (!item) throw new Error("Fixture queue item was not created");
    return item;
  }

  const dispatchDeps = {
    startJob: (payload: Parameters<typeof jobs.start>[1]) =>
      jobs.start("printer-upload", payload, "default"),
    getStatus: async () => ({ state: "idle" as const }),
  };

  async function dispatch(id: string) {
    const result = await dispatchPrinterSendQueueItem(repo, exportsDir, id, dispatchDeps);
    if ("error" in result) throw new Error(result.error);
    const terminal = await jobs.waitForTerminal(result.job_id, 3000, "default");
    await new Promise<void>((resolve) => setImmediate(resolve));
    return terminal;
  }

  return { repo, jobs, exportsDir, app, upload, fetch, stage, enqueue, dispatch, dispatchDeps };
}

describe("printer upload artifact ownership", () => {
  it("retains a failed queued upload for explicit retry and removes it after success", async () => {
    const f = await fixture();
    const item = f.enqueue("retry", true);
    f.upload.mockRejectedValueOnce(new Error("temporary upload failure"));

    expect((await f.dispatch(item.id)).status).toBe("error");
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("error");
    expect(existsSync(item.artifact_path)).toBe(true);
    expect(readFileSync(item.artifact_path, "utf8")).toBe("; isolated upload fixture\n");
    expect(await drainPrinterSendQueue(f.repo, f.exportsDir, f.dispatchDeps)).toEqual([]);
    expect(f.upload).toHaveBeenCalledTimes(1);

    expect((await f.dispatch(item.id)).status).toBe("done");
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("done");
    expect(f.upload).toHaveBeenCalledTimes(2);
    expect(f.upload).toHaveBeenLastCalledWith(
      expect.anything(),
      { path: item.artifact_path },
      "plate.gcode",
      { start: true },
    );
    expect(existsSync(item.artifact_path)).toBe(false);
    expect(f.fetch).not.toHaveBeenCalled();
  });

  it.each(["done", "error"] as const)("cleans direct uploads after %s", async (status) => {
    const f = await fixture();
    const path = f.stage(`direct-${status}`);
    if (status === "error") f.upload.mockRejectedValueOnce(new Error("direct upload failed"));
    const jobId = await f.jobs.start(
      "printer-upload",
      {
        printer_id: "fixture-printer",
        artifact_path: path,
        filename: "plate.gcode",
        start: false,
      },
      "default",
    );
    expect((await f.jobs.waitForTerminal(jobId, 3000, "default")).status).toBe(status);
    expect(existsSync(path)).toBe(false);
    expect(f.fetch).not.toHaveBeenCalled();
  });

  it.each(["queued", "error"] as const)(
    "cleans a %s queue artifact when removed",
    async (state) => {
      const f = await fixture();
      const item = f.enqueue(`remove-${state}`);
      if (state === "error") {
        f.upload.mockRejectedValueOnce(new Error("temporary upload failure"));
        expect((await f.dispatch(item.id)).status).toBe("error");
      }
      expect(existsSync(item.artifact_path)).toBe(true);
      const response = await f.app.inject({
        method: "DELETE",
        url: `/printer-send-queue/${item.id}`,
      });
      expect(response.statusCode).toBe(200);
      expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("cancelled");
      expect(existsSync(item.artifact_path)).toBe(false);
    },
  );

  it("refuses removal while an upload still owns the artifact", async () => {
    const f = await fixture();
    const item = f.enqueue("sending");
    let finishUpload: () => void = () => {
      throw new Error("Upload gate was not created");
    };
    const transfer = new Promise<{ ok: boolean; started: boolean }>((resolve) => {
      finishUpload = () => resolve({ ok: true, started: false });
    });
    f.upload.mockReturnValueOnce(transfer);
    const dispatched = await dispatchPrinterSendQueueItem(
      f.repo,
      f.exportsDir,
      item.id,
      f.dispatchDeps,
    );
    if ("error" in dispatched) throw new Error(dispatched.error);
    try {
      await vi.waitFor(() => expect(f.upload).toHaveBeenCalledOnce());
      const response = await f.app.inject({
        method: "DELETE",
        url: `/printer-send-queue/${item.id}`,
      });
      expect(response.statusCode).toBe(409);
      expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("sending");
      expect(existsSync(item.artifact_path)).toBe(true);
    } finally {
      finishUpload();
      await f.jobs.waitForTerminal(dispatched.job_id, 3000, "default");
    }
  });

  it("does not clean an artifact outside the tenant export directory when cancelling", async () => {
    const f = await fixture();
    const outsideDir = join(f.exportsDir, "..", "..", "outside", "printer-uploads", "fixture");
    mkdirSync(outsideDir, { recursive: true });
    const outsideFile = join(outsideDir, "plate.gcode");
    writeFileSync(outsideFile, "must remain untouched");
    const item = enqueuePrinterSend(f.repo, {
      filename: "plate.gcode",
      artifact_path: outsideFile,
      printer_id: "fixture-printer",
      start: false,
    });
    if (!item) throw new Error("Fixture queue item was not created");
    const response = await f.app.inject({
      method: "DELETE",
      url: `/printer-send-queue/${item.id}`,
    });
    expect(response.statusCode).toBe(200);
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("cancelled");
    expect(readFileSync(outsideFile, "utf8")).toBe("must remain untouched");
  });
});
