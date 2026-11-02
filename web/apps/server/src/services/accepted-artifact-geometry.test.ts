import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import type { AcceptedOperationalArtifact } from "../db/accepted-plan-operational.js";
import { openVerifiedAcceptedArtifact } from "./accepted-artifacts.js";
import { loadAcceptedArtifactGeometry } from "./accepted-artifact-geometry.js";
import { parseRequiredUnitToken } from "./required-units.js";

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "pp-accepted-geometry-"));
  roots.push(root);
  const reposDir = join(root, "repos");
  const snapshotRoot = join(reposDir, "snapshots", "one");
  mkdirSync(snapshotRoot, { recursive: true });
  const bytes = Buffer.from(`solid accepted
facet normal 0 0 1
outer loop
vertex -2 -3 -4
vertex 1 -3 -4
vertex -2 2 6
endloop
endfacet
endsolid accepted`);
  writeFileSync(join(snapshotRoot, "part.stl"), bytes);
  const artifact: Extract<AcceptedOperationalArtifact, { kind: "tracked" }> = {
    kind: "tracked",
    sourceId: 1,
    sourceRevisionId: 2,
    snapshotRoot,
    relativePath: "part.stl",
    expectedSha256: createHash("sha256").update(bytes).digest("hex"),
  };
  return { artifact, reposDir };
}

function trackedFixture(bytes: Buffer) {
  const root = mkdtempSync(join(tmpdir(), "pp-accepted-geometry-tracked-"));
  roots.push(root);
  const reposDir = join(root, "repos");
  const snapshotRoot = join(reposDir, "snapshots", "tracked");
  mkdirSync(snapshotRoot, { recursive: true });
  writeFileSync(join(snapshotRoot, "part.stl"), bytes);
  const artifact: Extract<AcceptedOperationalArtifact, { kind: "tracked" }> = {
    kind: "tracked",
    sourceId: 8,
    sourceRevisionId: 13,
    snapshotRoot,
    relativePath: "part.stl",
    expectedSha256: createHash("sha256").update(bytes).digest("hex"),
  };
  return { artifact, reposDir };
}

describe("loadAcceptedArtifactGeometry", () => {
  it("opens one accepted descriptor once and projects geometry to every Required unit", async () => {
    const { artifact, reposDir } = fixture();
    const tokens = [1, 2].map((value) =>
      parseRequiredUnitToken(`ppu_${value.toString(16).padStart(32, "0")}`),
    );
    let opens = 0;

    const result = await loadAcceptedArtifactGeometry({
      reposDir,
      units: tokens.map((token) => ({ token, artifact })),
      limits: {
        maxArtifactBytes: 1_000_000,
        maxTotalSourceBytes: 1_000_000,
        maxObjects: 2,
        maxTriangles: 2,
      },
      openArtifact: (request) => {
        opens += 1;
        return openVerifiedAcceptedArtifact(request);
      },
    });

    expect(opens).toBe(1);
    expect(result.kind).toBe("ready");
    if (result.kind !== "ready") throw new Error("Accepted geometry load failed");
    expect(
      [...result.geometryByToken.entries()].map(([token, geometry]) => [
        token,
        geometry.dimensions,
      ]),
    ).toEqual([
      [tokens[0], { widthUm: 3_000, depthUm: 5_000, heightUm: 10_000 }],
      [tokens[1], { widthUm: 3_000, depthUm: 5_000, heightUm: 10_000 }],
    ]);
  });

  it("loads exact tracked binary STL bytes once for duplicate descriptor units", async () => {
    const bytes = Buffer.alloc(134);
    bytes.write("tracked binary", 0, "ascii");
    bytes.writeUInt32LE(1, 80);
    let offset = 96;
    for (const [x, y, z] of [
      [-2, -3, -4],
      [1, -3, -4],
      [-2, 2, 6],
    ]) {
      bytes.writeFloatLE(x, offset);
      bytes.writeFloatLE(y, offset + 4);
      bytes.writeFloatLE(z, offset + 8);
      offset += 12;
    }
    const { artifact, reposDir } = trackedFixture(bytes);
    const tokens = [4, 5].map((value) =>
      parseRequiredUnitToken(`ppu_${value.toString(16).padStart(32, "0")}`),
    );
    let opens = 0;

    const result = await loadAcceptedArtifactGeometry({
      reposDir,
      units: tokens.map((token) => ({ token, artifact })),
      limits: {
        maxArtifactBytes: bytes.length,
        maxTotalSourceBytes: bytes.length,
        maxObjects: 2,
        maxTriangles: 2,
      },
      openArtifact: (request) => {
        opens += 1;
        return openVerifiedAcceptedArtifact(request);
      },
    });

    expect(opens).toBe(1);
    expect(result.kind).toBe("ready");
    if (result.kind !== "ready") throw new Error("tracked binary geometry load failed");
    expect([...result.geometryByToken].map(([token, value]) => [token, value.dimensions])).toEqual([
      [tokens[0], { widthUm: 3_000, depthUm: 5_000, heightUm: 10_000 }],
      [tokens[1], { widthUm: 3_000, depthUm: 5_000, heightUm: 10_000 }],
    ]);
  });

  it("rejects unavailable accepted artifacts before parsing", async () => {
    const token = parseRequiredUnitToken("ppu_00000000000000000000000000000001");
    await expect(
      loadAcceptedArtifactGeometry({
        reposDir: "/tmp/unused-accepted-geometry",
        units: [{ token, artifact: { kind: "unavailable", reason: "untracked_source" } }],
        limits: {
          maxArtifactBytes: 1,
          maxTotalSourceBytes: 1,
          maxObjects: 1,
          maxTriangles: 1,
        },
      }),
    ).resolves.toEqual({ kind: "artifact_unavailable", token, reason: "untracked_source" });
  });

  it("distinguishes parseable STL with degenerate geometry from invalid STL", async () => {
    const bytes = Buffer.from(`solid degenerate
facet normal 0 0 1
outer loop
vertex 0 0 0
vertex 10 0 0
vertex 0 20 0
endloop
endfacet
endsolid degenerate`);
    const { artifact, reposDir } = trackedFixture(bytes);
    const token = parseRequiredUnitToken("ppu_00000000000000000000000000000003");

    await expect(
      loadAcceptedArtifactGeometry({
        reposDir,
        units: [{ token, artifact }],
        limits: {
          maxArtifactBytes: 1_000_000,
          maxTotalSourceBytes: 1_000_000,
          maxObjects: 1,
          maxTriangles: 1,
        },
      }),
    ).resolves.toEqual({ kind: "degenerate_geometry", token });
  });
});
