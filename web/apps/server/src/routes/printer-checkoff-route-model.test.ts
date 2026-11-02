import type { PrinterCheckoffLink } from "@print-partner/contracts";
import { describe, expect, it } from "vitest";
import type { UnattributedPrint } from "../services/unattributed-print-store.js";
import {
  filterLinkedUnattributedPrints,
  linkedCheckoffLinks,
  printMatchesLink,
} from "./printer-checkoff-route-model.js";

function link(overrides: Partial<PrinterCheckoffLink>): PrinterCheckoffLink {
  return {
    id: "link-1",
    profile_id: 1,
    printer_id: "printer-1",
    integration_id: "host-1",
    host_name: "Printer 1",
    filename: "plate.gcode",
    state: "watching",
    saw_active: false,
    created_at: "2026-08-26T00:00:00.000Z",
    units: [],
    ...overrides,
  };
}

function print(overrides: Partial<UnattributedPrint>): UnattributedPrint {
  return {
    id: "print-1",
    integration_id: "host-1",
    printer_id: "printer-1",
    host_name: "Printer 1",
    filename: "plate.gcode",
    completed_at: "2026-08-26T00:00:00.000Z",
    gcode_objects: [],
    candidates: [],
    ...overrides,
  };
}

describe("printer-checkoff route model", () => {
  it("keeps only link states that can explain a print", () => {
    expect(
      linkedCheckoffLinks([
        link({ id: "watching", state: "watching" }),
        link({ id: "awaiting", state: "awaiting_verify" }),
        link({ id: "verified", state: "verified" }),
        link({ id: "dismissed", state: "dismissed" }),
      ]).map((row) => row.id),
    ).toEqual(["watching", "awaiting", "verified"]);
  });

  it("matches filenames after printer normalization", () => {
    expect(
      printMatchesLink(print({ filename: "Plate_1.gcode" }), link({ filename: "plate_1.gcode" })),
    ).toBe(true);
  });

  it("filters unattributed prints that already have an eligible link", () => {
    const visible = filterLinkedUnattributedPrints(
      [
        print({ id: "matched", filename: "plate.gcode" }),
        print({ id: "other", filename: "other.gcode" }),
        print({ id: "other-host", integration_id: "host-2" }),
      ],
      [link({ filename: "plate.gcode" })],
      "host-1",
    );

    expect(visible.map((row) => row.id)).toEqual(["other"]);
  });
});
