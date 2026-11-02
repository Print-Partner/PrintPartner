import { basename } from "node:path";
import type {
  IntegrationConfig,
  PrinterCheckoffLink,
  PrinterCheckoffReconcileUpdate,
  PrinterCheckoffUnit,
  PrinterFileIdentity,
  PrinterHostStatus,
  PrinterStoredFile,
} from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import type { PrinterFileAccess } from "../integrations/store.js";
import {
  listWatchingPrinterCheckoffLinks,
  loadPrinterCheckoffLinks,
  updatePrinterCheckoffLink,
} from "./printer-checkoff-store.js";

/** Normalize host filenames for matching (paths, case). */
export function normalizePrinterFilename(name: string | undefined | null): string {
  if (!name?.trim()) return "";
  return basename(name.trim().replace(/\\/g, "/")).toLowerCase();
}

export function printerFilenamesMatch(
  a: string | undefined | null,
  b: string | undefined | null,
): boolean {
  const left = normalizePrinterFilename(a);
  const right = normalizePrinterFilename(b);
  if (!left || !right) return false;
  return left === right;
}

export function parseCheckoffUnits(raw: unknown): PrinterCheckoffUnit[] {
  if (typeof raw === "string") {
    try {
      return parseCheckoffUnits(JSON.parse(raw) as unknown);
    } catch {
      return [];
    }
  }
  if (!Array.isArray(raw)) return [];
  const out: PrinterCheckoffUnit[] = [];
  const seen = new Set<string>();
  for (const item of raw) {
    if (!item || typeof item !== "object") continue;
    const row = item as Record<string, unknown>;
    const partId = Number(row.part_id);
    const unitIndex = Number(row.unit_index);
    if (!Number.isInteger(partId) || partId <= 0) continue;
    if (!Number.isInteger(unitIndex) || unitIndex < 0) continue;
    const key = `${partId}:${unitIndex}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const unit: PrinterCheckoffUnit = { part_id: partId, unit_index: unitIndex };
    const objectName = typeof row.object_name === "string" ? row.object_name.trim() : "";
    if (objectName) unit.object_name = objectName.slice(0, 200);
    out.push(unit);
  }
  return out;
}

/** Parse optional unlabeled object-name list from multipart / JSON payload. */
export function parseUnlabeledNames(raw: unknown): string[] {
  if (typeof raw === "string") {
    try {
      return parseUnlabeledNames(JSON.parse(raw) as unknown);
    } catch {
      return [];
    }
  }
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((n): n is string => typeof n === "string" && n.trim().length > 0)
    .map((n) => n.trim().slice(0, 200))
    .slice(0, 200);
}

export function unitKey(partId: number, unitIndex: number): string {
  return `${partId}:${unitIndex}`;
}

function resolvedUnitKeys(link: PrinterCheckoffLink): Set<string> {
  const keys = new Set<string>();
  for (const u of link.resolved_units ?? []) {
    keys.add(unitKey(u.part_id, u.unit_index));
  }
  return keys;
}

export function pendingCheckoffUnits(link: PrinterCheckoffLink): PrinterCheckoffUnit[] {
  const done = resolvedUnitKeys(link);
  return link.units.filter((u) => !done.has(unitKey(u.part_id, u.unit_index)));
}

type CheckoffReconcileDecision =
  | { action: "noop" }
  | { action: "mark_active"; progress?: number }
  | { action: "await_verify" }
  | { action: "host_failed"; reason: "cancelled" | "error" };

/**
 * Decide how a watching link should advance given the latest host status.
 * Idle/cancel/error never await-verify unless we saw ~100% then missed a brief `complete`.
 */
export function decideCheckoffReconcile(
  link: PrinterCheckoffLink,
  status: PrinterHostStatus,
): CheckoffReconcileDecision {
  if (link.state !== "watching") return { action: "noop" };

  const filenameMatches =
    printerFilenamesMatch(status.filename, link.filename) ||
    printerFilenamesMatch(status.filename, link.remote_path);

  if (status.state === "printing" || status.state === "paused") {
    if (!filenameMatches) return { action: "noop" };
    const progress =
      typeof status.progress === "number" && Number.isFinite(status.progress)
        ? status.progress
        : undefined;
    return { action: "mark_active", progress };
  }

  if (status.state === "complete") {
    if (filenameMatches) return { action: "await_verify" };
    // Host cleared filename after finish — only then trust saw_active / upload&start.
    const hostFilenameEmpty = !normalizePrinterFilename(status.filename);
    if (hostFilenameEmpty && (link.saw_active || link.started)) {
      return { action: "await_verify" };
    }
    return { action: "noop" };
  }

  if (status.state === "error") {
    if (link.saw_active || filenameMatches) return { action: "host_failed", reason: "error" };
    return { action: "noop" };
  }

  if (status.state === "idle") {
    if (!link.saw_active) return { action: "noop" };
    // Missed a brief complete/FINISHED: treat near-done as success.
    if (link.last_progress != null && link.last_progress >= 99) {
      return { action: "await_verify" };
    }
    return { action: "host_failed", reason: "cancelled" };
  }

  return { action: "noop" };
}

/**
 * Reconcile watching links for one host against a status snapshot.
 * Idempotent: non-watching links are never re-processed.
 * Host success → awaiting_verify (no Progress mutation).
 */
export function reconcilePrinterCheckoff(
  repo: AppRepository,
  integrationId: string,
  status: PrinterHostStatus,
): PrinterCheckoffReconcileUpdate[] {
  const watching = listWatchingPrinterCheckoffLinks(repo, integrationId);
  const updates: PrinterCheckoffReconcileUpdate[] = [];
  const STALE_MS = 48 * 60 * 60 * 1000;

  for (const link of watching) {
    const createdMs = Date.parse(link.created_at);
    const stale =
      Number.isFinite(createdMs) && Date.now() - createdMs > STALE_MS && !link.saw_active;
    if (
      stale &&
      (status.state === "idle" || status.state === "offline" || status.state === "unknown")
    ) {
      updatePrinterCheckoffLink(
        repo,
        link.id,
        {
          state: "dismissed",
          host_outcome: "unknown",
        },
        { requireState: "watching" },
      );
      continue;
    }

    const decision = decideCheckoffReconcile(link, status);
    if (decision.action === "noop") continue;

    if (decision.action === "mark_active") {
      const patch: {
        saw_active: boolean;
        last_progress?: number;
      } = { saw_active: true };
      if (decision.progress != null) {
        patch.last_progress = Math.max(link.last_progress ?? 0, decision.progress);
      }
      if (
        link.saw_active &&
        (patch.last_progress == null || patch.last_progress === link.last_progress)
      )
        continue;
      updatePrinterCheckoffLink(repo, link.id, patch, { requireState: "watching" });
      continue;
    }

    if (decision.action === "host_failed") {
      const claimed = updatePrinterCheckoffLink(
        repo,
        link.id,
        {
          state: "host_failed",
          host_outcome: decision.reason === "error" ? "failed" : "cancelled",
        },
        { requireState: "watching" },
      );
      if (!claimed) continue;
      updates.push({
        link_id: link.id,
        host_name: link.host_name,
        profile_id: link.profile_id,
        filename: link.filename,
        event: "host_failed",
        host_outcome: claimed.host_outcome ?? "failed",
        units_pending: pendingCheckoffUnits(link).length,
      });
      continue;
    }

    // Claim before notifying so concurrent reconciles cannot double-fire.
    const completedAt = new Date().toISOString();
    const claimed = updatePrinterCheckoffLink(
      repo,
      link.id,
      {
        state: "awaiting_verify",
        host_outcome: "success",
        completed_at: completedAt,
        saw_active: true,
      },
      { requireState: "watching" },
    );
    if (!claimed) continue;

    updates.push({
      link_id: link.id,
      host_name: link.host_name,
      profile_id: link.profile_id,
      filename: link.filename,
      event: "awaiting_verify",
      host_outcome: "success",
      units_pending: pendingCheckoffUnits(claimed).length,
    });
  }

  return updates;
}

/** The durable provider-side identity of one stored file, as the host reports it. */
export function printerStoredFileIdentity(entry: PrinterStoredFile): PrinterFileIdentity {
  const identity: PrinterFileIdentity = {};
  if (entry.size_bytes !== undefined) identity.size_bytes = entry.size_bytes;
  if (entry.modified_at !== undefined) identity.modified_at = entry.modified_at;
  if (entry.provider_revision !== undefined) identity.provider_revision = entry.provider_revision;
  return identity;
}

/**
 * Re-observe every active link's provider file on one host and record drift.
 *
 * This runs on the same poll as reconcile, so a file that changed underneath a
 * link is noticed without anyone asking. Providers describe files in directory
 * listings, so this browses each distinct containing directory once rather than
 * once per link.
 *
 * A browse that fails is skipped, never treated as a missing file: a host that
 * is briefly unreachable has not changed anybody's artifact.
 */
export async function observePrinterCheckoffFileDrift(input: {
  readonly repo: AppRepository;
  readonly integrationId: string;
  readonly files: PrinterFileAccess;
  readonly config: IntegrationConfig;
}): Promise<void> {
  const pathsByDirectory = new Map<string, Set<string>>();
  for (const link of loadPrinterCheckoffLinks(input.repo)) {
    if (link.state !== "watching" && link.state !== "awaiting_verify") continue;
    if (link.integration_id !== input.integrationId || !link.remote_path) continue;
    const slash = link.remote_path.lastIndexOf("/");
    const directory = slash < 0 ? "" : link.remote_path.slice(0, slash);
    const paths = pathsByDirectory.get(directory);
    if (paths) paths.add(link.remote_path);
    else pathsByDirectory.set(directory, new Set([link.remote_path]));
  }

  for (const [directory, paths] of pathsByDirectory) {
    let observed: Map<string, PrinterFileIdentity>;
    try {
      const listing = await input.files.browse(input.config, directory);
      observed = new Map(
        listing.entries.flatMap((entry) =>
          entry.kind === "file" ? [[entry.path, printerStoredFileIdentity(entry)] as const] : [],
        ),
      );
    } catch {
      continue;
    }
    for (const remotePath of paths) {
      input.repo.observePrinterCheckoffRemoteFile({
        integrationId: input.integrationId,
        remotePath,
        observed: observed.get(remotePath) ?? null,
      });
    }
  }
}
