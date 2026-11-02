import { StatusExecution } from "../integrations/status-execution.js";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { moonrakerAdapter } from "../integrations/adapters/moonraker.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import { createIntegrationPort } from "../integrations/store.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import { createJobRunner } from "./job-runner.js";
import { saveFleet } from "./printer-fleet.js";
import {
  INTERRUPTED_PRINTER_SEND_ERROR,
  dispatchPrinterSendQueueItem,
  drainPrinterSendQueue,
  interruptInFlightPrinterSends,
  interruptPrinterUploadsOnShutdown,
  resetPrinterUploadHaltForTests,
} from "./printer-send-queue.js";
import {
  enqueuePrinterSend,
  getPrinterSendQueueItem,
  updatePrinterSendQueueItem,
} from "./printer-send-queue-store.js";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  resetPrinterUploadHaltForTests();
  for (const close of cleanup.splice(0)) await close();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function fixture(tenantId = "default") {
  const dir = mkdtempSync(join(tmpdir(), "pp-upload-shutdown-"));
  const ports = createSelfHostPorts(dir);
  await ports.db.connect();
  const repo = ports.repository;
  const integrations = createIntegrationPort({
    repo,
    getAdapter: getIntegrationAdapter,
    statusExecution: new StatusExecution(),
  });
  tenantStorage.run(tenantId, () => {
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
  });
  const jobs = createJobRunner(() => repo, dir);
  const exportsDir = jobs.getExportsDir(tenantId);
  const inTenant = <T>(work: () => T): T => tenantStorage.run(tenantId, work);
  cleanup.push(async () => {
    await ports.db.close();
    rmSync(dir, { recursive: true, force: true });
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new Error("Unexpected network request"))),
  );
  const upload = vi.spyOn(moonrakerAdapter, "uploadFile");
  const artifact = "; shutdown upload fixture\n";

  function enqueue(id: string, start = false) {
    const artifactDir = join(exportsDir, "printer-uploads", id);
    mkdirSync(artifactDir, { recursive: true });
    const path = join(artifactDir, "plate.gcode");
    writeFileSync(path, artifact);
    const item = tenantStorage.run(tenantId, () =>
      enqueuePrinterSend(repo, {
        filename: "plate.gcode",
        artifact_path: path,
        printer_id: "fixture-printer",
        start,
        wait_for_idle: true,
        match: "pinned",
      }),
    );
    if (!item) throw new Error("Fixture queue item was not created");
    return item;
  }

  const dispatchDeps = {
    startJob: (payload: Parameters<typeof jobs.start>[1]) =>
      jobs.start("printer-upload", payload, tenantId),
    getStatus: async () => ({ state: "idle" as const }),
  };

  return {
    dir,
    repo,
    jobs,
    exportsDir,
    upload,
    artifact,
    enqueue,
    dispatchDeps,
    inTenant,
    tenantId,
  };
}

