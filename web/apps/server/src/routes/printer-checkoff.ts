import { basename } from "node:path";
import { randomUUID } from "node:crypto";
import type { FastifyInstance } from "fastify";
import {
  isPrintReady,
  isManualIntegrationId,
  manualIntegrationId,
  UNMANAGED_PRINTER_ID,
  UNMANAGED_PRINTER_NAME,
  type PrinterCheckoffLink,
  type PrinterCheckoffUnit,
  type PrinterFileIdentity,
  type PrintFileClassification,
  type PrintFileMatchReview,
  type PrinterObjectMapping,
  type IntegrationConfig,
} from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import {
  AcceptedPlanOperationalIntegrityError,
  type AcceptedPlanOperationalSnapshot,
} from "../db/accepted-plan-operational.js";
import {
  getIntegrationConfig,
  type IntegrationPort,
  type PrinterFileAccess,
} from "../integrations/store.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import { sendProblem } from "../lib/api-error.js";
import { readFeatureAccess } from "../lib/feature-access.js";
import {
  cancelResponseBody,
  readBoundedResponseBody,
  ResponseBodyTooLargeError,
} from "../lib/bounded-response.js";
import {
  observePrinterCheckoffFileDrift,
  printerStoredFileIdentity,
  reconcilePrinterCheckoff,
} from "../services/printer-checkoff.js";
import {
  dismissHostFailedLink,
  verifyPrinterCheckoff,
} from "../services/printer-checkoff-verify.js";
import { dispatchWebhooks } from "../services/webhook-store.js";
import {
  getPrinterCheckoffLink,
  listAwaitingVerifyPrinterCheckoffLinks,
  listWatchingPrinterCheckoffLinks,
  loadPrinterCheckoffLinks,
  updatePrinterCheckoffLink,
  type PrinterCheckoffLinkPatch,
} from "../services/printer-checkoff-store.js";
import { summarizePrintOutcomes } from "../services/printer-outcomes-store.js";
import { groupObjectsByPart, matchObjectsToFilenames } from "../services/gcode-object-parser.js";
import {
  claimUnattributedPrint,
  createUnattributedPrint,
  discardDismissedUnattributedPrints,
  dismissUnattributedPrint,
  listOpenUnattributedPrints,
  listUnattributedPrints,
  saveUnattributedPrint,
} from "../services/unattributed-print-store.js";
import { normalizePrinterFilename } from "../services/printer-checkoff.js";
import { filterLinkedUnattributedPrints } from "./printer-checkoff-route-model.js";
import { loadFleet } from "../services/printer-fleet.js";
import { deductSpoolmanFilamentAfterVerify } from "../services/spoolman-deduct.js";
import {
  confirmAcceptedPrinterUnits,
  parsePrinterCheckoffUnitToken,
  resolveAcceptedPrinterAttribution,
  type MaterializeAcceptedPrinterLinkResult,
} from "../db/accepted-printer-attribution.js";
import {
  classifyPrintFileBytes,
  printFileNextAction,
  printFileRejectionMessage,
  MAX_CLASSIFIABLE_BYTES,
} from "../lib/print-file-classification.js";
import { MAX_PRINT_FILE_UPLOAD_BYTES as MAX_UPLOAD_BYTES } from "../services/upload-limits.js";
import { HOSTED_TENANT_DISK_QUOTA_BYTES } from "@print-partner/contracts";
import { sendIfTenantDiskQuotaExceeded } from "../lib/tenant-disk-quota.js";
import {
  readPrinterFileSnapshot,
  consumePrinterFileSnapshot,
} from "../services/printer-file-snapshots.js";

type RouteDeps = {
  repo: AppRepository;
  integrations: IntegrationPort;
  dataDir?: string;
  reposDir?: string;
  hostedPlanning?: boolean;
};

async function getObjectListForIntegration(
  repo: AppRepository,
  integrationId: string,
): Promise<string[]> {
  try {
    const integration = getIntegrationConfig(repo, integrationId);
    if (!integration) return [];
    const adapter = getIntegrationAdapter(integration.type);
    if (!adapter?.getObjectList) return [];
    return await adapter.getObjectList(integration.config);
  } catch {
    return [];
  }
}

async function buildCandidatesFromObjectNames(
  repo: AppRepository,
  objectNames: string[],
): Promise<Array<{ stl_basename: string; copy_count: number; matching_filenames: string[] }>> {
  if (!objectNames.length) return [];

  const profiles = repo.listProfileHeaders();
  const allFilenames = new Set<string>();
  for (const profile of profiles) {
    const { parts } = repo.listParts(profile.id, 10000, 0);
    for (const part of parts) {
      if (part.filename) allFilenames.add(part.filename);
    }
  }

  const grouped = groupObjectsByPart(objectNames);
  const matched = matchObjectsToFilenames(grouped, [...allFilenames]);

  const candidates: Array<{
    stl_basename: string;
    copy_count: number;
    matching_filenames: string[];
  }> = [];
  for (const [stlBasename, plateMatch] of grouped) {
    candidates.push({
      stl_basename: stlBasename,
      copy_count: plateMatch.count,
      matching_filenames: matched.get(stlBasename) ?? [],
    });
  }
  return candidates;
}

function repairEmptyAwaitingLinks(
  repo: AppRepository,
  links: PrinterCheckoffLink[],
  beforeRepair: (link: PrinterCheckoffLink) => void,
): PrinterCheckoffLink[] {
  return links.map((link) => {
    if (link.state !== "awaiting_verify" || link.units.length > 0 || link.imported_inventory)
      return link;
    beforeRepair(link);
    const repaired = repo.materializeAcceptedPrinterLink({ kind: "repair", expectedLink: link });
    return repaired.kind === "repaired" ? repaired.link : link;
  });
}

function claimMatchingUnattributedPrints(repo: AppRepository, link: PrinterCheckoffLink): void {
  for (const print of listOpenUnattributedPrints(repo)) {
    const normalizedFilename = normalizePrinterFilename(print.filename);
    if (
      print.integration_id === link.integration_id &&
      normalizePrinterFilename(link.filename) === normalizedFilename
    ) {
      claimUnattributedPrint(repo, print.id, link.profile_id);
    }
  }
}

/**
 * The link state a host-confirmed finish produces: printing is done and an
 * operator still has to verify it. Spread a fresh `completed_at` over it.
 * Exported for fixtures/parity/printer-file-assignment/generate.mjs.
 *
 * @public
 */
export const HOST_COMPLETED_PATCH = {
  state: "awaiting_verify",
  host_outcome: "success",
  saw_active: true,
  last_progress: 100,
} as const satisfies PrinterCheckoffLinkPatch;

type MaterializeProblem = { status: number; title: string; detail: string };

/**
 * One wording for every way materializing a printer link can fail, so the
 * assignment route and the unattributed-claim route cannot drift apart. Returns
 * null for the outcomes that produced a link.
 */
