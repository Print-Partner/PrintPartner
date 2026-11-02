import type { FastifyRequest } from "fastify";
import type { AcceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { MAX_ACCEPTED_PLATE_UM } from "../db/accepted-plates.js";
import { parseRequiredUnitToken, type RequiredUnitToken } from "../services/required-units.js";
import { isRecord, positiveSafeInteger } from "../lib/guards.js";

type AcceptedPlateInitializeRequest = Readonly<{
  expected: AcceptedPlanBasis;
  expectedPlateRevisionId: number | null;
  assignments: readonly Readonly<{ token: RequiredUnitToken; printerId: string | null }>[];
}>;

type AcceptedPlateRevisionRequest = Readonly<{
  expected: AcceptedPlanBasis;
  expectedPlateRevisionId: number;
}>;

type AcceptedPlateMoveRequest = AcceptedPlateRevisionRequest &
  Readonly<{ xUm: number; yUm: number }>;

type AcceptedPlatePinRequest = AcceptedPlateRevisionRequest & Readonly<{ pinned: boolean }>;

type AcceptedPlateTransferRequest = AcceptedPlateRevisionRequest &
  (Readonly<{ targetPlateId: string }> | Readonly<{ targetPrinterId: string }>);

type AcceptedPlateArrangeRequest = AcceptedPlateRevisionRequest &
  Readonly<{ mode: "unplaced" | "all" }>;

type AcceptedPlateRestoreRequest = AcceptedPlateRevisionRequest &
  Readonly<{ restorePlateRevisionId: number }>;

export function plateCoordinate(value: unknown): number | null {
  return Number.isSafeInteger(value) && Number(value) >= 0 && Number(value) <= MAX_ACCEPTED_PLATE_UM
    ? Number(value)
    : null;
}

export function profileId(request: FastifyRequest): number | null {
  if (!isRecord(request.params)) return null;
  const value = request.params.id;
  if (typeof value !== "string" || !/^\d+$/.test(value)) return null;
  return positiveSafeInteger(Number(value));
}

export function parseBasis(value: unknown): AcceptedPlanBasis | null {
  if (!isRecord(value)) return null;
  const profileId = positiveSafeInteger(value.profile_id);
  const planVersion = positiveSafeInteger(value.plan_version);
  const revisionId = positiveSafeInteger(value.plan_revision_id);
  const revisionDigest = value.plan_revision_digest;
  const mappingDigest = value.required_unit_mapping_digest;
  if (
    profileId == null ||
    planVersion == null ||
    revisionId == null ||
    typeof revisionDigest !== "string" ||
    !/^[a-f0-9]{64}$/.test(revisionDigest) ||
    typeof mappingDigest !== "string" ||
    !/^[a-f0-9]{64}$/.test(mappingDigest)
  ) {
    return null;
  }
  return {
    profileId,
    planVersion,
    revisionId,
    revisionDigest,
    requiredUnitMappingDigest: mappingDigest,
  };
}

export function parseToken(value: unknown): RequiredUnitToken | null {
  if (typeof value !== "string") return null;
  try {
    return parseRequiredUnitToken(value);
  } catch {
    return null;
  }
}

export function parseInitializeRequest(value: unknown): AcceptedPlateInitializeRequest | null {
  if (!isRecord(value) || !Array.isArray(value.assignments)) return null;
  const expected = parseBasis(value.expected);
  const expectedPlateRevisionId =
    value.expected_plate_revision_id === null
      ? null
      : positiveSafeInteger(value.expected_plate_revision_id);
  if (
    !expected ||
    (expectedPlateRevisionId === null && value.expected_plate_revision_id !== null)
  ) {
    return null;
  }

  const assignments: Array<{ token: RequiredUnitToken; printerId: string | null }> = [];
  for (const raw of value.assignments) {
    if (!isRecord(raw)) return null;
    const token = parseToken(raw.token);
    const printerId =
      raw.printer_id === null
        ? null
        : typeof raw.printer_id === "string" &&
            raw.printer_id.trim().length > 0 &&
            raw.printer_id.trim().length <= 200
          ? raw.printer_id.trim()
          : undefined;
    if (!token || printerId === undefined) return null;
    assignments.push({ token, printerId });
  }
  return { expected, expectedPlateRevisionId, assignments };
}

export function parseMoveRequest(value: unknown): AcceptedPlateMoveRequest | null {
  if (!isRecord(value)) return null;
  const revision = parseRevisionRequest(value);
  const xUm = plateCoordinate(value.x_um);
  const yUm = plateCoordinate(value.y_um);
  if (!revision || xUm == null || yUm == null) return null;
  return { ...revision, xUm, yUm };
}

export function parseRevisionRequest(value: unknown): AcceptedPlateRevisionRequest | null {
  if (!isRecord(value)) return null;
  const expected = parseBasis(value.expected);
  const expectedPlateRevisionId = positiveSafeInteger(value.expected_plate_revision_id);
  return expected && expectedPlateRevisionId != null ? { expected, expectedPlateRevisionId } : null;
}

export function parsePinRequest(value: unknown): AcceptedPlatePinRequest | null {
  const revision = parseRevisionRequest(value);
  return revision && isRecord(value) && typeof value.pinned === "boolean"
    ? { ...revision, pinned: value.pinned }
    : null;
}

export function parseTransferRequest(value: unknown): AcceptedPlateTransferRequest | null {
  const revision = parseRevisionRequest(value);
  if (!revision || !isRecord(value)) return null;
  const targetPlateId =
    typeof value.target_plate_id === "string" ? value.target_plate_id.trim() : null;
  const targetPrinterId =
    typeof value.target_printer_id === "string" ? value.target_printer_id.trim() : null;
  if ((targetPlateId === null) === (targetPrinterId === null)) return null;
  if (targetPlateId !== null) {
    return /^plate_[0-9a-f]{32}$/.test(targetPlateId) ? { ...revision, targetPlateId } : null;
  }
  return targetPrinterId && targetPrinterId.length <= 200 ? { ...revision, targetPrinterId } : null;
}

export function parseArrangeRequest(value: unknown): AcceptedPlateArrangeRequest | null {
  const revision = parseRevisionRequest(value);
  if (!revision || !isRecord(value) || (value.mode !== "unplaced" && value.mode !== "all")) {
    return null;
  }
  return { ...revision, mode: value.mode };
}

export function parseRestoreRequest(value: unknown): AcceptedPlateRestoreRequest | null {
  const revision = parseRevisionRequest(value);
  if (!revision || !isRecord(value)) return null;
  const restorePlateRevisionId = positiveSafeInteger(value.restore_plate_revision_id);
  return restorePlateRevisionId == null ? null : { ...revision, restorePlateRevisionId };
}
