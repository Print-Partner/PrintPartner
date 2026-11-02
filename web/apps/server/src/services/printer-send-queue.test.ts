import { mkdirSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppRepository } from "../db/repository.js";

vi.mock("../integrations/store.js", () => ({
  getIntegrationConfig: vi.fn(),
}));
vi.mock("./printer-fleet.js", () => ({
  loadFleet: vi.fn(),
}));

import { getIntegrationConfig } from "../integrations/store.js";
import { loadFleet } from "./printer-fleet.js";
import {
  DRAIN_ITEM_CAP,
  dispatchPrinterSendQueueItem,
  drainPrinterSendQueue,
} from "./printer-send-queue.js";
import { enqueuePrinterSend, loadPrinterSendQueue } from "./printer-send-queue-store.js";

const getIntegrationConfigMock = vi.mocked(getIntegrationConfig);
const loadFleetMock = vi.mocked(loadFleet);

function memoryRepo(): AppRepository {
  const settings = new Map<string, string>();
  return {
    getSetting: (k: string) => settings.get(k) ?? null,
    setSetting: (k: string, v: string) => {
      settings.set(k, v);
    },
    transaction: <T>(fn: () => T) => fn(),
  } as unknown as AppRepository;
}

describe("printer-send-queue dispatch/drain", () => {
  let exportsDir: string;
  let repo: AppRepository;

  beforeEach(() => {
    exportsDir = mkdtempSync(join(tmpdir(), "pp-send-q-"));
    repo = memoryRepo();
    getIntegrationConfigMock.mockReset();
    loadFleetMock.mockReset();
    getIntegrationConfigMock.mockImplementation((_r, id) =>
      id
        ? ({
            id,
            type: "moonraker",
            name: "Host",
            config: { base_url: "http://127.0.0.1:7125" },
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
  });

  afterEach(() => {
    rmSync(exportsDir, { recursive: true, force: true });
  });

  function stageArtifact(id: string): string {
    const dir = join(exportsDir, "printer-uploads", id);
    mkdirSync(dir, { recursive: true });
    const path = join(dir, "a.gcode");
    writeFileSync(path, ";gcode");
    return path;
  }

  it("treats preferred-host getStatus rejection as 409 (does not throw)", async () => {
    const path = stageArtifact("art1");
    const item = enqueuePrinterSend(repo, {
      filename: "a.gcode",
      artifact_path: path,
      printer_id: "p1",
      start: false,
      wait_for_idle: true,
      match: "pinned",
    });
    expect(item).toBeTruthy();

    const result = await dispatchPrinterSendQueueItem(repo, exportsDir, item!.id, {
      startJob: async () => "job-1",
      getStatus: async () => {
        throw new Error("host unreachable");
      },
    });
    expect("error" in result).toBe(true);
    if ("error" in result) {
      expect(result.status).toBe(409);
      expect(result.error).toMatch(/unreachable/i);
    }
  });

  it("bounds drain examination to DRAIN_ITEM_CAP", async () => {
    for (let i = 0; i < DRAIN_ITEM_CAP + 3; i++) {
      enqueuePrinterSend(repo, {
        filename: `a${i}.gcode`,
        artifact_path: stageArtifact(`art-${i}`),
        printer_id: "p1",
        start: false,
        wait_for_idle: true,
        match: "pinned",
      });
    }
    expect(loadPrinterSendQueue(repo).filter((i) => i.state === "queued")).toHaveLength(
      DRAIN_ITEM_CAP + 3,
    );

    let statusCalls = 0;
    await drainPrinterSendQueue(repo, exportsDir, {
      startJob: async () => "job",
      getStatus: async () => {
        statusCalls += 1;
        return { state: "printing" };
      },
    });

    // All examined items wait_for_idle → getStatus once each, capped.
    expect(statusCalls).toBe(DRAIN_ITEM_CAP);
  });

  it("continues drain after a failed startJob on another printer", async () => {
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
      {
        id: "p2",
        name: "P2",
        model: "P2",
        bed_width_mm: 250,
        bed_depth_mm: 250,
        bed_height_mm: 250,
        margin_mm: 5,
        max_filament_slots: 1,
        loaded_filaments: [],
        integration_id: "int-2",
      },
    ]);

    const a = enqueuePrinterSend(repo, {
      filename: "a.gcode",
      artifact_path: stageArtifact("a"),
      printer_id: "p1",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    })!;
    const b = enqueuePrinterSend(repo, {
      filename: "b.gcode",
      artifact_path: stageArtifact("b"),
      printer_id: "p2",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    })!;

    const results = await drainPrinterSendQueue(repo, exportsDir, {
      startJob: async (payload) => {
        if (payload.printer_id === "p1") throw new Error("boom");
        return "job-ok";
      },
      getStatus: async () => ({ state: "idle" }),
    });

    expect(results.some((r) => r.item_id === a.id && r.error)).toBe(true);
    expect(results.some((r) => r.item_id === b.id && r.job_id === "job-ok")).toBe(true);
  });

  it("scopes a drain to the requested queued Printer", async () => {
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
      {
        id: "p2",
        name: "P2",
        model: "P2",
        bed_width_mm: 250,
        bed_depth_mm: 250,
        bed_height_mm: 250,
        margin_mm: 5,
        max_filament_slots: 1,
        loaded_filaments: [],
        integration_id: "int-2",
      },
    ]);
    const first = enqueuePrinterSend(repo, {
      filename: "first.gcode",
      artifact_path: stageArtifact("scoped-first"),
      printer_id: "p1",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    })!;
    const second = enqueuePrinterSend(repo, {
      filename: "second.gcode",
      artifact_path: stageArtifact("scoped-second"),
      printer_id: "p2",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    })!;
    const started: string[] = [];

    const results = await drainPrinterSendQueue(
      repo,
      exportsDir,
      {
        startJob: async (payload) => {
          started.push(payload.printer_id);
          return "job-scoped";
        },
        getStatus: async () => ({ state: "idle" }),
      },
      { printerId: "p1" },
    );

    expect(results).toEqual([{ item_id: first.id, job_id: "job-scoped" }]);
    expect(started).toEqual(["p1"]);
    expect(loadPrinterSendQueue(repo).find((item) => item.id === second.id)?.state).toBe("queued");
  });

  it("does not send a sliced file to a same-bed twin when the queued Printer is busy", async () => {
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
      {
        id: "p2",
        name: "P2",
        model: "P2",
        bed_width_mm: 250,
        bed_depth_mm: 250,
        bed_height_mm: 250,
        margin_mm: 5,
        max_filament_slots: 1,
        loaded_filaments: [],
        integration_id: "int-2",
      },
    ]);
    const item = enqueuePrinterSend(repo, {
      filename: "a.gcode",
      artifact_path: stageArtifact("twin"),
      printer_id: "p1",
      start: false,
      wait_for_idle: true,
      match: "compatible",
    })!;
    const started: string[] = [];

    const result = await dispatchPrinterSendQueueItem(repo, exportsDir, item.id, {
      startJob: async (payload) => {
        started.push(payload.printer_id);
        return "job-twin";
      },
      getStatus: async (integrationId) => ({
        state: integrationId === "int-1" ? "printing" : "idle",
      }),
    });

    expect("error" in result).toBe(true);
    if ("error" in result) expect(result.status).toBe(409);
    expect(started).toEqual([]);
    expect(loadPrinterSendQueue(repo).find((row) => row.id === item.id)?.printer_id).toBe("p1");
  });

  it("refuses to claim a second item while another send for that printer is already sending", async () => {
    const first = enqueuePrinterSend(repo, {
      filename: "first.gcode",
      artifact_path: stageArtifact("race-first"),
      printer_id: "p1",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    })!;
    const second = enqueuePrinterSend(repo, {
      filename: "second.gcode",
      artifact_path: stageArtifact("race-second"),
      printer_id: "p1",
      start: false,
      wait_for_idle: false,
      match: "pinned",
    })!;

    const firstResult = await dispatchPrinterSendQueueItem(repo, exportsDir, first.id, {
      startJob: async () => "job-first",
      getStatus: async () => ({ state: "idle" }),
    });
    expect("job_id" in firstResult).toBe(true);

    const started: string[] = [];
    const secondResult = await dispatchPrinterSendQueueItem(repo, exportsDir, second.id, {
      startJob: async (payload) => {
        started.push(payload.printer_id);
        return "job-second";
      },
      getStatus: async () => ({ state: "idle" }),
    });

    expect("error" in secondResult).toBe(true);
    if ("error" in secondResult) expect(secondResult.status).toBe(409);
    expect(started).toEqual([]);
    expect(loadPrinterSendQueue(repo).find((row) => row.id === second.id)?.state).toBe("queued");
  });
});