function materializeProblem(
  materialized: MaterializeAcceptedPrinterLinkResult,
): MaterializeProblem | null {
  switch (materialized.kind) {
    case "created":
    case "repaired":
    case "claimed":
      return null;
    case "transaction_unavailable":
      return {
        status: 503,
        title: "Service Unavailable",
        detail: "Accepted Plan update is unavailable",
      };
    case "empty":
      return {
        status: 409,
        title: "Conflict",
        detail: "Accepted Plan has no Required units",
      };
    case "accepted_state_unavailable":
      return {
        status: 409,
        title: "Conflict",
        detail:
          materialized.reason === "compatibility_dirty"
            ? "Accepted Plan requires compatibility repair"
            : "Accepted Plan operational state is not initialized",
      };
    case "stale_plan_revision":
      return {
        status: 409,
        title: "Conflict",
        detail: "The Accepted Plan moved on; reload the Build and choose the file again",
      };
    case "no_match":
      return {
        status: 409,
        title: "Conflict",
        detail: "That print file does not map to an incomplete Required unit in this Build",
      };
    case "already_linked":
      return {
        status: 409,
        title: "Conflict",
        detail: "That print file is already assigned",
      };
    case "print_changed":
      return {
        status: 409,
        title: "Conflict",
        detail: "Print changed or was already claimed",
      };
    case "link_not_found":
    case "link_changed":
    case "not_repairable":
      return {
        status: 409,
        title: "Conflict",
        detail: "Tracked print changed; reload and retry",
      };
    default: {
      const _exhaustive: never = materialized;
      return _exhaustive;
    }
  }
}

/**
 * Bound the bytes buffered for classification. The adapter owns download timeouts.
 */
async function readBoundedBody(response: Response): Promise<Uint8Array | null> {
  try {
    return await readBoundedResponseBody(response, MAX_CLASSIFIABLE_BYTES);
  } catch (error) {
    if (error instanceof ResponseBodyTooLargeError) return null;
    throw error;
  }
}

/** A host that can actually serve print files, with the config to reach it. */
type PrinterFileGateway = { files: PrinterFileAccess; config: IntegrationConfig };

function printerFileGateway(repo: AppRepository, integrationId: string): PrinterFileGateway | null {
  const integration = getIntegrationConfig(repo, integrationId);
  if (!integration || integration.config.enabled === false) return null;
  const files = getIntegrationAdapter(integration.type)?.files;
  return files ? { files, config: integration.config } : null;
}

/** What PrintPartner learned about a print file, whatever it came from. */
type PrintFileInspection =
  | {
      outcome: "inspected";
      classification: PrintFileClassification;
      identity: PrinterFileIdentity;
    }
  | { outcome: "rejected"; detail: string }
  /**
   * No host file access, no path, no upload, or the host would not serve the
   * bytes. Nothing is known about the file, and nothing may be assumed.
   */
  | { outcome: "unreadable" };

/**
 * Pull the provider's own metadata plus the real bytes for one storage path,
 * then classify those bytes. This is the only thing allowed to decide what a
 * print file is; the client's opinion never enters.
 */
async function inspectRemoteFile(
  repo: AppRepository,
  integrationId: string,
  remotePath: string,
): Promise<PrintFileInspection> {
  const gateway = printerFileGateway(repo, integrationId);
  if (!gateway) return { outcome: "unreadable" };

  const lastSlash = remotePath.lastIndexOf("/");
  let providerIdentity: PrinterFileIdentity = {};
  try {
    const listing = await gateway.files.browse(
      gateway.config,
      lastSlash < 0 ? "" : remotePath.slice(0, lastSlash),
    );
    const entry = listing.entries.find(
      (candidate) => candidate.kind === "file" && candidate.path === remotePath,
    );
    if (entry?.kind === "file") providerIdentity = printerStoredFileIdentity(entry);
  } catch {
    // A host that will not list its storage may still serve the bytes, and the
    // content hash alone is a usable identity.
  }

  let bytes: Uint8Array | null;
  try {
    const response = await gateway.files.open(gateway.config, remotePath);
    if (!response.ok) {
      await cancelResponseBody(response);
      return { outcome: "unreadable" };
    }
    bytes = await readBoundedBody(response);
  } catch {
    return { outcome: "unreadable" };
  }
  if (!bytes) return { outcome: "rejected", detail: "That print file is too large to inspect" };

  const classified = classifyPrintFileBytes(bytes);
  if (classified.outcome === "rejected") {
    return { outcome: "rejected", detail: printFileRejectionMessage(classified.reason) };
  }
  return {
    outcome: "inspected",
    classification: classified.classification,
    identity: {
      ...providerIdentity,
      size_bytes: classified.size_bytes,
      sha256: classified.sha256,
    },
  };
}

/**
 * Print files an operator uploaded from their own computer, held between the
 * upload request that read them and the assignment request that records them.
 * The assignment is a JSON request carrying no file, so PrintPartner keeps the
 * accepted bytes and reclassifies them when it lands. An uploaded file and one
 * read off a printer host then reach their verdict through the same code.
 *
 * They are held in this process's memory, so both size bounds are memory
 * bounds. One upload is capped at 64 MB, about the largest sliced artifact a
 * single plate produces, and everything pending at once at two of those, so a
 * second operator recording a print at the same moment still gets through. A
 * token is good for 15 minutes, the window an operator needs to read the
 * suggestion, tick the Required units, and confirm; past that the bytes are
 * memory nobody is coming back for. Expired entries are swept whenever a new
 * upload arrives and a spent one is dropped on the spot, so none of this needs
 * a background sweeper.
 */
const MAX_PENDING_UPLOAD_BYTES = 2 * MAX_UPLOAD_BYTES;
const UPLOAD_TTL_MS = 15 * 60_000;

type PendingUpload = {
  /** The Build the token was issued for. A token is not portable between them. */
  profileId: number;
  bytes: Uint8Array;
  expiresAt: number;
};

const pendingUploads = new Map<string, PendingUpload>();

/** Drop expired uploads and report what the survivors still hold. */
function sweepPendingUploads(now: number): number {
  let held = 0;
  for (const [token, upload] of pendingUploads) {
    if (upload.expiresAt <= now) pendingUploads.delete(token);
    else held += upload.bytes.byteLength;
  }
  return held;
}

/**
 * Take an upload without spending it, because an assignment can fail for
 * reasons that have nothing to do with the file and the operator should not
 * have to send it again.
 */
function readPendingUpload(token: string, profileId: number): PendingUpload | null {
  const upload = pendingUploads.get(token);
  if (!upload) return null;
  if (upload.expiresAt <= Date.now()) {
    pendingUploads.delete(token);
    return null;
  }
  return upload.profileId === profileId ? upload : null;
}

