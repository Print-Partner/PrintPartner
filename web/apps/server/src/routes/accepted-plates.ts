import type { FastifyInstance, FastifyReply, FastifyRequest } from "fastify";
import type { AppRepository } from "../db/repository.js";
import { AcceptedPlateIntegrityError, type AcceptedPlateInput } from "../db/accepted-plates.js";
import { AcceptedPlanOperationalIntegrityError } from "../db/accepted-plan-operational.js";
import {
  acceptedPrinter,
  arrangeAcceptedPlates,
  initializeAcceptedPlates,
  readAcceptedPlateWorkspace,
  type AcceptedPlateWorkspaceDependencies,
  type InitializeAcceptedPlatesResult,
} from "../services/accepted-plate-workspace.js";
import { loadFleet } from "../services/printer-fleet.js";
import {
  parseArrangeRequest,
  parseInitializeRequest,
  parseMoveRequest,
  parsePinRequest,
  parseRestoreRequest,
  parseRevisionRequest,
  parseToken,
  parseTransferRequest,
  profileId,
} from "./accepted-plates-route-model.js";
import { isRecord } from "../lib/guards.js";
import { MAX_BULK_JSON_BODY_BYTES } from "../services/upload-limits.js";

type RouteDependencies = Readonly<{
  repo: AppRepository;
  reposDir: string;
}>;

const WORKSPACE_LIMITS = {
  maxTotalSourceBytes: 256 * 1024 * 1024,
  maxObjects: 10_000,
  maxTriangles: 5_000_000,
} as const;

function serviceDependencies(dependencies: RouteDependencies): AcceptedPlateWorkspaceDependencies {
  return {
    repository: dependencies.repo,
    reposDir: dependencies.reposDir,
    limits: WORKSPACE_LIMITS,
    loadPrinters: () => loadFleet(dependencies.repo),
  };
}

function fleetPrinterGeometry(
  repo: AppRepository,
  printerId: string,
): Omit<AcceptedPlateInput, "plateId" | "units"> | null {
  const machine = loadFleet(repo).find((candidate) => candidate.id === printerId);
  const printer = machine ? acceptedPrinter(machine) : null;
  return printer
    ? {
        printerId: printer.id,
        printerName: printer.name,
        printerModel: printer.model,
        bedWidthUm: printer.bed_width_um,
        bedDepthUm: printer.bed_depth_um,
        bedHeightUm: printer.bed_height_um,
        marginUm: printer.margin_um,
      }
    : null;
}

function sendError(
  reply: FastifyReply,
  status: number,
  code: string,
  detail: string,
  fields: Record<string, unknown> = {},
) {
  return reply.status(status).send({ detail, code, ...fields });
}

function sendReadFailure(
  reply: FastifyReply,
  result: Exclude<ReturnType<typeof readAcceptedPlateWorkspace>, { kind: "workspace" }>,
) {
  if (result.kind === "profile_not_found") {
    return sendError(reply, 404, "profile_not_found", "Plan not found");
  }
  if (result.kind === "transaction_unavailable") {
    return sendError(
      reply,
      503,
      "accepted_plate_update_unavailable",
      "Accepted Plate update is unavailable",
    );
  }
  return sendError(reply, 409, "accepted_state_unavailable", "Accepted Plan state is unavailable");
}

function sendInitializeFailure(
  reply: FastifyReply,
  result: Exclude<InitializeAcceptedPlatesResult, { kind: "workspace" }>,
) {
  switch (result.kind) {
    case "profile_not_found":
      return sendError(reply, 404, "profile_not_found", "Plan not found");
    case "empty_plan":
      return reply.send({ kind: "empty_plan" });
    case "missing_assignment":
    case "duplicate_assignment":
    case "unknown_unit_token":
    case "unassigned_units":
      return sendError(reply, 422, result.kind, "Printer assignments are incomplete", {
        tokens: result.tokens,
      });
    case "printer_not_found":
      return sendError(reply, 422, "printer_not_found", "Printer was not found", {
        printer_ids: result.printerIds,
      });
    case "missing_printer_geometry":
      return sendError(
        reply,
        422,
        "missing_printer_geometry",
        "Printer build geometry is incomplete",
        { printer_ids: result.printerIds },
      );
    case "unit_too_large":
      return sendError(
        reply,
        422,
        "unit_too_large",
        "Required unit does not fit the selected Printer",
        { token: result.token, printer_id: result.printerId },
      );
    case "artifact_unavailable":
      return sendError(reply, 409, result.reason, "Accepted artifact is unavailable", {
        token: result.token,
      });
    case "invalid_stl":
      return sendError(reply, 409, "invalid_stl", "Accepted artifact is not a valid STL", {
        token: result.token,
      });
    case "degenerate_geometry":
      return sendError(
        reply,
        422,
        "degenerate_geometry",
        "Accepted artifact geometry cannot be arranged",
        { token: result.token },
      );
    case "limit_exceeded":
      return sendError(reply, 413, "limit_exceeded", "Accepted Plate limit exceeded", {
        limit: result.limit,
      });
    case "stale_accepted_plan":
      return sendError(reply, 409, "accepted_plan_changed", "Accepted Plan changed");
    case "plate_revision_changed":
      return sendError(reply, 409, "plate_revision_changed", "Plate revision changed");
    case "plan_archived":
      return sendError(reply, 409, "plan_archived", "Plan is archived");
    case "accepted_state_unavailable":
      return sendError(
        reply,
        409,
        "accepted_state_unavailable",
        "Accepted Plan state is unavailable",
      );
    case "invalid_units":
      return sendError(reply, 422, "invalid_units", "Accepted Plate units are invalid");
    case "invalid_geometry":
      return sendError(reply, 422, result.reason, "Accepted Plate geometry is invalid");
    case "transaction_unavailable":
      return sendError(
        reply,
        503,
        "accepted_plate_update_unavailable",
        "Accepted Plate update is unavailable",
      );
  }
}

