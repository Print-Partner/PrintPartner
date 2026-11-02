import {
  folderKeyFromRelativePath,
  mergeConflictIssueMessage,
  ROOT_FOLDER,
} from "@print-partner/domain";
import type { AcceptedPlanBasisContract, PartRow } from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import type {
  AcceptedOperationalInput,
  AcceptedOperationalPart,
  AcceptedPlanOperationalSnapshot,
} from "../db/accepted-plan-operational.js";
import type { SpoolSummary } from "../integrations/spoolman-client.js";
import { formatSpoolSummaryBadge } from "../integrations/spoolman-client.js";
import { observeAcceptedMediaPng } from "../lib/accepted-media-cache.js";
import {
  createAcceptedArtifactObserver,
  observeAcceptedSnapshotRoot,
} from "./accepted-artifacts.js";
import { acceptedPartMediaIdentity } from "./accepted-part-media.js";
import { getColorById, resolvePartFilamentHex } from "./filament-catalog.js";
import type { FilamentResolveContext } from "./filament-resolve.js";

type AcceptedPlanReviewIssue = {
  readonly severity: "blocker" | "warning";
  readonly code: string;
  readonly message: string;
  readonly link_hint?: string;
};

type AcceptedPlanReviewPart = PartRow & {
  readonly printed_count: number;
  readonly print_units: readonly boolean[];
  readonly assembled_units: readonly boolean[];
  readonly missing: boolean;
  readonly stl_missing: boolean;
  readonly thumb_empty: boolean;
  readonly filament_display: string;
  readonly filament_hex: string | null;
  readonly spool_summary?: readonly SpoolSummary[];
  readonly spool_badge?: string;
};

export type AcceptedPlanReviewBody = {
  readonly profile_id: number;
  readonly accepted_basis: AcceptedPlanBasisContract | null;
  readonly plan_name: string;
  readonly layers: readonly {
    readonly id: number;
    readonly layer_type: string;
    readonly project_id: number;
    readonly project_name: string;
    readonly local_path: null;
    readonly synced: boolean;
    readonly last_synced_at: string | null;
  }[];
  readonly totals: {
    readonly included_parts: number;
    readonly total_print_units: number;
    readonly by_role: Readonly<Record<string, number>>;
    readonly by_filament: Readonly<Record<string, number>>;
  };
  readonly issues: readonly AcceptedPlanReviewIssue[];
  readonly has_blockers: boolean;
  readonly part_groups: readonly {
    readonly folder: string;
    readonly source_layer: string | null;
    readonly parts: readonly AcceptedPlanReviewPart[];
  }[];
};

type AcceptedFilamentContext = Pick<FilamentResolveContext, "resolve" | "spoolSummariesForPart">;

type ReadAcceptedPlanReviewInput = {
  readonly repo: AppRepository;
  readonly profileId: number;
  readonly includeExcluded: boolean;
  readonly reposDir: string;
  readonly thumbsDir: string | null;
  readonly reportTiming?: (timings: {
    snapshotMs: number;
    filamentMs: number;
    observationMs: number;
    projectionMs: number;
  }) => void;
  readonly loadFilamentContext?: (
    colorIds: readonly (string | null)[],
  ) => Promise<AcceptedFilamentContext>;
};

type ReadAcceptedPlanReviewResult =
  | { readonly kind: "not_found" }
  | { readonly kind: "empty"; readonly body: AcceptedPlanReviewBody }
  | { readonly kind: "ready"; readonly body: AcceptedPlanReviewBody }
  | {
      readonly kind: "accepted_state_unavailable";
      readonly reason: "compatibility_dirty" | "uninitialized";
    };

export function summarizeAcceptedPlanReview(review: AcceptedPlanReviewBody) {
  const blockers = review.issues.filter((issue) => issue.severity === "blocker");
  const warnings = review.issues.filter((issue) => issue.severity === "warning");
  return {
    plan_id: review.profile_id,
    plan_name: review.plan_name,
    has_blockers: review.has_blockers,
    blocker_count: blockers.length,
    warning_count: warnings.length,
    issue_codes: [...new Set(review.issues.map((issue) => issue.code))],
    sample_issues: review.issues.slice(0, 8).map((issue) => ({
      severity: issue.severity,
      code: issue.code,
      message: issue.message,
    })),
    totals: review.totals,
    layers: review.layers.map((layer) => ({
      type: layer.layer_type,
      source: layer.project_name,
      synced: layer.synced,
    })),
  };
}

