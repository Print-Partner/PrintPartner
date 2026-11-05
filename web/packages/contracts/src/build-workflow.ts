import { z } from "zod";

const positiveId = z.number().int().positive().max(Number.MAX_SAFE_INTEGER);
const nonnegativeCount = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const summary = z.string().min(1).max(500);
const actionLabel = z.string().min(1).max(120);
const actionReason = z.string().min(1).max(500);
const nullableCount = nonnegativeCount.nullable();
const stageIdSchema = z.enum(["sources", "plan", "production", "checkoff"]);

const buildWorkflowSourceStateSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("empty") }),
  z.strictObject({
    kind: z.literal("ready"),
    attached_count: nonnegativeCount,
  }),
  z.strictObject({
    kind: z.literal("stale"),
    attached_count: nonnegativeCount,
    issue_count: positiveId,
  }),
]);

export type BuildWorkflowSourceState = z.infer<
  typeof buildWorkflowSourceStateSchema
>;

const buildWorkflowAcceptedPlanSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("none") }),
  z.strictObject({
    kind: z.literal("ready"),
    revision_id: positiveId,
    plan_version: positiveId,
    total_units: nonnegativeCount,
    remaining_units: nonnegativeCount,
  }),
  z.strictObject({
    kind: z.literal("unavailable"),
    reason: summary,
  }),
]);

export type BuildWorkflowAcceptedPlan = z.infer<
  typeof buildWorkflowAcceptedPlanSchema
>;

const buildWorkflowWorkingPlanSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("none") }),
  z.strictObject({
    kind: z.literal("ready"),
    draft_id: positiveId,
    change_count: nonnegativeCount,
  }),
  z.strictObject({
    kind: z.enum(["needs_attention", "stale"]),
    draft_id: positiveId,
    change_count: nonnegativeCount,
    issue_count: positiveId,
  }),
]);

export type BuildWorkflowWorkingPlan = z.infer<
  typeof buildWorkflowWorkingPlanSchema
>;

const legacyStageStatusSchema = z.discriminatedUnion("kind", [
  z.strictObject({
    kind: z.enum(["not_started", "ready", "complete"]),
    summary,
  }),
  z.strictObject({
    kind: z.literal("in_progress"),
    summary,
    active_count: positiveId,
  }),
  z.strictObject({
    kind: z.enum(["needs_attention", "stale", "error"]),
    summary,
    task_count: positiveId,
  }),
]);

/** The Rust projection's stage status: every count is present, null when unused. */
const rustStageStatusSchema = z.strictObject({
  kind: z.enum([
    "not_started",
    "ready",
    "in_progress",
    "needs_attention",
    "stale",
    "complete",
    "error",
  ]),
  summary,
  task_count: nullableCount,
  active_count: nullableCount,
});

const buildWorkflowStageStatusSchema = z.union([
  legacyStageStatusSchema,
  rustStageStatusSchema,
]);

export type BuildWorkflowStageStatus = z.infer<
  typeof buildWorkflowStageStatusSchema
>;

function buildWorkflowStageSchema<
  Id extends "sources" | "plan" | "production" | "checkoff",
  Group extends "prepare" | "make",
>(id: Id, group: Group) {
  return z.strictObject({
    id: z.literal(id),
    group: z.literal(group),
    label: z.string().min(1).max(80),
    status: buildWorkflowStageStatusSchema,
  });
}

const buildWorkflowStagesSchema = z.tuple([
  buildWorkflowStageSchema("sources", "prepare"),
  buildWorkflowStageSchema("plan", "prepare"),
  buildWorkflowStageSchema("production", "make"),
  buildWorkflowStageSchema("checkoff", "make"),
]);

export type BuildWorkflowStage = z.infer<
  typeof buildWorkflowStagesSchema
>[number];

const actionBase = {
  label: actionLabel,
  reason: actionReason,
};

