import { describe, expect, it } from "vitest";
import type { PrinterSendQueueItem } from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";

import { wantedFilamentIdsForQueueItem } from "./printer-farm-match.js";

function fakeRepo(
  parts: Array<{ id: number; filament_color_id?: string | null }> = [],
): AppRepository {
  return {
    listParts: () => ({ parts, total: parts.length }),
  } as unknown as AppRepository;
}

function queueItem(partial: Partial<PrinterSendQueueItem> = {}): PrinterSendQueueItem {
  return {
    id: "q1",
    filename: "a.gcode",
    artifact_path: "/x",
    printer_id: "pref",
    match: "pinned",
    wait_for_idle: true,
    start: true,
    state: "queued",
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    ...partial,
  };
}

describe("printer-farm-match", () => {
  it("collects wanted filament ids from queued Checkoff units", () => {
    const repo = fakeRepo([
      { id: 1, filament_color_id: "red" },
      { id: 2, filament_color_id: "blue" },
      { id: 3, filament_color_id: null },
    ]);
    const ids = wantedFilamentIdsForQueueItem(
      repo,
      queueItem({
        profile_id: 9,
        checkoff_units: [
          { part_id: 1, unit_index: 0 },
          { part_id: 2, unit_index: 0 },
          { part_id: 3, unit_index: 0 },
        ],
      }),
    );
    expect([...ids].sort()).toEqual(["blue", "red"]);
  });
});