function noIncludedPartsIssue(): AcceptedPlanReviewIssue {
  return {
    severity: "blocker",
    code: "no_included_parts",
    message: "No parts are included in this build.",
    link_hint: "build",
  };
}

function emptyReview(profileId: number, planName: string): AcceptedPlanReviewBody {
  return {
    profile_id: profileId,
    accepted_basis: null,
    plan_name: planName,
    layers: [],
    totals: { included_parts: 0, total_print_units: 0, by_role: {}, by_filament: {} },
    issues: [noIncludedPartsIssue()],
    has_blockers: true,
    part_groups: [],
  };
}

function layerLabel(sourceLayer: string): { layerType: string; projectName: string } {
  const separator = sourceLayer.indexOf(":");
  return separator < 0
    ? { layerType: "source", projectName: sourceLayer }
    : {
        layerType: sourceLayer.slice(0, separator),
        projectName: sourceLayer.slice(separator + 1),
      };
}

function filamentFields(part: AcceptedOperationalPart, context?: AcceptedFilamentContext) {
  const resolved = context?.resolve(part.filamentColorId);
  const catalogColor =
    !resolved && part.filamentColorId ? getColorById(part.filamentColorId) : null;
  const spoolSummary =
    context?.spoolSummariesForPart(part.filamentColorId, part.spoolmanSpoolId) ?? [];
  return {
    display: resolved?.combo_label ?? catalogColor?.combo_label ?? "",
    hex: resolved?.hex ?? resolvePartFilamentHex(part),
    spoolSummary,
  };
}

function reviewPart(input: {
  readonly part: AcceptedOperationalPart;
  readonly media: { readonly artifactMissing: boolean; readonly thumbEmpty: boolean };
  readonly context?: AcceptedFilamentContext;
}): { readonly row: AcceptedPlanReviewPart; readonly artifactMissing: boolean } {
  const { part } = input;
  const printUnits = part.units.map((unit) => unit.completed);
  const assembledUnits = part.units.map((unit) => unit.assembled);
  const printedCount = printUnits.filter(Boolean).length;
  const filament = filamentFields(part, input.context);
  const { artifactMissing, thumbEmpty } = input.media;
  const base = {
    id: part.projectionPartId,
    match_key: part.partKey,
    relative_path: part.relativePath,
    filename: part.filename,
    source_layer: part.sourceLayer,
    status: part.status,
    role: part.effectiveRole,
    requirement: part.requirement,
    option_group_id: part.optionGroupId,
    included: part.included,
    filament_color_id: part.filamentColorId,
    filament_custom_hex: part.filamentCustomHex,
    spoolman_spool_id: part.spoolmanSpoolId,
    filament_display: filament.display,
    filament_hex: filament.hex,
    quantity_auto: part.quantityInferred,
    quantity_override: part.quantityOverride,
    quantity_effective: part.quantityEffective,
    printed_count: printedCount,
    print_units: printUnits,
    assembled_units: assembledUnits,
    missing: printedCount < part.quantityEffective,
    stl_missing: artifactMissing,
    thumb_empty: thumbEmpty,
  };
  return {
    row: filament.spoolSummary.length
      ? {
          ...base,
          spool_summary: filament.spoolSummary,
          spool_badge: formatSpoolSummaryBadge(filament.spoolSummary),
        }
      : base,
    artifactMissing,
  };
}

function acceptedLayers(input: {
  readonly snapshot: AcceptedPlanOperationalSnapshot;
  readonly availableInputRoots: ReadonlySet<number>;
}): {
  readonly layers: AcceptedPlanReviewBody["layers"];
  readonly issues: readonly AcceptedPlanReviewIssue[];
} {
  if (input.snapshot.provenance.kind === "legacy") return { layers: [], issues: [] };
  const issues: AcceptedPlanReviewIssue[] = [];
  const layers = [...input.snapshot.provenance.inputs]
    .sort((left, right) => left.layerOrder - right.layerOrder || left.inputId - right.inputId)
    .map((acceptedInput: AcceptedOperationalInput) => {
      const label = layerLabel(acceptedInput.sourceLayer);
      const synced =
        acceptedInput.trackingKind === "revision" &&
        input.availableInputRoots.has(acceptedInput.inputId);
      if (!synced) {
        issues.push({
          severity: "blocker",
          code: "unsynced_source",
          message: `Source "${label.projectName}" is not synced to a local folder.`,
          link_hint: "sources",
        });
      }
      return {
        id: acceptedInput.inputId,
        layer_type: label.layerType,
        project_id: acceptedInput.sourceId,
        project_name: label.projectName,
        local_path: null,
        synced,
        last_synced_at:
          acceptedInput.trackingKind === "revision" ? acceptedInput.sourceSyncedAt : null,
      };
    });
  return { layers, issues };
}

