import { createHash } from "node:crypto";
import {
  parseAcceptedPlateId,
  parseRequiredUnitTokenContract,
  type AcceptedPlatePlacedUnit,
  type AcceptedPlatePrinter,
  type AcceptedPlateSetupUnit as AcceptedPlateSetupUnitContract,
  type AcceptedPlateUnplacedUnit,
  type AcceptedPlateView,
  type AcceptedPlateWorkspace,
  type ProductionGroupingRule,
} from "@print-partner/contracts";
import {
  arrangeAcceptedUnits,
  packAcceptedUnits,
  type AcceptedPlacedPackingUnit,
  type PrinterMachine,
} from "@print-partner/domain";
import type {
  AcceptedPlate,
  AcceptedPlateInput,
  AcceptedPlateSetupUnit,
  PublishAcceptedPlatesCommand,
  PublishAcceptedPlatesResult,
  ReadAcceptedPlateWorkspaceInputResult,
} from "../db/accepted-plates.js";
import type { AcceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { parseRequiredUnitToken, type RequiredUnitToken } from "./required-units.js";
import {
  loadAcceptedArtifactGeometry,
  type AcceptedArtifactGeometryLimits,
  type LoadAcceptedArtifactGeometryResult,
} from "./accepted-artifact-geometry.js";
import {
  loadProductionPackingRules,
  productionPackingBuckets,
} from "./production-packing-rules.js";
import { productionSetupSettingKey } from "./production-setup-store.js";
import {
  acceptedPrinter,
  basisContract,
  compareUtf8,
  sameAcceptedPlanBasis,
} from "./accepted-plate-workspace-model.js";

export { acceptedPrinter } from "./accepted-plate-workspace-model.js";

type WorkspaceRepository = Readonly<{
  readAcceptedPlateWorkspaceInput(profileId: number): ReadAcceptedPlateWorkspaceInputResult;
  publishAcceptedPlates(command: PublishAcceptedPlatesCommand): PublishAcceptedPlatesResult;
  getSetting?(key: string): string | null;
}>;

export type AcceptedPlateWorkspaceDependencies = Readonly<{
  repository: WorkspaceRepository;
  reposDir: string;
  limits: AcceptedArtifactGeometryLimits;
  loadPrinters: () => readonly PrinterMachine[];
  loadGeometry?: typeof loadAcceptedArtifactGeometry;
}>;

type AcceptedPlateWorkspaceReadResult =
  | { readonly kind: "workspace"; readonly workspace: AcceptedPlateWorkspace }
  | { readonly kind: "profile_not_found" }
  | {
      readonly kind: "accepted_state_unavailable";
      readonly reason: "compatibility_dirty" | "uninitialized";
    }
  | { readonly kind: "transaction_unavailable" };

type InitializeAcceptedPlatesCommand = Readonly<{
  profileId: number;
  expected: AcceptedPlanBasis;
  expectedPlateRevisionId: number | null;
  assignments: readonly Readonly<{
    token: RequiredUnitToken;
    printerId: string | null;
  }>[];
}>;

type AssignmentFailure =
  | {
      readonly kind: "missing_assignment";
      readonly tokens: readonly RequiredUnitToken[];
    }
  | {
      readonly kind: "duplicate_assignment";
      readonly tokens: readonly RequiredUnitToken[];
    }
  | {
      readonly kind: "unknown_unit_token";
      readonly tokens: readonly RequiredUnitToken[];
    }
  | {
      readonly kind: "unassigned_units";
      readonly tokens: readonly RequiredUnitToken[];
    }
  | {
      readonly kind: "printer_not_found";
      readonly printerIds: readonly string[];
    }
  | {
      readonly kind: "missing_printer_geometry";
      readonly printerIds: readonly string[];
    };

export type InitializeAcceptedPlatesResult =
  | {
      readonly kind: "workspace";
      readonly workspace: Extract<AcceptedPlateWorkspace, { kind: "ready" }>;
    }
  | AssignmentFailure
  | {
      readonly kind: "unit_too_large";
      readonly token: RequiredUnitToken;
      readonly printerId: string;
    }
  | Exclude<LoadAcceptedArtifactGeometryResult, { readonly kind: "ready" }>
  | Exclude<PublishAcceptedPlatesResult, { readonly kind: "published" | "unchanged" }>
  | { readonly kind: "profile_not_found" }
  | { readonly kind: "empty_plan" };

function currentPrinters(dependencies: AcceptedPlateWorkspaceDependencies): AcceptedPlatePrinter[] {
  return dependencies
    .loadPrinters()
    .map(acceptedPrinter)
    .filter((printer): printer is AcceptedPlatePrinter => printer !== null)
    .sort((left, right) => compareUtf8(left.id, right.id));
}

function setupUnit(unit: AcceptedPlateSetupUnit): AcceptedPlateSetupUnitContract {
  return {
    token: parseRequiredUnitTokenContract(unit.token),
    part_id: unit.partId,
    object_name: unit.objectName,
    filename: unit.filename,
    relative_path: unit.relativePath,
    source_directory: unit.sourceDirectory,
    source_layer: unit.sourceLayer,
    role: unit.role,
    filament_color_id: unit.filamentColorId,
    ...(unit.filamentCustomHex == null ? {} : { filament_custom_hex: unit.filamentCustomHex }),
    ...(unit.filamentHex == null ? {} : { filament_hex: unit.filamentHex }),
    completed: unit.completed ?? false,
  };
}

function placedUnit(
  unit: AcceptedPlate["units"][number],
  setup: AcceptedPlateSetupUnit,
): AcceptedPlatePlacedUnit {
  return {
    ...setupUnit(setup),
    x_um: unit.xUm,
    y_um: unit.yUm,
    width_um: unit.widthUm,
    depth_um: unit.depthUm,
    height_um: unit.heightUm,
    placement: unit.placement === "manual" ? "manual" : "auto",
    pinned: unit.pinned === true,
  };
}

function unplacedWorkspaceUnits(
  plates: readonly AcceptedPlate[],
  setupByToken: ReadonlyMap<string, AcceptedPlateSetupUnit>,
): AcceptedPlateUnplacedUnit[] {
  return plates.flatMap((plate) =>
    plate.units
      .filter((unit) => unit.placement === "unplaced")
      .map((unit) => {
        const setup = setupByToken.get(unit.token);
        if (!setup) throw new Error("Accepted Plate setup metadata is missing");
        return {
          ...setupUnit(setup),
          plate_id: parseAcceptedPlateId(plate.plateId),
          printer_id: plate.printerId,
          width_um: unit.widthUm,
          depth_um: unit.depthUm,
          height_um: unit.heightUm,
        };
      }),
  );
}

function plateView(
  plate: AcceptedPlate,
  setupByToken: ReadonlyMap<string, AcceptedPlateSetupUnit>,
): AcceptedPlateView {
  return {
    plate_id: parseAcceptedPlateId(plate.plateId),
    ordinal: plate.ordinal,
    printer: {
      id: plate.printerId,
      name: plate.printerName,
      model: plate.printerModel,
      bed_width_um: plate.bedWidthUm,
      bed_depth_um: plate.bedDepthUm,
      bed_height_um: plate.bedHeightUm,
      margin_um: plate.marginUm,
    },
    units: plate.units
      .filter((unit) => unit.placement !== "unplaced")
      .map((unit) => {
        const setup = setupByToken.get(unit.token);
        if (!setup) throw new Error("Accepted Plate setup metadata is missing");
        return placedUnit(unit, setup);
      }),
  };
}

function publishedWorkspace(
  input: Readonly<{
    basis: AcceptedPlanBasis;
    plateRevisionId: number;
    plateRevisionNumber: number;
    undoFromRevisionId?: number | null;
    printers: readonly AcceptedPlatePrinter[];
    plates: readonly AcceptedPlateInput[];
    units: readonly AcceptedPlateSetupUnit[];
  }>,
): Extract<AcceptedPlateWorkspace, { kind: "ready" }> {
  const setupByToken = new Map<string, AcceptedPlateSetupUnit>(
    input.units.map((unit) => [unit.token, unit]),
  );
  const placedTokens = new Set(
    input.plates.flatMap((plate) => plate.units.map((unit) => unit.token)),
  );
  return {
    kind: "ready",
    basis: basisContract(input.basis),
    plate_revision_id: input.plateRevisionId,
    plate_revision_number: input.plateRevisionNumber,
    arrange_undo_revision_id: input.undoFromRevisionId ?? null,
    printers: [...input.printers],
    plates: input.plates.map((plate, index) =>
      plateView(
        {
          ...plate,
          ordinal: index + 1,
          units: plate.units.map((unit) => {
            const setup = setupByToken.get(unit.token);
            if (!setup) throw new Error("Accepted Plate setup metadata is missing");
            return { ...unit, objectName: setup.objectName };
          }),
        },
        setupByToken,
      ),
    ),
    unplaced: unplacedWorkspaceUnits(
      input.plates.map((plate, index) => ({
        ...plate,
        ordinal: index + 1,
        units: plate.units.map((unit) => {
          const setup = setupByToken.get(unit.token);
          if (!setup) throw new Error("Accepted Plate setup metadata is missing");
          return { ...unit, objectName: setup.objectName };
        }),
      })),
      setupByToken,
    ),
    unassigned: input.units.filter((unit) => !placedTokens.has(unit.token)).map(setupUnit),
  };
}

function presentWorkspace(
  dependencies: AcceptedPlateWorkspaceDependencies,
  input: Extract<ReadAcceptedPlateWorkspaceInputResult, { kind: "setup" | "ready" }>,
): AcceptedPlateWorkspace {
  if (input.units.length === 0) return { kind: "empty_plan" };
  const printers = currentPrinters(dependencies);
  if (input.kind === "setup") {
    return {
      kind: "setup",
      basis: basisContract(input.basis),
      expected_plate_revision_id: input.expectedPlateRevisionId,
      printers,
      units: input.units.map(setupUnit),
    };
  }
  const setupByToken = new Map(input.units.map((unit) => [unit.token, unit]));
  const placedTokens = new Set(
    input.plates.flatMap((plate) => plate.units.map((unit) => unit.token)),
  );
  return {
    kind: "ready",
    basis: basisContract(input.basis),
    plate_revision_id: input.plateRevisionId,
    plate_revision_number: input.plateRevisionNumber,
    arrange_undo_revision_id: input.undoFromRevisionId,
    printers,
    plates: input.plates.map((plate) => plateView(plate, setupByToken)),
    unplaced: unplacedWorkspaceUnits(input.plates, setupByToken),
    unassigned: input.units.filter((unit) => !placedTokens.has(unit.token)).map(setupUnit),
  };
}

export function readAcceptedPlateWorkspace(
  dependencies: AcceptedPlateWorkspaceDependencies,
  profileId: number,
): AcceptedPlateWorkspaceReadResult {
  const input = dependencies.repository.readAcceptedPlateWorkspaceInput(profileId);
  if (input.kind === "empty_plan") {
    return { kind: "workspace", workspace: { kind: "empty_plan" } };
  }
  if (input.kind !== "setup" && input.kind !== "ready") return input;
  return {
    kind: "workspace",
    workspace: presentWorkspace(dependencies, input),
  };
}

function assignmentFailure(
  units: readonly AcceptedPlateSetupUnit[],
  assignments: InitializeAcceptedPlatesCommand["assignments"],
): AssignmentFailure | null {
  const expected = new Set(units.map((unit) => unit.token));
  const seen = new Set<RequiredUnitToken>();
  const duplicates = new Set<RequiredUnitToken>();
  const unknown = new Set<RequiredUnitToken>();
  for (const assignment of assignments) {
    if (!expected.has(assignment.token)) unknown.add(assignment.token);
    if (seen.has(assignment.token)) duplicates.add(assignment.token);
    seen.add(assignment.token);
  }
  const byToken = (left: RequiredUnitToken, right: RequiredUnitToken) => compareUtf8(left, right);
  if (unknown.size > 0) return { kind: "unknown_unit_token", tokens: [...unknown].sort(byToken) };
  if (duplicates.size > 0)
    return {
      kind: "duplicate_assignment",
      tokens: [...duplicates].sort(byToken),
    };
  if (assignments.length === 0) {
    return { kind: "missing_assignment", tokens: [...expected].sort(byToken) };
  }
  const unassigned = assignments
    .filter((assignment) => assignment.printerId === null)
    .map((assignment) => assignment.token)
    .sort(byToken);
  return unassigned.length > 0 ? { kind: "unassigned_units", tokens: unassigned } : null;
}

export function plateId(basis: AcceptedPlanBasis, printerId: string, tokens: readonly string[]): string {
  const digest = createHash("sha256")
    .update(
      JSON.stringify([
        "accepted-plate-v1",
        basis.revisionDigest,
        basis.requiredUnitMappingDigest,
        printerId,
        [...tokens].sort(compareUtf8),
      ]),
    )
    .digest("hex");
  return `plate_${digest.slice(0, 32)}`;
}

type UnitDimensions = Readonly<{
  widthUm: number;
  depthUm: number;
  heightUm: number;
}>;

function packAssignedPlates(
  input: Readonly<{
    basis: AcceptedPlanBasis;
    units: readonly AcceptedPlateSetupUnit[];
    assignments: InitializeAcceptedPlatesCommand["assignments"];
    printers: ReadonlyMap<string, AcceptedPlatePrinter>;
    dimensions: ReadonlyMap<RequiredUnitToken, UnitDimensions>;
    rules: readonly ProductionGroupingRule[];
  }>,
):
  | readonly AcceptedPlateInput[]
  | {
      readonly kind: "unit_too_large";
      readonly token: RequiredUnitToken;
      readonly printerId: string;
    } {
  const assignmentByToken = new Map(
    input.assignments.map((assignment) => [assignment.token, assignment.printerId]),
  );
  const assignedIds = [...input.printers.keys()].sort(compareUtf8);
  const plates: AcceptedPlateInput[] = [];
  for (const printerId of assignedIds) {
    const printer = input.printers.get(printerId);
    if (!printer) throw new Error("Accepted Plate Printer geometry is missing");
    const units = input.units
      .filter((unit) => assignmentByToken.get(unit.token) === printerId)
      .map((unit) => {
        const dimensions = input.dimensions.get(unit.token);
        if (!dimensions) throw new Error("Accepted artifact geometry is missing");
        return { token: unit.token, ...dimensions };
      });
    const setupByToken = new Map(input.units.map((unit) => [unit.token, unit]));
    const packingBuckets = productionPackingBuckets(
      units.map((unit) => ({ ...setupByToken.get(unit.token)!, ...unit })),
      input.rules,
    );
    for (const bucket of packingBuckets) {
      const packed = packAcceptedUnits({
        printer: {
          bedWidthUm: printer.bed_width_um,
          bedDepthUm: printer.bed_depth_um,
          bedHeightUm: printer.bed_height_um,
          marginUm: printer.margin_um,
        },
        units: bucket,
      });
      if (packed.kind === "unit_too_large") {
        return {
          kind: "unit_too_large",
          token: parseRequiredUnitToken(packed.token),
          printerId,
        };
      }
      for (const packedPlate of packed.plates) {
        plates.push({
          plateId: plateId(
            input.basis,
            printerId,
            packedPlate.units.map((unit) => unit.token),
          ),
          printerId,
          printerName: printer.name,
          printerModel: printer.model,
          bedWidthUm: printer.bed_width_um,
          bedDepthUm: printer.bed_depth_um,
          bedHeightUm: printer.bed_height_um,
          marginUm: printer.margin_um,
          units: packedPlate.units,
        });
      }
    }
  }
  return plates;
}

function currentPlateInputs(plates: readonly AcceptedPlate[]): AcceptedPlateInput[] {
  return plates.map((plate) => ({
    plateId: plate.plateId,
    printerId: plate.printerId,
    printerName: plate.printerName,
    printerModel: plate.printerModel,
    bedWidthUm: plate.bedWidthUm,
    bedDepthUm: plate.bedDepthUm,
    bedHeightUm: plate.bedHeightUm,
    marginUm: plate.marginUm,
    units: plate.units.map((unit) => ({
      token: unit.token,
      xUm: unit.xUm,
      yUm: unit.yUm,
      widthUm: unit.widthUm,
      depthUm: unit.depthUm,
      heightUm: unit.heightUm,
      placement: unit.placement,
      pinned: unit.pinned,
    })),
  }));
}

function samePlateInputs(
  left: readonly AcceptedPlateInput[],
  right: readonly AcceptedPlateInput[],
): boolean {
  if (left.length !== right.length) return false;
  return left.every((plate, plateIndex) => {
    const candidate = right[plateIndex];
    if (!candidate || plate.units.length !== candidate.units.length) return false;
    if (
      plate.plateId !== candidate.plateId ||
      plate.printerId !== candidate.printerId ||
      plate.printerName !== candidate.printerName ||
      plate.printerModel !== candidate.printerModel ||
      plate.bedWidthUm !== candidate.bedWidthUm ||
      plate.bedDepthUm !== candidate.bedDepthUm ||
      plate.bedHeightUm !== candidate.bedHeightUm ||
      plate.marginUm !== candidate.marginUm
    )
      return false;
    return plate.units.every((unit, unitIndex) => {
      const other = candidate.units[unitIndex];
      return Boolean(
        other &&
        unit.token === other.token &&
        unit.xUm === other.xUm &&
        unit.yUm === other.yUm &&
        unit.widthUm === other.widthUm &&
        unit.depthUm === other.depthUm &&
        unit.heightUm === other.heightUm &&
        (unit.placement ?? "auto") === (other.placement ?? "auto") &&
        (unit.pinned ?? false) === (other.pinned ?? false),
      );
    });
  });
}

function isPackedPlateInputs(
  value: readonly AcceptedPlateInput[] | { readonly kind: "unit_too_large" },
): value is readonly AcceptedPlateInput[] {
  return Array.isArray(value);
}

export async function initializeAcceptedPlates(
  dependencies: AcceptedPlateWorkspaceDependencies,
  command: InitializeAcceptedPlatesCommand,
): Promise<InitializeAcceptedPlatesResult> {
  const input = dependencies.repository.readAcceptedPlateWorkspaceInput(command.profileId);
  if (
    input.kind === "empty_plan" ||
    input.kind === "profile_not_found" ||
    input.kind === "transaction_unavailable" ||
    input.kind === "accepted_state_unavailable"
  ) {
    return input;
  }
  if (
    !sameAcceptedPlanBasis(input.basis, command.expected) ||
    command.profileId !== command.expected.profileId
  ) {
    return { kind: "stale_accepted_plan" };
  }
  const packingRules = loadProductionPackingRules(
    dependencies.repository.getSetting?.(productionSetupSettingKey(command.profileId)),
    command.profileId,
  );
  const revisionMatches = input.expectedPlateRevisionId === command.expectedPlateRevisionId;
  if (!revisionMatches && input.kind !== "ready") return { kind: "plate_revision_changed" };
  const assignmentError = assignmentFailure(input.units, command.assignments);
  if (assignmentError) {
    return revisionMatches ? assignmentError : { kind: "plate_revision_changed" };
  }

  const fleet = dependencies.loadPrinters();
  const fleetById = new Map(fleet.map((printer) => [printer.id, printer]));
  const printers = fleet
    .map(acceptedPrinter)
    .filter((printer): printer is AcceptedPlatePrinter => printer !== null)
    .sort((left, right) => compareUtf8(left.id, right.id));
  const assignedIds = [...new Set(command.assignments.map((assignment) => assignment.printerId))]
    .filter((printerId): printerId is string => printerId !== null)
    .sort(compareUtf8);
  const unknownPrinters = assignedIds.filter((printerId) => !fleetById.has(printerId));
  if (unknownPrinters.length > 0) {
    return revisionMatches
      ? { kind: "printer_not_found", printerIds: unknownPrinters }
      : { kind: "plate_revision_changed" };
  }
  const printerById = new Map<string, AcceptedPlatePrinter>();
  const missingGeometry: string[] = [];
  for (const printerId of assignedIds) {
    const machine = fleetById.get(printerId);
    const printer = machine ? acceptedPrinter(machine) : null;
    if (printer) printerById.set(printerId, printer);
    else missingGeometry.push(printerId);
  }
  if (missingGeometry.length > 0) {
    return revisionMatches
      ? { kind: "missing_printer_geometry", printerIds: missingGeometry }
      : { kind: "plate_revision_changed" };
  }

  if (!revisionMatches && input.kind === "ready") {
    const dimensions = new Map<RequiredUnitToken, UnitDimensions>();
    for (const plate of input.plates) {
      for (const unit of plate.units) {
        dimensions.set(parseRequiredUnitToken(unit.token), {
          widthUm: unit.widthUm,
          depthUm: unit.depthUm,
          heightUm: unit.heightUm,
        });
      }
    }
    const replay = packAssignedPlates({
      basis: command.expected,
      units: input.units,
      assignments: command.assignments,
      printers: printerById,
      dimensions,
      rules: packingRules,
    });
    if (isPackedPlateInputs(replay) && samePlateInputs(replay, currentPlateInputs(input.plates))) {
      const workspace = presentWorkspace(dependencies, input);
      if (workspace.kind !== "ready") throw new Error("Accepted Plate replay workspace is empty");
      return { kind: "workspace", workspace };
    }
    return { kind: "plate_revision_changed" };
  }

  const assignedTokens = new Set(command.assignments.map((assignment) => assignment.token));
  const loaded = await (dependencies.loadGeometry ?? loadAcceptedArtifactGeometry)({
    reposDir: dependencies.reposDir,
    units: input.units.filter((unit) => assignedTokens.has(unit.token)),
    limits: dependencies.limits,
  });
  if (loaded.kind !== "ready") return loaded;

  const dimensions = new Map(
    [...loaded.geometryByToken].map(([unitToken, value]) => [unitToken, value.dimensions]),
  );
  const packed = packAssignedPlates({
    basis: command.expected,
    units: input.units,
    assignments: command.assignments,
    printers: printerById,
    dimensions,
    rules: packingRules,
  });
  if (!isPackedPlateInputs(packed)) return packed;
  const plates = packed;

  const published = dependencies.repository.publishAcceptedPlates({
    profileId: command.profileId,
    expected: command.expected,
    expectedPlateRevisionId: command.expectedPlateRevisionId,
    plates,
  });
  if (published.kind !== "published" && published.kind !== "unchanged") return published;
  return {
    kind: "workspace",
    workspace: publishedWorkspace({
      basis: input.basis,
      plateRevisionId: published.plateRevisionId,
      plateRevisionNumber: published.plateRevisionNumber,
      printers,
      plates,
      units: input.units,
    }),
  };
}

type ArrangeAcceptedPlatesServiceCommand = Readonly<{
  profileId: number;
  expected: AcceptedPlanBasis;
  expectedPlateRevisionId: number;
  mode: "unplaced" | "all";
}>;

function printerGeometry(plate: AcceptedPlateInput) {
  return {
    bedWidthUm: plate.bedWidthUm,
    bedDepthUm: plate.bedDepthUm,
    bedHeightUm: plate.bedHeightUm,
    marginUm: plate.marginUm,
  };
}

function arrangedPlates(
  mode: "unplaced" | "all",
  basis: AcceptedPlanBasis,
  plates: readonly AcceptedPlateInput[],
  setupUnits: readonly AcceptedPlateSetupUnit[],
  rules: readonly ProductionGroupingRule[],
):
  | readonly AcceptedPlateInput[]
  | {
      readonly kind: "unit_too_large";
      readonly token: RequiredUnitToken;
      readonly printerId: string;
    } {
  if (mode === "unplaced") {
    const next: AcceptedPlateInput[] = [];
    for (const plate of plates) {
      const units: AcceptedPlacedPackingUnit[] = plate.units.map((unit) => ({
        token: unit.token,
        widthUm: unit.widthUm,
        depthUm: unit.depthUm,
        heightUm: unit.heightUm,
        xUm: unit.xUm,
        yUm: unit.yUm,
        placement:
          unit.placement === "manual" || unit.placement === "unplaced" ? unit.placement : "auto",
        pinned: unit.pinned === true,
      }));
      const packed = arrangeAcceptedUnits({
        mode,
        printer: printerGeometry(plate),
        units,
      });
      if (packed.kind !== "packed") {
        return {
          kind: packed.kind,
          token: parseRequiredUnitToken(packed.token),
          printerId: plate.printerId,
        };
      }
      const originalByToken = new Map(units.map((unit) => [unit.token, unit]));
      for (const [index, packedPlate] of packed.plates.entries()) {
        next.push({
          ...plate,
          plateId:
            index === 0
              ? plate.plateId
              : plateId(
                  basis,
                  plate.printerId,
                  packedPlate.units.map((unit) => unit.token),
                ),
          units: packedPlate.units.map((unit) => {
            const original = originalByToken.get(unit.token);
            const fixed = original?.placement === "manual" || original?.pinned === true;
            return {
              ...unit,
              placement:
                fixed && original?.placement === "manual" ? ("manual" as const) : ("auto" as const),
              pinned: fixed ? original?.pinned === true : false,
            };
          }),
        });
      }
    }
    return next;
  }

  const byPrinter = new Map<string, AcceptedPlateInput[]>();
  for (const plate of plates) {
    const printerPlates = byPrinter.get(plate.printerId) ?? [];
    printerPlates.push(plate);
    byPrinter.set(plate.printerId, printerPlates);
  }
  const next: AcceptedPlateInput[] = [];
  const setupByToken = new Map(setupUnits.map((unit) => [unit.token, unit]));
  for (const printerPlates of byPrinter.values()) {
    const first = printerPlates[0];
    if (!first) continue;
    const units = printerPlates.flatMap((plate) =>
      plate.units.map(
        (unit) =>
          ({
            token: unit.token,
            widthUm: unit.widthUm,
            depthUm: unit.depthUm,
            heightUm: unit.heightUm,
            xUm: unit.xUm,
            yUm: unit.yUm,
            placement:
              unit.placement === "manual" || unit.placement === "unplaced"
                ? unit.placement
                : ("auto" as const),
            pinned: unit.pinned === true,
          }) satisfies AcceptedPlacedPackingUnit,
      ),
    );
    const buckets = productionPackingBuckets(
      units.map((unit) => ({
        ...setupByToken.get(parseRequiredUnitToken(unit.token))!,
        ...unit,
      })),
      rules,
    );
    let nextPlateIndex = 0;
    for (const bucket of buckets) {
      const packed = arrangeAcceptedUnits({
        mode,
        printer: printerGeometry(first),
        units: bucket,
      });
      if (packed.kind !== "packed") {
        return {
          kind: packed.kind,
          token: parseRequiredUnitToken(packed.token),
          printerId: first.printerId,
        };
      }
      for (const packedPlate of packed.plates) {
        next.push({
          ...first,
          plateId:
            printerPlates[nextPlateIndex]?.plateId ??
            plateId(
              basis,
              first.printerId,
              packedPlate.units.map((unit) => unit.token),
            ),
          units: packedPlate.units.map((unit) => ({
            ...unit,
            placement: "auto",
            pinned: false,
          })),
        });
        nextPlateIndex += 1;
      }
    }
  }
  return next;
}

export function arrangeAcceptedPlates(
  dependencies: AcceptedPlateWorkspaceDependencies,
  command: ArrangeAcceptedPlatesServiceCommand,
): InitializeAcceptedPlatesResult {
  const input = dependencies.repository.readAcceptedPlateWorkspaceInput(command.profileId);
  if (
    input.kind === "empty_plan" ||
    input.kind === "profile_not_found" ||
    input.kind === "transaction_unavailable" ||
    input.kind === "accepted_state_unavailable"
  ) {
    return input;
  }
  if (input.kind !== "ready") return { kind: "plate_revision_changed" };
  if (
    !sameAcceptedPlanBasis(input.basis, command.expected) ||
    command.profileId !== command.expected.profileId
  ) {
    return { kind: "stale_accepted_plan" };
  }
  if (input.plateRevisionId !== command.expectedPlateRevisionId) {
    return { kind: "plate_revision_changed" };
  }
  const packingRules = loadProductionPackingRules(
    dependencies.repository.getSetting?.(productionSetupSettingKey(command.profileId)),
    command.profileId,
  );
  const packed = arrangedPlates(
    command.mode,
    input.basis,
    currentPlateInputs(input.plates),
    input.units,
    packingRules,
  );
  if ("kind" in packed) return packed;
  const published = dependencies.repository.publishAcceptedPlates({
    profileId: command.profileId,
    expected: command.expected,
    expectedPlateRevisionId: command.expectedPlateRevisionId,
    plates: packed,
    undoFromRevisionId: command.mode === "all" ? input.plateRevisionId : null,
  });
  if (published.kind !== "published" && published.kind !== "unchanged") return published;
  return {
    kind: "workspace",
    workspace: publishedWorkspace({
      basis: input.basis,
      plateRevisionId: published.plateRevisionId,
      plateRevisionNumber: published.plateRevisionNumber,
      undoFromRevisionId:
        published.kind === "published"
          ? command.mode === "all"
            ? input.plateRevisionId
            : null
          : input.undoFromRevisionId,
      printers: currentPrinters(dependencies),
      plates: packed,
      units: input.units,
    }),
  };
}
