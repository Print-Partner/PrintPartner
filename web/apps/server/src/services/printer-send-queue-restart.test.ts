import { StatusExecution } from "../integrations/status-execution.js";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify from "fastify";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AppRepository } from "../db/repository.js";
import { registerJobRoutes } from "../routes/jobs.js";
import { registerPrinterSendQueueRoutes } from "../routes/printer-send-queue.js";
import { createJobRunner } from "./job-runner.js";
import {
  reconcileMissingPrinterUploadJob,
  reconcilePrinterSendsAfterProcessStart,
  reconcilePrinterSendsAgainstLiveJobs,
} from "./printer-send-queue.js";
import {
  enqueuePrinterSend,
  loadPrinterSendQueue,
  updatePrinterSendQueueItem,
} from "./printer-send-queue-store.js";

vi.mock("../integrations/store.js", () => ({
  getIntegrationConfig: vi.fn(),
}));
vi.mock("./printer-fleet.js", () => ({
  loadFleet: vi.fn(),
}));
vi.mock("./printer-upload-job.js", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./printer-upload-job.js")>();
  return {
    ...actual,
    runPrinterUploadJob: vi.fn(),
  };
});

import { getIntegrationConfig } from "../integrations/store.js";
import { loadFleet } from "./printer-fleet.js";
import { runPrinterUploadJob } from "./printer-upload-job.js";

const getIntegrationConfigMock = vi.mocked(getIntegrationConfig);
const loadFleetMock = vi.mocked(loadFleet);
const runPrinterUploadJobMock = vi.mocked(runPrinterUploadJob);

const INTERRUPTED =
  "Interrupted by process restart. Printer outcome is unknown, so this send was not marked successful. The staged file remains for an explicit retry. This process will not retry a print on its own.";
const ORIGINAL = Buffer.from("; original plate bytes\n");

function memoryRepo(): AppRepository {
  const settings = new Map<string, string>();
  return {
    getSetting: (key: string) => settings.get(key) ?? null,
    setSetting: (key: string, value: string) => {
      settings.set(key, value);
    },
    transaction: <T>(fn: () => T) => fn(),
  } as unknown as AppRepository;
}

function stage(exportsDir: string, id: string, filename: string): string {
  const dir = join(exportsDir, "printer-uploads", id);
  mkdirSync(dir, { recursive: true });
  const path = join(dir, filename);
  writeFileSync(path, ORIGINAL);
  return path;
}

