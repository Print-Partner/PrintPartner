import { describe, expect, it } from "vitest";
import { join } from "node:path";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import type { PrinterSendQueueItem } from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import {
  assertPrinterUploadArtifactPath,
  enqueuePrinterSend,
  loadPrinterSendQueue,
  migratePrinterSendQueueArtifactPaths,
  trimPrinterSendQueue,
} from "./printer-send-queue-store.js";

function item(id: string, state: PrinterSendQueueItem["state"]): PrinterSendQueueItem {
  return {
    id,
    filename: `${id}.gcode`,
    artifact_path: `/data/exports/printer-uploads/${id}/a.gcode`,
    printer_id: "p1",
    wait_for_idle: true,
    start: true,
    state,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
  };
}

describe("assertPrinterUploadArtifactPath", () => {
  const exportsDir = "/data/exports";

  it("accepts paths under printer-uploads", () => {
    const path = join(exportsDir, "printer-uploads", "abc", "a.gcode");
    expect(assertPrinterUploadArtifactPath(exportsDir, path)).toBe(path);
  });

  it("rejects path escape", () => {
    expect(() =>
      assertPrinterUploadArtifactPath(
        exportsDir,
        join(exportsDir, "printer-uploads", "..", "secrets.txt"),
      ),
    ).toThrow(/Invalid artifact path/);
  });
});

describe("trimPrinterSendQueue", () => {
  it("never drops active items even when over MAX_ITEMS", () => {
    const active = Array.from({ length: 55 }, (_, i) => item(`a${i}`, "queued"));
    const terminal = Array.from({ length: 10 }, (_, i) => item(`t${i}`, "done"));
    const trimmed = trimPrinterSendQueue([...active, ...terminal]);
    expect(trimmed.filter((i) => i.state === "queued")).toHaveLength(55);
    expect(trimmed.some((i) => i.state === "done")).toBe(false);
  });

  it("preserves error items as active/retryable", () => {
    const errors = Array.from({ length: 52 }, (_, i) => item(`e${i}`, "error"));
    const done = Array.from({ length: 5 }, (_, i) => item(`d${i}`, "done"));
    const trimmed = trimPrinterSendQueue([...errors, ...done]);
    expect(trimmed.filter((i) => i.state === "error")).toHaveLength(52);
    expect(trimmed.some((i) => i.state === "done")).toBe(false);
  });

  it("keeps newest terminal history within remaining capacity", () => {
    const active = [item("a1", "sending")];
    const terminal = Array.from({ length: 60 }, (_, i) => item(`t${i}`, "done"));
    const trimmed = trimPrinterSendQueue([...active, ...terminal]);
    expect(trimmed).toHaveLength(50);
    expect(trimmed[0]?.id).toBe("a1");
    expect(trimmed.at(-1)?.id).toBe("t59");
  });
});

describe("migratePrinterSendQueueArtifactPaths", () => {
  it("does not migrate a queue path to a symlink that escapes the tenant export root", () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-queue-migration-"));
    try {
      const legacyRoot = join(dir, "exports");
      const tenantRoot = join(legacyRoot, "tenant-default");
      const legacyArtifact = join(legacyRoot, "printer-uploads", "queued", "plate.gcode");
      const targetDir = join(tenantRoot, "printer-uploads", "queued");
      const targetArtifact = join(targetDir, "plate.gcode");
      const outside = join(dir, "outside.gcode");
      mkdirSync(targetDir, { recursive: true });
      writeFileSync(outside, "outside");
      symlinkSync(outside, targetArtifact);
      const repo = fakeRepo();
      enqueuePrinterSend(repo, {
        filename: "plate.gcode",
        artifact_path: legacyArtifact,
        printer_id: "p1",
        start: false,
      });

      expect(migratePrinterSendQueueArtifactPaths(repo, legacyRoot, tenantRoot)).toBe(0);
      expect(loadPrinterSendQueue(repo)[0]?.artifact_path).toBe(legacyArtifact);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("queued Progress evidence", () => {
  it("round-trips the Object name used to map each queued unit", () => {
    const repo = fakeRepo();
    const queued = enqueuePrinterSend(repo, {
      filename: "plate.gcode",
      artifact_path: "/data/exports/printer-uploads/x/plate.gcode",
      printer_id: "p1",
      start: false,
      profile_id: 7,
      checkoff_units: [
        { part_id: 11, unit_index: 0, object_name: "  bracket_01.stl  " },
        { part_id: 11, unit_index: 1 },
      ],
    });

    expect(queued?.checkoff_units).toEqual([
      { part_id: 11, unit_index: 0, object_name: "bracket_01.stl" },
      { part_id: 11, unit_index: 1 },
    ]);
    expect(loadPrinterSendQueue(repo)[0]?.checkoff_units).toEqual(queued?.checkoff_units);
  });
});

function fakeRepo(): AppRepository {
  const settings = new Map<string, string>();
  return {
    getSetting: (key: string) => settings.get(key) ?? null,
    setSetting: (key: string, value: string) => {
      settings.set(key, value);
    },
    transaction: <T>(fn: () => T) => fn(),
  } as unknown as AppRepository;
}
