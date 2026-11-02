import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join, relative, sep } from "node:path";
import { describe, expect, it } from "vitest";
import type {
  AcceptedPlanOperationalSnapshot,
  ReadAcceptedPlanOperationalSnapshotResult,
} from "../db/accepted-plan-operational.js";
import { toAcceptedCheckoffView, toAcceptedPartAssembledView } from "./accepted-plan-views.js";

function productionTypeScriptFiles(directory: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...productionTypeScriptFiles(path));
    if (entry.isFile() && entry.name.endsWith(".ts") && !entry.name.endsWith(".test.ts")) {
      files.push(path);
    }
  }
  return files;
}

function productionCallers(symbol: string) {
  const sourceRoot = fileURLToPath(new URL("../", import.meta.url));
  const pattern = new RegExp(`\\.${symbol}\\(`, "g");
  return productionTypeScriptFiles(sourceRoot)
    .map((path) => ({
      file: relative(sourceRoot, path).split(sep).join("/"),
      count: readFileSync(path, "utf8").match(pattern)?.length ?? 0,
    }))
    .filter((entry) => entry.count > 0);
}

function readySnapshot(): AcceptedPlanOperationalSnapshot {
  return {
    format: "accepted-plan-operational-v1",
    profile: {
      id: 7,
      name: "Trident",
      orderNumber: null,
      specialRequest: null,
      archivedAt: null,
    },
    planVersion: 3,
    revisionId: 12,
    revisionNumber: 2,
    revisionDigest: "a".repeat(64),
    acceptedAt: "2026-08-21T06:00:00.000Z",
    provenance: { kind: "legacy" },
    requiredUnitMappingDigest: "b".repeat(64),
    parts: [
      {
        revisionPartId: 1,
        projectionPartId: 90,
        partKey: "zeta-key",
        relativePath: "z/zeta.stl",
        filename: "zeta.stl",
        sourceLayer: "base:Repo",
        status: "ok",
        roleInferred: "primary",
        roleOverride: null,
        effectiveRole: "primary",
        filamentColorId: "test:blue",
        filamentCustomHex: null,
        spoolmanSpoolId: "spool-4",
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
        artifact: { kind: "unavailable", reason: "legacy" },
        units: [
          {
            unitIndex: 0,
            required: true,
            token: "ppu_00000000000000000000000000000001",
            objectName: "zeta__ppu_00000000000000000000000000000001",
            completed: true,
            assembled: true,
          },
          {
            unitIndex: 1,
            required: true,
            token: "ppu_00000000000000000000000000000002",
            objectName: "zeta__ppu_00000000000000000000000000000002",
            completed: false,
            assembled: false,
          },
        ],
      },
      {
        revisionPartId: 2,
        projectionPartId: 4,
        partKey: "alpha-key",
        relativePath: "a/alpha.stl",
        filename: "alpha.stl",
        sourceLayer: "addon:Repo",
        status: "ok",
        roleInferred: "accent",
        roleOverride: null,
        effectiveRole: "accent",
        filamentColorId: null,
        filamentCustomHex: "#112233",
        spoolmanSpoolId: null,
        quantityInferred: 1,
        quantityOverride: null,
        quantityEffective: 1,
        included: true,
        notes: "",
        githubBlobUrl: null,
        geometrySame: null,
        requirement: null,
        optionGroupId: null,
        manifestSource: null,
        artifact: { kind: "unavailable", reason: "legacy" },
        units: [
          {
            unitIndex: 0,
            required: true,
            token: "ppu_00000000000000000000000000000003",
            objectName: "alpha__ppu_00000000000000000000000000000003",
            completed: true,
            assembled: false,
          },
        ],
      },
      {
        revisionPartId: 3,
        projectionPartId: 3,
        partKey: "excluded-key",
        relativePath: "excluded.stl",
        filename: "excluded.stl",
        sourceLayer: "base:Repo",
        status: "ok",
        roleInferred: "primary",
        roleOverride: null,
        effectiveRole: "primary",
        filamentColorId: null,
        filamentCustomHex: null,
        spoolmanSpoolId: null,
        quantityInferred: 1,
        quantityOverride: null,
        quantityEffective: 1,
        included: false,
        notes: "",
        githubBlobUrl: null,
        geometrySame: null,
        requirement: null,
        optionGroupId: null,
        manifestSource: null,
        artifact: { kind: "unavailable", reason: "legacy" },
        units: [
          {
            unitIndex: 0,
            required: false,
            token: "ppu_00000000000000000000000000000004",
            objectName: "excluded__ppu_00000000000000000000000000000004",
            completed: false,
            assembled: false,
          },
        ],
      },
    ],
  };
}

