import { createHash } from "node:crypto";
import {
  encodeAcceptedPlate3mf,
  layOutDirectExportUnits,
  safePlanSlug,
  type DirectExportPlacement,
  type DirectExportPlate,
} from "@print-partner/domain";
import type { AcceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { acceptedPlanBasis } from "../db/accepted-plan-progress.js";
import type { AppRepository } from "../db/repository.js";
import { writeAcceptedExportFile } from "./accepted-export-publication.js";
import {
  loadAcceptedArtifactGeometry,
  type AcceptedArtifactGeometryLimit,
} from "./accepted-artifact-geometry.js";
import { parseRequiredUnitToken, type RequiredUnitToken } from "./required-units.js";
import type { AcceptedOperationalArtifact } from "../db/accepted-plan-operational.js";
import { TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";

export const DIRECT_EXPORT_3MF_LIMITS = {
  maxTotalSourceBytes: 256 * 1024 * 1024,
  maxObjects: 10_000,
  maxTriangles: 5_000_000,
  maxOutputBytes: 512 * 1024 * 1024,
} as const;

type MaterializeDirectExport3mfCommand = Readonly<{
  profileId: number;
  tokens: readonly string[];
}>;

type MaterializedDirectExport3mf = Readonly<{
  kind: "materialized";
  basis: AcceptedPlanBasis;
  filename: string;
  absolutePath: string;
  tokens: readonly RequiredUnitToken[];
}>;

export type MaterializeDirectExport3mfResult =
  | MaterializedDirectExport3mf
  | { readonly kind: "profile_not_found" }
  | { readonly kind: "empty_plan" }
  | {
      readonly kind: "accepted_state_unavailable";
      readonly reason: "compatibility_dirty" | "uninitialized";
    }
  | { readonly kind: "unknown_token"; readonly token: string }
  | {
      readonly kind: "artifact_unavailable";
      readonly token: RequiredUnitToken;
      readonly reason: string;
    }
  | { readonly kind: "invalid_stl"; readonly token: RequiredUnitToken }
  | { readonly kind: "artifact_geometry_mismatch"; readonly token: RequiredUnitToken }
  | {
      readonly kind: "limit_exceeded";
      readonly limit: AcceptedArtifactGeometryLimit | "output_bytes";
    }
  | { readonly kind: "output_failure" };

export type MaterializeDirectExport3mfDependencies = Readonly<{
  repository: Pick<
    AppRepository,
    | "getOwnedProfileIdentity"
    | "readAcceptedPlanOperationalSnapshot"
    | "readAcceptedPlateWorkspaceInput"
  >;
  reposDir: string;
  tenantExportsDir: string;
  limits?: typeof DIRECT_EXPORT_3MF_LIMITS;
}>;

type SelectedUnit = Readonly<{
  token: RequiredUnitToken;
  objectName: string;
  artifact: AcceptedOperationalArtifact;
}>;

function uniqueTokens(values: readonly string[]): RequiredUnitToken[] | { token: string } {
  const tokens: RequiredUnitToken[] = [];
  const seen = new Set<string>();
  for (const value of values) {
    let token: RequiredUnitToken;
    try {
      token = parseRequiredUnitToken(value);
    } catch {
      return { token: value };
    }
    if (seen.has(token)) continue;
    seen.add(token);
    tokens.push(token);
  }
  return tokens;
}

/**
 * The arrangement the Plates step put in front of the user. Direct export skips
 * Printer allocation, so this is best effort: before Plates are published there
 * is nothing to mirror and every unit falls through to the spaced-out strip.
 */
function shownArrangement(
  repository: MaterializeDirectExport3mfDependencies["repository"],
  profileId: number,
): Readonly<{
  plates: readonly DirectExportPlate[];
  placements: ReadonlyMap<string, DirectExportPlacement>;
}> {
  const workspace = repository.readAcceptedPlateWorkspaceInput(profileId);
  if (workspace.kind !== "ready") return { plates: [], placements: new Map() };
  const placements = new Map<string, DirectExportPlacement>();
  for (const plate of workspace.plates) {
    for (const unit of plate.units) {
      if (unit.placement === "unplaced") continue;
      placements.set(unit.token, { plateOrdinal: plate.ordinal, xUm: unit.xUm, yUm: unit.yUm });
    }
  }
  return {
    plates: workspace.plates.map((plate) => ({
      ordinal: plate.ordinal,
      bedWidthUm: plate.bedWidthUm,
      bedDepthUm: plate.bedDepthUm,
    })),
    placements,
  };
}

export async function materializeDirectExport3mf(
  dependencies: MaterializeDirectExport3mfDependencies,
  command: MaterializeDirectExport3mfCommand,
): Promise<MaterializeDirectExport3mfResult> {
  const limits = dependencies.limits ?? DIRECT_EXPORT_3MF_LIMITS;
  const profile = dependencies.repository.getOwnedProfileIdentity(command.profileId);
  if (!profile) return { kind: "profile_not_found" };
  const requested = uniqueTokens(command.tokens);
  if (!Array.isArray(requested) || requested.length === 0) {
    return { kind: "unknown_token", token: Array.isArray(requested) ? "" : requested.token };
  }

  const accepted = dependencies.repository.readAcceptedPlanOperationalSnapshot(command.profileId);
  if (accepted.kind === "empty") return { kind: "empty_plan" };
  if (accepted.kind !== "ready") {
    return { kind: "accepted_state_unavailable", reason: accepted.kind };
  }

  const available = new Map<string, SelectedUnit>();
  for (const part of accepted.snapshot.parts) {
    if (!part.included) continue;
    for (const unit of part.units) {
      if (!unit.required) continue;
      const token = parseRequiredUnitToken(unit.token);
      available.set(token, {
        token,
        objectName: unit.objectName,
        artifact: part.artifact,
      });
    }
  }

  const selected: SelectedUnit[] = [];
  for (const token of requested) {
    const unit = available.get(token);
    if (!unit) return { kind: "unknown_token", token };
    selected.push(unit);
  }

  const loaded = await loadAcceptedArtifactGeometry({
    reposDir: dependencies.reposDir,
    units: selected,
    limits,
  });
  if (loaded.kind === "degenerate_geometry") {
    return { kind: "artifact_geometry_mismatch", token: loaded.token };
  }
  if (loaded.kind !== "ready") return loaded;

  const geometryOf = (token: RequiredUnitToken) => {
    const geometry = loaded.geometryByToken.get(token);
    if (!geometry) throw new Error("Direct export mesh is missing");
    return geometry;
  };
  const arrangement = shownArrangement(dependencies.repository, command.profileId);
  const positions = layOutDirectExportUnits({
    units: selected.map((unit) => ({
      token: unit.token,
      widthUm: geometryOf(unit.token).dimensions.widthUm,
      depthUm: geometryOf(unit.token).dimensions.depthUm,
    })),
    placements: arrangement.placements,
    plates: arrangement.plates,
  });
  const bytes = encodeAcceptedPlate3mf(
    selected.map((unit) => {
      const position = positions.get(unit.token) ?? { xUm: 0, yUm: 0 };
      return {
        token: unit.token,
        objectName: unit.objectName,
        xUm: position.xUm,
        yUm: position.yUm,
        mesh: geometryOf(unit.token).mesh,
      };
    }),
  );
  if (bytes.byteLength > limits.maxOutputBytes) {
    return { kind: "limit_exceeded", limit: "output_bytes" };
  }

  const digest = createHash("sha256").update(bytes).digest("hex");
  const filename = `direct-${digest.slice(0, 16)}.3mf`;
  try {
    const absolutePath = writeAcceptedExportFile({
      root: dependencies.tenantExportsDir,
      directorySegments: [
        `profile-${profile.id}-${safePlanSlug(profile.name).slice(0, 80)}`,
        "direct-3mf",
      ],
      filename,
      bytes,
    });
    return {
      kind: "materialized",
      basis: acceptedPlanBasis(accepted.snapshot),
      filename,
      absolutePath,
      tokens: selected.map((unit) => unit.token),
    };
  } catch (error) {
    if (error instanceof TenantDiskQuotaError) throw error;
    return { kind: "output_failure" };
  }
}