/**
 * Classify an uploaded file. An unknown, expired, or foreign token is refused
 * outright rather than read as "no file to inspect": PrintPartner had these
 * bytes once, and quietly recording the print as unclassified would throw that
 * away.
 */
function inspectUploadedFile(token: string, profileId: number): PrintFileInspection {
  const upload = readPendingUpload(token, profileId);
  if (!upload) {
    return {
      outcome: "rejected",
      detail: "That upload is no longer available; upload the print file again",
    };
  }
  const classified = classifyPrintFileBytes(upload.bytes);
  if (classified.outcome === "rejected") {
    return { outcome: "rejected", detail: printFileRejectionMessage(classified.reason) };
  }
  return {
    outcome: "inspected",
    classification: classified.classification,
    identity: { size_bytes: classified.size_bytes, sha256: classified.sha256 },
  };
}

type PrintFileRequest =
  | {
      outcome: "parsed";
      profileId: number;
      printerId: string;
      printerName: string;
      filename: string;
      /** Where the bytes came from. A file has one source or none, never two. */
      remotePath?: string;
      uploadToken?: string;
      snapshotToken?: string;
      objectNames: string[];
      integrationId: string;
    }
  | { outcome: "invalid"; status: number; title: string; detail: string };

/** The printer a print is recorded against, or the wording for why there is not one. */
type AssignmentPrinter =
  | { outcome: "resolved"; printerId: string; printerName: string; integrationId: string }
  | { outcome: "invalid"; status: number; title: string; detail: string };

/**
 * Decide which printer a print is recorded against, and what watches it.
 *
 * A request either names a printer in the fleet or names none. Naming none
 * means a printer PrintPartner does not manage: recording that a print
 * happened must not require registering hardware PrintPartner cannot reach,
 * which is the whole reason the file was uploaded rather than picked off a
 * host. A named printer is still resolved in the fleet, so a typo is a 404
 * rather than a print quietly recorded against nothing.
 */
function resolveAssignmentPrinter(
  repo: AppRepository,
  request: { printerId: string; tracking: unknown; fromPrinterStorage: boolean },
): AssignmentPrinter {
  if (!request.printerId) {
    // Nothing polls a printer PrintPartner does not manage, so there is no host
    // to watch and no host to read a stored file off either. Both are refused
    // rather than quietly downgraded, because an operator who asked for host
    // tracking is expecting PrintPartner to follow the print.
    if (request.tracking === "host") {
      return {
        outcome: "invalid",
        status: 400,
        title: "Bad Request",
        detail:
          "PrintPartner cannot watch a printer it does not manage. Name the printer that made this print, or record the print as already made.",
      };
    }
    if (request.fromPrinterStorage) {
      return {
        outcome: "invalid",
        status: 400,
        title: "Bad Request",
        detail:
          "A file held on a printer needs the printer it sits on. Name that printer, or upload the file instead.",
      };
    }
    return {
      outcome: "resolved",
      printerId: UNMANAGED_PRINTER_ID,
      printerName: UNMANAGED_PRINTER_NAME,
      integrationId: manualIntegrationId(UNMANAGED_PRINTER_ID),
    };
  }
  const printer = loadFleet(repo).find((row) => row.id === request.printerId);
  if (!printer) {
    return { outcome: "invalid", status: 404, title: "Not Found", detail: "Printer not found" };
  }
  if (request.tracking === "manual") {
    return {
      outcome: "resolved",
      printerId: printer.id,
      printerName: printer.name,
      integrationId: manualIntegrationId(printer.id),
    };
  }
  const integrationId = printer.integration_id?.trim() ?? "";
  const integration = integrationId ? getIntegrationConfig(repo, integrationId) : null;
  if (!integration || integration.config.enabled === false) {
    return {
      outcome: "invalid",
      status: 409,
      title: "Conflict",
      detail: "Use manual tracking because this printer has no available host",
    };
  }
  return { outcome: "resolved", printerId: printer.id, printerName: printer.name, integrationId };
}

/**
 * The part of a print-file request that preview and assignment share, so a
 * preview cannot answer for one file while the assignment binds another.
 */
function parsePrintFileRequest(repo: AppRepository, raw: unknown): PrintFileRequest {
  const body = (raw ?? {}) as {
    profile_id?: unknown;
    printer_id?: unknown;
    filename?: unknown;
    remote_path?: unknown;
    upload_token?: unknown;
    snapshot_token?: unknown;
    object_names?: unknown;
    tracking?: unknown;
  };
  const invalid = (detail: string): PrintFileRequest => ({
    outcome: "invalid",
    status: 400,
    title: "Bad Request",
    detail,
  });
  const profileId = Number(body.profile_id);
  if (!Number.isInteger(profileId) || profileId <= 0) return invalid("profile_id is required");
  const filename = typeof body.filename === "string" ? body.filename.trim() : "";
  if (!filename || filename.length > 500) return invalid("filename is required");
  if (!/\.(?:gcode|gco|bgcode|3mf)$/i.test(filename)) {
    return invalid("Choose a .gcode, .gco, .bgcode, or .3mf print file");
  }
  const remotePath =
    typeof body.remote_path === "string" && body.remote_path.trim()
      ? body.remote_path.trim().slice(0, 1_000)
      : undefined;
  const uploadToken =
    typeof body.upload_token === "string" && body.upload_token.trim()
      ? body.upload_token.trim().slice(0, 200)
      : undefined;
  if (remotePath && uploadToken) {
    return invalid("Send remote_path or upload_token, not both");
  }
  const snapshotToken =
    typeof body.snapshot_token === "string" ? body.snapshot_token.trim() : undefined;
  if (body.snapshot_token !== undefined && (!snapshotToken || !remotePath || uploadToken)) {
    return invalid("A snapshot token requires its original printer file path");
  }
  const printer = resolveAssignmentPrinter(repo, {
    printerId: typeof body.printer_id === "string" ? body.printer_id.trim() : "",
    tracking: body.tracking,
    fromPrinterStorage: remotePath !== undefined,
  });
  if (printer.outcome === "invalid") return printer;
  if (
    Array.isArray(body.object_names) &&
    (body.object_names.length > 500 ||
      body.object_names.some((name) => typeof name !== "string" || name.trim().length > 200))
  ) {
    return invalid(
      "Import supports at most 500 object names of up to 200 characters; no objects were imported",
    );
  }
  return {
    outcome: "parsed",
    profileId,
    printerId: printer.printerId,
    printerName: printer.printerName,
    filename,
    remotePath,
    uploadToken,
    snapshotToken,
    objectNames: Array.isArray(body.object_names)
      ? body.object_names
          .filter((value): value is string => typeof value === "string")
          .map((value) => value.trim().slice(0, 200))
          .filter(Boolean)
          .slice(0, 500)
      : [],
    integrationId: printer.integrationId,
  };
}

type ParsedPrintFileRequest = Extract<PrintFileRequest, { outcome: "parsed" }>;

