import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { mergeConflictIssueMessage } from "@print-partner/domain";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AppRepository } from "../db/repository.js";
import type { AcceptedPlanOperationalSnapshot } from "../db/accepted-plan-operational.js";
import {
  projectAcceptedPlanReview,
  readAcceptedPlanReview,
  summarizeAcceptedPlanReview,
} from "./accepted-plan-review.js";
import { acceptedPartMediaIdentity } from "./accepted-part-media.js";
import { writeAcceptedMediaPng } from "../lib/accepted-media-cache.js";

const roots: string[] = [];

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "print-partner-accepted-review-"));
  roots.push(root);
  const reposDir = join(root, "repos");
  const snapshotRoot = join(reposDir, "snapshots", "one");
  const thumbsDir = join(root, "thumbs");
  mkdirSync(join(snapshotRoot, "parts"), { recursive: true });
  return { reposDir, snapshotRoot, thumbsDir };
}

function snapshot(snapshotRoot: string): AcceptedPlanOperationalSnapshot {
  return {
    format: "accepted-plan-operational-v1",
    profile: {
      id: 7,
      name: "Accepted Plan",
      orderNumber: null,
      specialRequest: null,
      archivedAt: null,
    },
    planVersion: 3,
    revisionId: 11,
    revisionNumber: 2,
    revisionDigest: "d".repeat(64),
    acceptedAt: "2026-08-20T00:00:00.000Z",
    provenance: {
      kind: "tracked",
      inputSetId: 12,
      inputSetDigest: "e".repeat(64),
      inputs: [
        {
          inputId: 31,
          sourceId: 41,
          sourceLayer: "addon:Captured Source",
          layerOrder: 0,
          effectiveNamingDigest: "f".repeat(64),
          trackingKind: "revision",
          sourceRevisionId: 51,
          manifestDigest: "1".repeat(64),
          snapshotRoot,
          sourceSyncedAt: "2026-08-19T00:00:00.000Z",
        },
      ],
    },
    requiredUnitMappingDigest: "2".repeat(64),
    parts: [
      {
        revisionPartId: 62,
        projectionPartId: 72,
        partKey: "parts/widget.stl",
        relativePath: "parts/widget.stl",
        filename: "widget.stl",
        sourceLayer: "addon:Captured Source",
        status: "ok",
        roleInferred: "accent",
        roleOverride: null,
        effectiveRole: "accent",
        filamentColorId: null,
        filamentCustomHex: "#AABBCC",
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
        artifact: {
          kind: "tracked",
          sourceId: 41,
          sourceRevisionId: 51,
          snapshotRoot,
          relativePath: "parts/widget.stl",
          expectedSha256: "a".repeat(64),
        },
        units: [
          {
            unitIndex: 0,
            required: true,
            token: "part:0",
            objectName: "widget",
            completed: true,
            assembled: true,
          },
          {
            unitIndex: 1,
            required: true,
            token: "part:1",
            objectName: "widget",
            completed: false,
            assembled: false,
          },
        ],
      },
    ],
  };
}