const legacyNextActionSchema = z.discriminatedUnion("kind", [
  z.strictObject({
    kind: z.literal("attach_sources"),
    stage_id: z.literal("sources"),
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("review_source_changes"),
    stage_id: z.literal("sources"),
    issue_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("create_working_plan"),
    stage_id: z.literal("plan"),
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("resolve_plan_issues"),
    stage_id: z.literal("plan"),
    draft_id: positiveId,
    issue_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("refresh_working_plan"),
    stage_id: z.literal("plan"),
    draft_id: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("accept_working_plan"),
    stage_id: z.literal("plan"),
    draft_id: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("review_plan_status"),
    stage_id: z.literal("plan"),
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("review_failed_prints"),
    stage_id: z.literal("checkoff"),
    item_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("recover_printer_jobs"),
    stage_id: z.literal("production"),
    item_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("verify_prints"),
    stage_id: z.literal("checkoff"),
    item_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("monitor_production"),
    stage_id: z.literal("production"),
    item_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("review_production_queue"),
    stage_id: z.literal("production"),
    item_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("prepare_production"),
    stage_id: z.literal("production"),
    unit_count: positiveId,
    ...actionBase,
  }),
  z.strictObject({
    kind: z.literal("view_completed_build"),
    stage_id: z.literal("checkoff"),
    ...actionBase,
  }),
]);

/**
 * The Rust projection's next action (GRE-1123): every count is present, null
 * when unused, and `target` names the Build tab and draft it opens.
 */
const rustNextActionSchema = z.strictObject({
  kind: z.enum([
    "review_failed_prints",
    "recover_printer_jobs",
    "verify_prints",
    "monitor_production",
    "review_production_queue",
    "review_plan_status",
    "attach_sources",
    "resolve_plan_issues",
    "refresh_working_plan",
    "review_working_plan",
    "review_source_changes",
    "create_working_plan",
    "resolve_stock_shortfall",
    "prepare_production",
    "check_off_manually",
    "view_completed_build",
  ]),
  stage_id: stageIdSchema,
  label: actionLabel,
  reason: actionReason,
  target: z.strictObject({
    plan_id: positiveId,
    tab: stageIdSchema,
    draft_id: positiveId.nullable(),
  }),
  item_count: nullableCount,
  draft_id: positiveId.nullable(),
  issue_count: nullableCount,
  unit_count: nullableCount,
});

const buildWorkflowNextActionSchema = z.union([
  legacyNextActionSchema,
  rustNextActionSchema,
]);

export type BuildWorkflowNextAction = z.infer<
  typeof buildWorkflowNextActionSchema
>;

const buildWorkflowWorkspaceSchema = z.strictObject({
  build: z.strictObject({
    id: positiveId,
    name: z.string().min(1).max(500),
  }),
  sources: buildWorkflowSourceStateSchema,
  accepted_plan: buildWorkflowAcceptedPlanSchema,
  working_plan: buildWorkflowWorkingPlanSchema,
  /** Only the Rust projection reports the separate stock shortfall input. */
  stock: z
    .discriminatedUnion("kind", [
      z.strictObject({ kind: z.literal("not_ready") }),
      z.strictObject({ kind: z.literal("ready"), shortfalls: nonnegativeCount }),
    ])
    .optional(),
  stages: buildWorkflowStagesSchema,
  next_action: buildWorkflowNextActionSchema,
  active_work: z.strictObject({
    queued_jobs: nonnegativeCount,
    sending_jobs: nonnegativeCount,
    printing_jobs: nonnegativeCount,
    failed_jobs: nonnegativeCount,
    awaiting_verification: nonnegativeCount,
    failed_verifications: nonnegativeCount,
    total_units: nonnegativeCount,
    remaining_units: nonnegativeCount,
  }),
});

export type BuildWorkflowWorkspace = z.infer<
  typeof buildWorkflowWorkspaceSchema
>;

export function parseBuildWorkflowWorkspace(
  value: unknown,
): BuildWorkflowWorkspace {
  return buildWorkflowWorkspaceSchema.parse(value);
}
