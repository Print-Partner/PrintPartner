import { randomUUID } from "node:crypto";
import type {
  PrintOutcomeEvent,
  PrintOutcomesSummary,
  PrintRejectReason,
} from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";

/**
 * Legacy blob key. Still read during startup migration so existing events are
 * not lost. Once migrated they live exclusively in print_job_parts.
 */
const SETTINGS_KEY = "printer.print_outcomes";

const REASONS = new Set<PrintRejectReason>([
  "bed_adhesion",
  "layer_shift",
  "warping",
  "stringing",
  "under_extrusion",
  "over_extrusion",
  "dimensional",
  "collision",
  "wrong_filament",
  "other",
]);

export function isPrintRejectReason(value: unknown): value is PrintRejectReason {
  return typeof value === "string" && REASONS.has(value as PrintRejectReason);
}

// ── Legacy blob parse (read-only, used only for blob→SQL migration) ──────────

function parseEvent(raw: unknown): PrintOutcomeEvent | null {
  if (!raw || typeof raw !== "object") return null;
  const row = raw as Record<string, unknown>;
  const id = typeof row.id === "string" ? row.id.trim() : "";
  const result = row.result;
  if (result !== "confirmed" && result !== "rejected") return null;
  const profileId = Number(row.profile_id);
  const partId = Number(row.part_id);
  const unitIndex = Number(row.unit_index);
  if (!id || !Number.isInteger(profileId) || profileId <= 0) return null;
  if (!Number.isInteger(partId) || partId <= 0) return null;
  if (!Number.isInteger(unitIndex) || unitIndex < 0) return null;
  const event: PrintOutcomeEvent = {
    id,
    at: typeof row.at === "string" && row.at ? row.at : new Date().toISOString(),
    profile_id: profileId,
    part_id: partId,
    unit_index: unitIndex,
    result,
  };
  if (isPrintRejectReason(row.reason)) event.reason = row.reason;
  if (typeof row.note === "string" && row.note.trim()) {
    event.note = row.note.trim().slice(0, 500);
  }
  if (typeof row.host_integration_id === "string" && row.host_integration_id.trim()) {
    event.host_integration_id = row.host_integration_id.trim();
  }
  if (typeof row.filename === "string" && row.filename.trim()) {
    event.filename = row.filename.trim();
  }
  if (typeof row.match_key === "string" && row.match_key.trim()) {
    event.match_key = row.match_key.trim();
  }
  if (typeof row.role === "string" && row.role.trim()) {
    event.role = row.role.trim();
  }
  if (typeof row.filament_display === "string" && row.filament_display.trim()) {
    event.filament_display = row.filament_display.trim();
  }
  if (typeof row.link_id === "string" && row.link_id.trim()) {
    event.link_id = row.link_id.trim();
  }
  return event;
}

function loadBlobEvents(repo: AppRepository): PrintOutcomeEvent[] {
  const raw = repo.getSetting(SETTINGS_KEY);
  if (!raw?.trim()) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.map(parseEvent).filter((x): x is PrintOutcomeEvent => x != null);
  } catch {
    return [];
  }
}

// ── Blob → SQL one-time migration ────────────────────────────────────────────

const MIGRATED_KEY = "printer.print_outcomes_migrated_v1";

/**
 * Migrate any events still in the blob into print_job_parts rows.
 * Safe to call on every startup — idempotent via MIGRATED_KEY flag.
 */
export function migratePrintOutcomesBlob(repo: AppRepository): void {
  if (repo.getSetting(MIGRATED_KEY) === "1") return;
  const events = loadBlobEvents(repo);
  if (events.length > 0) {
    repo.insertPrintJobParts(
      events.map((e) => ({
        id: e.id,
        at: e.at,
        profileId: e.profile_id,
        partId: e.part_id,
        unitIndex: e.unit_index,
        result: e.result,
        reason: e.reason,
        note: e.note,
        hostIntegrationId: e.host_integration_id,
        filename: e.filename,
        matchKey: e.match_key,
        role: e.role,
        filamentDisplay: e.filament_display,
        linkId: e.link_id,
      })),
    );
  }
  repo.setSetting(MIGRATED_KEY, "1");
}

