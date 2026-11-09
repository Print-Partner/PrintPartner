import type {
  AcceptDraftRequest,
  BuildHeaderView,
  DraftAcceptPreview,
  DraftHeaderView,
  DraftIdentity,
  DraftWorkspace,
  ReconcileDraftRequest,
  SaveDraftHeaderRequest,
  SavePlanChoicesRequest,
} from "@print-partner/rust-client";
import { CoreError } from "../core/client";

export type BuildHeader = BuildHeaderView["build"];
type PlanDraftBasis = SaveDraftHeaderRequest["expected_base"];
export type PlanDraftIdentity = NonNullable<SaveDraftHeaderRequest["expected_draft"]>;
export type PlanChoice = SavePlanChoicesRequest["decisions"][number];
export type UnitDecision = ReconcileDraftRequest["decisions"][number];
export type UnitConflict = Extract<
  DraftWorkspace["reconciliation"],
  { kind: "unresolved" }
>["conflicts"][number];

/** What the Plan tab reads: the Build's header, the Accepted base, and the open draft. */
export interface PlanData {
  build: BuildHeader;
  base: PlanDraftBasis;
  draft: { workspace: DraftWorkspace; header: DraftHeaderView["draft"] } | null;
}

export type PartView = DraftWorkspace["parts"][number];
export type PreviewChanges = Extract<DraftAcceptPreview, { kind: "changed" }>["changes"];

/** The header as the form edits it: every field is text, and blank means none. */
export interface HeaderFields {
  name: string;
  order_number: string;
  customer: string;
  due_date: string;
}

export const fieldsOf = (header: BuildHeader): HeaderFields => ({
  name: header.name,
  order_number: header.order_number ?? "",
  customer: header.customer ?? "",
  due_date: header.due_date ?? "",
});

const orNull = (value: string) => (value.trim() === "" ? null : value.trim());

/** The header a save sends: trimmed, with blank optional fields as null. */
export const headerOf = (fields: HeaderFields): BuildHeader => ({
  name: fields.name.trim(),
  order_number: orNull(fields.order_number),
  customer: orNull(fields.customer),
  due_date: orNull(fields.due_date),
});

export const sameHeader = (a: BuildHeader, b: BuildHeader) =>
  a.name === b.name &&
  a.order_number === b.order_number &&
  a.customer === b.customer &&
  a.due_date === b.due_date;

/** The open draft as a write names it, so a stale write is refused instead of applied. */
export const identityOf = (draft: DraftIdentity): PlanDraftIdentity => ({
  draft_id: draft.draft_id,
  state: "open",
  lifecycle_version: draft.lifecycle_version,
  snapshot_digest: draft.snapshot_digest,
  base: draft.base,
});

const targetOf = (part: PartView) => ({
  part_key: part.part_key,
  relative_path: part.relative_path,
  source_layer: part.source_layer,
});

export const includeChoice = (part: PartView, value: boolean): PlanChoice => ({
  kind: "set_included",
  target: targetOf(part),
  value,
});

/** A quantity override from the field: blank clears it, otherwise 1 to 10,000 or null. */
export function quantityValue(text: string): number | null | "invalid" {
  if (text.trim() === "") return null;
  if (!/^\d+$/.test(text.trim())) return "invalid";
  const value = Number(text.trim());
  return value >= 1 && value <= 10_000 ? value : "invalid";
}

export const quantityChoice = (part: PartView, value: number | null): PlanChoice => ({
  kind: "set_quantity_override",
  target: targetOf(part),
  value,
});

/** Accept's body is the previewed revision, so Accept refuses anything that moved since. */
export function acceptBody(preview: DraftAcceptPreview): AcceptDraftRequest {
  const revision = preview.revision;
  return {
    expected_snapshot_digest: revision.expected_snapshot_digest,
    expected_lifecycle_version: revision.expected_lifecycle_version,
    expected_base: revision.expected_base,
    expected_header_version: revision.expected_header_version,
    remap_checkoff_links: false,
  };
}

/**
 * The Required-unit conflicts the user must decide. A draft with no selected reconciliation
 * reports `unresolved` with no conflicts, and so does an empty list: both settle with the
 * core's automatic selection.
 */
export const unitConflicts = (workspace: DraftWorkspace): UnitConflict[] =>
  workspace.reconciliation.kind === "unresolved" ? workspace.reconciliation.conflicts : [];

/** A draft Part's name by its ID, for a conflict's target. */
export function draftPartName(workspace: DraftWorkspace, id: number): string {
  return workspace.parts.find((p) => p.draft_part_id === id)?.filename ?? `Part ${id}`;
}

/** An accepted Part's name by its revision Part ID, from what the workspace shows of the base. */
export function acceptedPartName(workspace: DraftWorkspace, id: number): string {
  const { changed, removed } = workspace.diff;
  return (
    removed.find((p) => p.revision_part_id === id)?.filename ??
    changed.find((c) => c.before.revision_part_id === id)?.before.filename ??
    workspace.parts.find((p) => p.base_revision_part_id === id)?.filename ??
    `accepted Part ${id}`
  );
}

