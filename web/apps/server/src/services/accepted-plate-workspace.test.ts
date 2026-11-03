import { describe, expect, it, vi } from "vitest";
import { parseAcceptedStlMesh } from "@print-partner/domain";
import type { AcceptedPlateInput, AcceptedPlateSetupUnit } from "../db/accepted-plates.js";
import { parseRequiredUnitToken } from "./required-units.js";
import {
  initializeAcceptedPlates,
  readAcceptedPlateWorkspace,
  type AcceptedPlateWorkspaceDependencies,
} from "./accepted-plate-workspace.js";

const basis = {
  profileId: 7,
  planVersion: 3,
  revisionId: 11,
  revisionDigest: "a".repeat(64),
  requiredUnitMappingDigest: "b".repeat(64),
};

const token = parseRequiredUnitToken("ppu_00000000000000000000000000000001");
const secondToken = parseRequiredUnitToken("ppu_00000000000000000000000000000002");
const setupUnit: AcceptedPlateSetupUnit = {
  token,
  partId: null,
  objectName: `bracket__${token}`,
  filename: "bracket.stl",
  relativePath: "parts/bracket.stl",
  sourceDirectory: "parts",
  sourceLayer: "base:test",
  role: "primary",
  filamentColorId: "black",
  artifact: { kind: "unavailable", reason: "untracked_source" },
};
const setupInput = {
  kind: "setup" as const,
  basis,
  expectedPlateRevisionId: null,
  units: [setupUnit],
};

function dependencies(): AcceptedPlateWorkspaceDependencies & {
  publish: ReturnType<typeof vi.fn>;
  loadGeometry: ReturnType<typeof vi.fn>;
} {
  const publish = vi.fn();
  const loadGeometry = vi.fn();
  return {
    repository: {
      readAcceptedPlateWorkspaceInput: vi.fn(() => setupInput),
      publishAcceptedPlates: publish,
      getSetting: vi.fn<() => string | null>(() => null),
    },
    reposDir: "/tmp/unused-accepted-workspace",
    limits: {
      maxArtifactBytes: 1_000_000,
      maxTotalSourceBytes: 1_000_000,
      maxObjects: 10,
      maxTriangles: 10,
    },
    loadPrinters: () => [
      {
        id: "printer-one",
        name: "Printer One",
        model: "Model One",
        bed_width_mm: 250,
        bed_depth_mm: 210,
        bed_height_mm: 200,
        margin_mm: 4,
        max_filament_slots: 1,
        loaded_filaments: [{ slot: 1, filament_color_id: "other", label: "Other" }],
      },
    ],
    publish,
    loadGeometry,
  };
}

function geometry(widthUm = 30_000, depthUm = 20_000, heightUm = 10_000) {
  const mesh = parseAcceptedStlMesh(
    Buffer.from(`solid accepted
facet normal 0 0 1
outer loop
vertex 0 0 0
vertex 30 0 0
vertex 0 20 10
endloop
endfacet
endsolid accepted`),
  );
  if (!mesh) throw new Error("test STL is invalid");
  return { mesh, dimensions: { widthUm, depthUm, heightUm } };
}

function publishingDependencies(machineChange: Record<string, unknown> = {}) {
  let plates: readonly AcceptedPlateInput[] | null = null;
  const publish = vi.fn((command: { plates: readonly AcceptedPlateInput[] }) => {
    plates = command.plates;
    return { kind: "published" as const, plateRevisionId: 31, plateRevisionNumber: 1 };
  });
  const read = vi.fn(() => {
    if (!plates) return setupInput;
    return {
      kind: "ready" as const,
      basis,
      expectedPlateRevisionId: 31,
      plateRevisionId: 31,
      plateRevisionNumber: 1,
      undoFromRevisionId: null,
      units: setupInput.units,
      plates: plates.map((plate, index) => ({
        ...plate,
        ordinal: index + 1,
        units: plate.units.map((unit) => ({ ...unit, objectName: `bracket__${token}` })),
      })),
    };
  });
  const loadGeometry = vi.fn(async () => ({
    kind: "ready" as const,
    geometryByToken: new Map([[token, geometry()]]),
  }));
  return {
    repository: {
      readAcceptedPlateWorkspaceInput: read,
      publishAcceptedPlates: publish,
      getSetting: vi.fn<() => string | null>(() => null),
    },
    reposDir: "/tmp/unused-accepted-workspace",
    limits: {
      maxArtifactBytes: 1_000_000,
      maxTotalSourceBytes: 1_000_000,
      maxObjects: 10,
      maxTriangles: 10,
    },
    loadPrinters: () => [
      {
        id: "printer-one",
        name: "Printer One",
        model: "Model One",
        bed_width_mm: 250,
        bed_depth_mm: 210,
        bed_height_mm: 200,
        margin_mm: 4,
        max_filament_slots: 1,
        loaded_filaments: [{ slot: 1, filament_color_id: "wrong", label: "Wrong" }],
        ...machineChange,
      },
    ],
    loadGeometry,
    publish,
  } satisfies AcceptedPlateWorkspaceDependencies & {
    publish: typeof publish;
  };
}

