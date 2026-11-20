/** Configurable STL filename naming rules (ported from Python stl_naming.py). */

import {
  DEFAULT_STL_NAMING_PROFILE,
  isSymbolBoundedMarker,
  parseStlNamingProfile,
  parseStlNamingProfileOverride,
  type StlNamingFolderRule,
  type StlNamingProfile,
  type StlNamingProfileOverride,
  type StlNamingRole,
  type StlNamingRoleId,
} from "@print-partner/contracts";

export type {
  StlNamingFolderRule,
  StlNamingProfileOverride,
  StlNamingRole,
  StlNamingRoleId,
};

export const STL_NAMING_DEFAULTS_KEY = "stl_naming_defaults";

export type StlNamingProfileDict = StlNamingProfile;

export const DEFAULT_NAMING_PROFILE = DEFAULT_STL_NAMING_PROFILE;

/**
 * How a role marker leaves a part slug. A marker that starts and ends with a symbol, like
 * `[a]`, cannot be part of a word, so it is removed anywhere in the name (`anywhere`); `units`
 * holds each UTF-16 code unit of it as the index of the first `NamingProfile.markerUnitRes`
 * regex it matches. A marker that starts or ends with a letter or digit, like a custom `A`, is
 * removed only as a prefix, so `gear` keeps its `a`; `unitRes` holds one case-insensitive regex
 * per code unit of it, compared at the current offset.
 */
export type MarkerStrip =
  | { anywhere: true; units: readonly number[]; fail: readonly number[] }
  | { anywhere: false; unitRes: readonly RegExp[] };

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Builds `markerStrip` entries. Each distinct marker code unit gets one case-insensitive
 * regex, so code units compare exactly as the per-marker `i` regexes did. Only symbol-bounded
 * markers add to `unitRes`, which classifies every unit of a name.
 */
function markerStripper() {
  const unitRes: RegExp[] = [];
  const prefixRes = new Map<string, RegExp>();
  const unitRe = (unit: string) => new RegExp(`^(?:${escapeRegExp(unit)})$`, "i");
  const unitClass = (unit: string) => {
    const found = unitRes.findIndex((re) => re.test(unit));
    if (found >= 0) return found;
    unitRes.push(unitRe(unit));
    return unitRes.length - 1;
  };
  const strip = (marker: string): MarkerStrip => {
    if (!isSymbolBoundedMarker(marker)) {
      const prefixRe = (unit: string) => prefixRes.get(unit) ?? prefixRes.set(unit, unitRe(unit)).get(unit)!;
      return { anywhere: false, unitRes: Array.from({ length: marker.length }, (_, i) => prefixRe(marker[i]!)) };
    }
    const units = Array.from({ length: marker.length }, (_, i) => unitClass(marker[i]!));
    // KMP prefix table: `fail[i]` is the longest proper prefix of `units[..=i]` that is also its suffix.
    const fail = [0];
    for (let i = 1, q = 0; i < units.length; i++) {
      while (q > 0 && units[i] !== units[q]) q = fail[q - 1]!;
      if (units[i] === units[q]) q++;
      fail.push(q);
    }
    return { units, fail, anywhere: true };
  };
  return { unitRes, strip };
}

export type PartFunctionalClass = "functional" | "cosmetic" | "unclassified";

export type NamingProfile = {
  roles: readonly StlNamingRole[];
  quantityRegex: string;
  quantityDefault: number;
  slugStripMarkers: boolean;
  slugStripQuantity: boolean;
  folderRules: readonly StlNamingFolderRule[];
  exportRoleOrder: readonly StlNamingRoleId[];
  quantityRe: RegExp;
  markerRoleMap: ReadonlyArray<readonly [string, StlNamingRoleId]>;
  /** One per marker, in role order. See `MarkerStrip`. */
  markerStrips: readonly MarkerStrip[];
  /** One case-insensitive regex per distinct marker code unit. See `MarkerStrip`. */
  markerUnitRes: readonly RegExp[];
  /** Removes the quantity suffix from an `.stl` slug. */
  quantityStripRe: RegExp | null;
  /** The same for a `.3mf` slug, which also accepts a regex anchored on `\.3mf$`. */
  quantityStrip3mfRe: RegExp | null;
  toDict(): StlNamingProfileDict;
};