/**
 * Read whatever source the request names. A print file either sits on a printer
 * PrintPartner can reach or arrived from the operator's own computer; with
 * neither, nothing about it is known and nothing may be inferred.
 */
async function inspectPrintFile(
  repo: AppRepository,
  parsed: ParsedPrintFileRequest,
): Promise<PrintFileInspection> {
  if (parsed.snapshotToken && parsed.remotePath) {
    return (
      readPrinterFileSnapshot(repo, parsed.snapshotToken, {
        profileId: parsed.profileId,
        printerId: parsed.printerId,
        remotePath: parsed.remotePath,
        filename: parsed.filename,
      }) ?? {
        outcome: "rejected",
        detail: "This file preview expired or its source changed. Cancel and reopen the file.",
      }
    );
  }
  if (parsed.remotePath) return inspectRemoteFile(repo, parsed.integrationId, parsed.remotePath);
  if (parsed.uploadToken) return inspectUploadedFile(parsed.uploadToken, parsed.profileId);
  return { outcome: "unreadable" };
}

/**
 * What PrintPartner thinks a print file produced, before an operator says so.
 * Suggest only: the filename fallback runs here so the operator can see what a
 * filename match would imply, and confirm or reject it.
 */
function suggestPrintFileAttribution(
  snapshot: AcceptedPlanOperationalSnapshot,
  observation: { objectNames: string[]; fallbackFilename: string },
): {
  suggested_units: PrinterCheckoffUnit[];
  suggestion_basis: "none" | "filename" | "object_names";
  unlabeled_names: string[];
  plan_revision_id: number;
  match_review: PrintFileMatchReview;
} {
  const suggestion = resolveAcceptedPrinterAttribution(snapshot, {
    ...observation,
    positiveOnly: true,
  });
  const named = confirmAcceptedPrinterUnits({ snapshot, confirmed: suggestion.units });
  return {
    suggested_units:
      named.kind === "confirmed"
        ? named.units.map((unit) => ({ ...unit }))
        : suggestion.units.map((unit) => ({ ...unit })),
    suggestion_basis:
      suggestion.units.length === 0
        ? "none"
        : suggestion.fallback === "used"
          ? "filename"
          : "object_names",
    unlabeled_names: [...suggestion.unmatchedObjectNames],
    plan_revision_id: snapshot.revisionId,
    match_review: {
      notices: suggestion.outcomes.flatMap((outcome) =>
        outcome.kind === "already_completed"
          ? [`${outcome.rawName} is already checked off.`]
          : outcome.kind === "duplicate_observation"
            ? [`${outcome.rawName} repeats the same identified unit; it is not another copy.`]
            : [],
      ),
      objects: suggestion.outcomes.flatMap((outcome) =>
        outcome.kind === "unmatched" || outcome.kind === "ambiguous_filename"
          ? [{ object_index: outcome.inputIndex, name: outcome.rawName }]
          : [],
      ),
      parts: snapshot.parts
        .filter((part) => part.included && part.units.some((unit) => unit.required))
        .map((part) => ({
          part_id: part.projectionPartId,
          filename: part.filename,
          relative_path: part.relativePath || part.filename,
          source_label: part.sourceLayer,
          units: part.units
            .filter((unit) => unit.required && !unit.completed)
            .map((unit) => ({
              part_id: part.projectionPartId,
              unit_index: unit.unitIndex,
              object_name: unit.objectName,
            })),
        })),
    },
  };
}

/**
 * The Accepted Plan a print file is attributed against, or the wording for why
 * there is not one to attribute against.
 */
function readAttributionSnapshot(
  repo: AppRepository,
  profileId: number,
):
  | { outcome: "ready"; snapshot: AcceptedPlanOperationalSnapshot }
  | { outcome: "unavailable"; problem: MaterializeProblem } {
  const accepted = repo.readAcceptedPlanOperationalSnapshot(profileId);
  if (accepted.kind === "ready") return { outcome: "ready", snapshot: accepted.snapshot };
  const problem = materializeProblem(
    accepted.kind === "empty"
      ? { kind: "empty" }
      : { kind: "accepted_state_unavailable", reason: accepted.kind },
  );
  if (!problem) throw new Error("Accepted Plan attribution lost its problem");
  return { outcome: "unavailable", problem };
}

/**
 * Object labels the browser read out of the file it is uploading, sent as a
 * JSON array because multipart has no shape of its own. Anything unreadable
 * falls back to the filename basis instead of failing the upload: these labels
 * only sharpen a suggestion the operator confirms by hand.
 */
function parseUploadedObjectNames(raw: string): string[] {
  if (!raw.trim()) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  return parsed
    .filter((value): value is string => typeof value === "string")
    .map((value) => value.trim().slice(0, 200))
    .filter(Boolean)
    .slice(0, 500);
}

