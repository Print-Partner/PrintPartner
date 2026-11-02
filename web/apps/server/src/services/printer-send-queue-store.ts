import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { relative, resolve } from "node:path";
import type {
  PrinterCheckoffUnit,
  PrinterSendQueueItem,
  PrinterSendQueueMatch,
  PrinterSendQueueState,
} from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import { resolvedFileUnderRoot } from "../lib/secure-path.js";

export const PRINTER_SEND_QUEUE_SETTINGS_KEY = "printer.send_queue";
const SETTINGS_KEY = PRINTER_SEND_QUEUE_SETTINGS_KEY;
const MAX_ITEMS = 50;

const STATES = new Set<PrinterSendQueueState>(["queued", "sending", "done", "error", "cancelled"]);

function normalizeUnit(x: unknown): PrinterCheckoffUnit | null {
  if (!x || typeof x !== "object") return null;
  const row = x as Record<string, unknown>;
  const partId = Number(row.part_id);
  const unitIndex = Number(row.unit_index);
  if (!Number.isInteger(partId) || partId <= 0) return null;
  if (!Number.isInteger(unitIndex) || unitIndex < 0) return null;
  const unit: PrinterCheckoffUnit = { part_id: partId, unit_index: unitIndex };
  const objectName = typeof row.object_name === "string" ? row.object_name.trim() : "";
  if (objectName) unit.object_name = objectName.slice(0, 200);
  return unit;
}

function parseItem(raw: unknown): PrinterSendQueueItem | null {
  if (!raw || typeof raw !== "object") return null;
  const row = raw as Record<string, unknown>;
  const state = row.state;
  if (typeof state !== "string" || !STATES.has(state as PrinterSendQueueState)) return null;
  const id = typeof row.id === "string" ? row.id.trim() : "";
  const filename = typeof row.filename === "string" ? row.filename.trim() : "";
  const artifactPath = typeof row.artifact_path === "string" ? row.artifact_path.trim() : "";
  const printerId = typeof row.printer_id === "string" ? row.printer_id.trim() : "";
  if (!id || !filename || !artifactPath || !printerId) return null;
  const profileId = row.profile_id == null ? undefined : Number(row.profile_id);
  const item: PrinterSendQueueItem = {
    id,
    filename,
    artifact_path: artifactPath,
    printer_id: printerId,
    wait_for_idle: row.wait_for_idle !== false,
    start: Boolean(row.start),
    state: state as PrinterSendQueueState,
    created_at:
      typeof row.created_at === "string" && row.created_at
        ? row.created_at
        : new Date().toISOString(),
    updated_at:
      typeof row.updated_at === "string" && row.updated_at
        ? row.updated_at
        : new Date().toISOString(),
  };
  if (profileId != null && Number.isInteger(profileId) && profileId > 0) {
    item.profile_id = profileId;
  }
  if (Array.isArray(row.checkoff_units)) {
    const units = row.checkoff_units.flatMap((item) => {
      const unit = normalizeUnit(item);
      return unit ? [unit] : [];
    });
    if (units.length) item.checkoff_units = units;
  }
  if (typeof row.upload_job_id === "string" && row.upload_job_id.trim()) {
    item.upload_job_id = row.upload_job_id.trim();
  }
  if (typeof row.error === "string" && row.error.trim()) {
    item.error = row.error.trim().slice(0, 500);
  }
  if (typeof row.host_name === "string" && row.host_name.trim()) {
    item.host_name = row.host_name.trim();
  }
  if (row.match === "compatible" || row.match === "pinned") {
    item.match = row.match;
  }
  return item;
}

export function loadPrinterSendQueue(repo: AppRepository): PrinterSendQueueItem[] {
  const raw = repo.getSetting(SETTINGS_KEY);
  if (!raw?.trim()) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.map(parseItem).filter((x): x is PrinterSendQueueItem => x != null);
  } catch {
    return [];
  }
}

function isActiveSendQueueState(state: PrinterSendQueueState): boolean {
  return state === "queued" || state === "sending" || state === "error";
}

/** Cap terminal history only — never drop queued/sending/error items. */
export function trimPrinterSendQueue(items: PrinterSendQueueItem[]): PrinterSendQueueItem[] {
  const active = items.filter((i) => isActiveSendQueueState(i.state));
  const terminal = items.filter((i) => !isActiveSendQueueState(i.state));
  const keepTerminal = Math.max(0, MAX_ITEMS - active.length);
  if (keepTerminal === 0) return active;
  return [...active, ...terminal.slice(-keepTerminal)];
}

function savePrinterSendQueue(repo: AppRepository, items: PrinterSendQueueItem[]): void {
  repo.setSetting(SETTINGS_KEY, JSON.stringify(trimPrinterSendQueue(items)));
}

