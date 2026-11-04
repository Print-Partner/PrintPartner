/**
 * The per-Build next action and open-task count. They come from the shared Rust
 * workflow projection (GRE-1123), the same read Today and MCP use, so this UI
 * never ranks work itself.
 *
 * Until that route is in the generated client, readBuildWorkflows makes no
 * request and resolves "pending". Swap its body for the generated call when the
 * route lands; the screens already render both kinds.
 */
export interface BuildWorkflow {
  openTasks: number;
  nextAction: { label: string; reason: string; target: string } | null;
}

export type BuildWorkflows =
  | { kind: "pending" }
  | { kind: "ready"; byBuild: ReadonlyMap<number, BuildWorkflow> };

export const WORKFLOW_PENDING_TEXT = "Not available yet";
export const WORKFLOW_PENDING_NOTE =
  "Open tasks and next actions are not available from the core yet. They will come from the shared workflow projection.";

export function readBuildWorkflows(): Promise<BuildWorkflows> {
  return Promise.resolve({ kind: "pending" });
}

export const workflowQuery = {
  queryKey: ["core", "workflow"],
  queryFn: readBuildWorkflows,
} as const;