/** What the user can pick for one conflict, each as the decision it sends. */
export function conflictOptions(
  workspace: DraftWorkspace,
  conflict: UnitConflict,
): { label: string; decision: UnitDecision }[] {
  const target = conflict.target_draft_part_id;
  const replace = {
    label: "Start with new units. Units already printed for it do not count.",
    decision: { kind: "replace" as const, target_draft_part_id: target },
  };
  switch (conflict.kind) {
    case "ambiguous_exact_match":
      return [
        ...conflict.candidate_revision_part_ids.map((id) => ({
          label: `Keep the printed units of the accepted ${acceptedPartName(workspace, id)} (Part ${id}).`,
          decision: {
            kind: "select_exact_predecessor" as const,
            target_draft_part_id: target,
            predecessor_revision_part_id: id,
          },
        })),
        replace,
      ];
    case "unsafe_predecessor":
      return [
        {
          label: `Keep the units already printed for the accepted ${acceptedPartName(workspace, conflict.predecessor_revision_part_id)}.`,
          decision: {
            kind: "accept_prior_completion",
            target_draft_part_id: target,
            predecessor_revision_part_id: conflict.predecessor_revision_part_id,
          },
        },
        replace,
      ];
    case "predecessor_claimed":
      return [];
  }
}

/** One conflict as a question in plain words. */
export function conflictQuestion(workspace: DraftWorkspace, conflict: UnitConflict): string {
  const target = draftPartName(workspace, conflict.target_draft_part_id);
  switch (conflict.kind) {
    case "ambiguous_exact_match":
      return `${target} matches more than one Part in the accepted Plan. Which printed units does it keep?`;
    case "unsafe_predecessor":
      return `${target} changed since it was accepted, so the units printed for it may no longer fit. Keep them?`;
    case "predecessor_claimed":
      return `${target} and another Part both use the printed units of the accepted ${acceptedPartName(workspace, conflict.predecessor_revision_part_id)}. Only one can keep them. Start the match over to choose again, or change the Parts.`;
  }
}

const FIELD_NAMES: Record<string, string> = {
  name: "Name",
  order_number: "Order number",
  customer: "Customer",
  due_date: "Due date",
  included: "Included",
  quantity_override: "Quantity",
  quantity_effective: "Quantity",
  role_override: "Role",
  status: "Status",
};

export const fieldName = (field: string) => FIELD_NAMES[field] ?? field.replaceAll("_", " ");

type FieldValue = PreviewChanges["fields"][number]["new"];

export function valueText(value: FieldValue | string | null): string {
  if (value === null) return "none";
  if (typeof value === "string") return value;
  if (value.kind === "flag") return value.value ? "yes" : "no";
  return String(value.value);
}

const REFUSALS: Record<string, string> = {
  draft_changed: "The draft changed after this preview. Review the new preview before accepting.",
  base_changed:
    "The accepted Plan changed since this draft was made, so this draft cannot be accepted.",
  header_changed: "The header was changed somewhere else. The latest values are shown.",
  duplicate_build_name: "Another Build already has this name. Choose a different name.",
  duplicate_order:
    "Another Build already holds this order number. Change the order number, or archive the other Build first.",
  build_archived: "This Build is archived, so its Plan cannot change.",
  draft_not_found: "This draft no longer exists.",
  idempotency_conflict: "A different change was already sent with this request. Try again.",
  outcome_unknown: "The core could not answer for certain. Check again.",
  already_applied: "This draft was already accepted.",
  reconciliation_required:
    "Accept needs the printed units matched to this draft first. Match them, then check again.",
};

/** Accept keeps its key after an unknown outcome, so pressing it again replays the first answer. */
const ACCEPT_REFUSALS: Record<string, string> = {
  ...REFUSALS,
  outcome_unknown:
    "The core could not confirm whether Accept went through. Press Accept again to check; it will not accept twice.",
};

/** Draft saves preview nothing, so their stale refusals say what to redo instead. */
const SAVE_REFUSALS: Record<string, string> = {
  ...REFUSALS,
  draft_changed:
    "The draft changed somewhere else. The latest draft is shown; make the change again.",
  base_changed:
    "The accepted Plan changed since this draft was made, so this draft takes no more changes.",
  outcome_unknown:
    "The core could not confirm this save. Check the field, and make the change again if it is missing.",
};

/** A match of printed units keeps its key after an unknown outcome, as Accept does. */
const MATCH_REFUSALS: Record<string, string> = {
  ...REFUSALS,
  draft_changed: "The draft changed somewhere else. The latest draft is shown; match again.",
  outcome_unknown:
    "The core could not confirm the match. Send it again to check; it will not match twice.",
};

/**
 * A refusal in words: the code's sentence for an Accept or a draft save, else the core's
 * detail, else the error.
 */
export function refusalText(
  error: Error,
  write: "preview" | "accept" | "save" | "match" = "preview",
): string {
  if (error instanceof CoreError) {
    const texts = {
      preview: REFUSALS,
      accept: ACCEPT_REFUSALS,
      save: SAVE_REFUSALS,
      match: MATCH_REFUSALS,
    }[write];
    return (error.code && texts[error.code]) || error.detail || error.message;
  }
  return error.message;
}