describe("printer upload interrupted by shutdown", () => {
  it("does not mark the send successful or start it again", async () => {
    const f = await fixture();
    const item = f.enqueue("inflight", true);
    let finishUpload: (result: { ok: true; started: true; remote_path: string }) => void = () => {
      throw new Error("Upload gate was not created");
    };
    f.upload.mockReturnValueOnce(
      new Promise((resolve) => {
        finishUpload = resolve;
      }),
    );

    const dispatched = await dispatchPrinterSendQueueItem(
      f.repo,
      f.exportsDir,
      item.id,
      f.dispatchDeps,
    );
    if ("error" in dispatched) throw new Error(dispatched.error);
    await vi.waitFor(() => expect(f.upload).toHaveBeenCalledOnce());

    const interrupted = interruptInFlightPrinterSends(f.repo);
    expect(interrupted.map((row) => row.id)).toEqual([item.id]);
    expect(getPrinterSendQueueItem(f.repo, item.id)).toMatchObject({
      state: "error",
      error: INTERRUPTED_PRINTER_SEND_ERROR,
      upload_job_id: dispatched.job_id,
    });
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).not.toBe("done");

    finishUpload({ ok: true, started: true, remote_path: "plate.gcode" });
    const terminal = await f.jobs.waitForTerminal(dispatched.job_id, 3000, "default");

    expect(terminal.status).toBe("error");
    expect(terminal.error).toBe(INTERRUPTED_PRINTER_SEND_ERROR);
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("error");
    expect(existsSync(item.artifact_path)).toBe(true);
    expect(readFileSync(item.artifact_path, "utf8")).toBe(f.artifact);
    expect(f.upload).toHaveBeenCalledTimes(1);

    expect(await drainPrinterSendQueue(f.repo, f.exportsDir, f.dispatchDeps)).toEqual([]);
    expect(f.upload).toHaveBeenCalledTimes(1);
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("error");

    f.upload.mockResolvedValueOnce({ ok: true, started: true, remote_path: "plate.gcode" });
    const retried = await dispatchPrinterSendQueueItem(
      f.repo,
      f.exportsDir,
      item.id,
      f.dispatchDeps,
    );
    if ("error" in retried) throw new Error(retried.error);
    expect((await f.jobs.waitForTerminal(retried.job_id, 3000, "default")).status).toBe("done");
    expect(f.upload).toHaveBeenCalledTimes(2);
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("done");
  });

  it("does not record the original upload as success when a retry takes the queue row", async () => {
    const f = await fixture();
    const item = f.enqueue("retried-early", true);
    let finishOriginal: (result: { ok: true; started: true; remote_path: string }) => void = () => {
      throw new Error("Original upload gate was not created");
    };
    let finishRetry: (result: { ok: true; started: true; remote_path: string }) => void = () => {
      throw new Error("Retry upload gate was not created");
    };
    f.upload
      .mockReturnValueOnce(
        new Promise((resolve) => {
          finishOriginal = resolve;
        }),
      )
      .mockReturnValueOnce(
        new Promise((resolve) => {
          finishRetry = resolve;
        }),
      );

    const first = await dispatchPrinterSendQueueItem(f.repo, f.exportsDir, item.id, f.dispatchDeps);
    if ("error" in first) throw new Error(first.error);
    await vi.waitFor(() => expect(f.upload).toHaveBeenCalledOnce());

    expect(interruptInFlightPrinterSends(f.repo).map((row) => row.id)).toEqual([item.id]);
    const retried = await dispatchPrinterSendQueueItem(
      f.repo,
      f.exportsDir,
      item.id,
      f.dispatchDeps,
    );
    if ("error" in retried) throw new Error(retried.error);
    await vi.waitFor(() => expect(f.upload).toHaveBeenCalledTimes(2));

    finishOriginal({ ok: true, started: true, remote_path: "plate.gcode" });
    const original = await f.jobs.waitForTerminal(first.job_id, 3000, "default");
    expect(original.status).toBe("error");
    expect(original.error).toBe(INTERRUPTED_PRINTER_SEND_ERROR);
    expect(existsSync(item.artifact_path)).toBe(true);
    expect(readFileSync(item.artifact_path, "utf8")).toBe(f.artifact);
    expect(getPrinterSendQueueItem(f.repo, item.id)).toMatchObject({
      state: "sending",
      upload_job_id: retried.job_id,
    });

    finishRetry({ ok: true, started: true, remote_path: "plate.gcode" });
    expect((await f.jobs.waitForTerminal(retried.job_id, 3000, "default")).status).toBe("done");
    expect(f.upload).toHaveBeenCalledTimes(2);
    expect(getPrinterSendQueueItem(f.repo, item.id)?.state).toBe("done");
  });

  it.each(["unscoped", "mapped", "unavailable"])(
    "retains a direct upload after %s shutdown",
    async (mode) => {
      const tenant = mode === "unscoped" ? "default" : "shop-a";
      const f = await fixture(tenant);
      const artifactDir = join(f.exportsDir, "printer-uploads", "direct");
      mkdirSync(artifactDir, { recursive: true });
      const artifactPath = join(artifactDir, "plate.gcode");
      writeFileSync(artifactPath, f.artifact);
      let finishUpload: (result: { ok: true; started: true; remote_path: string }) => void = () => {
        throw new Error("Upload gate was not created");
      };
      f.upload.mockReturnValueOnce(
        new Promise((resolve) => {
          finishUpload = resolve;
        }),
      );

      const jobId = await f.jobs.start(
        "printer-upload",
        {
          printer_id: "fixture-printer",
          artifact_path: artifactPath,
          filename: "plate.gcode",
          start: true,
        },
        tenant,
      );
      await vi.waitFor(() => expect(f.upload).toHaveBeenCalledOnce());
      const foreign = tenantStorage.run("shop-b", () => {
        const item = enqueuePrinterSend(f.repo, {
          filename: "foreign.gcode",
          artifact_path: artifactPath,
          printer_id: "foreign",
          start: true,
        });
        if (!item) throw new Error("Foreign queue fixture missing");
        return updatePrinterSendQueueItem(f.repo, item.id, { state: "sending" });
      });
      if (!foreign) throw new Error("Foreign queue fixture missing");

      if (mode === "unscoped") interruptPrinterUploadsOnShutdown(f.dir, { repository: f.repo });
      else
        interruptPrinterUploadsOnShutdown(
          f.dir,
          mode === "unavailable" ? {} : { repository: f.repo },
          tenant,
        );
      finishUpload({ ok: true, started: true, remote_path: "plate.gcode" });
      const terminal = await f.jobs.waitForTerminal(jobId, 3000, tenant);

      expect(terminal.status).toBe("error");
      expect(terminal.error).toBe(INTERRUPTED_PRINTER_SEND_ERROR);
      expect(existsSync(artifactPath)).toBe(true);
      expect(readFileSync(artifactPath, "utf8")).toBe(f.artifact);
      expect(f.upload).toHaveBeenCalledTimes(1);
      expect(
        tenantStorage.run("shop-b", () => getPrinterSendQueueItem(f.repo, foreign.id)),
      ).toEqual(
        mode === "unscoped"
          ? expect.objectContaining({ state: "error", error: INTERRUPTED_PRINTER_SEND_ERROR })
          : foreign,
      );
    },
  );

  it.each(["unscoped", "scoped"])(
    "interrupts an in-flight tenant send during %s shutdown",
    async (mode) => {
      const f = await fixture("shop-b");
      const item = f.enqueue("inflight", true);
      let finishUpload: (result: { ok: true; started: true; remote_path: string }) => void = () => {
        throw new Error("Upload gate was not created");
      };
      f.upload.mockReturnValueOnce(
        new Promise((resolve) => {
          finishUpload = resolve;
        }),
      );

      const dispatched = await f.inTenant(() =>
        dispatchPrinterSendQueueItem(f.repo, f.exportsDir, item.id, f.dispatchDeps),
      );
      if ("error" in dispatched) throw new Error(dispatched.error);
      await vi.waitFor(() => expect(f.upload).toHaveBeenCalledOnce());

      const interrupted = interruptPrinterUploadsOnShutdown(
        f.dir,
        { repository: f.repo },
        mode === "scoped" ? "shop-b" : undefined,
      );
      expect(interrupted.map((row) => row.id)).toEqual([item.id]);
      finishUpload({ ok: true, started: true, remote_path: "plate.gcode" });
      const terminal = await f.jobs.waitForTerminal(dispatched.job_id, 3000, "shop-b");

      expect(terminal.status).toBe("error");
      expect(terminal.error).toBe(INTERRUPTED_PRINTER_SEND_ERROR);
      expect(f.inTenant(() => getPrinterSendQueueItem(f.repo, item.id)?.state)).toBe("error");
      expect(existsSync(item.artifact_path)).toBe(true);
      expect(readFileSync(item.artifact_path, "utf8")).toBe(f.artifact);
      expect(
        await f.inTenant(() => drainPrinterSendQueue(f.repo, f.exportsDir, f.dispatchDeps)),
      ).toEqual([]);
      expect(f.upload).toHaveBeenCalledTimes(1);
    },
  );
});
