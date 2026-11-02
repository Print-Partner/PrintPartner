import { describe, expect, it } from "vitest";
import type { PrinterCheckoffLink, PrinterHostStatus } from "@print-partner/contracts";
import {
  decideCheckoffReconcile,
  normalizePrinterFilename,
  parseCheckoffUnits,
  pendingCheckoffUnits,
  printerFilenamesMatch,
} from "./printer-checkoff.js";
import {
  createPrinterCheckoffLink,
  loadPrinterCheckoffLinks,
  trimPrinterCheckoffLinks,
} from "./printer-checkoff-store.js";
import type { AppRepository } from "../db/repository.js";

function link(overrides: Partial<PrinterCheckoffLink> = {}): PrinterCheckoffLink {
  return {
    id: "link-1",
    profile_id: 1,
    integration_id: "int-1",
    printer_id: "printer-1",
    host_name: "Trident",
    filename: "frame_x.gcode",
    units: [{ part_id: 10, unit_index: 0 }],
    state: "watching",
    saw_active: false,
    created_at: new Date().toISOString(),
    ...overrides,
  };
}

function status(overrides: Partial<PrinterHostStatus> = {}): PrinterHostStatus {
  return { state: "idle", ...overrides };
}

function settingsRepo(): AppRepository {
  const settings = new Map<string, string>();
  return {
    getSetting: (key: string) => settings.get(key) ?? null,
    setSetting: (key: string, value: string) => {
      settings.set(key, value);
    },
    transaction: <T>(run: () => T) => run(),
  } as unknown as AppRepository;
}

describe("printer-checkoff helpers", () => {
  it("normalizes filenames for matching", () => {
    expect(normalizePrinterFilename("gcodes/Frame_X.gcode")).toBe("frame_x.gcode");
    expect(printerFilenamesMatch("gcodes/a.gcode", "A.gcode")).toBe(true);
    expect(printerFilenamesMatch("a.gcode", "b.gcode")).toBe(false);
  });

  it("parses checkoff unit JSON", () => {
    expect(
      parseCheckoffUnits('[{"part_id":1,"unit_index":0},{"part_id":1,"unit_index":0}]'),
    ).toEqual([{ part_id: 1, unit_index: 0 }]);
    expect(parseCheckoffUnits("not-json")).toEqual([]);
  });

  it("keeps Object-name evidence through parsing and link persistence", () => {
    const units = parseCheckoffUnits(
      '[{"part_id":11,"unit_index":0,"object_name":"  bracket_01.stl  "},{"part_id":11,"unit_index":1}]',
    );
    expect(units).toEqual([
      { part_id: 11, unit_index: 0, object_name: "bracket_01.stl" },
      { part_id: 11, unit_index: 1 },
    ]);

    const repo = settingsRepo();
    createPrinterCheckoffLink(repo, {
      profile_id: 7,
      integration_id: "moonraker",
      printer_id: "trident",
      host_name: "Trident",
      filename: "plate.gcode",
      units,
    });

    expect(loadPrinterCheckoffLinks(repo)[0]?.units).toEqual(units);
  });

  it("computes pending units after partial resolve", () => {
    expect(
      pendingCheckoffUnits(
        link({
          units: [
            { part_id: 1, unit_index: 0 },
            { part_id: 1, unit_index: 1 },
          ],
          resolved_units: [{ part_id: 1, unit_index: 0, result: "confirmed" }],
        }),
      ),
    ).toEqual([{ part_id: 1, unit_index: 1 }]);
  });

  it("trims terminal links before active ones", () => {
    const watching = link({ id: "w1", state: "watching" });
    const done = Array.from({ length: 5 }, (_, i) =>
      link({ id: `d${i}`, state: "verified", filename: `f${i}.gcode` }),
    );
    // Force tiny cap by calling with overflow: keep watching + last verifieds
    const trimmed = trimPrinterCheckoffLinks([...done, watching]);
    expect(trimmed.some((l) => l.id === "w1")).toBe(true);
  });
  it("retains imported extras after ordinary terminal history is trimmed", () => {
    const saved = link({
      id: "inventory",
      state: "verified",
      imported_inventory: {
        extras: [
          {
            name: "extra.stl",
            kind: "object",
            checkoff: { result: "confirmed", checked_at: "2026-09-05" },
          },
        ],
      },
    });
    const history = Array.from({ length: 210 }, (_, i) =>
      link({ id: `new-${i}`, state: "verified" }),
    );
    expect(
      trimPrinterCheckoffLinks([saved, ...history]).find((item) => item.id === "inventory"),
    ).toEqual(saved);
  });
});

describe("decideCheckoffReconcile", () => {
  it("marks active while printing matching filename", () => {
    expect(
      decideCheckoffReconcile(
        link(),
        status({ state: "printing", filename: "frame_x.gcode", progress: 40 }),
      ),
    ).toEqual({ action: "mark_active", progress: 40 });
  });

  it("awaits verify on complete when filename matches", () => {
    expect(
      decideCheckoffReconcile(
        link({ saw_active: true }),
        status({ state: "complete", filename: "frame_x.gcode" }),
      ),
    ).toEqual({ action: "await_verify" });
    expect(
      decideCheckoffReconcile(link(), status({ state: "complete", filename: "frame_x.gcode" })),
    ).toEqual({ action: "await_verify" });
  });

  it("does not await verify for a different filename after saw_active", () => {
    expect(
      decideCheckoffReconcile(
        link({ saw_active: true }),
        status({ state: "complete", filename: "other.gcode" }),
      ),
    ).toEqual({ action: "noop" });
  });

  it("awaits verify with cleared filename after saw_active or started", () => {
    expect(
      decideCheckoffReconcile(link({ saw_active: true }), status({ state: "complete" })),
    ).toEqual({ action: "await_verify" });
    expect(decideCheckoffReconcile(link({ started: true }), status({ state: "complete" }))).toEqual(
      { action: "await_verify" },
    );
  });

  it("does not await verify on idle/cancel/error without near-done progress", () => {
    expect(
      decideCheckoffReconcile(
        link({ saw_active: true, last_progress: 42 }),
        status({ state: "idle" }),
      ),
    ).toEqual({ action: "host_failed", reason: "cancelled" });
    expect(decideCheckoffReconcile(link({ saw_active: true }), status({ state: "error" }))).toEqual(
      { action: "host_failed", reason: "error" },
    );
  });

  it("awaits verify on idle when last progress was essentially complete", () => {
    expect(
      decideCheckoffReconcile(
        link({ saw_active: true, last_progress: 100 }),
        status({ state: "idle" }),
      ),
    ).toEqual({ action: "await_verify" });
  });

  it("ignores idle before the job was observed", () => {
    expect(decideCheckoffReconcile(link(), status({ state: "idle" }))).toEqual({
      action: "noop",
    });
  });

  it("ignores already verified links", () => {
    expect(
      decideCheckoffReconcile(
        link({ state: "verified" }),
        status({ state: "complete", filename: "frame_x.gcode" }),
      ),
    ).toEqual({ action: "noop" });
  });
});
