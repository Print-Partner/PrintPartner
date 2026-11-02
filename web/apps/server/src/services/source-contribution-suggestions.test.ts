import { describe, expect, it } from "vitest";
import { suggestSourceContributions } from "./source-contribution-suggestions.js";

describe("Source contribution suggestions", () => {
  it("discovers Build-scoped slots without domain aliases", () => {
    const suggestions = suggestSourceContributions({
      evidenceId: "nerf-mod",
      sourceName: "Caliburn mods",
      printablePaths: [
        "models/flywheel_cage/cage_left.stl",
        "models/flywheel_cage/cage_right.stl",
        "models/magazine_system/magwell.stl",
      ],
      knownSlots: ["toolhead", "extruder", "probe"],
    });

    expect(suggestions).toEqual([
      expect.objectContaining({
        slot: "flywheel_cage",
        path_scopes: ["models/flywheel_cage/**"],
        confidence: "medium",
      }),
      expect.objectContaining({
        slot: "magazine_system",
        path_scopes: ["models/magazine_system/**"],
        confidence: "medium",
      }),
    ]);
  });

  it("reuses known slots when the path names the slot's function", () => {
    const suggestions = suggestSourceContributions({
      evidenceId: "printer-mod",
      sourceName: "Toolhead mods",
      printablePaths: [
        "STLs/toolhead/hotend_mount/front.stl",
        "STLs/toolhead/hotend_mount/rear.stl",
      ],
      knownSlots: ["toolhead", "hotend", "extruder"],
    });

    expect(suggestions).toEqual([
      expect.objectContaining({
        slot: "hotend",
        confidence: "high",
        path_scopes: ["STLs/toolhead/hotend_mount/**"],
      }),
    ]);
  });

  it("matches product names only when the catalog supplies them", () => {
    const paths = [
      "STLs/Stealthburner/Rapido_UHF/front.stl",
      "STLs/Stealthburner/Rapido_UHF/rear.stl",
    ];
    const knownSlots = ["toolhead", "hotend", "extruder"];

    // No catalog aliases: fall back to the first meaningful folder name rather
    // than guessing a functional slot from a product name.
    expect(
      suggestSourceContributions({
        evidenceId: "printer-mod",
        sourceName: "Toolhead mods",
        printablePaths: paths,
        knownSlots,
      }),
    ).toEqual([expect.objectContaining({ slot: "stealthburner", confidence: "medium" })]);

    // With the catalog naming that product for a slot, it resolves.
    expect(
      suggestSourceContributions({
        evidenceId: "printer-mod",
        sourceName: "Toolhead mods",
        printablePaths: paths,
        knownSlots,
        slotAliases: new Map([["hotend", ["rapido"]]]),
      }),
    ).toEqual([
      expect.objectContaining({
        slot: "hotend",
        confidence: "high",
        path_scopes: ["STLs/Stealthburner/Rapido_UHF/**"],
      }),
    ]);
  });
});
