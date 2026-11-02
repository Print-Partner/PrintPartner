import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { strFromU8, unzipSync } from "fflate";
import { afterEach, describe, expect, it } from "vitest";
import { runWithTenantDiskQuota, TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";
import type {
  AcceptedOperationalArtifact,
  AcceptedPlanOperationalSnapshot,
} from "../db/accepted-plan-operational.js";
import type {
  AcceptedPlate,
  ReadAcceptedPlateWorkspaceInputResult,
} from "../db/accepted-plates.js";
import { parseRequiredUnitToken } from "./required-units.js";
import {
  materializeDirectExport3mf,
  type MaterializeDirectExport3mfDependencies,
} from "./accepted-direct-export-3mf.js";

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

const tokens = [1, 2].map((value) =>
  parseRequiredUnitToken(`ppu_${value.toString(16).padStart(32, "0")}`),
);

/** A 4mm x 4mm x 4mm corner tetrahedron with its minimum at the origin. */
const stl = Buffer.from(`solid accepted
facet normal 0 0 -1
outer loop
vertex 0 0 0
vertex 4 0 0
vertex 0 4 0
endloop
endfacet
facet normal 0 -1 0
outer loop
vertex 0 0 0
vertex 0 0 4
vertex 4 0 0
endloop
endfacet
facet normal -1 0 0
outer loop
vertex 0 0 0
vertex 0 4 0
vertex 0 0 4
endloop
endfacet
facet normal 1 1 1
outer loop
vertex 4 0 0
vertex 0 0 4
vertex 0 4 0
endloop
endfacet
endsolid accepted`);

function snapshot(artifact: AcceptedOperationalArtifact): AcceptedPlanOperationalSnapshot {
  return {
    format: "accepted-plan-operational-v1",
    profile: { id: 7, name: "Build", orderNumber: null, specialRequest: null, archivedAt: null },
    planVersion: 3,
    revisionId: 11,
    revisionNumber: 1,
    revisionDigest: "a".repeat(64),
    acceptedAt: "2026-01-01T00:00:00.000Z",
    provenance: { kind: "legacy" },
    requiredUnitMappingDigest: "b".repeat(64),
    parts: [
      {
        revisionPartId: 1,
        projectionPartId: 1,
        partKey: "part",
        relativePath: "part.stl",
        filename: "part.stl",
        sourceLayer: "base",
        status: "active",
        roleInferred: "part",
        roleOverride: null,
        effectiveRole: "part",
        filamentColorId: null,
        filamentCustomHex: null,
        spoolmanSpoolId: null,
        quantityInferred: 2,
        quantityOverride: null,
        quantityEffective: 2,
        included: true,
        notes: "",
        githubBlobUrl: null,
        geometrySame: null,
        requirement: null,
        optionGroupId: null,
        manifestSource: null,
        artifact,
        units: tokens.map((token, index) => ({
          unitIndex: index + 1,
          required: true,
          token,
          objectName: `Part ${index + 1}`,
          completed: false,
          assembled: false,
        })),
      },
    ],
  };
}

function plate(
  ordinal: number,
  plateId: string,
  token: string,
  xUm: number,
  yUm: number,
): AcceptedPlate {
  return {
    plateId,
    ordinal,
    printerId: `p${ordinal}`,
    printerName: `Printer ${ordinal}`,
    printerModel: "Model",
    bedWidthUm: 250_000,
    bedDepthUm: 250_000,
    bedHeightUm: 250_000,
    marginUm: 0,
    units: [
      {
        token,
        objectName: "Part",
        xUm,
        yUm,
        widthUm: 4_000,
        depthUm: 4_000,
        heightUm: 4_000,
        placement: "auto",
      },
    ],
  };
}

function fixture(workspace: ReadAcceptedPlateWorkspaceInputResult) {
  const root = mkdtempSync(join(tmpdir(), "pp-direct-export-"));
  roots.push(root);
  const reposDir = join(root, "repos");
  const snapshotRoot = join(reposDir, "snapshots", "one");
  mkdirSync(snapshotRoot, { recursive: true });
  writeFileSync(join(snapshotRoot, "part.stl"), stl);
  const artifact: AcceptedOperationalArtifact = {
    kind: "tracked",
    sourceId: 1,
    sourceRevisionId: 2,
    snapshotRoot,
    relativePath: "part.stl",
    expectedSha256: createHash("sha256").update(stl).digest("hex"),
  };
  const dependencies: MaterializeDirectExport3mfDependencies = {
    reposDir,
    tenantExportsDir: join(root, "exports"),
    repository: {
      getOwnedProfileIdentity: () => ({
        id: 7,
        name: "Build",
        orderNumber: null,
        archivedAt: null,
      }),
      readAcceptedPlanOperationalSnapshot: () => ({ kind: "ready", snapshot: snapshot(artifact) }),
      readAcceptedPlateWorkspaceInput: () => workspace,
    },
  };
  return dependencies;
}

function vertexOrigins(path: string): Array<{ x: number; y: number }> {
  const model = strFromU8(unzipSync(readFileSync(path))["3D/3dmodel.model"]!);
  return model
    .split("<object ")
    .slice(1)
    .map((object) => {
      const vertices = [...object.matchAll(/<vertex x="([^"]+)" y="([^"]+)" z="([^"]+)"\/>/g)];
      return {
        x: Math.min(...vertices.map((vertex) => Number(vertex[1]))),
        y: Math.min(...vertices.map((vertex) => Number(vertex[2]))),
      };
    });
}

const readyWorkspace = (
  plates: readonly AcceptedPlate[],
): ReadAcceptedPlateWorkspaceInputResult => ({
  kind: "ready",
  basis: {
    profileId: 7,
    planVersion: 3,
    revisionId: 11,
    revisionDigest: "a".repeat(64),
    requiredUnitMappingDigest: "b".repeat(64),
  },
  expectedPlateRevisionId: 5,
  plateRevisionId: 5,
  plateRevisionNumber: 1,
  plates,
  units: [],
  undoFromRevisionId: null,
});

describe("materializeDirectExport3mf", () => {
  it("preserves quota rejection from accepted export publication", async () => {
    const dependencies = fixture(readyWorkspace([]));
    const dataDir = join(dependencies.tenantExportsDir, "..");

    await expect(
      runWithTenantDiskQuota(
        {
          dataDir,
          reposDir: dependencies.reposDir,
          tenantId: "default",
          sourceIds: () => [],
          quotaBytes: 1,
        },
        async () =>
          materializeDirectExport3mf(dependencies, {
            profileId: 7,
            tokens: [tokens[0]!],
          }),
      ),
    ).rejects.toBeInstanceOf(TenantDiskQuotaError);
  });

  it("keeps the arrangement the Plates step showed, one Plate beside the next", async () => {
    const dependencies = fixture(
      readyWorkspace([
        plate(1, "plate-one", tokens[0]!, 12_000, 34_000),
        plate(2, "plate-two", tokens[1]!, 12_000, 34_000),
      ]),
    );
    const result = await materializeDirectExport3mf(dependencies, { profileId: 7, tokens });
    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(vertexOrigins(result.absolutePath)).toEqual([
      { x: 12, y: 34 },
      { x: 250 + 10 + 12, y: 34 },
    ]);
  });

  it("spaces unplaced units out instead of stacking them at the origin", async () => {
    const dependencies = fixture({
      kind: "setup",
      basis: {
        profileId: 7,
        planVersion: 3,
        revisionId: 11,
        revisionDigest: "a".repeat(64),
        requiredUnitMappingDigest: "b".repeat(64),
      },
      expectedPlateRevisionId: null,
      units: [],
    });
    const result = await materializeDirectExport3mf(dependencies, { profileId: 7, tokens });
    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(vertexOrigins(result.absolutePath)).toEqual([
      { x: 0, y: 0 },
      { x: 14, y: 0 },
    ]);
  });
});