function compileQuantityStrip(quantityRegex: string, extension: "stl" | "3mf"): RegExp | null {
  let strip = quantityRegex.trim();
  if (!strip) return null;
  strip = strip.replace(/\(\?P<\w+>/g, "(");
  strip = strip.replace(/\(\?:/g, "(");
  strip = strip.replace(/\([^?][^)]*\)/, "[0-9]+");
  strip = strip.replace(/\\\.stl\$/, "$");
  strip = strip.replace(/\.stl$/i, "$");
  if (extension === "3mf") {
    strip = strip.replace(/\\\.3mf\$/, "$");
    strip = strip.replace(/\.3mf$/i, "$");
  }
  try {
    return new RegExp(strip, "i");
  } catch {
    return null;
  }
}

function buildProfile(data: StlNamingProfileDict): NamingProfile {
  const roles = data.roles;
  const quantityRegex = data.quantity.regex;
  const quantityRe = new RegExp(quantityRegex, "i");
  const quantityDefault = data.quantity.default;

  const markerPairs: Array<[string, StlNamingRoleId]> = [];
  const strips: MarkerStrip[] = [];
  const stripper = markerStripper();
  for (const role of roles) {
    for (const marker of role.markers) {
      if (!marker.trim()) continue;
      markerPairs.push([marker, role.id]);
      strips.push(stripper.strip(marker));
    }
  }
  markerPairs.sort((a, b) => b[0].length - a[0].length);

  const exportOrder = data.export_role_order;

  const profile: NamingProfile = {
    roles,
    quantityRegex,
    quantityDefault,
    slugStripMarkers: data.slug.strip_markers ?? true,
    slugStripQuantity: data.slug.strip_quantity ?? true,
    folderRules: data.folder_rules ?? [],
    exportRoleOrder: exportOrder,
    quantityRe,
    markerRoleMap: markerPairs,
    markerStrips: strips,
    markerUnitRes: stripper.unitRes,
    quantityStripRe: compileQuantityStrip(quantityRegex, "stl"),
    quantityStrip3mfRe: compileQuantityStrip(quantityRegex, "3mf"),
    toDict() {
      return {
        roles: [...this.roles],
        quantity: { regex: this.quantityRegex, default: this.quantityDefault },
        slug: {
          strip_markers: this.slugStripMarkers,
          strip_quantity: this.slugStripQuantity,
        },
        folder_rules: [...this.folderRules],
        export_role_order: [...this.exportRoleOrder],
      };
    },
  };
  return profile;
}

let defaultProfile: NamingProfile | null = null;

export function getDefaultNamingProfile(): NamingProfile {
  if (!defaultProfile) {
    defaultProfile = buildProfile(structuredClone(DEFAULT_NAMING_PROFILE));
  }
  return defaultProfile;
}

export function resetDefaultNamingProfileCache(): void {
  defaultProfile = null;
}

export function validateNamingProfile(data: unknown): StlNamingProfileDict {
  return parseStlNamingProfile(data);
}

export function namingProfileFromDict(data: StlNamingProfileDict): NamingProfile {
  return buildProfile(validateNamingProfile(data));
}

export function mergeNamingProfiles(
  base: StlNamingProfileDict,
  override: StlNamingProfileOverride,
): StlNamingProfileDict {
  const rolesById = new Map(base.roles.map((role) => [role.id, structuredClone(role)]));
  for (const roleOverride of override.roles ?? []) {
    const current = rolesById.get(roleOverride.id) ?? {
      id: roleOverride.id,
      label: roleOverride.id,
      markers: [],
    };
    rolesById.set(roleOverride.id, {
      id: roleOverride.id,
      label: roleOverride.label ?? current.label,
      markers: roleOverride.markers ?? current.markers,
    });
  }
  const merged = {
    roles: [...rolesById.values()],
    quantity: { ...base.quantity, ...override.quantity },
    slug: { ...base.slug, ...override.slug },
    folder_rules: structuredClone(override.folder_rules ?? base.folder_rules),
    export_role_order: structuredClone(override.export_role_order ?? base.export_role_order),
  };
  return validateNamingProfile(merged);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function sourceNamingMetadata(
  metadata: Record<string, unknown> | null | undefined,
): { useDefaults: boolean; overrideValue: unknown } {
  if (!metadata || !isRecord(metadata.naming)) {
    return { useDefaults: true, overrideValue: {} };
  }
  return {
    useDefaults: metadata.naming.use_defaults !== false,
    overrideValue: metadata.naming.override ?? {},
  };
}

export function parseSourceNamingMetadata(
  metadata: Record<string, unknown> | null | undefined,
): { useDefaults: boolean; override: StlNamingProfileOverride } {
  const { useDefaults, overrideValue } = sourceNamingMetadata(metadata);
  try {
    return { useDefaults, override: parseStlNamingProfileOverride(overrideValue) };
  } catch {
    return { useDefaults, override: {} };
  }
}

export function parseSourceNamingMetadataStrict(
  metadata: Record<string, unknown> | null | undefined,
): { useDefaults: boolean; override: StlNamingProfileOverride } {
  const { useDefaults, overrideValue } = sourceNamingMetadata(metadata);
  return {
    useDefaults,
    override: useDefaults ? {} : parseStlNamingProfileOverride(overrideValue),
  };
}

export function resolveNamingProfile(
  globalDict: StlNamingProfileDict,
  metadata: Record<string, unknown> | null | undefined,
): NamingProfile {
  const { useDefaults, override } = parseSourceNamingMetadata(metadata);
  if (useDefaults) return namingProfileFromDict(globalDict);
  if (Object.keys(override).length > 0) {
    try {
      return namingProfileFromDict(validateNamingProfile(override));
    } catch {
      // Older Source metadata stored sparse overrides. Keep those readable by
      // filling their omitted fields from the current global profile.
    }
    return namingProfileFromDict(mergeNamingProfiles(globalDict, override));
  }
  return namingProfileFromDict(globalDict);
}

/**
 * Classify a part as functional, cosmetic, or unclassified using folder rules
 * with functional_class set, and the _optional_ filename convention.
 */
export function classifyPartFunctional(
  relativePath: string,
  filename: string,
  folderRules: readonly StlNamingFolderRule[],
): PartFunctionalClass {
  // _optional_ in filename -> cosmetic (common upstream convention)
  if (filename.toLowerCase().includes("_optional_")) return "cosmetic";

  const normalizedPath = relativePath.replace(/\\/g, "/").toLowerCase();
  for (const rule of folderRules) {
    if (!rule.functional_class) continue;
    if (normalizedPath.includes(rule.path_contains.toLowerCase())) {
      return rule.functional_class;
    }
  }

  return "unclassified";
}
