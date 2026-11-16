import { CoreError } from "../core/client";

/** The failure text for this Source in `result.failures`, when that list names it. */
export function syncFailure(sourceId: number, result: unknown): string | null {
  if (typeof result !== "object" || result === null || !("failures" in result)) return null;
  const { failures } = result;
  if (!Array.isArray(failures)) return null;
  for (const item of failures) {
    if (typeof item !== "object" || item === null) continue;
    const failure = item as { project_id?: unknown; error?: unknown };
    if (failure.project_id === sourceId && typeof failure.error === "string" && failure.error) {
      return failure.error;
    }
  }
  return null;
}

/** Why a sync post was refused or not confirmed. A lost answer is not sent again. */
export function syncStartFailure(error: unknown): string {
  if (error instanceof CoreError) return error.detail ?? error.message;
  const message = error instanceof Error ? error.message : "The sync was not confirmed";
  return `${message} It was not sent again.`;
}
