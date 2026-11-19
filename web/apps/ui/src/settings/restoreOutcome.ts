import { useSyncExternalStore } from "react";

/** What the restore request became. `refused` left this installation unchanged. */
export type Restore =
  | { kind: "restoring" }
  | { kind: "done" }
  | { kind: "refused"; text: string; chooseAgain?: true }
  | { kind: "lost"; text: string };

/**
 * The restore outcome for this page load. It lives outside React, so leaving Settings and
 * coming back keeps it, and a restore still running when Settings closes records its answer
 * here. Only a reload clears a sent restore: once one leaves, restore and Create backup stay
 * locked until the page loads again from the server.
 */
let current: Restore | null = null;
const listeners = new Set<() => void>();

/** True once a restore has left and the core did not refuse it. */
export const restoreSent = (restore: Restore | null = current) =>
  restore !== null && restore.kind !== "refused";

function publish(next: Restore | null): void {
  current = next;
  for (const listener of listeners) listener();
}

/**
 * Records the restore's state. A `done` or `lost` outcome is final for this page load: later
 * calls are ignored, so no screen can unlock restore without a reload.
 */
export function setRestore(next: Restore | null): void {
  if (current?.kind === "done" || current?.kind === "lost") return;
  publish(next);
}

/** The same value as `current`, read synchronously so a second click cannot send twice. */
export const currentRestore = () => current;

export function useRestore(): Restore | null {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
    () => current,
  );
}

/** Tests only: a reload does this in the app. */
export function forgetRestore(): void {
  publish(null);
}
