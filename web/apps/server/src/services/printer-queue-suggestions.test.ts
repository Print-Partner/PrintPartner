import { describe, expect, it, vi } from "vitest";
import type { PrinterSendQueueItem } from "@print-partner/contracts";
import type { PrinterMachine } from "@print-partner/domain";
import type { AppRepository } from "../db/repository.js";
import { computePrinterQueueSuggestions } from "./printer-queue-suggestions.js";

vi.mock("./printer-farm-match.js", () => ({
  wantedFilamentIdsForQueueItem: vi.fn(
    (_repo: unknown, item: PrinterSendQueueItem): Set<string> => {
      // Return filament ids embedded in the item's filename for test simplicity.
      // e.g. filename "red_blue.gcode" → Set<"red","blue">
      const base = item.filename.replace(/\.gcode$/, "");
      const parts = base.split("_").filter(Boolean);
      return new Set(parts.length ? parts : []);
    },
  ),
}));

function machine(
  partial: Partial<PrinterMachine> & Pick<PrinterMachine, "id" | "name">,
): PrinterMachine {
  return {
    model: partial.model ?? partial.name,
    bed_width_mm: 250,
    bed_depth_mm: 250,
    bed_height_mm: 250,
    margin_mm: 5,
    max_filament_slots: 1,
    loaded_filaments: [{ slot: 1, filament_color_id: null, label: "" }],
    integration_id: `int-${partial.id}`,
    ...partial,
  };
}

function fakeRepo(): AppRepository {
  return {} as unknown as AppRepository;
}

function queueItem(
  partial: Partial<PrinterSendQueueItem> &
    Pick<PrinterSendQueueItem, "id" | "printer_id" | "filename">,
): PrinterSendQueueItem {
  return {
    artifact_path: "/exports/x.gcode",
    match: "pinned",
    wait_for_idle: true,
    start: true,
    state: "queued",
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    ...partial,
  };
}

describe("computePrinterQueueSuggestions", () => {
  it("returns empty when no queue items", () => {
    const p = machine({ id: "p1", name: "Voron", integration_id: "int-p1" });
    const result = computePrinterQueueSuggestions(fakeRepo(), [p], new Set(["int-p1"]), []);
    expect(result).toEqual([]);
  });

  it("returns empty when no idle printers", () => {
    const p = machine({ id: "p1", name: "Voron", integration_id: "int-p1" });
    const item = queueItem({ id: "q1", printer_id: "p1", filename: "red.gcode" });
    const result = computePrinterQueueSuggestions(
      fakeRepo(),
      [p],
      new Set(), // no idle printers
      [item],
    );
    expect(result).toEqual([]);
  });

  it("does not suggest a legacy compatible item to another same-bed Printer", () => {
    const preferred = machine({ id: "p1", name: "Main", integration_id: "int-p1" });
    const idle = machine({ id: "p2", name: "Twin", integration_id: "int-p2" });
    const item = queueItem({
      id: "q1",
      printer_id: "p1",
      filename: "red.gcode",
      match: "compatible",
    });
    const result = computePrinterQueueSuggestions(
      fakeRepo(),
      [preferred, idle],
      new Set(["int-p2"]),
      [item],
    );
    expect(result).toEqual([]);
  });

  it("pinned item only matches exact printer", () => {
    const p1 = machine({ id: "p1", name: "Main", integration_id: "int-p1" });
    const p2 = machine({ id: "p2", name: "Twin", integration_id: "int-p2" });
    const item = queueItem({
      id: "q1",
      printer_id: "p1",
      filename: "red.gcode",
      match: "pinned",
    });
    // p2 is idle, but item is pinned to p1
    const result = computePrinterQueueSuggestions(fakeRepo(), [p1, p2], new Set(["int-p2"]), [
      item,
    ]);
    expect(result).toHaveLength(0);
  });

  it("pinned item matches when correct printer is idle", () => {
    const p1 = machine({ id: "p1", name: "Main", integration_id: "int-p1" });
    const item = queueItem({
      id: "q1",
      printer_id: "p1",
      filename: "red.gcode",
      match: "pinned",
    });
    const result = computePrinterQueueSuggestions(fakeRepo(), [p1], new Set(["int-p1"]), [item]);
    expect(result).toHaveLength(1);
    expect(result[0].printer_id).toBe("p1");
  });

  it("excludes items not in queued state", () => {
    const p = machine({ id: "p1", name: "Voron", integration_id: "int-p1" });
    const sending = queueItem({
      id: "q1",
      printer_id: "p1",
      filename: "red.gcode",
      state: "sending",
    });
    const done = queueItem({
      id: "q2",
      printer_id: "p1",
      filename: "blue.gcode",
      state: "done",
    });
    const result = computePrinterQueueSuggestions(fakeRepo(), [p], new Set(["int-p1"]), [
      sending,
      done,
    ]);
    expect(result).toHaveLength(0);
  });

  it("sorts items by filament overlap desc", () => {
    const printer = machine({
      id: "p1",
      name: "XL",
      integration_id: "int-p1",
      max_filament_slots: 2,
      loaded_filaments: [
        { slot: 1, filament_color_id: "red", label: "Red" },
        { slot: 2, filament_color_id: "blue", label: "Blue" },
      ],
    });
    const noOverlap = queueItem({
      id: "q1",
      printer_id: "p1",
      filename: "green.gcode",
    });
    const oneOverlap = queueItem({
      id: "q2",
      printer_id: "p1",
      filename: "red.gcode",
    });
    const twoOverlap = queueItem({
      id: "q3",
      printer_id: "p1",
      filename: "red_blue.gcode",
    });
    const result = computePrinterQueueSuggestions(fakeRepo(), [printer], new Set(["int-p1"]), [
      noOverlap,
      oneOverlap,
      twoOverlap,
    ]);
    expect(result).toHaveLength(1);
    const ids = result[0].items.map((i) => i.item_id);
    expect(ids).toEqual(["q3", "q2", "q1"]); // 2 overlap, 1 overlap, 0 overlap
  });

  it("suggests a legacy compatible item only when its exact Printer is idle", () => {
    const p = machine({ id: "p1", name: "Voron", integration_id: "int-p1" });
    const item = queueItem({
      id: "q1",
      printer_id: "p1",
      filename: "red.gcode",
      match: "compatible",
    });
    const result = computePrinterQueueSuggestions(fakeRepo(), [p], new Set(["int-p1"]), [item]);
    expect(result).toHaveLength(1);
    expect(result[0].printer_id).toBe("p1");
  });
});