export async function registerPrinterCheckoffRoutes(
  app: FastifyInstance,
  deps: RouteDeps,
): Promise<void> {
  app.get("/printer-checkoff", async (request, reply) => {
    let repairContext: { linkId: string; profileId: number } | undefined;
    try {
      const query = request.query as {
        state?: string;
        integration_id?: string;
        profile_id?: string;
      };
      const integrationId = query.integration_id?.trim();
      const profileIdRaw = query.profile_id?.trim();
      const profileId =
        profileIdRaw && Number.isInteger(Number(profileIdRaw)) ? Number(profileIdRaw) : undefined;

      if (query.state === "watching") {
        const links = listWatchingPrinterCheckoffLinks(deps.repo, integrationId);
        return {
          links: profileId == null ? links : links.filter((link) => link.profile_id === profileId),
        };
      }
      let links =
        query.state === "awaiting_verify"
          ? listAwaitingVerifyPrinterCheckoffLinks(deps.repo, profileId)
          : loadPrinterCheckoffLinks(deps.repo);
      if (integrationId) {
        links = links.filter((link) => link.integration_id === integrationId);
      }
      if (profileId != null) {
        links = links.filter((link) => link.profile_id === profileId);
      }
      if (
        query.state === "host_failed" ||
        query.state === "dismissed" ||
        query.state === "verified" ||
        query.state === "applied"
      ) {
        const want = query.state === "applied" ? "verified" : query.state;
        links = links.filter((link) => link.state === want);
      }
      links = repairEmptyAwaitingLinks(deps.repo, links, (link) => {
        repairContext = { linkId: link.id, profileId: link.profile_id };
      });
      return { links };
    } catch (error) {
      if (error instanceof AcceptedPlanOperationalIntegrityError) {
        request.log.error(
          { failure: "integrity", code: error.code, ...repairContext },
          "Accepted printer link repair failed",
        );
      } else {
        request.log.error(
          { failure: "unexpected", ...repairContext },
          "Accepted printer link repair failed",
        );
      }
      return sendProblem(reply, 500, "Internal Server Error", "Internal Server Error");
    }
  });

  app.post(
    "/printer-checkoff/reconcile",
    { config: { rateLimit: { max: 300, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const body = request.body as { integration_id?: string };
      const integrationId = String(body.integration_id ?? "").trim();
      if (!integrationId) {
        return sendProblem(reply, 400, "Bad Request", "integration_id is required");
      }
      const integrationSummary = deps.integrations.get(integrationId);
      if (!integrationSummary) {
        return sendProblem(reply, 404, "Not Found", "Integration not found");
      }
      if (integrationSummary.type === "bambu") {
        const access = readFeatureAccess(() => deps.repo, "bambu");
        if (access.kind !== "allowed") {
          const status = access.kind === "disabled" ? 403 : 500;
          return reply.status(status).send({
            status,
            title: access.kind === "disabled" ? "Forbidden" : "Internal Server Error",
            detail: access.error,
            code: access.code,
          });
        }
      }

      // Always fetch live host status — never trust a client-supplied snapshot.
      const status = await deps.integrations.getStatus(integrationId);
      const updates = reconcilePrinterCheckoff(deps.repo, integrationId, status);
      const createdLinks: PrinterCheckoffLink[] = [];

      // Same poll, so a provider file that changed underneath a link is noticed
      // without an operator asking.
      const gateway = printerFileGateway(deps.repo, integrationId);
      if (gateway) {
        await observePrinterCheckoffFileDrift({
          repo: deps.repo,
          integrationId,
          files: gateway.files,
          config: gateway.config,
        });
      }

      // Handle externally-completed prints (no watching link transitioned)
      if (status.state === "complete" && updates.length === 0 && status.filename) {
        const normalizedFilename = normalizePrinterFilename(status.filename);
        // Check if we already stored this as unattributed
        const alreadyTracked = () =>
          listUnattributedPrints(deps.repo).some(
            (p) =>
              p.integration_id === integrationId &&
              normalizePrinterFilename(p.filename) === normalizedFilename,
          ) ||
          loadPrinterCheckoffLinks(deps.repo).some(
            (link) =>
              link.integration_id === integrationId &&
              normalizePrinterFilename(link.filename) === normalizedFilename,
          );
        if (!alreadyTracked()) {
          const objectNames = await getObjectListForIntegration(deps.repo, integrationId);
          const candidates = await buildCandidatesFromObjectNames(deps.repo, objectNames);
          if (!alreadyTracked()) {
            const unattributedPrint = createUnattributedPrint(
              integrationId,
              "default",
              integrationSummary.name || "Printer",
              status.filename,
              objectNames,
              candidates,
            );
            saveUnattributedPrint(deps.repo, unattributedPrint);
          }
        }
      }

      const openUnattributed = filterLinkedUnattributedPrints(
        listOpenUnattributedPrints(deps.repo),
        loadPrinterCheckoffLinks(deps.repo),
        integrationId,
      );

      if ((status.state === "printing" || status.state === "paused") && status.filename) {
        const normalizedFilename = normalizePrinterFilename(status.filename);
        const dismissedIds = listUnattributedPrints(deps.repo)
          .filter(
            (print) =>
              print.dismissed &&
              print.integration_id === integrationId &&
              normalizePrinterFilename(print.filename) === normalizedFilename,
          )
          .map((print) => print.id);
        discardDismissedUnattributedPrints(deps.repo, dismissedIds);

        const watching = listWatchingPrinterCheckoffLinks(deps.repo, integrationId);
        const alreadyWatching = watching.some(
          (l) => normalizePrinterFilename(l.filename) === normalizedFilename,
        );

        if (!alreadyWatching) {
          let attributionProfileId: number | undefined;
          try {
            const bindingsRaw = deps.repo.getSetting("printer.plan_bindings");
            const bindings: Array<{ integration_id: string; profile_id: number | null }> =
              bindingsRaw ? JSON.parse(bindingsRaw) : [];
            const binding = bindings.find((b) => b.integration_id === integrationId);
            if (binding?.profile_id) {
              attributionProfileId = binding.profile_id;
              const objectNames = await getObjectListForIntegration(deps.repo, integrationId);
              const fleet = loadFleet(deps.repo);
              const machine = fleet.find((m) => m.integration_id === integrationId);
              const created = deps.repo.materializeAcceptedPrinterLink({
                kind: "observe",
                profileId: binding.profile_id,
                objectNames,
                link: {
                  integrationId,
                  printerId: machine?.id ?? integrationId,
                  hostName: integrationSummary.name,
                  filename: normalizePrinterFilename(status.filename) || status.filename,
                  started: false,
                },
              });
              if (created.kind === "created") createdLinks.push(created.link);
            }
          } catch (error) {
            if (error instanceof AcceptedPlanOperationalIntegrityError) {
              request.log.error(
                {
                  failure: "integrity",
                  code: error.code,
                  ...(attributionProfileId == null ? {} : { profileId: attributionProfileId }),
                  integrationId,
                },
                "Accepted printer auto-attribution failed",
              );
            } else {
              request.log.error(
                {
                  failure: "unexpected",
                  ...(attributionProfileId == null ? {} : { profileId: attributionProfileId }),
                  integrationId,
                },
                "Accepted printer auto-attribution failed",
              );
            }
          }
        }
      }

      // Keep `applied` alias empty for older clients; prefer `updates`.
      return {
        status,
        updates,
        created_links: createdLinks,
        applied: [],
        unattributed: openUnattributed,
      };
    },
  );

  app.post(
    "/printer-checkoff/verify",
    { config: { rateLimit: { max: 30, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const body = request.body as {
        link_id?: string;
        decisions?: unknown;
        additional_decisions?: unknown;
      };
      const linkId = String(body.link_id ?? "").trim();
      if (!linkId) {
        return sendProblem(reply, 400, "Bad Request", "link_id is required");
      }
      let result;
      try {
        result = verifyPrinterCheckoff(
          deps.repo,
          linkId,
          body.decisions,
          body.additional_decisions,
        );
      } catch (error) {
        if (error instanceof AcceptedPlanOperationalIntegrityError) {
          request.log.error({ code: error.code, linkId }, "Accepted Plan integrity failure");
          return sendProblem(
            reply,
            500,
            "Internal Server Error",
            "Accepted Plan data is inconsistent",
          );
        }
        request.log.error(
          { failure: "unexpected", linkId },
          "Accepted printer verification failed",
        );
        return sendProblem(reply, 500, "Internal Server Error", "Internal Server Error");
      }
      if ("error" in result) {
        return sendProblem(
          reply,
          result.status,
          result.status === 404
            ? "Not Found"
            : result.status === 409
              ? "Conflict"
              : result.status === 503
                ? "Service Unavailable"
                : "Bad Request",
          result.error,
        );
      }

      // Dispatch print.verified / print.rejected webhooks for confirmed and rejected units.
      const { link, units_confirmed, units_rejected } = result;
      claimMatchingUnattributedPrints(deps.repo, link);
      const webhookBase = {
        link_id: link.id,
        profile_id: link.profile_id,
        filename: link.filename,
        integration_id: link.integration_id,
      };
      if (units_confirmed > 0) {
        void dispatchWebhooks(deps.repo, "print.verified", {
          ...webhookBase,
          units_confirmed,
        });
      }
      if (units_rejected > 0) {
        void dispatchWebhooks(deps.repo, "print.rejected", {
          ...webhookBase,
          units_rejected,
        });
      }

      // Best-effort: deduct consumed filament from Spoolman when units are confirmed.
      if (units_confirmed > 0) {
        const rawDecisions = (request.body as { decisions?: unknown }).decisions;
        const confirmedDecisions = Array.isArray(rawDecisions)
          ? rawDecisions.filter(
              (d): d is { part_id: number; unit_index: number; result: "confirmed" } =>
                !!d &&
                typeof d === "object" &&
                (d as { result?: unknown }).result === "confirmed" &&
                typeof (d as { part_id?: unknown }).part_id === "number" &&
                typeof (d as { unit_index?: unknown }).unit_index === "number",
            )
          : [];
        void deductSpoolmanFilamentAfterVerify(
          deps.repo,
          link.integration_id,
          link.profile_id,
          confirmedDecisions,
          link.units.length,
        );
      }

      return result;
    },
  );

  app.post(
    "/printer-checkoff/dismiss",
    { config: { rateLimit: { max: 30, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const body = request.body as { link_id?: string };
      const linkId = String(body.link_id ?? "").trim();
      if (!linkId) {
        return sendProblem(reply, 400, "Bad Request", "link_id is required");
      }
      const result = dismissHostFailedLink(deps.repo, linkId);
      if ("error" in result) {
        return sendProblem(
          reply,
          result.status,
          result.status === 404 ? "Not Found" : "Conflict",
          result.error,
        );
      }
      return { link: result };
    },
  );

  app.post(
    "/printer-checkoff/file-assignments/preview",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const parsed = parsePrintFileRequest(deps.repo, request.body);
      if (parsed.outcome === "invalid") {
        return sendProblem(reply, parsed.status, parsed.title, parsed.detail);
      }
      const accepted = readAttributionSnapshot(deps.repo, parsed.profileId);
      if (accepted.outcome === "unavailable") {
        const { status, title, detail } = accepted.problem;
        return sendProblem(reply, status, title, detail);
      }

      const basis = suggestPrintFileAttribution(accepted.snapshot, {
        objectNames: parsed.objectNames,
        fallbackFilename: parsed.filename,
      });
      const inspection = await inspectPrintFile(deps.repo, parsed);
      if (inspection.outcome === "rejected") {
        return sendProblem(reply, 409, "Conflict", inspection.detail);
      }
      // `inspected` is the discriminant, so the unknown case omits
      // classification and print_ready rather than nulling them. PrintPartner
      // either read the bytes or it did not, and an operator must never be
      // shown a guess.
      return inspection.outcome === "inspected"
        ? {
            inspected: true,
            classification: inspection.classification,
            print_ready: isPrintReady(inspection.classification),
            next_action: printFileNextAction(inspection.classification),
            ...basis,
          }
        : { inspected: false, ...basis };
    },
  );

  /**
   * Take a print file off the operator's own computer, for a print made on a
   * printer PrintPartner cannot reach. The bytes are classified here, exactly
   * as bytes pulled off a printer host are, and held under `upload_token` for
   * the assignment that follows.
   */
  app.post(
    "/printer-checkoff/file-assignments/upload",
    {
      config: { rateLimit: { max: 10, timeWindow: "1 minute" } },
      schema: { consumes: ["multipart/form-data"] },
    },
    async (request, reply) => {
      if (!request.isMultipart()) {
        return sendProblem(
          reply,
          415,
          "Unsupported Media Type",
          "Send the print file as multipart/form-data",
        );
      }

      let bytes: Uint8Array | null = null;
      let uploadedName = "";
      let profileIdField = "";
      let objectNamesField = "";
      try {
        for await (const part of request.parts({
          limits: { fileSize: MAX_UPLOAD_BYTES, files: 1, fields: 4, fieldSize: 64 * 1024 },
        })) {
          if (part.type !== "file") {
            if (part.fieldname === "profile_id") profileIdField = String(part.value);
            if (part.fieldname === "object_names") objectNamesField = String(part.value);
            continue;
          }
          if (part.fieldname !== "file") {
            // Busboy stalls on a file stream nobody reads, so drain it.
            part.file.resume();
            continue;
          }
          bytes = await part.toBuffer();
          uploadedName = part.filename ?? "";
        }
      } catch (error) {
        const code = error instanceof Error && "code" in error ? String(error.code) : "";
        if (code === "FST_REQ_FILE_TOO_LARGE") {
          return sendProblem(
            reply,
            413,
            "Payload Too Large",
            `That print file is larger than the ${MAX_UPLOAD_BYTES / 1024 / 1024} MB PrintPartner will take as an upload`,
          );
        }
        if (code === "FST_FILES_LIMIT") {
          return sendProblem(reply, 400, "Bad Request", "Upload one print file at a time");
        }
        request.log.warn({ failure: code || "unreadable" }, "Print file upload could not be read");
        return sendProblem(reply, 400, "Bad Request", "That upload could not be read");
      }

      const profileId = Number(profileIdField);
      if (!Number.isInteger(profileId) || profileId <= 0) {
        return sendProblem(reply, 400, "Bad Request", "profile_id is required");
      }
      if (!bytes) {
        return sendProblem(reply, 400, "Bad Request", "Attach the print file as the file part");
      }
      const filename = basename(uploadedName.replace(/\\/g, "/")).trim();
      if (!filename || filename.length > 500) {
        return sendProblem(reply, 400, "Bad Request", "filename is required");
      }
      if (!/\.(?:gcode|gco|bgcode|3mf)$/i.test(filename)) {
        return sendProblem(
          reply,
          400,
          "Bad Request",
          "Choose a .gcode, .gco, .bgcode, or .3mf print file",
        );
      }

      const accepted = readAttributionSnapshot(deps.repo, profileId);
      if (accepted.outcome === "unavailable") {
        const { status, title, detail } = accepted.problem;
        return sendProblem(reply, status, title, detail);
      }

      const classified = classifyPrintFileBytes(bytes);
      if (classified.outcome === "rejected") {
        return sendProblem(reply, 409, "Conflict", printFileRejectionMessage(classified.reason));
      }

      if (
        deps.hostedPlanning &&
        deps.dataDir &&
        deps.reposDir &&
        (await sendIfTenantDiskQuotaExceeded(reply, {
          dataDir: deps.dataDir,
          reposDir: deps.reposDir,
          tenantId: request.tenantId,
          sourceIds: deps.repo.listSources().map((source) => source.id),
          additionalBytes: bytes.byteLength,
          quotaBytes: HOSTED_TENANT_DISK_QUOTA_BYTES,
        }))
      ) {
        return;
      }

      const now = Date.now();
      if (sweepPendingUploads(now) + bytes.byteLength > MAX_PENDING_UPLOAD_BYTES) {
        return sendProblem(
          reply,
          503,
          "Service Unavailable",
          "PrintPartner is holding as many uploaded print files as it will; record one of them, then try again",
        );
      }
      const uploadToken = randomUUID();
      pendingUploads.set(uploadToken, { profileId, bytes, expiresAt: now + UPLOAD_TTL_MS });

      // Same body as a preview, so one flow renders a file off a printer and a
      // file off the operator's computer. `inspected` is always true: the bytes
      // are in hand or the upload was refused above.
      return {
        inspected: true,
        classification: classified.classification,
        print_ready: isPrintReady(classified.classification),
        next_action: printFileNextAction(classified.classification),
        upload_token: uploadToken,
        ...suggestPrintFileAttribution(accepted.snapshot, {
          objectNames: parseUploadedObjectNames(objectNamesField),
          fallbackFilename: filename,
        }),
      };
    },
  );

  app.post(
    "/printer-checkoff/file-assignments",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const body = request.body as {
        completed?: unknown;
        plan_revision_id?: unknown;
        unit_tokens?: unknown;
        object_mappings?: unknown;
        retain_unmatched?: unknown;
      };
      const parsed = parsePrintFileRequest(deps.repo, request.body);
      if (parsed.outcome === "invalid") {
        return sendProblem(reply, parsed.status, parsed.title, parsed.detail);
      }
      const planRevisionId = Number(body.plan_revision_id);
      if (!Number.isInteger(planRevisionId) || planRevisionId <= 0) {
        return sendProblem(reply, 400, "Bad Request", "plan_revision_id is required");
      }
      if (!Array.isArray(body.unit_tokens)) {
        return sendProblem(
          reply,
          400,
          "Bad Request",
          "unit_tokens is required; confirm the Required units this file produces",
        );
      }
      const confirmedUnits: PrinterCheckoffUnit[] = [];
      if (body.unit_tokens.length > 500) {
        return sendProblem(reply, 400, "Bad Request", "At most 500 units can be assigned at once");
      }
      for (const rawToken of body.unit_tokens.slice(0, 500)) {
        const unit = typeof rawToken === "string" ? parsePrinterCheckoffUnitToken(rawToken) : null;
        if (!unit) {
          return sendProblem(reply, 400, "Bad Request", "unit_tokens holds an unreadable unit");
        }
        confirmedUnits.push(unit);
      }
      let objectMappings: PrinterObjectMapping[] | undefined;
      if (body.object_mappings !== undefined) {
        if (!Array.isArray(body.object_mappings) || body.object_mappings.length > 500) {
          return sendProblem(
            reply,
            400,
            "Bad Request",
            "object_mappings must be a list of at most 500 choices",
          );
        }
        objectMappings = [];
        for (const entry of body.object_mappings) {
          const value: unknown = entry;
          if (
            typeof value !== "object" ||
            value === null ||
            !("object_index" in value) ||
            typeof value.object_index !== "number" ||
            !Number.isInteger(value.object_index) ||
            value.object_index < 0 ||
            !("part_id" in value) ||
            typeof value.part_id !== "number" ||
            !Number.isInteger(value.part_id) ||
            value.part_id <= 0 ||
            !("unit_index" in value) ||
            typeof value.unit_index !== "number" ||
            !Number.isInteger(value.unit_index) ||
            value.unit_index < 0
          ) {
            return sendProblem(
              reply,
              400,
              "Bad Request",
              "object_mappings contains an invalid choice",
            );
          }
          objectMappings.push({
            object_index: value.object_index,
            part_id: value.part_id,
            unit_index: value.unit_index,
          });
        }
      }

      // The bytes decide. Nothing the client sent about this file is trusted.
      //
      // Print-readiness only gates a file PrintPartner might still print. When
      // the operator is recording a print that already happened, the file is
      // evidence of what was made, not instructions to run, and refusing it
      // for "needs slicing" would refuse the very thing being recorded. A
      // Bambu or Orca project 3MF is the normal case here: those slicers only
      // write Metadata/plate_N.gcode when you export a sliced file, so the
      // project save an operator has on disk carries the object names and no
      // toolpath. A file PrintPartner could not READ is still refused above,
      // because then there is nothing to attribute.
      const alreadyPrinted = body.completed === true;
      let classification: PrintFileClassification | undefined;
      let remoteIdentity: PrinterFileIdentity | undefined;
      const inspection = await inspectPrintFile(deps.repo, parsed);
      if (inspection.outcome === "rejected") {
        return sendProblem(reply, 409, "Conflict", inspection.detail);
      }
      if (inspection.outcome === "inspected") {
        if (!alreadyPrinted && !isPrintReady(inspection.classification)) {
          return sendProblem(
            reply,
            409,
            "Conflict",
            printFileNextAction(inspection.classification),
          );
        }
        classification = inspection.classification;
        remoteIdentity = inspection.identity;
        if (parsed.remotePath && !parsed.snapshotToken) {
          // Another link may already point at this path. Re-observing it here
          // is what turns a changed provider file into recorded drift. An
          // upload has no path to drift against, so there is nothing to record.
          deps.repo.observePrinterCheckoffRemoteFile({
            integrationId: parsed.integrationId,
            remotePath: parsed.remotePath,
            observed: remoteIdentity,
          });
        }
      }
      if (!classification && /\.3mf$/i.test(parsed.filename)) {
        return sendProblem(
          reply,
          409,
          "Conflict",
          "PrintPartner has to read a 3MF to tell whether it holds printer instructions; upload the file so PrintPartner can read it",
        );
      }

      let materialized: MaterializeAcceptedPrinterLinkResult;
      try {
        materialized = deps.repo.materializeAcceptedPrinterLink({
          kind: "create",
          profileId: parsed.profileId,
          expectedPlanRevisionId: planRevisionId,
          objectNames: parsed.objectNames,
          confirmedUnits,
          objectMappings: body.retain_unmatched === true ? (objectMappings ?? []) : objectMappings,
          retainUnmatched: body.retain_unmatched === true,
          link: {
            integrationId: parsed.integrationId,
            printerId: parsed.printerId,
            hostName: parsed.printerName,
            filename: parsed.filename,
            remotePath: parsed.remotePath,
            remoteIdentity,
            classification,
            started: false,
          },
        });
      } catch (error) {
        request.log.error(
          {
            failure:
              error instanceof AcceptedPlanOperationalIntegrityError ? "integrity" : "unexpected",
            code: error instanceof AcceptedPlanOperationalIntegrityError ? error.code : undefined,
            profileId: parsed.profileId,
            printerId: parsed.printerId,
          },
          "Print file assignment failed",
        );
        return sendProblem(reply, 500, "Internal Server Error", "Internal Server Error");
      }

      const problem = materializeProblem(materialized);
      if (problem) {
        return sendProblem(reply, problem.status, problem.title, problem.detail);
      }
      if (materialized.kind !== "created") {
        request.log.error(
          { failure: "unexpected", outcome: materialized.kind },
          "Print file assignment failed",
        );
        return sendProblem(reply, 500, "Internal Server Error", "Internal Server Error");
      }
      // The link now carries everything the bytes said, so the copy PrintPartner
      // was holding has no reader left.
      if (parsed.uploadToken) pendingUploads.delete(parsed.uploadToken);
      if (parsed.snapshotToken) consumePrinterFileSnapshot(deps.repo, parsed.snapshotToken);
      if (body.completed !== true) {
        return { link: materialized.link, attribution: materialized.attribution };
      }
      const completed = updatePrinterCheckoffLink(
        deps.repo,
        materialized.link.id,
        { ...HOST_COMPLETED_PATCH, completed_at: new Date().toISOString() },
        { requireState: "watching" },
      );
      if (!completed) {
        return sendProblem(reply, 409, "Conflict", "Print assignment changed; retry");
      }
      return { link: completed, attribution: materialized.attribution };
    },
  );

  app.post(
    "/printer-checkoff/:id/recheck-remote-file",
    { config: { rateLimit: { max: 30, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const { id } = request.params as { id: string };
      const link = getPrinterCheckoffLink(deps.repo, id);
      if (!link) return sendProblem(reply, 404, "Not Found", "Tracked print not found");
      if (!link.remote_path) {
        return sendProblem(reply, 409, "Conflict", "This tracked print has no printer file path");
      }
      const gateway = printerFileGateway(deps.repo, link.integration_id);
      if (!gateway) {
        return sendProblem(
          reply,
          503,
          "Service Unavailable",
          "That printer host is not serving its file list right now",
        );
      }
      await observePrinterCheckoffFileDrift({
        repo: deps.repo,
        integrationId: link.integration_id,
        files: gateway.files,
        config: gateway.config,
      });
      return { link: getPrinterCheckoffLink(deps.repo, id) };
    },
  );

  app.post(
    "/printer-checkoff/:id/manual-complete",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const id = (request.params as { id: string }).id;
      const link = getPrinterCheckoffLink(deps.repo, id);
      if (!link) return sendProblem(reply, 404, "Not Found", "Tracked print not found");
      if (!isManualIntegrationId(link.integration_id, link.printer_id)) {
        return sendProblem(reply, 409, "Conflict", "This print is monitored by its printer host");
      }
      const completed = updatePrinterCheckoffLink(
        deps.repo,
        id,
        { ...HOST_COMPLETED_PATCH, completed_at: new Date().toISOString() },
        { requireState: "watching" },
      );
      if (!completed) {
        return sendProblem(reply, 409, "Conflict", "Tracked print is no longer waiting to finish");
      }
      return { link: completed };
    },
  );

  app.get("/printer-outcomes/summary", async (request, reply) => {
    const query = request.query as { profile_id?: string };
    const profileId = Number(query.profile_id);
    if (!Number.isInteger(profileId) || profileId <= 0) {
      return sendProblem(reply, 400, "Bad Request", "profile_id is required");
    }
    return summarizePrintOutcomes(deps.repo, profileId);
  });

  // --- Unattributed prints routes ---

  app.get("/printer-checkoff/unattributed", async () => {
    const prints = filterLinkedUnattributedPrints(
      listOpenUnattributedPrints(deps.repo),
      loadPrinterCheckoffLinks(deps.repo),
    );
    return { prints };
  });

  app.post(
    "/printer-checkoff/unattributed/:id/claim",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const { id } = request.params as { id: string };
      const body = request.body as {
        profile_id?: unknown;
        selected_stl_basenames?: unknown;
      };
      const profileId = Number(body.profile_id);
      if (!Number.isInteger(profileId) || profileId <= 0) {
        return sendProblem(reply, 400, "Bad Request", "profile_id is required");
      }
      const selectedStlBasenames = Array.isArray(body.selected_stl_basenames)
        ? body.selected_stl_basenames
            .filter((value): value is string => typeof value === "string")
            .map((value) => value.trim().toLowerCase())
            .filter(Boolean)
        : null;

      const allPrints = listUnattributedPrints(deps.repo);
      const print = allPrints.find((p) => p.id === id);
      if (!print) {
        return sendProblem(reply, 404, "Not Found", "Unattributed print not found");
      }
      if (print.claimed_at) {
        return sendProblem(reply, 409, "Conflict", "Print already claimed");
      }

      let objectNames: string[] | undefined;
      if (selectedStlBasenames != null) {
        if (selectedStlBasenames.length === 0) {
          return sendProblem(reply, 400, "Bad Request", "Select at least one plate file");
        }
        const selected = new Set(selectedStlBasenames);
        const grouped = groupObjectsByPart(print.gcode_objects);
        objectNames = [...grouped.values()]
          .filter((group) => selected.has(group.stlBasename.toLowerCase()))
          .flatMap((group) => group.objects.map((object) => object.name));
        if (objectNames.length === 0) {
          return sendProblem(reply, 400, "Bad Request", "Selected files are not on this plate");
        }
      }

      let materialized: MaterializeAcceptedPrinterLinkResult;
      try {
        materialized = deps.repo.materializeAcceptedPrinterLink({
          kind: "claim",
          profileId,
          expectedPrint: print,
          objectNames,
        });
      } catch (error) {
        if (error instanceof AcceptedPlanOperationalIntegrityError) {
          request.log.error(
            { failure: "integrity", code: error.code, profileId, printId: print.id },
            "Accepted printer claim failed",
          );
        } else {
          request.log.error(
            { failure: "unexpected", profileId, printId: print.id },
            "Accepted printer claim failed",
          );
        }
        return sendProblem(reply, 500, "Internal Server Error", "Internal Server Error");
      }
      const problem = materializeProblem(materialized);
      if (problem) {
        return sendProblem(reply, problem.status, problem.title, problem.detail);
      }
      if (materialized.kind !== "claimed") {
        request.log.error(
          { failure: "unexpected", profileId, printId: print.id, outcome: materialized.kind },
          "Accepted printer claim failed",
        );
        return sendProblem(reply, 500, "Internal Server Error", "Internal Server Error");
      }
      return { link: materialized.link, ok: true };
    },
  );

  app.post(
    "/printer-checkoff/unattributed/:id/dismiss",
    { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } },
    async (request, reply) => {
      const { id } = request.params as { id: string };
      const ok = dismissUnattributedPrint(deps.repo, id);
      if (!ok) {
        return sendProblem(reply, 404, "Not Found", "Unattributed print not found");
      }
      return { ok: true };
    },
  );
}
