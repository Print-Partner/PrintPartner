import { describe, expect, it } from "vitest";
import { parsePartSlug, parseQuantity, parseRole, parseStlPath } from "./parsers.js";
import {
  DEFAULT_NAMING_PROFILE,
  mergeNamingProfiles,
  namingProfileFromDict,
  validateNamingProfile,
} from "./stl-naming.js";

describe("parsers", () => {
  it("parses accent quantity", () => {
    const p = parseStlPath("parts/[a]_foo_x4.stl");
    expect(p.role).toBe("accent");
    expect(p.quantity).toBe(4);
    expect(p.partSlug.toLowerCase()).toContain("foo");
  });

  it("defaults primary quantity", () => {
    const p = parseStlPath("body/plate.stl");
    expect(p.role).toBe("primary");
    expect(p.quantity).toBe(1);
  });

  it("detects clear in path", () => {
    expect(parseStlPath("[c]/lens.stl").role).toBe("clear");
  });

  it("detects opaque marker", () => {
    expect(parseStlPath("shell/[o]_cover.stl").role).toBe("opaque");
  });

  it("parses space quantity variant", () => {
    expect(parseQuantity("widget x2.stl")).toBe(2);
  });

  it("parses role from filename only", () => {
    expect(parseRole("[a]bracket.stl")).toBe("accent");
  });

  it.each([
    ["[a]_belt_clip.stl", "belt_clip"],
    ["FlowChamber_BPM[a].stl", "FlowChamber_BPM"],
    ["FlowChamber_BPM_[a].stl", "FlowChamber_BPM"],
    ["Duct_[c]_Left.stl", "Duct_Left"],
    ["Duct-[o]-Left.stl", "Duct-Left"],
    ["Duct[A]Left.stl", "DuctLeft"],
    ["[o] Cover x2.stl", "Cover"],
    ["[a]_Bracket_x4.STL", "Bracket"],
  ])("strips role markers anywhere in %s", (filename, slug) => {
    expect(parsePartSlug(filename)).toBe(slug);
  });

  it.each([
    ["[a]_[a]_belt_clip.stl", "belt_clip"],
    ["belt_clip_[a]_[a].stl", "belt_clip"],
    ["[c][a]belt_clip.stl", "belt_clip"],
    ["belt_[a]_[o]_clip.stl", "belt_clip"],
  ])("strips a run of markers in %s without leaving a separator", (filename, slug) => {
    expect(parsePartSlug(filename)).toBe(slug);
    expect(parsePartSlug(filename)).toBe(parsePartSlug("belt_clip.stl"));
  });

  it("strips a letter or digit marker only as a prefix", () => {
    const profile = namingProfileFromDict(
      mergeNamingProfiles(validateNamingProfile(DEFAULT_NAMING_PROFILE), {
        roles: [{ id: "accent", markers: ["A"] }],
      }),
    );
    expect(parsePartSlug("gear.stl", profile)).toBe("gear");
    expect(parsePartSlug("Bracket_A.stl", profile)).toBe("Bracket_A");
    expect(parsePartSlug("A_gear.stl", profile)).toBe("gear");
    expect(parsePartSlug("Agear.stl", profile)).toBe("gear");
  });

  it("keeps the marker text when it is the whole name", () => {
    expect(parsePartSlug("[a].stl")).toBe("[a]");
  });

  it("reads .3mf names like .stl names", () => {
    const p = parseStlPath("STLs/Preset_Print_PlatesR1/[a]_Skirt_Duct_x2.3mf");
    expect(p).toEqual({
      role: "accent",
      quantity: 2,
      partSlug: "Skirt_Duct",
      filename: "[a]_Skirt_Duct_x2.3mf",
    });
    expect(parsePartSlug("Panel_Clip.3MF")).toBe("Panel_Clip");
  });

  it("keeps a saved quantity regex written for the .3mf name", () => {
    const profile = (regex: string) =>
      namingProfileFromDict(
        mergeNamingProfiles(validateNamingProfile(DEFAULT_NAMING_PROFILE), {
          quantity: { regex, default: 1 },
        }),
      );
    const threeMf = profile("_x([0-9]+)\\.3mf$");
    expect(parseStlPath("Duct_x2.3mf", threeMf)).toMatchObject({ quantity: 2, partSlug: "Duct" });
    // The regex does not match an .stl name, so the slug keeps its suffix, as before.
    expect(parseStlPath("Duct_x2.stl", threeMf)).toMatchObject({ quantity: 1, partSlug: "Duct_x2" });

    // A regex for any name matches the .3mf name as written.
    const anyName = profile("_qty([0-9]+)");
    expect(parseQuantity("Duct_qty3.3mf", anyName)).toBe(3);

    // A regex that matches the .3mf name without a number gives the default, as before.
    const optional = profile("(?:_x([0-9]+))?\\.3mf$");
    expect(parseQuantity("Duct.3mf", optional)).toBe(1);
  });
});

// File paths copied from VoronDesign/voron-chariot and VoronDesign/voron-downdraft (created
// 2026-10-24, reported Nov 8). Chariot uses snake_case; DownDraft uses Title_Case and nests
// parts two or three folders deep.
describe("Voron repositories", () => {
  it.each([
    ["STLs/[a]_belt_clip.stl", "accent", 1, "belt_clip"],
    ["STLs/[a]_belt_clip_adhesion_helper.stl", "accent", 1, "belt_clip_adhesion_helper"],
    ["STLs/probe_mounts/[a]_probe_mount_klicky.stl", "accent", 1, "probe_mount_klicky"],
    ["STLs/carriage_clamps/carriage_clamp_00i.stl", "primary", 1, "carriage_clamp_00i"],
    ["STLs/front_plate.stl", "primary", 1, "front_plate"],
  ])("voron-chariot %s", (path, role, quantity, partSlug) => {
    expect(parseStlPath(path)).toMatchObject({ role, quantity, partSlug });
  });

  it.each([
    ["STLs/Dual_4010/[a]_4010_Front_Mount_High.stl", "accent", 1, "4010_Front_Mount_High"],
    ["STLs/Dual_5015/5015_Fan_Spring_x2.stl", "primary", 2, "5015_Fan_Spring"],
    ["STLs/Common_Parts/Hotend_Mounts/Dragon_Ace_Mount_MZE.stl", "primary", 1, "Dragon_Ace_Mount_MZE"],
    ["STLs/Common_Parts/PCB_And_Chain_Mounts/PCB_10mm_Spacer_x2.stl", "primary", 2, "PCB_10mm_Spacer"],
    ["STLs/Common_Parts/LED_Mounts/Logo_LED/[c]_LED_Diffuser.stl", "clear", 1, "LED_Diffuser"],
    ["STLs/Common_Parts/LED_Mounts/Logo_LED/[o]_Neopixel_Carrier.stl", "opaque", 1, "Neopixel_Carrier"],
    [
      "STLs/Extras/Front_Mount_Cutter_Models/[a]_4010_Front_Mount_High_Brick.3mf",
      "accent",
      1,
      "4010_Front_Mount_High_Brick",
    ],
  ])("voron-downdraft %s", (path, role, quantity, partSlug) => {
    expect(parseStlPath(path)).toMatchObject({ role, quantity, partSlug });
  });
});
