import { createHash } from "node:crypto";
import {
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { strFromU8, unzipSync } from "fflate";
import { MAX_ACCEPTED_PLATES } from "@print-partner/domain";
import { afterEach, describe, expect, it } from "vitest";
import { acceptedPlanBasis } from "../db/accepted-plan-progress.js";
import type { AcceptedOperationalArtifact } from "../db/accepted-plan-operational.js";
import type {
  AcceptedPlateExportInput,
  ReadAcceptedPlateExportInputResult,
} from "../db/accepted-plates.js";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { parseRequiredUnitToken } from "./required-units.js";
import {
  generateAcceptedPlate3mfArtifacts,
  type AcceptedPlate3mfLimit,
  type AcceptedPlate3mfLimits,
} from "./accepted-plate-3mf.js";
import { openVerifiedAcceptedArtifact } from "./accepted-artifacts.js";

const roots: string[] = [];
const databases: SqliteDatabase[] = [];
const generous: AcceptedPlate3mfLimits = {
  maxArtifactBytes: 1_000_000,
  maxTotalSourceBytes: 2_000_000,
  maxObjects: 100,
  maxTriangles: 100,
  maxOutputBytes: 10_000_000,
  maxPlates: 100,
};

afterEach(() => {
  for (const database of databases.splice(0)) database.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "pp-accepted-plate-3mf-"));
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
  const tokens = [1, 2].map((value) =>
    parseRequiredUnitToken(`ppu_${value.toString(16).padStart(32, "0")}`),
  );
  const input: AcceptedPlateExportInput = {
    basis: {
      profileId: 7,
      planVersion: 3,
      revisionId: 11,
      revisionDigest: "a".repeat(64),
      requiredUnitMappingDigest: "b".repeat(64),
    },
    plateRevisionId: 19,
    plateRevisionNumber: 2,
    layoutDigest: "c".repeat(64),
    plates: [
      {
        plateId: "z-first",
        ordinal: 1,
        printerId: "p1",
        printerName: "Printer One",
        printerModel: "Model One",
        bedWidthUm: 250_000,
        bedDepthUm: 250_000,
        bedHeightUm: 250_000,
        marginUm: 0,
        units: [
          {
            token: tokens[0]!,
            objectName: "First & Exact",
            xUm: 1_250,
            yUm: 2_500,
            widthUm: 3_000,
            depthUm: 5_000,
            heightUm: 10_000,
            artifact,
          },
        ],
      },
      {
        plateId: "a-second",
        ordinal: 2,
        printerId: "p2",
        printerName: "Printer Two",
        printerModel: "Model Two",
        bedWidthUm: 250_000,
        bedDepthUm: 250_000,
        bedHeightUm: 250_000,
        marginUm: 0,
        units: [
          {
            token: tokens[1]!,
            objectName: "Second Exact",
            xUm: 5_000,
            yUm: 6_000,
            widthUm: 3_000,
            depthUm: 5_000,
            heightUm: 10_000,
            artifact,
          },
        ],
      },
      {
        plateId: "empty-third",
        ordinal: 3,
        printerId: "p3",
        printerName: "Printer Three",
        printerModel: "Model Three",
        bedWidthUm: 250_000,
        bedDepthUm: 250_000,
        bedHeightUm: 250_000,
        marginUm: 0,
        units: [],
      },
    ],
  };
  return { reposDir, snapshotRoot, bytes, artifact, input, tokens };
}

function dependencies(input: AcceptedPlateExportInput, reposDir: string, limits = generous) {
  return {
    repository: { readAcceptedPlateExportInput: () => ({ kind: "ready" as const, input }) },
    reposDir,
    limits,
  };
}

