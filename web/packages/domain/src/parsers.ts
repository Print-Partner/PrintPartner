/** STL filename conventions (ported from Python parsers.py). */

import type { MarkerStrip, NamingProfile } from "./stl-naming.js";
import { getDefaultNamingProfile } from "./stl-naming.js";

export type PartRole = "primary" | "accent" | "clear" | "opaque";

export type ParsedPart = {
  role: PartRole;
  quantity: number;
  partSlug: string;
  filename: string;
};

function resolveProfile(profile?: NamingProfile | null): NamingProfile {
  return profile ?? getDefaultNamingProfile();
}

function roleFromId(roleId: string): PartRole {
  if (roleId === "accent" || roleId === "clear" || roleId === "opaque") return roleId;
  return "primary";
}

function checkRoleInText(text: string, profile: NamingProfile): PartRole | null {
  const lower = text.toLowerCase();
  for (const [marker, roleId] of profile.markerRoleMap) {
    if (lower.includes(marker.toLowerCase())) return roleFromId(roleId);
  }
  return null;
}

function folderRole(relativePath: string, profile: NamingProfile): PartRole | null {
  const posix = relativePath.replace(/\\/g, "/").toLowerCase();
  for (const rule of profile.folderRules) {
    if (posix.includes(rule.path_contains.toLowerCase())) {
      return roleFromId(rule.role_id);
    }
  }
  return null;
}

function basename(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  const idx = normalized.lastIndexOf("/");
  return idx >= 0 ? normalized.slice(idx + 1) : normalized;
}

function parentParts(path: string): string[] {
  const normalized = path.replace(/\\/g, "/");
  const parts = normalized.split("/");
  parts.pop();
  return parts;
}

export function parseRole(pathOrName: string, profile?: NamingProfile | null): PartRole {
  const prof = resolveProfile(profile);
  const foundFolder = folderRole(pathOrName, prof);
  if (foundFolder) return foundFolder;
  const segments = [...parentParts(pathOrName), basename(pathOrName)];
  for (const segment of segments) {
    const found = checkRoleInText(segment, prof);
    if (found) return found;
  }
  return "primary";
}

const MODEL_EXTENSION_RE = /\.(?:stl|3mf)$/i;
const THREE_MF_RE = /\.3mf$/i;

const SEPARATORS = new Set([" ", "_", "-"]);

/** A UTF-16 code unit and the marker code unit it matches ignoring case, or -1. */
type Unit = readonly [string, number];

/**
 * Repeat rounds before one combined scan finishes. Results equal the old repeat-until-stable
 * regexes for names nested up to this many alternating levels; past it, the kept separator
 * may differ. Rust's `catalog::naming::markers::MAX_ROUNDS` has the same value.
 */
export const MARKER_ROUNDS = 8;

const unitMatches = (units: readonly Unit[], from: number, marker: readonly number[]) =>
  from >= 0 && marker.every((unit, index) => units[from + index]?.[1] === unit);

/**
 * Removes every occurrence of `markers`, leftmost first; with several, the first in role
 * order that the stack ends with. The stack holds the scanned text with earlier matches
 * removed, so this finds what repeated regex searches found. Each marker's KMP state is kept
 * per stacked unit, so a step costs O(1) amortized per marker and a scan is linear.
 */
function stripEverywhere(input: readonly Unit[], markers: readonly MarkerStrip[]): Unit[] {
  const out: Unit[] = [];
  const states: number[] = [];
  const k = markers.length;
  const push = (unit: Unit) => {
    let found = 0;
    const previous = states.length - k;
    for (let j = 0; j < k; j++) {
      const { units, fail } = markers[j]!;
      let q = out.length > 0 ? states[previous + j]! : 0;
      if (q === units.length) q = fail[q - 1]!;
      while (q > 0 && units[q] !== unit[1]) q = fail[q - 1]!;
      if (units[q] === unit[1]) q++;
      if (q === units.length && !found) found = q;
      states.push(q);
    }
    out.push(unit);
    return found;
  };
  let next = 0;
  while (next < input.length) {
    for (let length = push(input[next++]!); length; ) {
      out.length -= length;
      const before = SEPARATORS.has(out.at(-1)?.[0] ?? "") ? out.pop() : undefined;
      states.length = out.length * k;
      const after = SEPARATORS.has(input[next]?.[0] ?? "") ? input[next++] : undefined;
      const keep = before ?? after;
      length = keep && out.length > 0 && next < input.length ? push(keep) : 0;
    }
  }
  return out;
}

