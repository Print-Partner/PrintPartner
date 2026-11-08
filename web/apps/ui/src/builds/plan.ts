import type {
  AcceptDraftRequest,
  BuildHeaderView,
  DraftAcceptPreview,
  DraftHeaderView,
  DraftIdentity,
  DraftWorkspace,
  SaveDraftHeaderRequest,
  SavePlanChoicesRequest,
} from "@print-partner/rust-client";
import { CoreError } from "../core/client";

export type BuildHeader = BuildHeaderView["build"];
type PlanDraftBasis = SaveDraftHeaderRequest["expected_base"];
export type PlanDraftIdentity = NonNullable<SaveDraftHeaderRequest["expected_draft"]>;
export type PlanChoice = SavePlanChoicesRequest["decisions"][number];

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

/**
 * A refusal in words: the code's sentence for an Accept or a draft save, else the core's
 * detail, else the error.
 */
export function refusalText(
  error: Error,
  write: "preview" | "accept" | "save" = "preview",
): string {
  if (error instanceof CoreError) {
    const texts =
      write === "save" ? SAVE_REFUSALS : write === "accept" ? ACCEPT_REFUSALS : REFUSALS;
    return (error.code && texts[error.code]) || error.detail || error.message;
  }
  return error.message;
}
