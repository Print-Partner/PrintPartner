import {
  acceptedPlateZipEpoch,
  encodeAcceptedPlate3mf,
  MAX_ACCEPTED_PLATES,
} from "@print-partner/domain";
import { strToU8, zipSync } from "fflate";
import type {
  AcceptedPlateExportInput,
  AcceptedPlateExportUnit,
  ReadAcceptedPlateExportInputResult,
} from "../db/accepted-plates.js";
import type { AcceptedArtifactVerificationFailure } from "./accepted-artifacts.js";
import {
  loadAcceptedArtifactGeometry,
  type AcceptedArtifactOpener,
} from "./accepted-artifact-geometry.js";

export type AcceptedPlate3mfLimit =
  "artifact_bytes" | "total_source_bytes" | "objects" | "triangles" | "output_bytes" | "plates";

export type AcceptedPlate3mfLimits = Readonly<{
  maxArtifactBytes?: number;
  maxTotalSourceBytes: number;
  maxObjects: number;
  maxTriangles: number;
  maxOutputBytes: number;
  maxPlates: number;
}>;

type AcceptedPlateExportRepository = Readonly<{
  readAcceptedPlateExportInput(profileId: number): ReadAcceptedPlateExportInputResult;
}>;

export type AcceptedPlate3mfDependencies = Readonly<{
  repository: AcceptedPlateExportRepository;
  reposDir: string;
  limits: AcceptedPlate3mfLimits;
  openArtifact?: AcceptedArtifactOpener;
  bundleEncoder?: (entries: Record<string, Uint8Array>) => Uint8Array;
}>;

export type GeneratedAcceptedPlate3mf = Readonly<{
  plateId: string;
  ordinal: number;
  entryName: string;
  bytes: Uint8Array;
}>;

export type GenerateAcceptedPlate3mfArtifactsResult =
  | {
      readonly kind: "generated";
      readonly basis: AcceptedPlateExportInput["basis"];
      readonly plateRevisionId: number;
      readonly plateRevisionNumber: number;
      readonly layoutDigest: string;
      readonly manifest: Uint8Array;
      readonly plates: readonly GeneratedAcceptedPlate3mf[];
      readonly bundle: Uint8Array | null;
    }
  | {
      readonly kind:
        "empty_plan" | "plates_not_published" | "stale_accepted_plan" | "unplaced_units";
    }
  | {
      readonly kind: "accepted_state_unavailable";
      readonly reason: "compatibility_dirty" | "uninitialized";
    }
  | { readonly kind: "transaction_unavailable" }
  | { readonly kind: "plate_revision_changed" }
  | {
      readonly kind: "artifact_unavailable";
      readonly token: AcceptedPlateExportUnit["token"];
      readonly reason: "legacy" | "untracked_source" | AcceptedArtifactVerificationFailure;
    }
  | { readonly kind: "invalid_stl"; readonly token: AcceptedPlateExportUnit["token"] }
  | {
      readonly kind: "artifact_geometry_mismatch";
      readonly token: AcceptedPlateExportUnit["token"];
    }
  | { readonly kind: "limit_exceeded"; readonly limit: AcceptedPlate3mfLimit };

function validLimits(limits: AcceptedPlate3mfLimits): boolean {
  return (
    Object.values(limits).every((value) => Number.isSafeInteger(value) && value >= 0) &&
    limits.maxPlates <= MAX_ACCEPTED_PLATES
  );
}

function entryName(ordinal: number): string {
  return `plates/${String(ordinal).padStart(4, "0")}.3mf`;
}

function manifestBytes(input: AcceptedPlateExportInput): Uint8Array {
  const manifest = {
    format: "accepted-plate-3mf-manifest-v1",
    basis: {
      profile_id: input.basis.profileId,
      plan_version: input.basis.planVersion,
      plan_revision_id: input.basis.revisionId,
      plan_revision_digest: input.basis.revisionDigest,
      required_unit_mapping_digest: input.basis.requiredUnitMappingDigest,
      plate_revision_id: input.plateRevisionId,
      plate_revision_number: input.plateRevisionNumber,
      layout_digest: input.layoutDigest,
    },
    plates: input.plates.map((plate) => ({
      ordinal: plate.ordinal,
      plate_id: plate.plateId,
      printer: { id: plate.printerId, name: plate.printerName, model: plate.printerModel },
      entry: entryName(plate.ordinal),
      units: plate.units.map((unit, index) => ({
        token: unit.token,
        object_name: unit.objectName,
        object_id: index + 1,
        artifact_sha256: unit.artifact.kind === "tracked" ? unit.artifact.expectedSha256 : null,
      })),
    })),
  };
  return strToU8(`${JSON.stringify(manifest, null, 2)}\n`);
}

