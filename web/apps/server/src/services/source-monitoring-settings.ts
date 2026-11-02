import { isRecord } from "../lib/guards.js";

export const DEFAULT_SOURCE_UPDATE_INTERVAL_HOURS = 24;
const MIN_SOURCE_UPDATE_INTERVAL_HOURS = 1;
const MAX_SOURCE_UPDATE_INTERVAL_HOURS = 168;

type SourceMonitoringUpdate = {
  intervalHours?: number;
  autoSyncUpdates?: boolean;
};

type SourceMonitoringUpdateParseResult =
  { kind: "valid"; update: SourceMonitoringUpdate } | { kind: "invalid"; detail: string };

function isSourceUpdateIntervalHours(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    (value === 0 ||
      (value >= MIN_SOURCE_UPDATE_INTERVAL_HOURS && value <= MAX_SOURCE_UPDATE_INTERVAL_HOURS))
  );
}

export function parseSourceMonitoringUpdate(body: unknown): SourceMonitoringUpdateParseResult {
  if (!isRecord(body)) {
    return { kind: "invalid", detail: "request body must be an object" };
  }

  const update: SourceMonitoringUpdate = {};
  if ("interval_hours" in body) {
    if (!isSourceUpdateIntervalHours(body.interval_hours)) {
      return {
        kind: "invalid",
        detail: "interval_hours must be 0 or a whole number from 1 through 168",
      };
    }
    update.intervalHours = body.interval_hours;
  }
  if ("auto_sync_updates" in body) {
    if (typeof body.auto_sync_updates !== "boolean") {
      return { kind: "invalid", detail: "auto_sync_updates must be a boolean" };
    }
    update.autoSyncUpdates = body.auto_sync_updates;
  }
  return { kind: "valid", update };
}

export function readStoredSourceUpdateIntervalHours(value: string | null): number {
  if (value === null || value.trim() === "") return DEFAULT_SOURCE_UPDATE_INTERVAL_HOURS;
  const intervalHours = Number(value);
  return isSourceUpdateIntervalHours(intervalHours)
    ? intervalHours
    : DEFAULT_SOURCE_UPDATE_INTERVAL_HOURS;
}
