import { createWriteStream, mkdirSync, rmSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { pipeline } from "node:stream/promises";
import type { Readable } from "node:stream";
import type { PrinterCheckoffUnit } from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import { readFeatureAccess } from "../lib/feature-access.js";
import { desktopFeaturesEnabled } from "../lib/feature-toggles.js";
import { getIntegrationAdapter } from "../integrations/registry.js";
import { getIntegrationConfig } from "../integrations/store.js";
import { createPrinterCheckoffLink } from "./printer-checkoff-store.js";
import { resolvePlanIdForPrinterFetch } from "./printer-plan-bind.js";
import { loadFleet } from "./printer-fleet.js";
import { INTERRUPTED_PRINTER_SEND_ERROR, printerUploadsAreHalted } from "./printer-send-queue.js";
import { listActivePrinterSendQueue } from "./printer-send-queue-store.js";

const ALLOWED_EXTENSIONS = new Set([".gcode", ".bgcode", ".gco"]);

/** Remove `exports/.../printer-uploads/<jobId>/` after the host transfer finishes. */
export function cleanupPrinterUploadArtifactDir(artifactPath: string): void {
  const trimmed = artifactPath?.trim();
  if (!trimmed) return;
  try {
    const dir = dirname(trimmed);
    if (basename(dirname(dir)) !== "printer-uploads") return;
    rmSync(dir, { recursive: true, force: true });
  } catch {
    /* ignore */
  }
}

function sanitizePrinterUploadFilename(filename: string): string {
  const base = basename(filename.replace(/\\/g, "/")).trim() || "print.gcode";
  const cleaned = base.replace(/[/\\]/g, "_");
  if (!cleaned || cleaned === "." || cleaned === ".." || /^\.+$/.test(cleaned)) {
    return "print.gcode";
  }
  return cleaned;
}

export function isAllowedPrinterUploadFilename(filename: string): boolean {
  const lower = sanitizePrinterUploadFilename(filename).toLowerCase();
  for (const ext of ALLOWED_EXTENSIONS) {
    if (lower.endsWith(ext)) return true;
  }
  return false;
}

type PrinterUploadJobInput = {
  printer_id: string;
  artifact_path: string;
  filename: string;
  start: boolean;
  host_name?: string;
  /** Active plan for Progress verify tracking (Phase D). */
  profile_id?: number;
  /** Incomplete Progress units to mark when the host job completes. */
  checkoff_units?: PrinterCheckoffUnit[];
  /** Object names that did not map — Progress preview only. */
  unlabeled_names?: string[];
  upload_job_id?: string;
};

type PrinterUploadJobEmit = (patch: { message?: string; progress?: number }) => void;

function queueSendRow(repo: AppRepository, input: PrinterUploadJobInput) {
  if (input.upload_job_id === undefined) return null;
  return (
    listActivePrinterSendQueue(repo).find(
      (item) =>
        item.upload_job_id === input.upload_job_id && item.artifact_path === input.artifact_path,
    ) ?? null
  );
}

export async function runPrinterUploadJob(
  repo: AppRepository,
  input: PrinterUploadJobInput,
  emit: PrinterUploadJobEmit,
): Promise<Record<string, unknown>> {
  const ownedAtStart = queueSendRow(repo, input);
  if (ownedAtStart && ownedAtStart.state !== "sending") {
    throw new Error(INTERRUPTED_PRINTER_SEND_ERROR);
  }
  const queuedArtifact = ownedAtStart?.state === "sending";
  let succeeded = false;
  try {
    if (printerUploadsAreHalted()) {
      throw new Error(INTERRUPTED_PRINTER_SEND_ERROR);
    }
    const result = await runPrinterUploadJobInner(repo, input, emit, ownedAtStart);
    succeeded = true;
    return result;
  } finally {
    if (succeeded || (!queuedArtifact && !printerUploadsAreHalted())) {
      cleanupPrinterUploadArtifactDir(input.artifact_path);
    }
  }
}

async function runPrinterUploadJobInner(
  repo: AppRepository,
  input: PrinterUploadJobInput,
  emit: PrinterUploadJobEmit,
  ownedAtStart: ReturnType<typeof queueSendRow>,
): Promise<Record<string, unknown>> {
  if (
    desktopFeaturesEnabled() &&
    ownedAtStart &&
    (ownedAtStart.printer_id !== input.printer_id || ownedAtStart.profile_id !== input.profile_id)
  ) {
    throw new Error("Stored printer upload does not match its queue item");
  }
  const fleet = loadFleet(repo);
  const machine = fleet.find((m) => m.id === input.printer_id);
  if (!machine) {
    throw new Error("Fleet printer not found");
  }
  const integrationId = machine.integration_id?.trim();
  if (!integrationId) {
    throw new Error(
      "Printer is not linked to a host. Link a Moonraker or PrusaLink host in Settings.",
    );
  }

  const integration = getIntegrationConfig(repo, integrationId);
  if (!integration) {
    throw new Error("Linked printer host integration was not found");
  }
  if (desktopFeaturesEnabled()) {
    const farmHost = integration.type === "moonraker" || integration.type === "prusalink";
    const feature = integration.type === "bambu" ? "bambu" : farmHost ? "farm" : null;
    if (feature) {
      const profileId = input.profile_id;
      if (profileId == null || !Number.isSafeInteger(profileId) || profileId <= 0) {
        throw new Error("Pick a plan to bind this send (profile_id required)");
      }
      if (!repo.getOwnedProfileIdentity(profileId)) {
        throw new Error("Profile not found");
      }
      const access = readFeatureAccess(() => repo, feature, profileId);
      if (access.kind !== "allowed") {
        throw new Error(`${access.code}: ${access.error}`);
      }
    }
  }
  if (integration.config.enabled === false) {
    throw new Error("Linked printer host is disabled");
  }

  const adapter = getIntegrationAdapter(integration.type);
  if (!adapter?.uploadFile) {
    if (integration.type === "bambu") {
      throw new Error(
        "Direct Bambu upload is not available. Use Export → Open in Bambu Connect (official URL handoff), or Moonraker/PrusaLink for G-code upload.",
      );
    }
    throw new Error(`Upload is not supported for ${integration.type}`);
  }

  const hostLabel = input.host_name ?? integration.name ?? machine.name;
  const filename = basename(input.filename);
  if (!isAllowedPrinterUploadFilename(filename)) {
    throw new Error("Only .gcode / .bgcode / .gco files can be sent to a printer");
  }

  emit({
    message: `Uploading ${filename} to ${hostLabel}`,
    progress: 20,
  });

  emit({
    message: `Uploading ${filename} to ${hostLabel}`,
    progress: 45,
  });

  const result = await adapter.uploadFile(
    integration.config,
    { path: input.artifact_path },
    filename,
    {
      start: input.start,
    },
  );

  if (!result.ok) {
    throw new Error(result.message ?? "Printer upload failed");
  }
  if (printerUploadsAreHalted()) {
    throw new Error(INTERRUPTED_PRINTER_SEND_ERROR);
  }
  const ownedAfterUpload = queueSendRow(repo, input);
  const lostOwnedSend =
    ownedAtStart?.state === "sending" &&
    (!ownedAfterUpload ||
      ownedAfterUpload.id !== ownedAtStart.id ||
      ownedAfterUpload.state !== "sending");
  if (lostOwnedSend || (ownedAfterUpload && ownedAfterUpload.state !== "sending")) {
    throw new Error(INTERRUPTED_PRINTER_SEND_ERROR);
  }

  emit({
    message: result.started ? `Printing on ${hostLabel}` : `Uploaded ${filename} to ${hostLabel}`,
    progress: 90,
  });

  const checkoffUnits = input.checkoff_units ?? [];
  const unlabeledNames = Array.isArray(input.unlabeled_names)
    ? input.unlabeled_names.filter((n) => typeof n === "string" && n.trim())
    : [];
  let checkoffLinkId: string | undefined;
  const planId = resolvePlanIdForPrinterFetch(input.profile_id, null);
  if (planId != null) {
    const link = createPrinterCheckoffLink(repo, {
      profile_id: planId,
      integration_id: integrationId,
      printer_id: machine.id,
      host_name: hostLabel,
      filename,
      remote_path: result.remote_path ?? filename,
      upload_job_id: input.upload_job_id,
      units: checkoffUnits,
      unlabeled_names: unlabeledNames.length ? unlabeledNames : undefined,
      started: Boolean(result.started),
    });
    checkoffLinkId = link?.id;
  }

  return {
    printer_id: machine.id,
    printer_name: machine.name,
    integration_id: integrationId,
    integration_type: integration.type,
    host_name: hostLabel,
    filename,
    remote_path: result.remote_path ?? filename,
    started: Boolean(result.started),
    message: result.message,
    checkoff_link_id: checkoffLinkId,
    checkoff_units: checkoffLinkId ? checkoffUnits.length : 0,
  };
}

/** Stream a multipart file to disk; reject when the multipart limit truncates the body. */
export async function streamPrinterUploadArtifact(
  exportsDir: string,
  jobId: string,
  filename: string,
  file: Readable & { truncated?: boolean },
): Promise<string> {
  const safeName = sanitizePrinterUploadFilename(filename);
  const dir = join(exportsDir, "printer-uploads", jobId);
  mkdirSync(dir, { recursive: true });
  const path = join(dir, safeName);
  try {
    await pipeline(file, createWriteStream(path));
  } catch (err) {
    cleanupPrinterUploadArtifactDir(path);
    throw err;
  }
  if (file.truncated) {
    cleanupPrinterUploadArtifactDir(path);
    throw new Error("Upload exceeded size limit");
  }
  return path;
}
