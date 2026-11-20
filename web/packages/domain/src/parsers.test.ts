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

  it.each([
    ["STLs/Panel_[a[c]]_Clip.stl", "Panel_Clip"],
    ["[o[a[c]]]_Panel.stl", "Panel"],
  ])("strips a marker that removing a nested marker exposes in %s", (path, slug) => {
    expect(parsePartSlug(path)).toBe(slug);
  });

  // The same matrix is asserted against the Rust naming preview in catalog_operations.rs.
  it.each([
    ["[o]-Panel Clip.STL", "opaque", "Panel Clip"],
    ["Panel-[c]-Clip.stl", "clear", "Panel-Clip"],
    ["Panel Clip_[a].stl", "accent", "Panel Clip"],
    ["Panel_[a[c]]_Clip.stl", "clear", "Panel_Clip"],
    ["[[a]].stl", "accent", "[]"],
    ["Panel_[a][c]_Clip.stl", "accent", "Panel_Clip"],
    ["[a][c].stl", "accent", "[a][c]"],
    ["[o[a[c]]]_Panel.stl", "clear", "Panel"],
  ])("reads %s as role %s and slug %s", (filename, role, partSlug) => {
    expect(parseStlPath(filename)).toEqual({ role, quantity: 1, partSlug, filename });
  });

  it("strips markers from a 30,000-character repeated-nesting name within 2 seconds", () => {
    const filename = `Panel_${"[a[c]]_".repeat(4285)}Clip.stl`;
    const started = performance.now();
    expect(parsePartSlug(filename)).toBe("Panel_Clip");
    expect(performance.now() - started).toBeLessThan(2000);
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

// The marker removal at 342b2c09 (stl-naming.ts `markerStrip`, parsers.ts `stripMarker` and
// `parsePartSlug`), kept to check the one-scan passes against. Test names have no folders,
// so `basename` is the identity here.
const reference342b2c09 = (() => {
  const WORD_CHARACTER = /^[\p{Alphabetic}\p{N}]$/u;
  const MODEL_EXTENSION_RE = /\.(?:stl|3mf)$/i;
  const THREE_MF_RE = /\.3mf$/i;
  type MarkerStrip = { re: RegExp; anywhere: boolean };
  function markerStrip(marker: string): MarkerStrip {
    const characters = [...marker];
    const anywhere =
      !WORD_CHARACTER.test(characters[0]!) && !WORD_CHARACTER.test(characters.at(-1)!);
    const escaped = marker.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    return {
      re: new RegExp(anywhere ? `([ _-]?)${escaped}([ _-]?)` : `^()${escaped}([ _-]?)`, "i"),
      anywhere,
    };
  }
  function stripMarker(stem: string, strip: MarkerStrip): string {
    let current = stem;
    for (;;) {
      const match = strip.re.exec(current);
      if (!match) return current;
      const [whole, before = "", after = ""] = match;
      const end = match.index + whole.length;
      const keep = match.index > 0 && end < current.length ? before || after : "";
      current = current.slice(0, match.index) + keep + current.slice(end);
      if (!strip.anywhere) return current;
    }
  }
  return function parsePartSlug(file: string, prof: ReturnType<typeof namingProfileFromDict>) {
    const markerStrips = prof.roles
      .flatMap((role) => role.markers)
      .filter((marker) => marker.trim())
      .map(markerStrip);
    const name = file.replace(MODEL_EXTENSION_RE, "");
    let stem = name;
    if (prof.slugStripMarkers) {
      for (const strip of markerStrips) {
        stem = stripMarker(stem, strip);
      }
      for (let before = name; stem !== before; ) {
        before = stem;
        for (const strip of markerStrips) {
          if (strip.anywhere) stem = stripMarker(stem, strip);
        }
      }
    }
    const quantityStrip = THREE_MF_RE.test(file) ? prof.quantityStrip3mfRe : prof.quantityStripRe;
    if (prof.slugStripQuantity && quantityStrip) {
      stem = stem.replace(quantityStrip, "");
    }
    return stem || name;
  };
})();

describe("marker removal matches 342b2c09", () => {
  const profile = (markers: Partial<Record<"primary" | "accent", string[]>>) =>
    namingProfileFromDict(
      mergeNamingProfiles(validateNamingProfile(DEFAULT_NAMING_PROFILE), {
        roles: Object.entries(markers).map(([id, list]) => ({ id: id as "primary", markers: list })),
      }),
    );
  // Role-ordered marker lists match `PROFILES` in rust/crates/pp-storage/src/catalog/naming/markers.rs.
  const profiles = {
    default: profile({}),
    "multi-part [a]-[c]": profile({ primary: ["[a]-[c]"] }),
    "non-ASCII [Σ]": profile({ accent: ["[Σ]"] }),
    "prefix letter A": profile({ accent: ["A"] }),
  };
  // The same token list, generator and seed as markers.rs.
  const TOKENS = [
    "[", "]", "a", "A", "c", "C", "o", "O", "-", "_", " ", ".", "P", "n", "l", "x", "2",
    "Σ", "ς", "σ", "é", "中", "[a]", "[A]", "[c]", "[o]", "[Σ]", "[ς]", "[a]-[c]", "[a[c]]",
  ];
  const SEED = 1396;
  const NAMES = 20_000;
  function mulberry32(seed: number) {
    let state = seed >>> 0;
    return () => {
      state = (state + 0x6d2b79f5) >>> 0;
      let t = state;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return (t ^ (t >>> 14)) >>> 0;
    };
  }
  const random = mulberry32(SEED);
  const names = Array.from({ length: NAMES }, () => {
    const length = random() % 41;
    let name = "";
    while (name.length < length) name += TOKENS[random() % TOKENS.length];
    return name;
  });

  it.each(Object.entries(profiles))(
    `gives the 342b2c09 slug and role for ${NAMES} generated names (seed ${SEED}) with the %s profile`,
    (_, prof) => {
      let changed = 0;
      for (const name of names) {
        const filename = `${name}.stl`;
        const expected = reference342b2c09(filename, prof);
        if (expected !== name) changed += 1;
        const actual = parseStlPath(filename, prof);
        // `parseRole` is unchanged since 342b2c09; the role is asserted so a change shows here.
        if (actual.partSlug !== expected || actual.role !== parseRole(filename, prof)) {
          expect({ name, slug: actual.partSlug, role: actual.role }).toEqual({
            name,
            slug: expected,
            role: parseRole(filename, prof),
          });
        }
      }
      // The generator must exercise marker removal, not only pass names through.
      expect(changed).toBeGreaterThan(NAMES / 2);
    },
  );

  it.each([
    ["Panel[c]-_[a].stl", profiles.default, "accent", "Panel"],
    ["Panel[a]-[c]_Clip.stl", profiles["multi-part [a]-[c]"], "primary", "Panel_Clip"],
    // `parseRole` matches markers with a lower-case `includes`, so `[ς]` is not `[Σ]` there.
    ["Panel_[ς]_Clip.stl", profiles["non-ASCII [Σ]"], "primary", "Panel_Clip"],
  ])("keeps the 342b2c09 result for %s", (filename, prof, role, partSlug) => {
    expect(parseStlPath(filename, prof)).toMatchObject({ role, partSlug });
    expect(reference342b2c09(filename, prof)).toBe(partSlug);
  });
});
