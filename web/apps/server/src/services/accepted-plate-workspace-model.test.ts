import { describe, expect, it } from "vitest";
import type { PrinterMachine } from "@print-partner/domain";
import {
  acceptedPrinter,
  basisContract,
  compareUtf8,
  millimetresToMicrometres,
  sameAcceptedPlanBasis,
} from "./accepted-plate-workspace-model.js";
import type { AcceptedPlanBasis } from "../db/accepted-plan-progress.js";

const basis = {
  profileId: 1,
  planVersion: 2,
  revisionId: 3,
  revisionDigest: "a".repeat(64),
  requiredUnitMappingDigest: "b".repeat(64),
} satisfies AcceptedPlanBasis;

const printer = {
  id: " printer-1 ",
  name: " MK4 ",
  model: " Prusa MK4 ",
  bed_width_mm: 250,
  bed_depth_mm: 210,
  bed_height_mm: 220,
  margin_mm: 5,
  max_filament_slots: 1,
  preset_id: null,
  integration_id: null,
  device_id: null,
  loaded_filaments: [],
  preferred_slicer: null,
} satisfies PrinterMachine;

describe("accepted plate workspace model", () => {
  it("maps accepted plan basis to the contract shape", () => {
    expect(basisContract(basis)).toEqual({
      profile_id: 1,
      plan_version: 2,
      plan_revision_id: 3,
      plan_revision_digest: "a".repeat(64),
      required_unit_mapping_digest: "b".repeat(64),
    });
  });

  it("compares accepted plan basis fields", () => {
    expect(sameAcceptedPlanBasis(basis, { ...basis })).toBe(true);
    expect(sameAcceptedPlanBasis(basis, { ...basis, planVersion: 99 })).toBe(false);
    expect(
      sameAcceptedPlanBasis(basis, {
        ...basis,
        requiredUnitMappingDigest: "c".repeat(64),
      }),
    ).toBe(false);
  });

  it("converts millimetres to micrometres only when safe", () => {
    expect(millimetresToMicrometres(12.345)).toBe(12_345);
    expect(millimetresToMicrometres(Number.MAX_SAFE_INTEGER)).toBeNull();
  });

  it("adapts valid printer machines to accepted plate printer geometry", () => {
    expect(acceptedPrinter(printer)).toEqual({
      id: "printer-1",
      name: "MK4",
      model: "Prusa MK4",
      bed_width_um: 250_000,
      bed_depth_um: 210_000,
      bed_height_um: 220_000,
      margin_um: 5_000,
    });
  });

  it("rejects invalid printer geometry", () => {
    expect(acceptedPrinter({ ...printer, id: " " })).toBeNull();
    expect(acceptedPrinter({ ...printer, bed_height_mm: null })).toBeNull();
    expect(acceptedPrinter({ ...printer, margin_mm: 130 })).toBeNull();
  });

  it("sorts identifiers by UTF-8 byte order", () => {
    expect(["b", "ä", "a"].sort(compareUtf8)).toEqual(["a", "b", "ä"]);
  });
});