function sendMoveFailure(
  reply: FastifyReply,
  result: Exclude<
    ReturnType<AppRepository["moveAcceptedPlateUnit"]>,
    { kind: "moved" | "unchanged" }
  >,
) {
  switch (result.kind) {
    case "stale_accepted_plan":
      return sendError(reply, 409, "accepted_plan_changed", "Accepted Plan changed");
    case "plate_revision_changed":
      return sendError(reply, 409, "plate_revision_changed", "Plate revision changed");
    case "plan_archived":
      return sendError(reply, 409, "plan_archived", "Plan is archived");
    case "accepted_state_unavailable":
      return sendError(
        reply,
        409,
        "accepted_state_unavailable",
        "Accepted Plan state is unavailable",
      );
    case "unit_not_found":
    case "invalid_units":
      return sendError(reply, 422, result.kind, "Accepted Plate unit is invalid");
    case "invalid_geometry":
      return sendError(reply, 422, result.reason, "Accepted Plate geometry is invalid");
    case "transaction_unavailable":
      return sendError(
        reply,
        503,
        "accepted_plate_update_unavailable",
        "Accepted Plate update is unavailable",
      );
  }
}

function sendIntegrityFailure(request: FastifyRequest, reply: FastifyReply, error: unknown) {
  if (
    error instanceof AcceptedPlateIntegrityError ||
    error instanceof AcceptedPlanOperationalIntegrityError
  ) {
    request.log.error(
      { err: error, operation: "accepted_plate", code: error.code },
      "Accepted Plate integrity failure",
    );
  } else {
    request.log.error(
      { err: error, operation: "accepted_plate", failure: "unexpected" },
      "Accepted Plate request failed",
    );
  }
  return sendError(reply, 500, "internal_error", "Accepted Plate data is inconsistent");
}

