import { describe, expect, it } from "vitest";
import {
  currentPresetDetails,
  nullablePositiveMm,
  parsePrinterDetails,
  positiveMm,
  samePresetSnapshot,
} from "./printer-route-model.js";

const details = {
  name: " Core One ",
  model: "Prusa Core One",
  bed_width_mm: 250,
  bed_depth_mm: 220,
  bed_height_mm: 270,
  margin_mm: 5,
  max_filament_slots: 1,
  preset_id: " preset-core-one ",
};

describe("printer route model", () => {
  it("parses and trims printer details", () => {
    expect(parsePrinterDetails(details)).toEqual({
      name: "Core One",
      model: "Prusa Core One",
      bed_width_mm: 250,
      bed_depth_mm: 220,
      bed_height_mm: 270,
      margin_mm: 5,
      max_filament_slots: 1,
      preset_id: "preset-core-one",
    });
  });

  it("rejects invalid printer details", () => {
    expect(() => parsePrinterDetails(null)).toThrow("Printer details are required");
    expect(() => parsePrinterDetails({ ...details, name: "" })).toThrow("name is required");
    expect(() => parsePrinterDetails({ ...details, bed_width_mm: 0 })).toThrow(
      "bed_width_mm must be greater than 0",
    );
    expect(() => parsePrinterDetails({ ...details, margin_mm: -1 })).toThrow(
      "margin_mm must be 0 or greater",
    );
    expect(() => parsePrinterDetails({ ...details, max_filament_slots: 5 })).toThrow(
      "max_filament_slots must be an integer from 1 to 4",
    );
    expect(() => parsePrinterDetails({ ...details, preset_id: "" })).toThrow(
      "preset_id must be null or a non-empty string",
    );
  });

  it("parses optional positive dimensions", () => {
    expect(positiveMm(undefined, 250, "bed_width_mm")).toBe(250);
    expect(positiveMm(300, 250, "bed_width_mm")).toBe(300);
    expect(() => positiveMm(0, 250, "bed_width_mm")).toThrow("bed_width_mm must be greater than 0");

    expect(nullablePositiveMm(undefined, 270, "bed_height_mm")).toBe(270);
    expect(nullablePositiveMm(null, 270, "bed_height_mm")).toBeNull();
    expect(nullablePositiveMm(300, 270, "bed_height_mm")).toBe(300);
    expect(() => nullablePositiveMm(0, 270, "bed_height_mm")).toThrow(
      "bed_height_mm must be null or greater than 0",
    );
  });

  it("compares preset geometry snapshots", () => {
    const parsed = parsePrinterDetails(details);
    expect(samePresetSnapshot(parsed, parsed)).toBe(true);
    expect(samePresetSnapshot(parsed, { ...parsed, margin_mm: 6 })).toBe(false);
  });

  it("builds current preset details from a preset", () => {
    expect(
      currentPresetDetails(
        {
          id: "preset-core-one",
          name: "Core One",
          bed_width_mm: 250,
          bed_depth_mm: 220,
          bed_height_mm: 270,
          max_filament_slots: 1,
        },
        "Shop Core One",
      ),
    ).toMatchObject({
      name: "Shop Core One",
      model: "Core One",
      preset_id: "preset-core-one",
    });
  });
});