describe("accepted Plate workspace", () => {
  it("projects setup wire data without exposing accepted artifact descriptors", () => {
    const deps = dependencies();

    expect(readAcceptedPlateWorkspace(deps, 7)).toEqual({
      kind: "workspace",
      workspace: {
        kind: "setup",
        basis: {
          profile_id: 7,
          plan_version: 3,
          plan_revision_id: 11,
          plan_revision_digest: "a".repeat(64),
          required_unit_mapping_digest: "b".repeat(64),
        },
        expected_plate_revision_id: null,
        printers: [
          {
            id: "printer-one",
            name: "Printer One",
            model: "Model One",
            bed_width_um: 250_000,
            bed_depth_um: 210_000,
            bed_height_um: 200_000,
            margin_um: 4_000,
          },
        ],
        units: [
          {
            token,
            part_id: null,
            object_name: `bracket__${token}`,
            filename: "bracket.stl",
            relative_path: "parts/bracket.stl",
            source_directory: "parts",
            source_layer: "base:test",
            role: "primary",
            filament_color_id: "black",
            completed: false,
          },
        ],
      },
    });
  });

  it("returns Unassigned tokens before opening artifacts or writing Plates", async () => {
    const deps = dependencies();

    await expect(
      initializeAcceptedPlates(deps, {
        profileId: 7,
        expected: basis,
        expectedPlateRevisionId: null,
        assignments: [{ token, printerId: null }],
      }),
    ).resolves.toEqual({ kind: "unassigned_units", tokens: [token] });

    expect(deps.loadGeometry).not.toHaveBeenCalled();
    expect(deps.publish).not.toHaveBeenCalled();
  });

  it("rejects duplicate assignment rows before opening artifacts", async () => {
    const deps = dependencies();

    await expect(
      initializeAcceptedPlates(deps, {
        profileId: 7,
        expected: basis,
        expectedPlateRevisionId: null,
        assignments: [
          { token, printerId: "printer-one" },
          { token, printerId: "printer-one" },
        ],
      }),
    ).resolves.toEqual({ kind: "duplicate_assignment", tokens: [token] });

    expect(deps.loadGeometry).not.toHaveBeenCalled();
    expect(deps.publish).not.toHaveBeenCalled();
  });

  it.each([
    [[], { kind: "missing_assignment", tokens: [token] }],
    [
      [{ token: secondToken, printerId: "printer-one" }],
      {
        kind: "unknown_unit_token",
        tokens: [secondToken],
      },
    ],
  ])(
    "rejects empty or unknown accepted token selections before opening artifacts %#",
    async (assignments, expected) => {
      const deps = dependencies();

      await expect(
        initializeAcceptedPlates(deps, {
          profileId: 7,
          expected: basis,
          expectedPlateRevisionId: null,
          assignments,
        }),
      ).resolves.toEqual(expected);

      expect(deps.loadGeometry).not.toHaveBeenCalled();
      expect(deps.publish).not.toHaveBeenCalled();
    },
  );

  it("publishes a selected subset and returns the remaining Required unit as Unassigned", async () => {
    const deps = publishingDependencies();
    const secondUnit = {
      ...setupInput.units[0],
      token: secondToken,
      objectName: `clip__${secondToken}`,
      filename: "clip.stl",
    };
    vi.mocked(deps.repository.readAcceptedPlateWorkspaceInput).mockReturnValue({
      ...setupInput,
      units: [...setupInput.units, secondUnit],
    });
    deps.loadGeometry.mockResolvedValue({
      kind: "ready",
      geometryByToken: new Map([[token, geometry()]]),
    });

    const result = await initializeAcceptedPlates(deps, {
      profileId: 7,
      expected: basis,
      expectedPlateRevisionId: null,
      assignments: [{ token, printerId: "printer-one" }],
    });

    expect(deps.loadGeometry).toHaveBeenCalledWith(
      expect.objectContaining({
        units: [setupInput.units[0]],
      }),
    );
    expect(deps.publish).toHaveBeenCalledWith(
      expect.objectContaining({
        plates: [
          expect.objectContaining({
            units: [expect.objectContaining({ token })],
          }),
        ],
      }),
    );
    expect(result).toMatchObject({
      kind: "workspace",
      workspace: {
        kind: "ready",
        unassigned: [{ token: secondToken, completed: false }],
      },
    });
  });

  it("uses the explicit Printer even when its loaded filament does not match", async () => {
    const deps = publishingDependencies();

    const result = await initializeAcceptedPlates(deps, {
      profileId: 7,
      expected: basis,
      expectedPlateRevisionId: null,
      assignments: [{ token, printerId: "printer-one" }],
    });

    expect(result).toMatchObject({
      kind: "workspace",
      workspace: {
        kind: "ready",
        plate_revision_id: 31,
        plates: [
          {
            printer: { id: "printer-one", model: "Model One" },
            units: [{ token, x_um: 4_000, y_um: 4_000, width_um: 30_000, depth_um: 20_000 }],
          },
        ],
      },
    });
    expect(deps.publish).toHaveBeenCalledOnce();
  });

  it("publishes separate Plates when the saved Build setup separates by color", async () => {
    const deps = publishingDependencies();
    const secondUnit = {
      ...setupInput.units[0],
      token: secondToken,
      objectName: `clip__${secondToken}`,
      filename: "clip.stl",
      filamentColorId: "orange",
    };
    vi.mocked(deps.repository.readAcceptedPlateWorkspaceInput).mockReturnValue({
      ...setupInput,
      units: [...setupInput.units, secondUnit],
    });
    deps.repository.getSetting = vi.fn(() =>
      JSON.stringify({
        format: "production-setup-v1",
        profile_id: 7,
        preferred_slicer_instance_id: null,
        selection: { mode: "all_incomplete" },
        rules: [{ id: "colors", enabled: true, kind: "separate_by", field: "color" }],
        updated_at: "2026-08-23T00:00:00.000Z",
      }),
    );
    deps.loadGeometry.mockResolvedValue({
      kind: "ready",
      geometryByToken: new Map([
        [token, geometry()],
        [secondToken, geometry()],
      ]),
    });

    await initializeAcceptedPlates(deps, {
      profileId: 7,
      expected: basis,
      expectedPlateRevisionId: null,
      assignments: [
        { token, printerId: "printer-one" },
        { token: secondToken, printerId: "printer-one" },
      ],
    });

    expect(deps.publish).toHaveBeenCalledWith(
      expect.objectContaining({
        plates: [
          expect.objectContaining({ units: [expect.objectContaining({ token })] }),
          expect.objectContaining({ units: [expect.objectContaining({ token: secondToken })] }),
        ],
      }),
    );
  });

  it("publishes separate Plates when custom hex colors are separated", async () => {
    const deps = publishingDependencies();
    const firstUnit = {
      ...setupInput.units[0],
      filamentColorId: null,
      filamentCustomHex: "#FF6600",
    };
    const secondUnit = {
      ...firstUnit,
      token: secondToken,
      objectName: `clip__${secondToken}`,
      filename: "clip.stl",
      filamentCustomHex: "#008000",
    };
    vi.mocked(deps.repository.readAcceptedPlateWorkspaceInput).mockReturnValue({
      ...setupInput,
      units: [firstUnit, secondUnit],
    });
    deps.repository.getSetting = vi.fn(() =>
      JSON.stringify({
        format: "production-setup-v1",
        profile_id: 7,
        preferred_slicer_instance_id: null,
        selection: { mode: "all_incomplete" },
        rules: [{ id: "colors", enabled: true, kind: "separate_by", field: "color" }],
        updated_at: "2026-08-23T00:00:00.000Z",
      }),
    );
    deps.loadGeometry.mockResolvedValue({
      kind: "ready",
      geometryByToken: new Map([
        [token, geometry()],
        [secondToken, geometry()],
      ]),
    });

    await initializeAcceptedPlates(deps, {
      profileId: 7,
      expected: basis,
      expectedPlateRevisionId: null,
      assignments: [
        { token, printerId: "printer-one" },
        { token: secondToken, printerId: "printer-one" },
      ],
    });

    expect(deps.publish).toHaveBeenCalledWith(
      expect.objectContaining({
        plates: [
          expect.objectContaining({ units: [expect.objectContaining({ token })] }),
          expect.objectContaining({ units: [expect.objectContaining({ token: secondToken })] }),
        ],
      }),
    );
  });

  it("returns the published revision without rereading state changed after the write", async () => {
    let state:
      | typeof setupInput
      | {
          kind: "accepted_state_unavailable";
          reason: "uninitialized";
        } = setupInput;
    const read = vi.fn(() => state);
    const base = dependencies();
    const deps = {
      ...base,
      repository: {
        readAcceptedPlateWorkspaceInput: read,
        publishAcceptedPlates: vi.fn(() => {
          state = { kind: "accepted_state_unavailable", reason: "uninitialized" };
          return { kind: "published" as const, plateRevisionId: 31, plateRevisionNumber: 1 };
        }),
      },
    } satisfies AcceptedPlateWorkspaceDependencies;
    deps.loadGeometry.mockResolvedValue({
      kind: "ready",
      geometryByToken: new Map([[token, geometry()]]),
    });

    await expect(
      initializeAcceptedPlates(deps, {
        profileId: 7,
        expected: basis,
        expectedPlateRevisionId: null,
        assignments: [{ token, printerId: "printer-one" }],
      }),
    ).resolves.toMatchObject({
      kind: "workspace",
      workspace: {
        kind: "ready",
        plate_revision_id: 31,
        plate_revision_number: 1,
      },
    });

    expect(read).toHaveBeenCalledOnce();
  });

  it("returns an exact initialization replay without reopening artifacts or writing", async () => {
    const deps = publishingDependencies();
    const command = {
      profileId: 7,
      expected: basis,
      expectedPlateRevisionId: null,
      assignments: [{ token, printerId: "printer-one" }],
    } as const;
    const first = await initializeAcceptedPlates(deps, command);
    expect(first).toMatchObject({ kind: "workspace", workspace: { plate_revision_id: 31 } });
    deps.loadGeometry.mockClear();
    deps.publish.mockClear();

    const replay = await initializeAcceptedPlates(deps, command);

    expect(replay).toEqual(first);
    expect(deps.loadGeometry).not.toHaveBeenCalled();
    expect(deps.publish).not.toHaveBeenCalled();
  });

  it("rejects missing Printer height before opening artifacts", async () => {
    const deps = publishingDependencies({ bed_height_mm: null });

    await expect(
      initializeAcceptedPlates(deps, {
        profileId: 7,
        expected: basis,
        expectedPlateRevisionId: null,
        assignments: [{ token, printerId: "printer-one" }],
      }),
    ).resolves.toEqual({ kind: "missing_printer_geometry", printerIds: ["printer-one"] });

    expect(deps.loadGeometry).not.toHaveBeenCalled();
    expect(deps.publish).not.toHaveBeenCalled();
  });

  it("reports a part that fits only after rotation as too large", async () => {
    const deps = publishingDependencies({
      bed_width_mm: 100,
      bed_depth_mm: 140,
      bed_height_mm: 80,
      margin_mm: 10,
    });
    deps.loadGeometry.mockResolvedValue({
      kind: "ready",
      geometryByToken: new Map([[token, geometry(100_000, 70_000, 10_000)]]),
    });

    await expect(
      initializeAcceptedPlates(deps, {
        profileId: 7,
        expected: basis,
        expectedPlateRevisionId: null,
        assignments: [{ token, printerId: "printer-one" }],
      }),
    ).resolves.toEqual({ kind: "unit_too_large", token, printerId: "printer-one" });

    expect(deps.publish).not.toHaveBeenCalled();
  });

  it("does not publish when accepted artifact verification fails", async () => {
    const deps = dependencies();
    deps.loadGeometry.mockResolvedValue({
      kind: "artifact_unavailable",
      token,
      reason: "digest_mismatch",
    });

    await expect(
      initializeAcceptedPlates(deps, {
        profileId: 7,
        expected: basis,
        expectedPlateRevisionId: null,
        assignments: [{ token, printerId: "printer-one" }],
      }),
    ).resolves.toEqual({
      kind: "artifact_unavailable",
      token,
      reason: "digest_mismatch",
    });

    expect(deps.publish).not.toHaveBeenCalled();
  });
});