describe("printer send restart reconciliation", () => {
  const dirs: string[] = [];

  afterEach(() => {
    vi.clearAllMocks();
    for (const dir of dirs) rmSync(dir, { recursive: true, force: true });
    dirs.length = 0;
  });

  function fixture() {
    const exportsDir = mkdtempSync(join(tmpdir(), "pp-send-restart-"));
    dirs.push(exportsDir);
    const repo = memoryRepo();
    getIntegrationConfigMock.mockImplementation((_repo, id) =>
      id
        ? ({
            id,
            type: "moonraker",
            name: "Host",
            config: { base_url: "http://127.0.0.1:9" },
            created_at: "",
            updated_at: "",
          } as ReturnType<typeof getIntegrationConfig>)
        : null,
    );
    loadFleetMock.mockReturnValue([
      {
        id: "p1",
        name: "P1",
        model: "P1",
        bed_width_mm: 250,
        bed_depth_mm: 250,
        bed_height_mm: 250,
        margin_mm: 5,
        max_filament_slots: 1,
        loaded_filaments: [],
        integration_id: "int-1",
      },
    ]);
    return { exportsDir, repo };
  }

  it("keeps a queued send queued and marks a missing sending job as an explicit error", () => {
    const { exportsDir, repo } = fixture();
    const queuedPath = stage(exportsDir, "queued", "queued.gcode");
    const sendingPath = stage(exportsDir, "sending", "sending.gcode");
    const queued = enqueuePrinterSend(repo, {
      filename: "queued.gcode",
      artifact_path: queuedPath,
      printer_id: "p1",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    });
    const sending = enqueuePrinterSend(repo, {
      filename: "sending.gcode",
      artifact_path: sendingPath,
      printer_id: "p1",
      start: true,
      wait_for_idle: false,
      match: "pinned",
    });
    updatePrinterSendQueueItem(
      repo,
      sending!.id,
      { state: "sending", upload_job_id: "job-gone" },
      { requireState: "queued" },
    );

    const changed = reconcilePrinterSendsAfterProcessStart(repo);
    const stored = loadPrinterSendQueue(repo);

    expect(changed.map((item) => item.id)).toEqual([sending!.id]);
    const queuedRow = stored.find((item) => item.id === queued!.id);
    expect(queuedRow?.state).toBe("queued");
    expect(queuedRow?.error).toBeUndefined();
    expect(stored.find((item) => item.id === sending!.id)).toMatchObject({
      state: "error",
      upload_job_id: "job-gone",
      error: INTERRUPTED,
    });
    expect(stored.some((item) => item.state === "done")).toBe(false);
    expect(readFileSync(queuedPath)).toEqual(ORIGINAL);
    expect(readFileSync(sendingPath)).toEqual(ORIGINAL);
    expect(reconcilePrinterSendsAfterProcessStart(repo)).toEqual([]);
  });

  it("leaves a sending row alone while its printer-upload job is still in this process", async () => {
    const { repo } = fixture();
    const item = enqueuePrinterSend(repo, {
      filename: "plate.gcode",
      artifact_path: "/data/exports/printer-uploads/live/plate.gcode",
      printer_id: "p1",
      start: true,
      wait_for_idle: false,
      match: "pinned",
    });
    updatePrinterSendQueueItem(
      repo,
      item!.id,
      { state: "sending", upload_job_id: "job-live" },
      { requireState: "queued" },
    );

    const changed = await reconcilePrinterSendsAgainstLiveJobs(
      repo,
      async (jobId) => jobId === "job-live",
    );

    expect(changed).toEqual([]);
    expect(loadPrinterSendQueue(repo)[0]).toMatchObject({
      state: "sending",
      upload_job_id: "job-live",
    });
  });

  it("turns a 404 job read into an error row without reporting success", async () => {
    const { exportsDir, repo } = fixture();
    const path = stage(exportsDir, "missing", "plate.gcode");
    const item = enqueuePrinterSend(repo, {
      filename: "plate.gcode",
      artifact_path: path,
      printer_id: "p1",
      start: true,
      wait_for_idle: false,
      match: "pinned",
    });
    updatePrinterSendQueueItem(
      repo,
      item!.id,
      { state: "sending", upload_job_id: "job-missing" },
      { requireState: "queued" },
    );
    const jobs = createJobRunner(() => repo, exportsDir);
    const app = Fastify();
    app.decorateRequest("tenantId", "default");
    await registerJobRoutes(app, jobs, { statusExecution: new StatusExecution() });

    try {
      const response = await app.inject({ method: "GET", url: "/jobs/job-missing" });
      expect(response.statusCode).toBe(404);
      expect(response.json()).toEqual({ detail: "Job not found" });
      expect(loadPrinterSendQueue(repo)[0]).toMatchObject({
        state: "error",
        error: INTERRUPTED,
      });
      expect(readFileSync(path)).toEqual(ORIGINAL);
      expect(reconcileMissingPrinterUploadJob(repo, "job-missing")).toBeNull();
    } finally {
      await app.close();
    }
  });

  it("keeps a live sending row when the queue is read", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-send-live-"));
    dirs.push(dataDir);
    const repo = memoryRepo();
    let releaseUpload: (result: Record<string, unknown>) => void = () => undefined;
    runPrinterUploadJobMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          releaseUpload = resolve;
        }),
    );
    const jobs = createJobRunner(() => repo, dataDir);
    const path = stage(jobs.getExportsDir("default"), "live", "plate.gcode");
    const app = Fastify();
    app.decorateRequest("tenantId", "default");
    await registerPrinterSendQueueRoutes(app, {
      repo,
      jobs,
      integrations: { getStatus: async () => ({ state: "idle" }) } as never,
    });

    try {
      const item = enqueuePrinterSend(repo, {
        filename: "plate.gcode",
        artifact_path: path,
        printer_id: "p1",
        start: true,
        wait_for_idle: false,
        match: "pinned",
      });
      const jobId = await jobs.start(
        "printer-upload",
        { printer_id: "p1", artifact_path: path, filename: "plate.gcode", start: true },
        "default",
      );
      updatePrinterSendQueueItem(
        repo,
        item!.id,
        { state: "sending", upload_job_id: jobId },
        { requireState: "queued" },
      );

      const listed = await app.inject({ method: "GET", url: "/printer-send-queue" });
      expect(listed.json().items).toEqual([
        expect.objectContaining({ id: item!.id, state: "sending", upload_job_id: jobId }),
      ]);
    } finally {
      releaseUpload({ message: "Uploaded" });
      await app.close();
    }
  });

  it("does not dispatch an interrupted print, and an explicit retry reads the original bytes", async () => {
    fixture();
    const dataDir = mkdtempSync(join(tmpdir(), "pp-send-retry-"));
    dirs.push(dataDir);
    const repo = memoryRepo();
    const jobs = createJobRunner(() => repo, dataDir);
    const exportsDir = jobs.getExportsDir("default");
    const interruptedPath = stage(exportsDir, "interrupted", "print.gcode");
    const queuedPath = stage(exportsDir, "waiting", "wait.gcode");
    const interrupted = enqueuePrinterSend(repo, {
      filename: "print.gcode",
      artifact_path: interruptedPath,
      printer_id: "p1",
      start: true,
      wait_for_idle: false,
      match: "pinned",
    });
    enqueuePrinterSend(repo, {
      filename: "wait.gcode",
      artifact_path: queuedPath,
      printer_id: "p1",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    });
    updatePrinterSendQueueItem(
      repo,
      interrupted!.id,
      { state: "sending", upload_job_id: "job-gone" },
      { requireState: "queued" },
    );
    const seen: Array<{ path: string; bytes: Buffer; start: boolean }> = [];
    runPrinterUploadJobMock.mockImplementation(async (_repo, input) => {
      seen.push({
        path: input.artifact_path,
        bytes: readFileSync(input.artifact_path),
        start: input.start,
      });
      return { message: "Uploaded" };
    });
    const app = Fastify();
    app.decorateRequest("tenantId", "default");
    await registerPrinterSendQueueRoutes(app, {
      repo,
      jobs,
      integrations: { getStatus: async () => ({ state: "idle" }) } as never,
    });

    try {
      const listed = await app.inject({ method: "GET", url: "/printer-send-queue" });
      expect(listed.statusCode).toBe(200);
      const items = listed.json().items as Array<{ id: string; state: string; error?: string }>;
      expect(items.find((item) => item.id === interrupted!.id)).toMatchObject({
        state: "error",
        error: INTERRUPTED,
      });
      expect(items.find((item) => item.state === "queued")?.state).toBe("queued");
      expect(items.some((item) => item.state === "done")).toBe(false);

      const drained = await app.inject({
        method: "POST",
        url: "/printer-send-queue/drain",
        payload: {},
      });
      expect(drained.statusCode).toBe(200);
      const drainedJobId = (drained.json().results as Array<{ job_id?: string }>)[0]?.job_id;
      expect(drainedJobId).toEqual(expect.any(String));
      await jobs.waitForTerminal(drainedJobId ?? "", 3000, "default");
      expect(seen).toEqual([{ path: queuedPath, bytes: ORIGINAL, start: false }]);
      expect(loadPrinterSendQueue(repo).find((item) => item.id === interrupted!.id)?.state).toBe(
        "error",
      );
      expect(readFileSync(interruptedPath)).toEqual(ORIGINAL);

      seen.length = 0;
      const retried = await app.inject({
        method: "POST",
        url: `/printer-send-queue/${interrupted!.id}/dispatch`,
        payload: { force: true },
      });
      expect(retried.statusCode).toBe(200);
      await jobs.waitForTerminal(retried.json().job_id, 3000, "default");
      expect(seen).toEqual([{ path: interruptedPath, bytes: ORIGINAL, start: true }]);
    } finally {
      await app.close();
    }
  });
});
