import type { BuildWorkflow, TaskFeed } from "@print-partner/rust-client";
import { readCore } from "./rust";

/**
 * Next actions and open tasks come from the shared Rust workflow projection
 * (GRE-1123), the same read Today and MCP use, so this UI never ranks work itself.
 */
export type Task = TaskFeed["tasks"][number];

/** `GET /api/v2/tasks`: each open Build's next action, most urgent first. */
export const tasksQuery = {
  queryKey: ["core", "tasks"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listTasks", {}, signal),
} as const;

/** `GET /api/v2/plans/{id}/workflow`: one Build's stages and next action. */
export const buildWorkflowQuery = (id: number) => ({
  queryKey: ["core", "workflow", id],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("readBuildWorkflow", { id }, signal),
});

/** Open decisions across a Build's stages: the sum of each stage's `task_count`. */
export function openDecisionCount(workflow: BuildWorkflow): number {
  return workflow.stages.reduce((sum, stage) => sum + (stage.status.task_count ?? 0), 0);
}

/** The feed's task for each Build. A Build with no entry has nothing open. */
export function tasksByBuild(feed: TaskFeed): ReadonlyMap<number, Task> {
  return new Map(feed.tasks.map((task) => [task.plan_id, task]));
}