export function projectAcceptedPlanReview(input: {
  readonly snapshot: AcceptedPlanOperationalSnapshot;
  readonly includeExcluded: boolean;
  readonly availableInputRoots: ReadonlySet<number>;
  readonly mediaByPartId: ReadonlyMap<
    number,
    { readonly artifactMissing: boolean; readonly thumbEmpty: boolean }
  >;
  readonly filamentContext?: AcceptedFilamentContext;
}): AcceptedPlanReviewBody {
  const layerResult = acceptedLayers(input);
  const included = input.snapshot.parts.filter((part) => part.included);
  const byRole: Record<string, number> = {};
  const byFilament: Record<string, number> = {};
  let printUnits = 0;
  for (const part of included) {
    const filament = filamentFields(part, input.filamentContext);
    const label = filament.display.trim() || part.filamentColorId?.trim() || "Unassigned";
    byRole[part.effectiveRole || "primary"] = (byRole[part.effectiveRole || "primary"] ?? 0) + 1;
    byFilament[label] = (byFilament[label] ?? 0) + part.quantityEffective;
    printUnits += part.quantityEffective;
  }

  const issues: AcceptedPlanReviewIssue[] = [...layerResult.issues];
  if (included.length === 0) issues.push(noIncludedPartsIssue());
  const displayParts = input.snapshot.parts
    .filter((part) => input.includeExcluded || part.included)
    .sort(
      (left, right) =>
        Buffer.compare(Buffer.from(left.filename), Buffer.from(right.filename)) ||
        left.projectionPartId - right.projectionPartId,
    );
  const projectedResults = displayParts.map((part) => {
    const media = input.mediaByPartId.get(part.projectionPartId);
    if (part.included && !media) {
      throw new Error("Included Part is missing accepted media observation");
    }
    const result = reviewPart({
      part,
      media: media ?? {
        artifactMissing: false,
        thumbEmpty: false,
      },
      context: input.filamentContext,
    });
    return { part, result };
  });
  for (const { part, result } of projectedResults) {
    if (part.included && result.artifactMissing) {
      issues.push({
        severity: "blocker",
        code: "missing_stl",
        message: `STL not found on disk: ${part.filename}`,
        link_hint: "sources",
      });
    }
  }
  for (const { part } of projectedResults) {
    if (part.included && part.status === "conflict") {
      issues.push({
        severity: "warning",
        code: "merge_conflict",
        message: mergeConflictIssueMessage(part.filename),
        link_hint: "build",
      });
    }
  }
  const projected = projectedResults.map(({ result }) => result.row);

  const grouped = new Map<string, AcceptedPlanReviewPart[]>();
  const sourceByFolder = new Map<string, string | null>();
  for (const part of projected) {
    const folder = folderKeyFromRelativePath(part.relative_path || part.filename);
    const rows = grouped.get(folder) ?? [];
    rows.push(part);
    grouped.set(folder, rows);
    if (!sourceByFolder.has(folder)) sourceByFolder.set(folder, part.source_layer);
  }
  const folders = [...grouped.keys()].sort((left, right) => {
    if (left === ROOT_FOLDER) return -1;
    if (right === ROOT_FOLDER) return 1;
    return left.localeCompare(right);
  });
  return {
    profile_id: input.snapshot.profile.id,
    accepted_basis: {
      profile_id: input.snapshot.profile.id,
      plan_version: input.snapshot.planVersion,
      plan_revision_id: input.snapshot.revisionId,
      plan_revision_digest: input.snapshot.revisionDigest,
      required_unit_mapping_digest: input.snapshot.requiredUnitMappingDigest,
    },
    plan_name: input.snapshot.profile.name,
    layers: layerResult.layers,
    totals: {
      included_parts: included.length,
      total_print_units: printUnits,
      by_role: byRole,
      by_filament: byFilament,
    },
    issues,
    has_blockers: issues.some((issue) => issue.severity === "blocker"),
    part_groups: folders.map((folder) => ({
      folder,
      source_layer: sourceByFolder.get(folder) ?? null,
      parts: grouped.get(folder) ?? [],
    })),
  };
}

