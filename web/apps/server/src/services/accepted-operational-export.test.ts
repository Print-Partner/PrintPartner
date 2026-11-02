import { describe, expect, it, vi } from "vitest";
import {
  AcceptedPlanOperationalIntegrityError,
  type AcceptedPlanOperationalSnapshot,
  type ReadAcceptedPlanOperationalSnapshotResult,
} from "../db/accepted-plan-operational.js";
import type { OwnedProfileIdentity } from "../db/repository.js";
import { captureAcceptedOperationalExport } from "./accepted-operational-export.js";

function acceptedSnapshot(): AcceptedPlanOperationalSnapshot {
  return {
    format: "accepted-plan-operational-v1",
    profile: {
      id: 7,
      name: "Accepted Build",
      orderNumber: "SO-7",
      specialRequest: null,
      archivedAt: null,
    },
    planVersion: 4,
    revisionId: 19,
    revisionNumber: 3,
    revisionDigest: "a".repeat(64),
    acceptedAt: "2026-08-21T10:00:00.000Z",
    provenance: { kind: "legacy" },
    requiredUnitMappingDigest: "b".repeat(64),
    parts: [
      {
        revisionPartId: 31,
        projectionPartId: 41,
        partKey: "base:frame.stl",
        relativePath: "parts/frame.stl",
        filename: "frame.stl",
        sourceLayer: "base:Frame",
        status: "ok",
        roleInferred: "primary",
        roleOverride: "accent",
        effectiveRole: "accent",
        filamentColorId: "red",
        filamentCustomHex: "#cc0000",
        spoolmanSpoolId: "12",
        quantityInferred: 1,
        quantityOverride: 2,
        quantityEffective: 2,
        included: true,
        notes: "accepted notes",
        githubBlobUrl: null,
        geometrySame: true,
        requirement: "left and right",
        optionGroupId: "frame",
        manifestSource: "kit.yaml",
        artifact: { kind: "unavailable", reason: "legacy" },
        units: [
          {
            unitIndex: 0,
            required: true,
            token: "frame:0",
            objectName: "frame",
            completed: true,
            assembled: false,
          },
          {
            unitIndex: 1,
            required: true,
            token: "frame:1",
            objectName: "frame",
            completed: false,
            assembled: true,
          },
        ],
      },
      {
        revisionPartId: 32,
        projectionPartId: 42,
        partKey: "addon:cap.stl",
        relativePath: "cap.stl",
        filename: "cap.stl",
        sourceLayer: "addon:Caps",
        status: "conflict",
        roleInferred: "clear",
        roleOverride: null,
        effectiveRole: "clear",
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
        artifact: { kind: "unavailable", reason: "untracked_source" },
        units: [
          {
            unitIndex: 0,
            required: true,
            token: "cap:0",
            objectName: "cap",
            completed: false,
            assembled: false,
          },
        ],
      },
    ],
  };
}

function repository(input: {
  identity?: OwnedProfileIdentity | null;
  accepted?: ReadAcceptedPlanOperationalSnapshotResult;
  failure?: Error;
}) {
  return {
    getOwnedProfileIdentity: vi.fn(() =>
      input.identity === undefined
        ? { id: 7, name: "Working Build", orderNumber: "WORK-7", archivedAt: null }
        : input.identity,
    ),
    readAcceptedPlanOperationalSnapshot: vi.fn((): ReadAcceptedPlanOperationalSnapshotResult => {
      if (input.failure) throw input.failure;
      return input.accepted ?? { kind: "ready", snapshot: acceptedSnapshot() };
    }),
  };
}

describe("captureAcceptedOperationalExport", () => {
  it("projects one accepted snapshot without compatibility progress identity", () => {
    const repo = repository({});

    const captured = captureAcceptedOperationalExport({ repository: repo, profileId: 7 });

    expect(captured).toEqual({
      kind: "ready",
      export: {
        basis: {
          profileId: 7,
          planVersion: 4,
          revisionId: 19,
          revisionDigest: "a".repeat(64),
          requiredUnitMappingDigest: "b".repeat(64),
        },
        profile: acceptedSnapshot().profile,
        provenance: { kind: "legacy" },
        parts: [
          expect.objectContaining({
            revisionPartId: 31,
            projectionPartId: 41,
            role: "accent",
            quantityEffective: 2,
            included: true,
            units: [
              { token: "frame:0", unitIndex: 0, completed: true, assembled: false },
              { token: "frame:1", unitIndex: 1, completed: false, assembled: true },
            ],
          }),
          expect.objectContaining({
            revisionPartId: 32,
            projectionPartId: 42,
            role: "clear",
            included: false,
            units: [{ token: "cap:0", unitIndex: 0, completed: false, assembled: false }],
          }),
        ],
      },
    });
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(1);
  });

  it.each([
    [
      { kind: "compatibility_dirty" },
      { kind: "accepted_state_unavailable", reason: "compatibility_dirty" },
    ],
    [{ kind: "uninitialized" }, { kind: "accepted_state_unavailable", reason: "uninitialized" }],
  ] satisfies readonly (readonly [
    ReadAcceptedPlanOperationalSnapshotResult,
    {
      readonly kind: "accepted_state_unavailable";
      readonly reason: "compatibility_dirty" | "uninitialized";
    },
  ])[])("maps %o to %o", (accepted, expected) => {
    expect(
      captureAcceptedOperationalExport({
        repository: repository({ accepted }),
        profileId: 7,
      }),
    ).toEqual(expected);
  });

  it("keeps missing, empty, and integrity outcomes distinct", () => {
    const missing = repository({ identity: null });
    expect(captureAcceptedOperationalExport({ repository: missing, profileId: 7 })).toEqual({
      kind: "profile_not_found",
    });
    expect(missing.readAcceptedPlanOperationalSnapshot).not.toHaveBeenCalled();

    expect(
      captureAcceptedOperationalExport({
        repository: repository({ accepted: { kind: "empty" } }),
        profileId: 7,
      }),
    ).toEqual({
      kind: "empty",
      profile: { id: 7, name: "Working Build", orderNumber: "WORK-7" },
    });

    expect(
      captureAcceptedOperationalExport({
        repository: repository({
          failure: new AcceptedPlanOperationalIntegrityError("progress", "private row details"),
        }),
        profileId: 7,
      }),
    ).toEqual({ kind: "integrity" });
  });

  it("stays on the captured accepted revision when working state and the accepted pointer change", () => {
    const first = acceptedSnapshot();
    const second = {
      ...acceptedSnapshot(),
      planVersion: 5,
      revisionId: 20,
      parts: acceptedSnapshot().parts.map((part) => ({
        ...part,
        quantityEffective: 1,
        units: part.units.slice(0, 1),
      })),
    } satisfies AcceptedPlanOperationalSnapshot;
    let current: AcceptedPlanOperationalSnapshot = first;
    const workingRead = vi.fn(() => {
      throw new Error("working Parts must remain invisible");
    });
    const repo = {
      ...repository({}),
      readAcceptedPlanOperationalSnapshot: vi.fn((): ReadAcceptedPlanOperationalSnapshotResult => ({
        kind: "ready",
        snapshot: current,
      })),
      listPartRows: workingRead,
    };

    const captured = captureAcceptedOperationalExport({ repository: repo, profileId: 7 });
    current = second;

    expect(captured.kind).toBe("ready");
    if (captured.kind !== "ready") return;
    expect(captured.export.basis).toMatchObject({ revisionId: 19, planVersion: 4 });
    expect(captured.export.parts[0]?.quantityEffective).toBe(2);
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(1);
    expect(workingRead).not.toHaveBeenCalled();
  });
});
