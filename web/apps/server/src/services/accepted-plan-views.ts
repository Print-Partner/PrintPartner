import { progressSummary } from "@print-partner/domain";
import type { SpoolSummary } from "../integrations/spoolman-client.js";
import { formatSpoolSummaryBadge } from "../integrations/spoolman-client.js";
import type { ReadAcceptedPlanOperationalSnapshotResult } from "../db/accepted-plan-operational.js";
import { getColorById, resolvePartFilamentHex } from "./filament-catalog.js";
import type { FilamentResolveContext } from "./filament-resolve.js";

type AcceptedUnavailableReason = "compatibility_dirty" | "uninitialized";

type AcceptedCollectionViewResult<T> =
  | { readonly kind: "ready" | "empty"; readonly body: T }
  | {
      readonly kind: "accepted_state_unavailable";
      readonly reason: AcceptedUnavailableReason;
    };

type AcceptedCheckoffPart = {
  readonly id: number;
  readonly filename: string;
  readonly match_key: string;
  readonly relative_path: string;
  readonly source_layer: string;
  readonly role: string;
  readonly quantity_effective: number;
  readonly printed_count: number;
  readonly print_units: readonly boolean[];
  readonly missing: boolean;
  readonly filament_display: string;
  readonly filament_hex: string | null;
  readonly spool_summary?: readonly SpoolSummary[];
  readonly spool_badge?: string;
};

type AcceptedCheckoffBody = {
  readonly profile_id: number;
  readonly summary: string;
  readonly parts: readonly AcceptedCheckoffPart[];
};

type AcceptedPartAssembledBody = {
  readonly part_id: number;
  readonly assembled_count: number;
  readonly assembled_units: readonly boolean[];
};

type AcceptedPartAssembledViewResult =
  | { readonly kind: "ready"; readonly body: AcceptedPartAssembledBody }
  | { readonly kind: "part_not_found" }
  | {
      readonly kind: "accepted_state_unavailable";
      readonly reason: AcceptedUnavailableReason;
    };

type AcceptedFilamentViewContext = Pick<
  FilamentResolveContext,
  "resolve" | "spoolSummariesForPart"
>;

function emptyCheckoff(profileId: number): AcceptedCheckoffBody {
  const parts: AcceptedCheckoffPart[] = [];
  return {
    profile_id: profileId,
    summary: progressSummary(parts),
    parts,
  };
}

export function toAcceptedCheckoffView(input: {
  readonly profileId: number;
  readonly accepted: ReadAcceptedPlanOperationalSnapshotResult;
  readonly filamentContext?: AcceptedFilamentViewContext;
}): AcceptedCollectionViewResult<AcceptedCheckoffBody> {
  if (input.accepted.kind === "empty") {
    return { kind: "empty", body: emptyCheckoff(input.profileId) };
  }
  if (input.accepted.kind !== "ready") {
    return {
      kind: "accepted_state_unavailable",
      reason: input.accepted.kind,
    };
  }

  const parts = input.accepted.snapshot.parts
    .filter((part) => part.included)
    .map((part): AcceptedCheckoffPart => {
      const printUnits = part.units.map((unit) => unit.completed);
      const printedCount = printUnits.filter(Boolean).length;
      const resolved = input.filamentContext?.resolve(part.filamentColorId);
      const catalogColor =
        !resolved && part.filamentColorId ? getColorById(part.filamentColorId) : null;
      const spoolSummary =
        input.filamentContext?.spoolSummariesForPart(part.filamentColorId, part.spoolmanSpoolId) ??
        [];
      const base = {
        id: part.projectionPartId,
        filename: part.filename,
        match_key: part.partKey,
        relative_path: part.relativePath,
        source_layer: part.sourceLayer,
        role: part.effectiveRole,
        quantity_effective: part.quantityEffective,
        printed_count: printedCount,
        print_units: printUnits,
        missing: printedCount < part.quantityEffective,
        filament_display: resolved?.combo_label ?? catalogColor?.combo_label ?? "",
        filament_hex: resolved?.hex ?? resolvePartFilamentHex(part),
      };
      return spoolSummary.length
        ? {
            ...base,
            spool_summary: spoolSummary,
            spool_badge: formatSpoolSummaryBadge(spoolSummary),
          }
        : base;
    })
    .sort(
      (left, right) =>
        Buffer.compare(Buffer.from(left.filename), Buffer.from(right.filename)) ||
        left.id - right.id,
    );

  return {
    kind: "ready",
    body: {
      profile_id: input.accepted.snapshot.profile.id,
      summary: progressSummary(parts),
      parts,
    },
  };
}

export function toAcceptedPartAssembledView(input: {
  readonly partId: number;
  readonly accepted: ReadAcceptedPlanOperationalSnapshotResult;
}): AcceptedPartAssembledViewResult {
  if (input.accepted.kind === "empty") return { kind: "part_not_found" };
  if (input.accepted.kind !== "ready") {
    return {
      kind: "accepted_state_unavailable",
      reason: input.accepted.kind,
    };
  }

  const part = input.accepted.snapshot.parts.find(
    (candidate) => candidate.projectionPartId === input.partId,
  );
  if (!part) return { kind: "part_not_found" };
  const assembledUnits = part.units.map((unit) => unit.assembled);
  return {
    kind: "ready",
    body: {
      part_id: input.partId,
      assembled_count: assembledUnits.filter(Boolean).length,
      assembled_units: assembledUnits,
    },
  };
}
