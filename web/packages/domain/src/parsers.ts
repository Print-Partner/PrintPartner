/** STL filename conventions (ported from Python parsers.py). */

import type { MarkerStrip, NamingProfile } from "./stl-naming.js";
import { foldCharacter, getDefaultNamingProfile } from "./stl-naming.js";

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

/**
 * Removes role markers from a slug. One left-to-right pass copies the name and drops an
 * `anywhere` marker, ignoring case, as soon as the copy ends with it, so a marker that
 * removing a nested one exposes, as in `Panel_[a[c]]_Clip`, goes too. Each removal takes
 * one separator on each side and puts one back when text remains on both sides. Prefix-only
 * markers then go once each. Each removal shortens the copy or consumes input, so the work
 * is linear in the name's length. Rust's `catalog::naming::strip_markers` matches this.
 */
function stripMarkers(stem: string, strips: readonly MarkerStrip[]): string {
  const markers = strips.filter((strip) => strip.folded.length > 0);
  const anywhere = markers.filter((strip) => strip.anywhere);
  const input = [...stem];
  const out: string[] = [];
  const folded: string[] = [];
  const push = (character: string) => {
    out.push(character);
    folded.push(foldCharacter(character));
  };
  const endsWith = (marker: readonly string[]) =>
    marker.length <= folded.length &&
    marker.every((character, index) => folded[folded.length - marker.length + index] === character);
  let next = 0;
  while (next < input.length) {
    push(input[next++]!);
    for (let match = anywhere.find((strip) => endsWith(strip.folded)); match; ) {
      out.length -= match.folded.length;
      folded.length = out.length;
      const before = SEPARATORS.has(out.at(-1) ?? "") ? out.pop() : undefined;
      folded.length = out.length;
      const after = SEPARATORS.has(input[next] ?? "") ? input[next++] : undefined;
      const keep = before ?? after;
      if (keep !== undefined && out.length > 0 && next < input.length) push(keep);
      match = anywhere.find((strip) => endsWith(strip.folded));
    }
  }
  let rest = out;
  for (const strip of markers) {
    if (strip.anywhere) continue;
    const prefix = strip.folded.every(
      (character, index) => rest[index] !== undefined && foldCharacter(rest[index]) === character,
    );
    if (!prefix) continue;
    rest = rest.slice(strip.folded.length);
    if (SEPARATORS.has(rest[0] ?? "")) rest = rest.slice(1);
  }
  return rest.join("");
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
    stem = stripMarkers(stem, prof.markerStrips);
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
