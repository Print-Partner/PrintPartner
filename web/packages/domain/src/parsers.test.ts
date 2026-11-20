import { describe, expect, it } from "vitest";
import { MARKER_ROUNDS, parsePartSlug, parseQuantity, parseRole, parseStlPath } from "./parsers.js";
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

// 342b2c09's marker removal (stl-naming.ts `markerStrip`, parsers.ts `stripMarker` and
// `parsePartSlug`), kept to check `stripMarkers` against. Test names have no folders.
function reference342b2c09(file: string, prof: ReturnType<typeof namingProfileFromDict>) {
  const word = (c?: string) => /^[\p{Alphabetic}\p{N}]$/u.test(c ?? "");
  const strips = prof.roles.flatMap((r) => r.markers).filter((m) => m.trim()).map((marker) => {
    const anywhere = !word([...marker][0]) && !word([...marker].at(-1));
    const escaped = marker.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const re = new RegExp(anywhere ? `([ _-]?)${escaped}([ _-]?)` : `^()${escaped}([ _-]?)`, "i");
    return { re, anywhere };
  });
  const stripMarker = (stem: string, strip: (typeof strips)[number]) => {
    for (let current = stem; ; ) {
      const match = strip.re.exec(current);
      if (!match) return current;
      const [whole, before = "", after = ""] = match;
      const end = match.index + whole.length;
      const keep = match.index > 0 && end < current.length ? before || after : "";
      current = current.slice(0, match.index) + keep + current.slice(end);
      if (!strip.anywhere) return current;
    }
  };
  const name = file.replace(/\.(?:stl|3mf)$/i, "");
  let stem = name;
  for (const strip of strips) stem = stripMarker(stem, strip);
  for (let before = name; stem !== before; ) {
    before = stem;
    for (const strip of strips) if (strip.anywhere) stem = stripMarker(stem, strip);
  }
  const quantityStrip = /\.3mf$/i.test(file) ? prof.quantityStrip3mfRe : prof.quantityStripRe;
  if (prof.slugStripQuantity && quantityStrip) stem = stem.replace(quantityStrip, "");
  return stem || name;
}

describe("marker removal matches 342b2c09", () => {
  const profile = (markers: Partial<Record<"primary" | "accent", string[]>>) =>
    namingProfileFromDict(
      mergeNamingProfiles(validateNamingProfile(DEFAULT_NAMING_PROFILE), {
        roles: Object.entries(markers).map(([id, list]) => ({ id: id as "primary", markers: list })),
      }),
    );
  // The profiles, TOKENS, seed and generator match the tests in Rust's naming/markers.rs.
  const profiles = {
    default: profile({}),
    "multi-part [a]-[c]": profile({ primary: ["[a]-[c]"] }),
    "non-ASCII [Σ]": profile({ accent: ["[Σ]"] }),
    "prefix letter A": profile({ accent: ["A"] }),
  };
  // prettier-ignore
  const TOKENS = [
    "[", "]", "a", "A", "c", "C", "o", "O", "-", "_", " ", ".", "P", "n", "l", "x", "2",
    "Σ", "ς", "σ", "é", "中", "[a]", "[A]", "[c]", "[o]", "[Σ]", "[ς]", "[a]-[c]", "[a[c]]",
  ];
  const SEED = 1396;
  const NAMES = 20_000;
  let state = SEED;
  const random = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), state | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return (t ^ (t >>> 14)) >>> 0;
  };
  const names = Array.from({ length: NAMES }, () => {
    const length = random() % 41;
    let name = "";
    while (name.length < length) name += TOKENS[random() % TOKENS.length];
    return name;
  });

  // Names of at most 47 characters nest fewer than MARKER_ROUNDS levels, so all must match.
  it.each(Object.entries(profiles))(
    `gives the 342b2c09 slug and role for ${NAMES} names (seed ${SEED}) with the %s profile`,
    (_, prof) => {
      let changed = 0;
      for (const name of names) {
        const filename = `${name}.stl`;
        const slug = reference342b2c09(filename, prof);
        changed += slug === name ? 0 : 1;
        // `parseRole` is unchanged since 342b2c09.
        const expected = { name, slug, role: parseRole(filename, prof) };
        const { partSlug, role } = parseStlPath(filename, prof);
        if (partSlug !== slug || role !== expected.role) {
          expect({ name, slug: partSlug, role }).toEqual(expected);
        }
      }
      // The generator must exercise removal, not only pass names through.
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

  // The old regexes need `levels` repeat rounds; Rust's markers.rs pins the same.
  const capped = (levels: number) => {
    const nest = "[a[c".repeat(levels) + "]]".repeat(levels);
    return `Panel[c${nest}]-_[a${nest}].stl`;
  };
  it("matches the old regexes at 32 alternating levels and scans once at 33", () => {
    expect(MARKER_ROUNDS).toBe(32);
    const both = (levels: number) => [
      parsePartSlug(capped(levels)),
      reference342b2c09(capped(levels), profiles.default),
    ];
    expect(both(32)).toEqual(["Panel", "Panel"]);
    // Past the cap the combined scan keeps the `-` the rounds would drop.
    expect(both(33)).toEqual(["Panel-", "Panel"]);
  });

  // The slowest scan the 64-unit marker limit allows: 63 units that keep matching, then a miss.
  it("scans 30,000 near misses of a 64-unit marker within 2 seconds", () => {
    const stem = "[".repeat(30_000);
    const started = performance.now();
    expect(parsePartSlug(`${stem}.stl`, profile({ primary: [`${"[".repeat(63)}]`] }))).toBe(stem);
    expect(performance.now() - started).toBeLessThan(2000);
  });

  it("strips 30,000 characters of alternating nesting within 5 seconds", () => {
    const started = performance.now();
    expect(parsePartSlug(`P_${"[a[c".repeat(5000)}${"]]".repeat(5000)}_C.stl`)).toBe("P_C");
    expect(performance.now() - started).toBeLessThan(5000);
  });
});