function repository(input: {
  profile?: { id: number; name: string } | null;
  accepted: ReturnType<AppRepository["readAcceptedPlanOperationalSnapshot"]>;
}) {
  return {
    getOwnedProfileIdentity: vi.fn(() =>
      input.profile === undefined
        ? { id: 7, name: "Working name", archivedAt: null }
        : input.profile,
    ),
    readAcceptedPlanOperationalSnapshot: vi.fn(() => input.accepted),
  } as unknown as AppRepository;
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("readAcceptedPlanReview", () => {
  it("exposes the complete immutable accepted basis used by progress commands", () => {
    const { snapshotRoot } = fixture();
    const accepted = snapshot(snapshotRoot);

    const body = projectAcceptedPlanReview({
      snapshot: accepted,
      includeExcluded: false,
      availableInputRoots: new Set([31]),
      mediaByPartId: new Map([[72, { artifactMissing: false, thumbEmpty: false }]]),
    });

    expect(body.accepted_basis).toEqual({
      profile_id: 7,
      plan_version: 3,
      plan_revision_id: 11,
      plan_revision_digest: "d".repeat(64),
      required_unit_mapping_digest: "2".repeat(64),
    });
  });

  it("fails closed when an included Part has no captured media observation", () => {
    const { snapshotRoot } = fixture();

    expect(() =>
      projectAcceptedPlanReview({
        snapshot: snapshot(snapshotRoot),
        includeExcluded: false,
        availableInputRoots: new Set([31]),
        mediaByPartId: new Map(),
      }),
    ).toThrow("missing accepted media observation");
  });

  it("orders issue categories after binary filename and projection ID order", () => {
    const { snapshotRoot } = fixture();
    const base = snapshot(snapshotRoot);
    const names = ["z.stl", "é.stl", "a.stl", "A.stl", "a.stl"];
    const relativePaths = ["z.stl", "beta/é.stl", "alpha/late.stl", "A.stl", "alpha/early.stl"];
    const parts = names.map((filename, index) => ({
      ...base.parts[0]!,
      revisionPartId: 100 + index,
      projectionPartId: 205 - index,
      partKey: filename,
      relativePath: relativePaths[index]!,
      filename,
      status: "conflict",
    }));
    const mediaByPartId = new Map(
      parts.map((part) => [part.projectionPartId, { artifactMissing: true, thumbEmpty: false }]),
    );

    const body = projectAcceptedPlanReview({
      snapshot: { ...base, parts },
      includeExcluded: false,
      availableInputRoots: new Set([31]),
      mediaByPartId,
    });

    const sortedNames = [...names].sort((left, right) =>
      Buffer.compare(Buffer.from(left), Buffer.from(right)),
    );
    expect(body.issues.map((issue) => `${issue.code}:${issue.message}`)).toEqual([
      ...sortedNames.map((name) => `missing_stl:STL not found on disk: ${name}`),
      ...sortedNames.map((name) => `merge_conflict:${mergeConflictIssueMessage(name)}`),
    ]);
    expect(summarizeAcceptedPlanReview(body).sample_issues).toEqual(
      body.issues.slice(0, 8).map((issue) => ({
        severity: issue.severity,
        code: issue.code,
        message: issue.message,
      })),
    );
    expect(summarizeAcceptedPlanReview(body).issue_codes).toEqual([
      "missing_stl",
      "merge_conflict",
    ]);
    expect(body.part_groups.map((group) => group.folder)).toEqual(["(root)", "alpha", "beta"]);
    expect(body.part_groups.map((group) => group.parts.map((part) => part.id))).toEqual([
      [202, 205],
      [201, 203],
      [204],
    ]);
  });

  it("returns not_found without an accepted read", async () => {
    const repo = repository({ profile: null, accepted: { kind: "empty" } });

    await expect(
      readAcceptedPlanReview({
        repo,
        profileId: 7,
        includeExcluded: false,
        reposDir: "/unused",
        thumbsDir: null,
      }),
    ).resolves.toEqual({ kind: "not_found" });
    expect(repo.readAcceptedPlanOperationalSnapshot).not.toHaveBeenCalled();
  });

  it("returns the exact empty Review body after one accepted read", async () => {
    const { reposDir, thumbsDir } = fixture();
    const repo = repository({ accepted: { kind: "empty" } });

    await expect(
      readAcceptedPlanReview({
        repo,
        profileId: 7,
        includeExcluded: false,
        reposDir,
        thumbsDir,
      }),
    ).resolves.toEqual({
      kind: "empty",
      body: {
        profile_id: 7,
        accepted_basis: null,
        plan_name: "Working name",
        layers: [],
        totals: { included_parts: 0, total_print_units: 0, by_role: {}, by_filament: {} },
        issues: [
          {
            severity: "blocker",
            code: "no_included_parts",
            message: "No parts are included in this build.",
            link_hint: "build",
          },
        ],
        has_blockers: true,
        part_groups: [],
      },
    });
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(1);
    expect(existsSync(thumbsDir)).toBe(false);
  });

  it("uses only the accepted thumbnail basis and ignores unrelated PNG cache entries", async () => {
    const { reposDir, snapshotRoot, thumbsDir } = fixture();
    const accepted = snapshot(snapshotRoot);
    const part = accepted.parts[0]!;
    writeFileSync(join(snapshotRoot, "parts", "widget.stl"), "solid accepted");
    const png = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    writeAcceptedMediaPng({ thumbsDir, basis: "c".repeat(64), png });
    const repo = repository({ accepted: { kind: "ready", snapshot: accepted } });

    const missing = await readAcceptedPlanReview({
      repo,
      profileId: 7,
      includeExcluded: false,
      reposDir,
      thumbsDir,
    });
    expect(missing.kind).toBe("ready");
    if (missing.kind !== "ready") throw new Error("Expected ready Review");
    expect(missing.body.part_groups[0]?.parts[0]?.thumb_empty).toBe(true);

    writeAcceptedMediaPng({
      thumbsDir,
      basis: acceptedPartMediaIdentity(part, "thumbnail").basis,
      png,
    });
    const present = await readAcceptedPlanReview({
      repo,
      profileId: 7,
      includeExcluded: false,
      reposDir,
      thumbsDir,
    });
    expect(present.kind).toBe("ready");
    if (present.kind !== "ready") throw new Error("Expected ready Review");
    expect(present.body.part_groups[0]?.parts[0]?.thumb_empty).toBe(false);
  });

  it("maps unavailable accepted state without enrichment", async () => {
    const repo = repository({ accepted: { kind: "compatibility_dirty" } });
    const loader = vi.fn();

    await expect(
      readAcceptedPlanReview({
        repo,
        profileId: 7,
        includeExcluded: false,
        reposDir: "/unused",
        thumbsDir: null,
        loadFilamentContext: loader,
      }),
    ).resolves.toEqual({
      kind: "accepted_state_unavailable",
      reason: "compatibility_dirty",
    });
    expect(loader).not.toHaveBeenCalled();
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(1);
  });

  it("projects accepted facts, progress, root availability, and media flags", async () => {
    const { reposDir, snapshotRoot, thumbsDir } = fixture();
    writeFileSync(join(snapshotRoot, "parts", "widget.stl"), "solid accepted");
    const repo = repository({ accepted: { kind: "ready", snapshot: snapshot(snapshotRoot) } });
    const loader = vi.fn(async () => ({
      resolve: () => null,
      spoolSummariesForPart: () => [],
    }));

    const result = await readAcceptedPlanReview({
      repo,
      profileId: 7,
      includeExcluded: false,
      reposDir,
      thumbsDir,
      loadFilamentContext: loader,
    });

    expect(result.kind).toBe("ready");
    if (result.kind !== "ready") throw new Error("Expected ready Review");
    expect(result.body.profile_id).toBe(7);
    expect(result.body.plan_name).toBe("Accepted Plan");
    expect(result.body.layers).toEqual([
      {
        id: 31,
        layer_type: "addon",
        project_id: 41,
        project_name: "Captured Source",
        local_path: null,
        synced: true,
        last_synced_at: "2026-08-19T00:00:00.000Z",
      },
    ]);
    expect(result.body.totals).toEqual({
      included_parts: 1,
      total_print_units: 2,
      by_role: { accent: 1 },
      by_filament: { Unassigned: 2 },
    });
    expect(result.body.issues).toEqual([]);
    expect(result.body.has_blockers).toBe(false);
    expect(result.body.part_groups).toHaveLength(1);
    expect(result.body.part_groups[0]?.parts[0]).toMatchObject({
      id: 72,
      role: "accent",
      filament_custom_hex: "#AABBCC",
      quantity_auto: 2,
      quantity_effective: 2,
      printed_count: 1,
      print_units: [true, false],
      assembled_units: [true, false],
      missing: true,
      stl_missing: false,
      thumb_empty: true,
    });
    expect(loader).toHaveBeenCalledWith([null]);
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(1);
  });

  it("reports unavailable accepted roots and artifacts without using current Source state", async () => {
    const { reposDir } = fixture();
    const accepted = snapshot(join(reposDir, "snapshots", "missing"));
    const repo = repository({ accepted: { kind: "ready", snapshot: accepted } });

    const result = await readAcceptedPlanReview({
      repo,
      profileId: 7,
      includeExcluded: false,
      reposDir,
      thumbsDir: null,
    });

    expect(result.kind).toBe("ready");
    if (result.kind !== "ready") throw new Error("Expected ready Review");
    expect(result.body.layers[0]).toMatchObject({
      id: 31,
      project_id: 41,
      local_path: null,
      synced: false,
    });
    expect(result.body.part_groups[0]?.parts[0]).toMatchObject({
      stl_missing: true,
      thumb_empty: false,
    });
    expect(result.body.issues.map((issue) => issue.code)).toEqual([
      "unsynced_source",
      "missing_stl",
    ]);
  });

  it("returns requested excluded Parts with false media flags and no legacy layers", async () => {
    const { reposDir, snapshotRoot } = fixture();
    const base = snapshot(snapshotRoot);
    const accepted: AcceptedPlanOperationalSnapshot = {
      ...base,
      provenance: { kind: "legacy" },
      parts: base.parts.map((part) => ({
        ...part,
        included: false,
        artifact: { kind: "unavailable", reason: "legacy" as const },
      })),
    };
    const repo = repository({ accepted: { kind: "ready", snapshot: accepted } });

    const result = await readAcceptedPlanReview({
      repo,
      profileId: 7,
      includeExcluded: true,
      reposDir,
      thumbsDir: null,
    });

    expect(result.kind).toBe("ready");
    if (result.kind !== "ready") throw new Error("Expected ready Review");
    expect(result.body.layers).toEqual([]);
    expect(result.body.totals.included_parts).toBe(0);
    expect(result.body.part_groups[0]?.parts[0]).toMatchObject({
      included: false,
      stl_missing: false,
      thumb_empty: false,
    });
    expect(result.body.issues.map((issue) => issue.code)).toEqual(["no_included_parts"]);
  });
});