export async function registerAcceptedPlateRoutes(
  app: FastifyInstance,
  dependencies: RouteDependencies,
): Promise<void> {
  const service = serviceDependencies(dependencies);

  app.get("/plans/:id/plates", async (request, reply) => {
    const id = profileId(request);
    if (id == null) return sendError(reply, 400, "invalid_request", "Request is invalid");
    try {
      const result = readAcceptedPlateWorkspace(service, id);
      return result.kind === "workspace" ? result.workspace : sendReadFailure(reply, result);
    } catch (error) {
      return sendIntegrityFailure(request, reply, error);
    }
  });

  app.post(
    "/plans/:id/plates/initialize",
    { bodyLimit: MAX_BULK_JSON_BODY_BYTES },
    async (request, reply) => {
      const id = profileId(request);
      const body = parseInitializeRequest(request.body);
      if (id == null || !body || body.expected.profileId !== id) {
        return sendError(reply, 400, "invalid_request", "Request is invalid");
      }
      try {
        const result = await initializeAcceptedPlates(service, { profileId: id, ...body });
        return result.kind === "workspace"
          ? result.workspace
          : sendInitializeFailure(reply, result);
      } catch (error) {
        return sendIntegrityFailure(request, reply, error);
      }
    },
  );

  app.patch("/plans/:id/plates/:plateId/units/:token", async (request, reply) => {
    const id = profileId(request);
    const params = isRecord(request.params) ? request.params : {};
    const plateId = typeof params.plateId === "string" ? params.plateId.trim() : "";
    const token = parseToken(params.token);
    const body = parseMoveRequest(request.body);
    if (
      id == null ||
      !plateId ||
      plateId.length > 200 ||
      !token ||
      !body ||
      body.expected.profileId !== id
    ) {
      return sendError(reply, 400, "invalid_request", "Request is invalid");
    }
    try {
      const result = dependencies.repo.moveAcceptedPlateUnit({
        profileId: id,
        expected: body.expected,
        expectedPlateRevisionId: body.expectedPlateRevisionId,
        plateId,
        token,
        xUm: body.xUm,
        yUm: body.yUm,
      });
      if (result.kind === "moved" || result.kind === "unchanged") {
        return {
          plate_revision_id: result.plateRevisionId,
          plate_revision_number: result.plateRevisionNumber,
        };
      }
      return sendMoveFailure(reply, result);
    } catch (error) {
      return sendIntegrityFailure(request, reply, error);
    }
  });

  app.patch("/plans/:id/plates/:plateId/units/:token/pin", async (request, reply) => {
    const id = profileId(request);
    const params = isRecord(request.params) ? request.params : {};
    const plateId = typeof params.plateId === "string" ? params.plateId.trim() : "";
    const token = parseToken(params.token);
    const body = parsePinRequest(request.body);
    if (
      id == null ||
      !/^plate_[0-9a-f]{32}$/.test(plateId) ||
      !token ||
      !body ||
      body.expected.profileId !== id
    ) {
      return sendError(reply, 400, "invalid_request", "Request is invalid");
    }
    try {
      const result = dependencies.repo.pinAcceptedPlateUnit({
        profileId: id,
        expected: body.expected,
        expectedPlateRevisionId: body.expectedPlateRevisionId,
        plateId,
        token,
        pinned: body.pinned,
      });
      if (result.kind === "moved" || result.kind === "unchanged") {
        return {
          plate_revision_id: result.plateRevisionId,
          plate_revision_number: result.plateRevisionNumber,
        };
      }
      return sendMoveFailure(reply, result);
    } catch (error) {
      return sendIntegrityFailure(request, reply, error);
    }
  });

  app.post("/plans/:id/plates/:plateId/units/:token/unplace", async (request, reply) => {
    const id = profileId(request);
    const params = isRecord(request.params) ? request.params : {};
    const plateId = typeof params.plateId === "string" ? params.plateId.trim() : "";
    const token = parseToken(params.token);
    const body = parseRevisionRequest(request.body);
    if (
      id == null ||
      !/^plate_[0-9a-f]{32}$/.test(plateId) ||
      !token ||
      !body ||
      body.expected.profileId !== id
    ) {
      return sendError(reply, 400, "invalid_request", "Request is invalid");
    }
    try {
      const result = dependencies.repo.unplaceAcceptedPlateUnit({
        profileId: id,
        expected: body.expected,
        expectedPlateRevisionId: body.expectedPlateRevisionId,
        plateId,
        token,
      });
      if (result.kind === "moved" || result.kind === "unchanged") {
        return {
          plate_revision_id: result.plateRevisionId,
          plate_revision_number: result.plateRevisionNumber,
        };
      }
      return sendMoveFailure(reply, result);
    } catch (error) {
      return sendIntegrityFailure(request, reply, error);
    }
  });

  app.post("/plans/:id/plates/:plateId/units/:token/transfer", async (request, reply) => {
    const id = profileId(request);
    const params = isRecord(request.params) ? request.params : {};
    const plateId = typeof params.plateId === "string" ? params.plateId.trim() : "";
    const token = parseToken(params.token);
    const body = parseTransferRequest(request.body);
    if (
      id == null ||
      !/^plate_[0-9a-f]{32}$/.test(plateId) ||
      !token ||
      !body ||
      body.expected.profileId !== id
    ) {
      return sendError(reply, 400, "invalid_request", "Request is invalid");
    }
    const target =
      "targetPlateId" in body
        ? { targetPlateId: body.targetPlateId }
        : (() => {
            const targetPrinter = fleetPrinterGeometry(dependencies.repo, body.targetPrinterId);
            return targetPrinter ? { targetPrinter } : null;
          })();
    if (!target) return sendError(reply, 422, "printer_not_found", "Printer was not found");
    try {
      const result = dependencies.repo.transferAcceptedPlateUnit({
        profileId: id,
        expected: body.expected,
        expectedPlateRevisionId: body.expectedPlateRevisionId,
        plateId,
        token,
        ...target,
      });
      if (result.kind === "moved" || result.kind === "unchanged") {
        return {
          plate_revision_id: result.plateRevisionId,
          plate_revision_number: result.plateRevisionNumber,
        };
      }
      return sendMoveFailure(reply, result);
    } catch (error) {
      return sendIntegrityFailure(request, reply, error);
    }
  });

  app.post("/plans/:id/plates/arrange", async (request, reply) => {
    const id = profileId(request);
    const body = parseArrangeRequest(request.body);
    if (id == null || !body || body.expected.profileId !== id) {
      return sendError(reply, 400, "invalid_request", "Request is invalid");
    }
    try {
      const result = arrangeAcceptedPlates(service, { profileId: id, ...body });
      return result.kind === "workspace" ? result.workspace : sendInitializeFailure(reply, result);
    } catch (error) {
      return sendIntegrityFailure(request, reply, error);
    }
  });

  app.post("/plans/:id/plates/restore", async (request, reply) => {
    const id = profileId(request);
    const body = parseRestoreRequest(request.body);
    if (id == null || !body || body.expected.profileId !== id) {
      return sendError(reply, 400, "invalid_request", "Request is invalid");
    }
    try {
      const result = dependencies.repo.restoreAcceptedPlates({ profileId: id, ...body });
      if (result.kind !== "restored" && result.kind !== "unchanged") {
        return sendMoveFailure(reply, result);
      }
      const workspace = readAcceptedPlateWorkspace(service, id);
      return workspace.kind === "workspace"
        ? workspace.workspace
        : sendReadFailure(reply, workspace);
    } catch (error) {
      return sendIntegrityFailure(request, reply, error);
    }
  });
}