describe("accepted Plan views", () => {
  it("builds the existing Checkoff body from accepted identity and progress", () => {
    const snapshot = readySnapshot();
    const accepted = {
      kind: "ready",
      snapshot,
    } satisfies ReadAcceptedPlanOperationalSnapshotResult;
    const before = structuredClone(snapshot);
    const result = toAcceptedCheckoffView({
      profileId: 7,
      accepted,
      filamentContext: {
        resolve(colorId) {
          return colorId === "test:blue"
            ? { combo_label: "Test Blue", hex: "#0000ff", display_name: "Blue" }
            : null;
        },
        spoolSummariesForPart(colorId, spoolRef) {
          return colorId === "test:blue" && spoolRef === "spool-4"
            ? [
                {
                  spool_id: 4,
                  remaining_g: 420,
                },
              ]
            : [];
        },
      },
    });

    expect(result).toEqual({
      kind: "ready",
      body: {
        profile_id: 7,
        summary: "1/2 parts fully printed · 2/3 units",
        parts: [
          {
            id: 4,
            filename: "alpha.stl",
            match_key: "alpha-key",
            relative_path: "a/alpha.stl",
            source_layer: "addon:Repo",
            role: "accent",
            quantity_effective: 1,
            printed_count: 1,
            print_units: [true],
            missing: false,
            filament_display: "",
            filament_hex: "#112233",
          },
          {
            id: 90,
            filename: "zeta.stl",
            match_key: "zeta-key",
            relative_path: "z/zeta.stl",
            source_layer: "base:Repo",
            role: "primary",
            quantity_effective: 2,
            printed_count: 1,
            print_units: [true, false],
            missing: true,
            filament_display: "Test Blue",
            filament_hex: "#0000ff",
            spool_summary: [
              {
                spool_id: 4,
                remaining_g: 420,
              },
            ],
            spool_badge: "~420 g on spool #4",
          },
        ],
      },
    });
    expect(snapshot).toEqual(before);
  });

  it("distinguishes empty and unavailable Checkoff states", () => {
    expect(toAcceptedCheckoffView({ profileId: 7, accepted: { kind: "empty" } })).toEqual({
      kind: "empty",
      body: {
        profile_id: 7,
        summary: "0/0 parts fully printed · 0/0 units",
        parts: [],
      },
    });
    expect(
      toAcceptedCheckoffView({
        profileId: 7,
        accepted: { kind: "compatibility_dirty" },
      }),
    ).toEqual({
      kind: "accepted_state_unavailable",
      reason: "compatibility_dirty",
    });
    expect(toAcceptedCheckoffView({ profileId: 7, accepted: { kind: "uninitialized" } })).toEqual({
      kind: "accepted_state_unavailable",
      reason: "uninitialized",
    });
  });

  it("finds assembled state by projection Part ID", () => {
    const accepted = {
      kind: "ready",
      snapshot: readySnapshot(),
    } satisfies ReadAcceptedPlanOperationalSnapshotResult;

    expect(toAcceptedPartAssembledView({ partId: 90, accepted })).toEqual({
      kind: "ready",
      body: {
        part_id: 90,
        assembled_count: 1,
        assembled_units: [true, false],
      },
    });
    expect(toAcceptedPartAssembledView({ partId: 999, accepted })).toEqual({
      kind: "part_not_found",
    });
    expect(toAcceptedPartAssembledView({ partId: 90, accepted: { kind: "empty" } })).toEqual({
      kind: "part_not_found",
    });
    expect(
      toAcceptedPartAssembledView({
        partId: 90,
        accepted: { kind: "compatibility_dirty" },
      }),
    ).toEqual({
      kind: "accepted_state_unavailable",
      reason: "compatibility_dirty",
    });
  });

  it("uses SQLite binary filename order", () => {
    const base = readySnapshot();
    const snapshot = {
      ...base,
      parts: base.parts.map((part) => {
        if (part.projectionPartId === 90) return { ...part, filename: "é.stl" };
        if (part.projectionPartId === 4) return { ...part, filename: "Z.stl" };
        return { ...part, filename: "a.stl", included: true };
      }),
    } satisfies AcceptedPlanOperationalSnapshot;
    const result = toAcceptedCheckoffView({
      profileId: 7,
      accepted: { kind: "ready", snapshot },
    });

    expect(result.kind).toBe("ready");
    if (result.kind !== "ready") throw new Error("accepted Checkoff view is missing");
    expect(result.body.parts.map((part) => part.id)).toEqual([4, 3, 90]);
  });

  it("keeps legacy accepted readers on the exact deferred allowlist", () => {
    expect(productionCallers("getCheckoff")).toEqual([]);
    expect(productionCallers("getPartAssembled")).toEqual([]);
    for (const symbol of [
      "archiveProfile",
      "patchPartProgress",
      "patchPartAssembled",
      "ensureProgressForPart",
    ]) {
      expect(productionCallers(symbol)).toEqual([]);
    }
    expect(productionCallers("printUnitTotals")).toEqual([]);
    expect(productionCallers("printUnitsByPartId")).toEqual([]);
  });
});
