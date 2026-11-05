import type { ProfileSummary } from "@print-partner/rust-client";

type Progress = ProfileSummary["accepted_progress"];
type Freshness = ProfileSummary["freshness"];

const UNAVAILABLE_PROGRESS: Record<Extract<Progress, { kind: "unavailable" }>["reason"], string> = {
  compatibility_dirty: "stored progress needs rebuilding",
  uninitialized: "not computed yet",
  integrity: "stored progress failed its integrity check",
  concurrent_update: "it changed while being read; reload to try again",
};

/** Accepted progress in words: printed of required units, never a guessed zero. */
export function progressText(progress: Progress): string {
  switch (progress.kind) {
    case "ready":
      return `${progress.total_units - progress.remaining_units} of ${progress.total_units} units printed`;
    case "empty":
      return "No accepted units";
    case "unavailable":
      return `Progress unavailable: ${UNAVAILABLE_PROGRESS[progress.reason]}`;
  }
}

export function dateText(iso: string | null): string | null {
  if (!iso) return null;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(
    date,
  );
}

type StaleReason = Extract<Freshness, { status: "stale" }>["reasons"][number];
type UntrackedReason = Extract<Freshness, { status: "untracked" }>["reasons"][number];

function reasonText(reason: StaleReason | UntrackedReason): string {
  switch (reason.kind) {
    case "source_revision_changed":
      return `${reason.source_name} has a newer revision than the accepted Plan`;
    case "source_revision_unavailable":
      return `The accepted revision of ${reason.source_name} is unavailable`;
    case "naming_rules_changed":
      return `The naming rules of ${reason.source_name} changed`;
    case "plan_inputs_invalid":
      return "The Plan inputs are invalid";
    case "plan_configuration_changed":
      return "The Plan configuration changed";
    case "no_accepted_inputs":
      return "No Plan has been accepted";
    case "source_revision_untracked":
      return `${reason.source_name} is not tracked by revision`;
  }
}

/** The Plan's freshness as a headline and its reasons, for the Overview. */
export function freshnessText(freshness: Freshness): { headline: string; reasons: string[] } {
  switch (freshness.status) {
    case "current":
      return { headline: `Current, accepted ${dateText(freshness.accepted_at)}`, reasons: [] };
    case "stale":
      return {
        headline: `Changed since it was accepted ${dateText(freshness.accepted_at)}`,
        reasons: [...freshness.reasons, ...freshness.untracked_sources].map(reasonText),
      };
    case "untracked":
      return { headline: "Not tracked", reasons: freshness.reasons.map(reasonText) };
    case "unavailable":
      return { headline: `Unavailable (${freshness.code})`, reasons: [] };
  }
}