/**
 * Removes role markers from a slug, as the per-marker regexes `([ _-]?)<marker>([ _-]?)` (flag
 * `i`) did. Markers run one at a time in role order: `anywhere` markers everywhere, prefix
 * markers once at the start with the separator after them. Each removal takes one separator
 * (space, `_` or `-`) on each side and puts one back when text remains on both sides. When
 * that changed the name, the `anywhere` markers run again in role order until a round removes
 * nothing, so a marker that removing a nested one exposes, as in `Panel_[a[c]]_Clip`, goes
 * too; after `MARKER_ROUNDS` rounds one combined scan removes the rest. Code units compare
 * through the old regexes' case folding, once per distinct unit.
 * Rust's `catalog::naming::strip_markers` is the same algorithm.
 */
function stripMarkers(stem: string, prof: NamingProfile): string {
  const classes = new Map<string, number>();
  const classOf = (unit: string) => {
    if (!classes.has(unit)) classes.set(unit, prof.markerUnitRes.findIndex((re) => re.test(unit)));
    return classes.get(unit)!;
  };
  const name: Unit[] = Array.from({ length: stem.length }, (_, i) => [stem[i]!, classOf(stem[i]!)]);
  let slug = name;
  for (const strip of prof.markerStrips) {
    const { anywhere, units } = strip;
    if (anywhere) slug = stripEverywhere(slug, [strip]);
    else if (unitMatches(slug, 0, units)) {
      slug = slug.slice(units.length + (SEPARATORS.has(slug[units.length]?.[0] ?? "") ? 1 : 0));
    }
  }
  const anywhere = prof.markerStrips.filter((strip) => strip.anywhere);
  // Every removal shortens the name, so an unchanged length means nothing was removed.
  for (let round = 0, before = name.length; slug.length !== before; round++) {
    before = slug.length;
    if (round === MARKER_ROUNDS) {
      slug = stripEverywhere(slug, anywhere);
      break;
    }
    for (const strip of anywhere) slug = stripEverywhere(slug, [strip]);
  }
  return slug.map(([unit]) => unit).join("");
}

export function parseQuantity(filename: string, profile?: NamingProfile | null): number {
  const prof = resolveProfile(profile);
  // The regex reads the name as written, so saved regexes keep their meaning. Most end in
  // `\.stl$`, so a `.3mf` name they miss is read again as the `.stl` of the same name.
  const m =
    prof.quantityRe.exec(filename) ??
    (THREE_MF_RE.test(filename) ? prof.quantityRe.exec(filename.replace(THREE_MF_RE, ".stl")) : null);
  if (m?.[1]) return Math.max(1, parseInt(m[1], 10));
  return Math.max(1, prof.quantityDefault);
}

export function parsePartSlug(filename: string, profile?: NamingProfile | null): string {
  const prof = resolveProfile(profile);
  const file = basename(filename);
  const name = file.replace(MODEL_EXTENSION_RE, "");
  let stem = name;
  if (prof.slugStripMarkers) {
    stem = stripMarkers(stem, prof);
  }
  const quantityStrip = THREE_MF_RE.test(file) ? prof.quantityStrip3mfRe : prof.quantityStripRe;
  if (prof.slugStripQuantity && quantityStrip) {
    stem = stem.replace(quantityStrip, "");
  }
  return stem || name;
}

export function parseStlPath(
  relativePath: string,
  profile?: NamingProfile | null,
): ParsedPart {
  const prof = resolveProfile(profile);
  const filename = basename(relativePath.replace(/\\/g, "/"));
  return {
    role: parseRole(relativePath, prof),
    quantity: parseQuantity(filename, prof),
    partSlug: parsePartSlug(filename, prof),
    filename,
  };
}

export function previewParse(
  relativePath: string,
  profile?: NamingProfile | null,
): { role: string; quantity: number; part_slug: string; filename: string } {
  const parsed = parseStlPath(relativePath, profile ?? undefined);
  return {
    role: parsed.role,
    quantity: parsed.quantity,
    part_slug: parsed.partSlug,
    filename: parsed.filename,
  };
}