export async function generateAcceptedPlate3mfArtifacts(
  dependencies: AcceptedPlate3mfDependencies,
  command: Readonly<{
    profileId: number;
    expectedPlateRevisionId: number;
    includeBundle?: boolean;
  }>,
): Promise<GenerateAcceptedPlate3mfArtifactsResult> {
  if (!validLimits(dependencies.limits))
    throw new RangeError("Accepted Plate 3MF limits must be nonnegative safe integers");
  const resolved = dependencies.repository.readAcceptedPlateExportInput(command.profileId);
  if (resolved.kind !== "ready") return resolved;
  const input = resolved.input;
  if (input.plateRevisionId !== command.expectedPlateRevisionId) {
    return { kind: "plate_revision_changed" };
  }
  if (input.plates.length > dependencies.limits.maxPlates) {
    return { kind: "limit_exceeded", limit: "plates" };
  }
  const units = input.plates.flatMap((plate) => plate.units);
  const plateByToken = new Map<
    AcceptedPlateExportUnit["token"],
    AcceptedPlateExportInput["plates"][number]
  >();
  for (const plate of input.plates) {
    for (const unit of plate.units) plateByToken.set(unit.token, plate);
  }
  const loaded = await loadAcceptedArtifactGeometry({
    reposDir: dependencies.reposDir,
    units,
    limits: dependencies.limits,
    openArtifact: dependencies.openArtifact,
  });
  if (loaded.kind === "degenerate_geometry") {
    return { kind: "artifact_geometry_mismatch", token: loaded.token };
  }
  if (loaded.kind !== "ready") return loaded;
  for (const unit of units) {
    const geometry = loaded.geometryByToken.get(unit.token);
    const plate = plateByToken.get(unit.token);
    if (
      !geometry ||
      !plate ||
      geometry.dimensions.widthUm !== unit.widthUm ||
      geometry.dimensions.depthUm !== unit.depthUm ||
      geometry.dimensions.heightUm !== unit.heightUm ||
      unit.xUm < plate.marginUm ||
      unit.yUm < plate.marginUm ||
      unit.xUm + geometry.dimensions.widthUm > plate.bedWidthUm - plate.marginUm ||
      unit.yUm + geometry.dimensions.depthUm > plate.bedDepthUm - plate.marginUm ||
      geometry.dimensions.heightUm > plate.bedHeightUm
    ) {
      return { kind: "artifact_geometry_mismatch", token: unit.token };
    }
  }

  const plates = input.plates.map((plate): GeneratedAcceptedPlate3mf => ({
    plateId: plate.plateId,
    ordinal: plate.ordinal,
    entryName: entryName(plate.ordinal),
    bytes: encodeAcceptedPlate3mf(
      plate.units.map((unit) => {
        const geometry = loaded.geometryByToken.get(unit.token);
        if (!geometry) throw new Error("Accepted Plate export mesh is missing");
        return {
          token: unit.token,
          objectName: unit.objectName,
          xUm: unit.xUm,
          yUm: unit.yUm,
          mesh: geometry.mesh,
        };
      }),
    ),
  }));
  const manifest = manifestBytes(input);
  const baseOutputBytes =
    manifest.length + plates.reduce((sum, plate) => sum + plate.bytes.length, 0);
  if (baseOutputBytes > dependencies.limits.maxOutputBytes) {
    return { kind: "limit_exceeded", limit: "output_bytes" };
  }
  let bundle: Uint8Array | null = null;
  if (command.includeBundle) {
    const entries: Record<string, Uint8Array> = { "manifest.json": manifest };
    for (const plate of plates) entries[plate.entryName] = plate.bytes;
    bundle = dependencies.bundleEncoder
      ? dependencies.bundleEncoder(entries)
      : zipSync(entries, { level: 6, mtime: acceptedPlateZipEpoch() });
  }
  const outputBytes = baseOutputBytes + (bundle?.length ?? 0);
  if (outputBytes > dependencies.limits.maxOutputBytes) {
    return { kind: "limit_exceeded", limit: "output_bytes" };
  }
  return {
    kind: "generated",
    basis: input.basis,
    plateRevisionId: input.plateRevisionId,
    plateRevisionNumber: input.plateRevisionNumber,
    layoutDigest: input.layoutDigest,
    manifest,
    plates,
    bundle,
  };
}
