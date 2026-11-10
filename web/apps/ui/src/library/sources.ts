import type { ProfileSummary, SourceActivity, SourceList } from "@print-partner/rust-client";
import { dateText } from "../builds/format";
import { readCore } from "../core/rust";

export type Source = SourceList["sources"][number];
export type SourceEvent = SourceActivity["events"][number];
type Freshness = ProfileSummary["freshness"];

export const sourcesQuery = {
  queryKey: ["core", "sources"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listSources", {}, signal),
} as const;

/** The most events the core returns in one read. */
export const ACTIVITY_LIMIT = 100;

export const sourceActivityQuery = {
  queryKey: ["core", "sources", "activity"],
  queryFn: ({ signal }: { signal: AbortSignal }) =>
    readCore("listSourceActivity", {}, signal, { limit: String(ACTIVITY_LIMIT) }),
} as const;

export interface Line {
  label: string;
  detail: string | null;
}

/** The Source's current revision: its tag or branch and short commit, and the revision ID. */
export function revisionText(source: Source): Line {
  if (source.current_source_revision_id == null) {
    return { label: "No revision yet", detail: "Recorded on the first sync" };
  }
  const ref = source.tag ?? source.branch;
  const commit = source.last_commit_sha?.slice(0, 7);
  return {
    label: commit ? `${ref} at ${commit}` : ref,
    detail: `Revision ${source.current_source_revision_id}`,
  };
}

/** Whether the Source matches its upstream, and when it last synced. */
export function syncText(source: Source): Line {
  const synced = dateText(source.last_synced_at ?? null);
  const when = synced ? `Last synced ${synced}` : "Never synced";
  switch (source.update_status) {
    case "up_to_date":
      return { label: "Up to date", detail: when };
    case "updates_available":
      return { label: "Update available", detail: when };
    case "unknown":
      return { label: "Not known", detail: `${when}. The last check could not tell.` };
    default:
      return { label: synced ? "Synced" : "Never synced", detail: synced ? when : null };
  }
}

/** One activity event in words. Kinds the UI does not know keep their stored name. */
export function eventText(event: SourceEvent): string {
  switch (event.kind) {
    case "source.update_available":
      return "Update available";
    case "source.updated":
      return "Updated";
    case "source.sync_failed":
      return event.detail ? `Sync failed: ${event.detail}` : "Sync failed";
    default:
      return event.kind;
  }
}

/** The newest event for each Source ID. The core lists events newest first. */
export function latestEvents(events: SourceEvent[]): Map<number, SourceEvent> {
  const latest = new Map<number, SourceEvent>();
  for (const event of events) {
    if (event.source_id != null && !latest.has(event.source_id)) latest.set(event.source_id, event);
  }
  return latest;
}

/**
 * Which revision of `source` the Build's Accepted Plan was built from, read from the Plan's
 * freshness. The core lists a reason for each Source whose revision or naming differs from
 * Accept, or that it does not track; every other Source was accepted at its current revision.
 * A Plan-wide reason, such as a Source attached after Accept, leaves that unknown.
 */
export function acceptedRevisionText(freshness: Freshness, source: Source): Line {
  const current = source.current_source_revision_id;
  const currentLine: Line = {
    label: "The current revision",
    detail: current == null ? null : `Revision ${current}`,
  };
  const untracked: Line = { label: "Not tracked by revision", detail: null };
  const names = (reason: { kind: string; source_id?: number }) => reason.source_id === source.id;
  switch (freshness.status) {
    case "unavailable":
      return { label: "Not known", detail: `Plan freshness is unavailable (${freshness.code})` };
    case "current":
      return currentLine;
    case "untracked":
      if (freshness.reasons.some((r) => r.kind === "no_accepted_inputs")) {
        return { label: "No Plan accepted yet", detail: null };
      }
      return freshness.reasons.some(names) ? untracked : currentLine;
    case "stale": {
      if (freshness.untracked_sources.some(names)) return untracked;
      const mine = freshness.reasons.filter(names);
      // Revision reasons first: a Source can also have changed naming rules.
      for (const reason of mine) {
        if (reason.kind === "source_revision_changed") {
          return {
            label: `Revision ${reason.accepted_revision_id}`,
            detail: `The Source is now at revision ${reason.current_revision_id}`,
          };
        }
        if (reason.kind === "source_revision_unavailable") {
          return {
            label: `Revision ${reason.accepted_revision_id}`,
            detail: "That revision is no longer available",
          };
        }
      }
      if (mine.length > 0)
        return { ...currentLine, detail: "Its naming rules changed since Accept" };
      return freshness.reasons.some((r) => !("source_id" in r))
        ? { label: "Not known", detail: "This Source may not be in the Accepted Plan" }
        : currentLine;
    }
  }
}