describe("generateAcceptedPlate3mfArtifacts", () => {
  it("rejects a changed Plate revision before opening artifacts or encoding a bundle", async () => {
    const { reposDir, input } = fixture();
    let artifactOpens = 0;
    let bundleEncodes = 0;

    const result = await generateAcceptedPlate3mfArtifacts(
      {
        ...dependencies(input, reposDir),
        openArtifact: () => {
          artifactOpens += 1;
          throw new Error("artifact opened");
        },
        bundleEncoder: () => {
          bundleEncodes += 1;
          throw new Error("bundle encoded");
        },
      },
      { profileId: 7, expectedPlateRevisionId: input.plateRevisionId + 1, includeBundle: true },
    );

    expect(result).toEqual({ kind: "plate_revision_changed" });
    expect(artifactOpens).toBe(0);
    expect(bundleEncodes).toBe(0);
  });

  it("reads the accepted Plate revision once during generation", async () => {
    const { reposDir, input } = fixture();
    let reads = 0;
    const result = await generateAcceptedPlate3mfArtifacts(
      {
        repository: {
          readAcceptedPlateExportInput: () => {
            reads += 1;
            return reads === 1
              ? { kind: "ready", input }
              : {
                  kind: "ready",
                  input: { ...input, plateRevisionId: input.plateRevisionId + 1 },
                };
          },
        },
        reposDir,
        limits: generous,
      },
      {
        profileId: input.basis.profileId,
        expectedPlateRevisionId: input.plateRevisionId,
      },
    );

    expect(result).toMatchObject({
      kind: "generated",
      plateRevisionId: input.plateRevisionId,
      layoutDigest: input.layoutDigest,
    });
    expect(reads).toBe(1);
  });

  const repositoryStates = [
    [{ kind: "empty_plan" }, { kind: "empty_plan" }],
    [{ kind: "plates_not_published" }, { kind: "plates_not_published" }],
    [{ kind: "stale_accepted_plan" }, { kind: "stale_accepted_plan" }],
    [{ kind: "unplaced_units" }, { kind: "unplaced_units" }],
    [
      { kind: "accepted_state_unavailable", reason: "compatibility_dirty" },
      { kind: "accepted_state_unavailable", reason: "compatibility_dirty" },
    ],
    [
      { kind: "accepted_state_unavailable", reason: "uninitialized" },
      { kind: "accepted_state_unavailable", reason: "uninitialized" },
    ],
    [{ kind: "transaction_unavailable" }, { kind: "transaction_unavailable" }],
  ] satisfies readonly (readonly [ReadAcceptedPlateExportInputResult, object])[];

  it.each(repositoryStates)(
    "forwards accepted repository state %# without inventing output",
    async (state, expected) => {
      const result = await generateAcceptedPlate3mfArtifacts(
        {
          repository: { readAcceptedPlateExportInput: () => state },
          reposDir: "/unused",
          limits: generous,
        },
        { profileId: 7, expectedPlateRevisionId: 19 },
      );
      expect(result).toEqual(expected);
    },
  );

  it("preserves ordinal order, exact identity, empty Plates, and deterministic bytes", async () => {
    const { reposDir, input, tokens } = fixture();
    const first = await generateAcceptedPlate3mfArtifacts(dependencies(input, reposDir), {
      profileId: 7,
      expectedPlateRevisionId: 19,
      includeBundle: true,
    });
    const second = await generateAcceptedPlate3mfArtifacts(dependencies(input, reposDir), {
      profileId: 7,
      expectedPlateRevisionId: 19,
      includeBundle: true,
    });
    expect(first).toEqual(second);
    expect(first.kind).toBe("generated");
    if (first.kind !== "generated") throw new Error("generation failed");
    expect(first.plates.map((plate) => [plate.plateId, plate.entryName])).toEqual([
      ["z-first", "plates/0001.3mf"],
      ["a-second", "plates/0002.3mf"],
      ["empty-third", "plates/0003.3mf"],
    ]);
    const models = first.plates.map((plate) =>
      strFromU8(unzipSync(plate.bytes)["3D/3dmodel.model"]!),
    );
    expect(models.join("\n").match(new RegExp(tokens[0]!, "g"))).toHaveLength(2);
    expect(models[0]).toContain('name="First &amp; Exact"');
    expect(models[0]).toContain('vertex x="1.25" y="2.5" z="0"');
    expect(Object.keys(unzipSync(first.bundle!))).toEqual([
      "manifest.json",
      "plates/0001.3mf",
      "plates/0002.3mf",
      "plates/0003.3mf",
    ]);
  });

  it("generates from a real tracked accepted Plan and durable Plate revision", async () => {
    const root = mkdtempSync(join(tmpdir(), "pp-accepted-plate-3mf-integration-"));
    roots.push(root);
    const database = new SqliteDatabase(root);
    databases.push(database);
    database.connect();
    const repo = new AppRepository(getDb(database), "default", database.reposDir);
    const source = repo.createSource({
      name: "Tracked 3MF source",
      url: "https://example.test/tracked-3mf",
      source_kind: "github",
    });
    const observed = repo.getProjectRow(source.id);
    if (!observed) throw new Error("tracked Source is missing");
    const locator = `${source.id}/revisions/accepted`;
    const snapshotRoot = join(database.reposDir, locator);
    mkdirSync(snapshotRoot, { recursive: true });
    const stl = Buffer.from(`solid tracked
facet normal 0 0 1
outer loop
vertex -2 -3 -4
vertex 1 -3 -4
vertex -2 2 6
endloop
endfacet
endsolid tracked`);
    writeFileSync(join(snapshotRoot, "bracket.stl"), stl);
    const sourceRevision = repo.recordSourceRevision({
      sourceId: source.id,
      upstreamRevisionKey: "accepted",
      manifestDigest: "d".repeat(64),
      snapshotLocator: locator,
      syncedAt: "2026-08-21T12:00:00.000Z",
      completeness: "complete",
    });
    repo.activateSourceRevision({
      sourceId: source.id,
      revisionId: sourceRevision.id,
      observed,
      sourceVersion: sourceRevision.upstream_revision_key,
    });
    const profile = repo.createProfile("Tracked 3MF Build", source.id);
    const created = repo.recomputePlanDraft({
      profileId: profile.id,
      actor: "test:user",
      idempotencyKey: "3mf-draft",
    });
    if (created.kind !== "created") throw new Error("tracked draft was not created");
    const reconciled = repo.savePlanDraftRequiredUnitReconciliation({
      profileId: profile.id,
      draftId: created.draft.id,
      expectedSnapshotDigest: created.draft.snapshotDigest,
      decisions: [],
      actorId: "test:user",
      idempotencyKey: "3mf-reconciliation",
    });
    if (reconciled.kind !== "saved") throw new Error("tracked reconciliation failed");
    const applied = repo.applyPlanChanges({
      profileId: profile.id,
      draftId: created.draft.id,
      expectedSnapshotDigest: reconciled.draft.snapshotDigest,
      expectedLifecycleVersion: 0,
      expectedBase: { kind: "empty", planVersion: 0 },
      actorId: "test:user",
      idempotencyKey: "3mf-apply",
    });
    if (applied.kind !== "applied") throw new Error("tracked Plan was not applied");
    const accepted = repo.readAcceptedPlanOperationalSnapshot(profile.id);
    if (accepted.kind !== "ready") throw new Error("tracked accepted Plan is unavailable");
    const part = accepted.snapshot.parts.find((candidate) => candidate.included);
    const unit = part?.units.find((candidate) => candidate.required);
    if (!part || !unit || part.artifact.kind !== "tracked")
      throw new Error("tracked accepted artifact is missing");
    expect(
      repo.publishAcceptedPlates({
        profileId: profile.id,
        expected: acceptedPlanBasis(accepted.snapshot),
        expectedPlateRevisionId: null,
        plates: [
          {
            plateId: "tracked-plate",
            printerId: "printer-one",
            printerName: "Printer One",
            printerModel: "Model One",
            bedWidthUm: 250_000,
            bedDepthUm: 250_000,
            bedHeightUm: 250_000,
            marginUm: 5_000,
            units: [
              {
                token: unit.token,
                xUm: 5_000,
                yUm: 5_000,
                widthUm: 3_000,
                depthUm: 5_000,
                heightUm: 10_000,
              },
            ],
          },
        ],
      }),
    ).toMatchObject({ kind: "published" });

    const plateRevision = repo.readAcceptedPlates(profile.id);
    if (plateRevision.kind !== "ready") throw new Error("tracked Plate revision is unavailable");
    const generated = await generateAcceptedPlate3mfArtifacts(
      { repository: repo, reposDir: database.reposDir, limits: generous },
      {
        profileId: profile.id,
        expectedPlateRevisionId: plateRevision.plateRevisionId,
      },
    );
    expect(generated.kind).toBe("generated");
    if (generated.kind !== "generated") throw new Error("tracked generation failed");
    const xml = strFromU8(unzipSync(generated.plates[0]!.bytes)["3D/3dmodel.model"]!);
    expect(xml).toContain(`name="${unit.objectName}"`);
    expect(xml).toContain(`<object id="1" name="${unit.objectName}" partnumber="${unit.token}"`);
    expect(xml).toContain(`<item objectid="1" partnumber="${unit.token}"/>`);
    expect(xml).toContain('vertex x="5" y="5" z="0"');
    expect(strFromU8(generated.manifest)).toContain(part.artifact.expectedSha256);
  });

  it("fails the whole operation on unavailable, mismatched, and invalid artifacts", async () => {
    const { reposDir, snapshotRoot, artifact, input, tokens } = fixture();
    const cases: Array<[AcceptedPlateExportInput, object]> = [
      [
        {
          ...input,
          plates: [
            {
              ...input.plates[0]!,
              units: [
                {
                  ...input.plates[0]!.units[0]!,
                  artifact: { kind: "unavailable", reason: "legacy" },
                },
              ],
            },
          ],
        },
        { kind: "artifact_unavailable", token: tokens[0], reason: "legacy" },
      ],
      [
        {
          ...input,
          plates: [
            {
              ...input.plates[0]!,
              units: [
                {
                  ...input.plates[0]!.units[0]!,
                  artifact: { kind: "unavailable", reason: "untracked_source" },
                },
              ],
            },
          ],
        },
        { kind: "artifact_unavailable", token: tokens[0], reason: "untracked_source" },
      ],
      [
        {
          ...input,
          plates: [
            {
              ...input.plates[0]!,
              units: [
                {
                  ...input.plates[0]!.units[0]!,
                  artifact: { ...artifact, expectedSha256: "f".repeat(64) },
                },
              ],
            },
          ],
        },
        { kind: "artifact_unavailable", token: tokens[0], reason: "digest_mismatch" },
      ],
    ];
    writeFileSync(join(snapshotRoot, "bad.stl"), "not an stl");
    const bad = Buffer.from("not an stl");
    cases.push([
      {
        ...input,
        plates: [
          {
            ...input.plates[0]!,
            units: [
              {
                ...input.plates[0]!.units[0]!,
                artifact: {
                  kind: "tracked",
                  sourceId: 1,
                  sourceRevisionId: 2,
                  snapshotRoot,
                  relativePath: "bad.stl",
                  expectedSha256: createHash("sha256").update(bad).digest("hex"),
                },
              },
            ],
          },
        ],
      },
      { kind: "invalid_stl", token: tokens[0] },
    ]);
    for (const [candidate, expected] of cases) {
      await expect(
        generateAcceptedPlate3mfArtifacts(dependencies(candidate, reposDir), {
          profileId: 7,
          expectedPlateRevisionId: candidate.plateRevisionId,
        }),
      ).resolves.toMatchObject(expected);
    }
  });

  it("maps parseable degenerate STL geometry to the export mismatch result", async () => {
    const { reposDir, snapshotRoot, input, tokens } = fixture();
    const bytes = Buffer.from(`solid degenerate
facet normal 0 0 1
outer loop
vertex 0 0 0
vertex 10 0 0
vertex 0 20 0
endloop
endfacet
endsolid degenerate`);
    writeFileSync(join(snapshotRoot, "degenerate.stl"), bytes);
    const artifact: Extract<AcceptedOperationalArtifact, { kind: "tracked" }> = {
      kind: "tracked",
      sourceId: 1,
      sourceRevisionId: 2,
      snapshotRoot,
      relativePath: "degenerate.stl",
      expectedSha256: createHash("sha256").update(bytes).digest("hex"),
    };
    const candidate = {
      ...input,
      plates: [
        {
          ...input.plates[0]!,
          units: [{ ...input.plates[0]!.units[0]!, artifact }],
        },
      ],
    };

    await expect(
      generateAcceptedPlate3mfArtifacts(dependencies(candidate, reposDir), {
        profileId: 7,
        expectedPlateRevisionId: candidate.plateRevisionId,
      }),
    ).resolves.toEqual({ kind: "artifact_geometry_mismatch", token: tokens[0] });
  });

  it.each([
    { widthUm: 3_001 },
    { depthUm: 5_001 },
    { heightUm: 10_001 },
    { xUm: 248_000 },
    { yUm: 246_000 },
  ])("rejects verified geometry that disagrees with captured Plate bounds %#", async (change) => {
    const { reposDir, input, tokens } = fixture();
    const unit = { ...input.plates[0]!.units[0]!, ...change };
    const candidate = { ...input, plates: [{ ...input.plates[0]!, units: [unit] }] };
    await expect(
      generateAcceptedPlate3mfArtifacts(dependencies(candidate, reposDir), {
        profileId: 7,
        expectedPlateRevisionId: candidate.plateRevisionId,
      }),
    ).resolves.toEqual({
      kind: "artifact_geometry_mismatch",
      token: tokens[0],
    });
  });

  it.each([
    { marginUm: 2_000 },
    { bedHeightUm: 9_999 },
    { bedWidthUm: 4_249 },
    { bedDepthUm: 7_499 },
  ])("rechecks captured build volume against verified geometry %#", async (plateChange) => {
    const { reposDir, input, tokens } = fixture();
    const candidate = { ...input, plates: [{ ...input.plates[0]!, ...plateChange }] };
    await expect(
      generateAcceptedPlate3mfArtifacts(dependencies(candidate, reposDir), {
        profileId: 7,
        expectedPlateRevisionId: candidate.plateRevisionId,
      }),
    ).resolves.toEqual({
      kind: "artifact_geometry_mismatch",
      token: tokens[0],
    });
  });

  it.each(["replace", "append"] satisfies readonly ("replace" | "append")[])(
    "uses the verified descriptor lease across a source %s race",
    async (race) => {
      const { reposDir, snapshotRoot, input } = fixture();
      const path = join(snapshotRoot, "part.stl");
      const result = await generateAcceptedPlate3mfArtifacts(
        {
          ...dependencies(input, reposDir),
          openArtifact(request) {
            const opened = openVerifiedAcceptedArtifact(request);
            if (opened.kind !== "verified") return opened;
            if (race === "append") appendFileSync(path, "malicious append");
            else {
              const replacement = join(snapshotRoot, "replacement.stl");
              writeFileSync(replacement, "not an stl");
              renameSync(replacement, path);
            }
            return opened;
          },
        },
        { profileId: 7, expectedPlateRevisionId: input.plateRevisionId },
      );

      expect(result.kind).toBe("generated");
      if (result.kind !== "generated") throw new Error("generation failed");
      const xml = strFromU8(unzipSync(result.plates[0]!.bytes)["3D/3dmodel.model"]!);
      expect(xml).toContain('vertex x="1.25" y="2.5" z="0"');
    },
  );

  it("contains no compatibility packing or path-based mesh imports", () => {
    const source = readFileSync(new URL("./accepted-plate-3mf.ts", import.meta.url), "utf8");
    for (const forbidden of [
      "MergePartExport",
      "buildMergePartsForProfile",
      "loadStlMesh",
      "assignPartsToPrinters",
      "matchKey",
      "fleet",
      "filament",
      "compatibility Parts",
    ])
      expect(source).not.toContain(forbidden);
  });

  it("passes each limit exactly and fails at one less", async () => {
    const { reposDir, input, bytes } = fixture();
    const baseline = await generateAcceptedPlate3mfArtifacts(dependencies(input, reposDir), {
      profileId: 7,
      expectedPlateRevisionId: input.plateRevisionId,
      includeBundle: true,
    });
    if (baseline.kind !== "generated") throw new Error("baseline failed");
    const outputBytes =
      baseline.manifest.length +
      baseline.plates.reduce((sum, plate) => sum + plate.bytes.length, 0) +
      baseline.bundle!.length;
    const exact = {
      ...generous,
      maxArtifactBytes: bytes.length,
      maxTotalSourceBytes: bytes.length,
      maxObjects: 2,
      maxTriangles: 2,
      maxOutputBytes: outputBytes,
    };
    await expect(
      generateAcceptedPlate3mfArtifacts(dependencies(input, reposDir, exact), {
        profileId: 7,
        expectedPlateRevisionId: input.plateRevisionId,
        includeBundle: true,
      }),
    ).resolves.toMatchObject({ kind: "generated" });
    for (const [limit, value] of [
      ["artifact_bytes", { maxArtifactBytes: bytes.length - 1 }],
      ["total_source_bytes", { maxTotalSourceBytes: bytes.length - 1 }],
      ["objects", { maxObjects: 1 }],
      ["triangles", { maxTriangles: 1 }],
      ["output_bytes", { maxOutputBytes: outputBytes - 1 }],
    ] satisfies readonly (readonly [AcceptedPlate3mfLimit, Partial<AcceptedPlate3mfLimits>])[]) {
      await expect(
        generateAcceptedPlate3mfArtifacts(dependencies(input, reposDir, { ...exact, ...value }), {
          profileId: 7,
          expectedPlateRevisionId: input.plateRevisionId,
          includeBundle: true,
        }),
      ).resolves.toEqual({ kind: "limit_exceeded", limit });
    }
  });

  it("checks base output before allocating an optional bundle", async () => {
    const { reposDir, input } = fixture();
    const baseline = await generateAcceptedPlate3mfArtifacts(dependencies(input, reposDir), {
      profileId: 7,
      expectedPlateRevisionId: input.plateRevisionId,
    });
    if (baseline.kind !== "generated") throw new Error("baseline failed");
    const baseBytes =
      baseline.manifest.length +
      baseline.plates.reduce((sum, plate) => sum + plate.bytes.length, 0);
    const result = await generateAcceptedPlate3mfArtifacts(
      {
        ...dependencies(input, reposDir, { ...generous, maxOutputBytes: baseBytes - 1 }),
        bundleEncoder() {
          throw new Error("bundle must not be allocated");
        },
      },
      { profileId: 7, expectedPlateRevisionId: input.plateRevisionId, includeBundle: true },
    );
    expect(result).toEqual({ kind: "limit_exceeded", limit: "output_bytes" });
  });

  it("passes the Plate limit exactly and rejects one over before artifact work", async () => {
    const { reposDir, input } = fixture();
    await expect(
      generateAcceptedPlate3mfArtifacts(
        dependencies(input, reposDir, { ...generous, maxPlates: 3 }),
        { profileId: 7, expectedPlateRevisionId: input.plateRevisionId },
      ),
    ).resolves.toMatchObject({ kind: "generated" });
    await expect(
      generateAcceptedPlate3mfArtifacts(
        {
          ...dependencies(input, reposDir, { ...generous, maxPlates: 2 }),
          openArtifact() {
            throw new Error("artifact work must not start");
          },
        },
        { profileId: 7, expectedPlateRevisionId: input.plateRevisionId },
      ),
    ).resolves.toEqual({ kind: "limit_exceeded", limit: "plates" });
    await expect(
      generateAcceptedPlate3mfArtifacts(
        dependencies(input, reposDir, { ...generous, maxPlates: MAX_ACCEPTED_PLATES + 1 }),
        { profileId: 7, expectedPlateRevisionId: input.plateRevisionId },
      ),
    ).rejects.toThrow(/limits/i);
  });
});
