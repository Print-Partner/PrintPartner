import { describe, expect, it } from "vitest";
import { parseVerifyBatchRequest, partFillBeds, plateBed, verifyBatchRequest } from "./checkoff-burst.js";
import type { PrinterCheckoffLink } from "./index.js";

const link = (id: string, state: PrinterCheckoffLink["state"], units: [number, number][]): PrinterCheckoffLink => ({
  id, profile_id: 1, integration_id: "sim-moonraker", printer_id: "sim-1", host_name: "Simulated Voron",
  filename: `${id}.gcode`, state, saw_active: false, created_at: "2020-01-01T00:00:00.000Z",
  units: units.map(([partId, unitIndex]) => ({ part_id: partId, unit_index: unitIndex })),
});
// The synthetic beds of fixtures/parity/printer-checkoff-verify, as GET /printer-checkoff returns them.
const links = [
  link("link-verify", "awaiting_verify", [[1, 1], [1, 2], [3, 0], [3, 1]]),
  link("link-burst", "awaiting_verify", [[2, 0], [3, 1]]),
  link("link-watching", "watching", [[1, 2]]),
];
const confirmed = (partId: number, unitIndex: number) => ({ part_id: partId, unit_index: unitIndex, result: "confirmed" });

describe("Checkoff burst contract", () => {
  it("maps P and F to the same request shape", () => {
    const plate = verifyBatchRequest("phone-burst-1", [plateBed(links[0]!)]);
    const fill = verifyBatchRequest("phone-burst-1", [1, 3].flatMap((part) => partFillBeds([links[0]!], part)));
    expect(fill).toEqual(plate);
    // The request rust/crates/pp-api/tests/checkoff_bursts_http.rs posts.
    expect(verifyBatchRequest("contract-burst", [plateBed(links[0]!), ...partFillBeds(links, 2)])).toEqual({
      idempotency_key: "contract-burst",
      beds: [
        { link_id: "link-verify", decisions: [confirmed(1, 1), confirmed(1, 2), confirmed(3, 0), confirmed(3, 1)] },
        { link_id: "link-burst", decisions: [confirmed(2, 0)] },
      ],
    });
  });

  it("only offers pending units on beds awaiting verify", () => {
    expect(plateBed(links[2]!).decisions).toEqual([]);
    expect(partFillBeds(links, 99)).toEqual([]);
    const resolved = { ...links[0]!, resolved_units: [{ part_id: 1, unit_index: 1, result: "confirmed" as const }] };
    expect(plateBed(resolved).decisions.map((d) => `${d.part_id}:${d.unit_index}`)).toEqual(["1:2", "3:0", "3:1"]);
  });

  it("refuses requests the Rust route refuses", () => {
    const bed = plateBed(links[0]!);
    expect(() => parseVerifyBatchRequest({ idempotency_key: " ", beds: [bed] })).toThrow();
    expect(() => parseVerifyBatchRequest({ idempotency_key: "k", beds: [] })).toThrow();
    expect(() => parseVerifyBatchRequest({ idempotency_key: "k", beds: [bed, bed] })).toThrow();
    expect(() => parseVerifyBatchRequest({ idempotency_key: "k", beds: Array(51).fill(bed) })).toThrow();
  });
});