export function migratePrinterSendQueueArtifactPaths(
  repo: AppRepository,
  legacyExportsDir: string,
  tenantExportsDir: string,
): number {
  const legacyUploads = resolve(legacyExportsDir, "printer-uploads");
  const tenantUploads = resolve(tenantExportsDir, "printer-uploads");
  const items = loadPrinterSendQueue(repo);
  let migrated = 0;
  for (const item of items) {
    const current = resolve(item.artifact_path);
    const relativeArtifact = relative(legacyUploads, current);
    if (
      !relativeArtifact ||
      relativeArtifact.startsWith("..") ||
      resolve(legacyUploads, relativeArtifact) !== current
    ) {
      continue;
    }
    const target = resolve(tenantUploads, relativeArtifact);
    if (existsSync(current) || !resolvedFileUnderRoot(tenantUploads, target)) {
      continue;
    }
    item.artifact_path = target;
    migrated += 1;
  }
  if (migrated > 0) savePrinterSendQueue(repo, items);
  return migrated;
}

type EnqueuePrinterSendInput = {
  filename: string;
  artifact_path: string;
  printer_id: string;
  start: boolean;
  wait_for_idle?: boolean;
  match?: PrinterSendQueueMatch;
  profile_id?: number;
  checkoff_units?: PrinterCheckoffUnit[];
  host_name?: string;
};

export function enqueuePrinterSend(
  repo: AppRepository,
  input: EnqueuePrinterSendInput,
): PrinterSendQueueItem | null {
  const filename = input.filename.trim();
  const artifactPath = input.artifact_path.trim();
  const printerId = input.printer_id.trim();
  if (!filename || !artifactPath || !printerId) return null;
  const match: PrinterSendQueueMatch = input.match === "compatible" ? "compatible" : "pinned";
  return repo.transaction(() => {
    const now = new Date().toISOString();
    const item: PrinterSendQueueItem = {
      id: randomUUID(),
      filename,
      artifact_path: artifactPath,
      printer_id: printerId,
      match,
      wait_for_idle: input.wait_for_idle !== false,
      start: Boolean(input.start),
      profile_id: input.profile_id,
      checkoff_units: input.checkoff_units?.flatMap((item) => {
        const unit = normalizeUnit(item);
        return unit ? [unit] : [];
      }),
      state: "queued",
      created_at: now,
      updated_at: now,
      host_name: input.host_name?.trim() || undefined,
    };
    const all = loadPrinterSendQueue(repo);
    all.push(item);
    savePrinterSendQueue(repo, all);
    return item;
  });
}

export function getPrinterSendQueueItem(
  repo: AppRepository,
  id: string,
): PrinterSendQueueItem | null {
  return loadPrinterSendQueue(repo).find((i) => i.id === id) ?? null;
}

export function updatePrinterSendQueueItem(
  repo: AppRepository,
  id: string,
  patch: Partial<
    Pick<
      PrinterSendQueueItem,
      "state" | "upload_job_id" | "error" | "updated_at" | "host_name" | "printer_id"
    >
  >,
  options?: {
    requireState?: PrinterSendQueueState | PrinterSendQueueState[];
    requireNoSendingForPrinter?: string;
  },
): PrinterSendQueueItem | null {
  return repo.transaction(() => {
    const all = loadPrinterSendQueue(repo);
    const idx = all.findIndex((i) => i.id === id);
    if (idx < 0) return null;
    if (options?.requireState) {
      const allowed = Array.isArray(options.requireState)
        ? options.requireState
        : [options.requireState];
      if (!allowed.includes(all[idx].state)) return null;
    }
    if (options?.requireNoSendingForPrinter) {
      const printerId = options.requireNoSendingForPrinter;
      if (
        all.some(
          (item, itemIdx) =>
            itemIdx !== idx && item.printer_id === printerId && item.state === "sending",
        )
      ) {
        return null;
      }
    }
    const next = {
      ...all[idx],
      ...patch,
      updated_at: patch.updated_at ?? new Date().toISOString(),
    };
    all[idx] = next;
    savePrinterSendQueue(repo, all);
    return next;
  });
}

export function cancelPrinterSendQueueItem(
  repo: AppRepository,
  id: string,
): PrinterSendQueueItem | null {
  return updatePrinterSendQueueItem(
    repo,
    id,
    { state: "cancelled", error: undefined },
    { requireState: ["queued", "error"] },
  );
}

export function listActivePrinterSendQueue(repo: AppRepository): PrinterSendQueueItem[] {
  return loadPrinterSendQueue(repo).filter((i) => isActiveSendQueueState(i.state));
}

/** Ensure artifact paths stay under exports/printer-uploads. */
export function assertPrinterUploadArtifactPath(exportsDir: string, artifactPath: string): string {
  const uploadsRoot = resolve(exportsDir, "printer-uploads");
  const resolved = resolve(artifactPath);
  const rel = relative(uploadsRoot, resolved);
  if (!rel || rel.startsWith("..") || resolve(uploadsRoot, rel) !== resolved) {
    throw new Error("Invalid artifact path");
  }
  return resolved;
}