// ── SQL-backed public API ─────────────────────────────────────────────────────

function rowToEvent(row: {
  id: string;
  at: string;
  profileId: number;
  partId: number;
  unitIndex: number;
  result: string;
  reason: string | null;
  note: string | null;
  hostIntegrationId: string | null;
  filename: string | null;
  matchKey: string | null;
  role: string | null;
  filamentDisplay: string | null;
  linkId: string | null;
}): PrintOutcomeEvent {
  const e: PrintOutcomeEvent = {
    id: row.id,
    at: row.at,
    profile_id: row.profileId,
    part_id: row.partId,
    unit_index: row.unitIndex,
    result: row.result as PrintOutcomeEvent["result"],
  };
  if (isPrintRejectReason(row.reason)) e.reason = row.reason;
  if (row.note) e.note = row.note;
  if (row.hostIntegrationId) e.host_integration_id = row.hostIntegrationId;
  if (row.filename) e.filename = row.filename;
  if (row.matchKey) e.match_key = row.matchKey;
  if (row.role) e.role = row.role;
  if (row.filamentDisplay) e.filament_display = row.filamentDisplay;
  if (row.linkId) e.link_id = row.linkId;
  return e;
}

export function appendPrintOutcomes(
  repo: AppRepository,
  events: Omit<PrintOutcomeEvent, "id" | "at">[],
): PrintOutcomeEvent[] {
  if (!events.length) return [];
  const now = new Date().toISOString();

  // Group events by (profile_id, link_id) into jobs. One job per unique
  // (profile_id × link_id) combo in this batch.
  const jobKey = (e: Omit<PrintOutcomeEvent, "id" | "at">) => `${e.profile_id}:${e.link_id ?? ""}`;
  const jobMap = new Map<string, string>(); // key → job id

  const parts = events.map((e) => {
    const key = jobKey(e);
    if (!jobMap.has(key)) {
      const jobId = randomUUID();
      jobMap.set(key, jobId);
      repo.insertPrintJob({
        id: jobId,
        profileId: e.profile_id,
        hostIntegrationId: e.host_integration_id,
        filename: e.filename,
        at: now,
        linkId: e.link_id,
      });
    }
    return {
      id: randomUUID(),
      jobId: jobMap.get(key)!,
      at: now,
      profileId: e.profile_id,
      partId: e.part_id,
      unitIndex: e.unit_index,
      result: e.result,
      reason: e.reason,
      note: e.note,
      hostIntegrationId: e.host_integration_id,
      filename: e.filename,
      matchKey: e.match_key,
      role: e.role,
      filamentDisplay: e.filament_display,
      linkId: e.link_id,
    };
  });

  const inserted = repo.insertPrintJobParts(parts);
  return inserted.map(rowToEvent);
}

export function summarizePrintOutcomes(
  repo: AppRepository,
  profileId: number,
): PrintOutcomesSummary {
  const events = repo.listPrintJobParts(profileId).map(rowToEvent);
  const by_reason: Partial<Record<PrintRejectReason, number>> = {};
  const by_role: Record<string, { confirmed: number; rejected: number }> = {};
  let total_confirmed = 0;
  let total_rejected = 0;

  for (const e of events) {
    if (e.result === "confirmed") total_confirmed += 1;
    else total_rejected += 1;

    if (e.result === "rejected" && e.reason) {
      by_reason[e.reason] = (by_reason[e.reason] ?? 0) + 1;
    }

    const role = e.role?.trim() || "(none)";
    const bucket = by_role[role] ?? { confirmed: 0, rejected: 0 };
    if (e.result === "confirmed") bucket.confirmed += 1;
    else bucket.rejected += 1;
    by_role[role] = bucket;
  }

  const recent_rejected = events
    .filter((e) => e.result === "rejected")
    .slice(-10)
    .reverse();

  return {
    profile_id: profileId,
    total_confirmed,
    total_rejected,
    by_reason,
    by_role,
    recent_rejected,
  };
}
