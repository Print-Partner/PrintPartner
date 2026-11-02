import type { AcceptedOperationalPart } from "../db/accepted-plan-operational.js";
import { acceptedMediaBasis, type AcceptedMediaVariant } from "../lib/accepted-media-cache.js";
import { getColorById } from "./filament-catalog.js";

type AcceptedPartMediaFields = Pick<
  AcceptedOperationalPart,
  "artifact" | "effectiveRole" | "filamentColorId" | "filamentCustomHex"
>;

function acceptedRenderHex(part: AcceptedPartMediaFields): string | null {
  const custom = part.filamentCustomHex?.trim() ?? "";
  if (/^#[0-9a-f]{6}$/i.test(custom)) return custom.toLowerCase();
  const catalog = part.filamentColorId ? getColorById(part.filamentColorId) : null;
  const catalogHex = catalog?.hex.trim() ?? "";
  return /^#[0-9a-f]{6}$/i.test(catalogHex) ? catalogHex.toLowerCase() : null;
}

export function acceptedPartMediaIdentity(
  part: AcceptedPartMediaFields,
  variant: AcceptedMediaVariant,
): { readonly hex: string | null; readonly basis: string } {
  if (part.artifact.kind !== "tracked") {
    throw new Error("Accepted Part artifact is unavailable");
  }
  const hex = acceptedRenderHex(part);
  return {
    hex,
    basis: acceptedMediaBasis({
      expectedSha256: part.artifact.expectedSha256,
      role: part.effectiveRole,
      hex,
      variant,
    }),
  };
}
