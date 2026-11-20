/** STL filename conventions (ported from Python parsers.py). */

import type { NamingProfile } from "./stl-naming.js";
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

const unitMatches = (units: readonly Unit[], from: number, marker: readonly number[]) =>
  marker.every((unit, index) => units[from + index]?.[1] === unit);

/**
 * One marker everywhere, leftmost first, as repeated regex removals did. The stack holds the
 * scanned text with every earlier match removed, so the first match in the current name is
 * the first time the stack ends with the marker. Each removal takes one separator on each
 * side and puts one back when text remains on both sides. Each step shortens the stack or
 * consumes input, so the pass is linear in the name's length.
 */
function stripEverywhere(input: readonly Unit[], marker: readonly number[]): Unit[] {
  const out: Unit[] = [];
  let next = 0;
  while (next < input.length) {
    out.push(input[next++]!);
    while (out.length >= marker.length && unitMatches(out, out.length - marker.length, marker)) {
      out.length -= marker.length;
      const before = SEPARATORS.has(out.at(-1)?.[0] ?? "") ? out.pop() : undefined;
      const after = SEPARATORS.has(input[next]?.[0] ?? "") ? input[next++] : undefined;
      const keep = before ?? after;
      if (keep && out.length > 0 && next < input.length) out.push(keep);
    }
  }
  return out;
}

/** One marker once, only at the start, with the separator after it. */
function stripPrefix(units: Unit[], marker: readonly number[]): Unit[] {
  if (units.length < marker.length || !unitMatches(units, 0, marker)) return units;
  const end = marker.length + (SEPARATORS.has(units[marker.length]?.[0] ?? "") ? 1 : 0);
  return units.slice(end);
}

/**
 * Removes role markers from a slug. Markers run one at a time in role order: `anywhere`
 * markers everywhere, prefix markers once at the start. When that changed the name, the
 * `anywhere` markers run again in role order until a round removes nothing, so a marker
 * that removing a nested one exposes, as in `Panel_[a[c]]_Clip`, goes too. The results are
 * the ones the per-marker regexes gave; Rust's `catalog::naming::strip_markers` matches.
 */
function stripMarkers(stem: string, prof: NamingProfile): string {
  const classes = new Map<string, number>();
  const classOf = (unit: string) => {
    let found = classes.get(unit);
    if (found === undefined) {
      found = prof.markerUnitRes.findIndex((re) => re.test(unit));
      classes.set(unit, found);
    }
    return found;
  };
  const name: Unit[] = Array.from({ length: stem.length }, (_, i) => [stem[i]!, classOf(stem[i]!)]);
  let slug = name;
  for (const strip of prof.markerStrips) {
    slug = strip.anywhere ? stripEverywhere(slug, strip.units) : stripPrefix(slug, strip.units);
  }
  // Every removal shortens the name, so an unchanged length means nothing was removed.
  for (let before = name.length; slug.length !== before; ) {
    before = slug.length;
    for (const strip of prof.markerStrips) {
      if (strip.anywhere) slug = stripEverywhere(slug, strip.units);
    }
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
