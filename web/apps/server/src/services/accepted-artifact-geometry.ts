import { buffer } from "node:stream/consumers";
import { parseAcceptedStlMesh, stlMeshDimensionsUm, type StlMesh } from "@print-partner/domain";
import type { AcceptedOperationalArtifact } from "../db/accepted-plan-operational.js";
import type { RequiredUnitToken } from "./required-units.js";
import {
  openVerifiedAcceptedArtifact,
  type AcceptedArtifactVerificationFailure,
} from "./accepted-artifacts.js";

export type AcceptedArtifactOpener = typeof openVerifiedAcceptedArtifact;

export type AcceptedArtifactGeometryLimit =
  "artifact_bytes" | "total_source_bytes" | "objects" | "triangles";

export type AcceptedArtifactGeometryLimits = Readonly<{
  maxArtifactBytes?: number;
  maxTotalSourceBytes: number;
  maxObjects: number;
  maxTriangles: number;
}>;

type AcceptedArtifactGeometry = Readonly<{
  mesh: StlMesh;
  dimensions: Readonly<{
    widthUm: number;
    depthUm: number;
    heightUm: number;
  }>;
}>;

export type LoadAcceptedArtifactGeometryResult =
  | {
      readonly kind: "ready";
      readonly geometryByToken: ReadonlyMap<RequiredUnitToken, AcceptedArtifactGeometry>;
    }
  | {
      readonly kind: "artifact_unavailable";
      readonly token: RequiredUnitToken;
      readonly reason: "legacy" | "untracked_source" | AcceptedArtifactVerificationFailure;
    }
  | { readonly kind: "invalid_stl"; readonly token: RequiredUnitToken }
  | { readonly kind: "degenerate_geometry"; readonly token: RequiredUnitToken }
  | { readonly kind: "limit_exceeded"; readonly limit: AcceptedArtifactGeometryLimit };

type GeometryUnit = Readonly<{
  token: RequiredUnitToken;
  artifact: AcceptedOperationalArtifact;
}>;

function descriptorKey(
  artifact: Extract<AcceptedOperationalArtifact, { kind: "tracked" }>,
): string {
  return JSON.stringify([
    artifact.sourceId,
    artifact.sourceRevisionId,
    artifact.snapshotRoot,
    artifact.relativePath,
    artifact.expectedSha256,
  ]);
}

export async function loadAcceptedArtifactGeometry(
  input: Readonly<{
    reposDir: string;
    units: readonly GeometryUnit[];
    limits: AcceptedArtifactGeometryLimits;
    openArtifact?: AcceptedArtifactOpener;
  }>,
): Promise<LoadAcceptedArtifactGeometryResult> {
  if (!Object.values(input.limits).every((value) => Number.isSafeInteger(value) && value >= 0)) {
    throw new RangeError("Accepted artifact geometry limits must be nonnegative safe integers");
  }
  if (input.units.length > input.limits.maxObjects) {
    return { kind: "limit_exceeded", limit: "objects" };
  }

  const geometryByDescriptor = new Map<string, AcceptedArtifactGeometry>();
  const geometryByToken = new Map<RequiredUnitToken, AcceptedArtifactGeometry>();
  let totalSourceBytes = 0;
  let repeatedTriangles = 0;

  for (const unit of input.units) {
    if (unit.artifact.kind === "unavailable") {
      return { kind: "artifact_unavailable", token: unit.token, reason: unit.artifact.reason };
    }
    const key = descriptorKey(unit.artifact);
    let geometry = geometryByDescriptor.get(key);
    if (!geometry) {
      const opened = (input.openArtifact ?? openVerifiedAcceptedArtifact)({
        reposDir: input.reposDir,
        artifact: unit.artifact,
        maxBytes: input.limits.maxArtifactBytes,
      });
      if (opened.kind !== "verified") {
        if (opened.reason === "too_large") {
          return { kind: "limit_exceeded", limit: "artifact_bytes" };
        }
        return { kind: "artifact_unavailable", token: unit.token, reason: opened.reason };
      }
      totalSourceBytes += opened.lease.size;
      if (totalSourceBytes > input.limits.maxTotalSourceBytes) {
        opened.lease.close();
        return { kind: "limit_exceeded", limit: "total_source_bytes" };
      }
      let bytes: Buffer;
      try {
        bytes = await buffer(opened.lease.createReadStream());
      } catch {
        return { kind: "artifact_unavailable", token: unit.token, reason: "io_error" };
      } finally {
        opened.lease.close();
      }
      const mesh = parseAcceptedStlMesh(bytes);
      if (!mesh) return { kind: "invalid_stl", token: unit.token };
      const dimensions = stlMeshDimensionsUm(mesh);
      if (!dimensions) return { kind: "degenerate_geometry", token: unit.token };
      geometry = { mesh, dimensions };
      geometryByDescriptor.set(key, geometry);
    }
    repeatedTriangles += geometry.mesh.faces.length;
    if (repeatedTriangles > input.limits.maxTriangles) {
      return { kind: "limit_exceeded", limit: "triangles" };
    }
    geometryByToken.set(unit.token, geometry);
  }

  return { kind: "ready", geometryByToken };
}