function observeAcceptedPlanReview(input: {
  readonly snapshot: AcceptedPlanOperationalSnapshot;
  readonly reposDir: string;
  readonly thumbsDir: string | null;
}): {
  readonly availableInputRoots: ReadonlySet<number>;
  readonly mediaByPartId: ReadonlyMap<
    number,
    { readonly artifactMissing: boolean; readonly thumbEmpty: boolean }
  >;
} {
  const availableInputRoots = new Set<number>();
  const observeAcceptedArtifact = createAcceptedArtifactObserver();
  if (input.snapshot.provenance.kind === "tracked") {
    for (const acceptedInput of input.snapshot.provenance.inputs) {
      if (
        acceptedInput.trackingKind === "revision" &&
        observeAcceptedSnapshotRoot({
          reposDir: input.reposDir,
          snapshotRoot: acceptedInput.snapshotRoot,
        }).kind === "available"
      ) {
        availableInputRoots.add(acceptedInput.inputId);
      }
    }
  }
  const mediaByPartId = new Map<
    number,
    { readonly artifactMissing: boolean; readonly thumbEmpty: boolean }
  >();
  for (const part of input.snapshot.parts) {
    if (!part.included) continue;
    const artifactMissing =
      observeAcceptedArtifact({
        reposDir: input.reposDir,
        artifact: part.artifact,
      }).kind !== "available";
    let thumbEmpty = false;
    if (!artifactMissing && part.artifact.kind === "tracked") {
      const { basis } = acceptedPartMediaIdentity(part, "thumbnail");
      thumbEmpty =
        input.thumbsDir == null ||
        observeAcceptedMediaPng({ thumbsDir: input.thumbsDir, basis }).kind === "missing";
    }
    mediaByPartId.set(part.projectionPartId, { artifactMissing, thumbEmpty });
  }
  return { availableInputRoots, mediaByPartId };
}

export async function readAcceptedPlanReview(
  input: ReadAcceptedPlanReviewInput,
): Promise<ReadAcceptedPlanReviewResult> {
  const profile = input.repo.getOwnedProfileIdentity(input.profileId);
  if (!profile) return { kind: "not_found" };
  const started = performance.now();
  const accepted = input.repo.readAcceptedPlanOperationalSnapshot(input.profileId);
  const snapshotDone = performance.now();
  if (accepted.kind === "empty") {
    return { kind: "empty", body: emptyReview(input.profileId, profile.name) };
  }
  if (accepted.kind !== "ready") {
    return { kind: "accepted_state_unavailable", reason: accepted.kind };
  }
  const body = await projectCapturedPlanReview({
    ...input,
    snapshot: accepted.snapshot,
    reportTiming: input.reportTiming
      ? (timing) => input.reportTiming?.({ ...timing, snapshotMs: snapshotDone - started })
      : undefined,
  });
  return { kind: "ready", body };
}

export async function projectCapturedPlanReview(
  input: Pick<
    ReadAcceptedPlanReviewInput,
    "includeExcluded" | "reposDir" | "thumbsDir" | "loadFilamentContext" | "reportTiming"
  > & {
    readonly snapshot: AcceptedPlanOperationalSnapshot;
  },
): Promise<AcceptedPlanReviewBody> {
  const snapshotDone = performance.now();
  const filamentContext = input.loadFilamentContext
    ? await input.loadFilamentContext(input.snapshot.parts.map((part) => part.filamentColorId))
    : undefined;
  const filamentDone = performance.now();
  const observations = observeAcceptedPlanReview({
    snapshot: input.snapshot,
    reposDir: input.reposDir,
    thumbsDir: input.thumbsDir,
  });
  const observationDone = performance.now();
  const body = projectAcceptedPlanReview({
    snapshot: input.snapshot,
    includeExcluded: input.includeExcluded,
    ...observations,
    filamentContext,
  });
  input.reportTiming?.({
    snapshotMs: 0,
    filamentMs: filamentDone - snapshotDone,
    observationMs: observationDone - filamentDone,
    projectionMs: performance.now() - observationDone,
  });
  return body;
}
