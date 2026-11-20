import { useSyncExternalStore } from "react";

/**
 * Each `setBuildBase` the UI sent and has not seen confirmed or refused by the core. One is
 * recorded at the press, before the request leaves; a 5xx, a redirect, a lost answer or any
 * answer that is not the core's own refusal keeps it, because the core may still apply that
 * send and a second one would replace one base with the other.
 *
 * The record is module state, not query-client state: sign-in and setup call
 * `queryClient.clear()` and sign-out removes every query but `auth`, and none of them may
 * forget a send. Only a page reload does. Keys carry the page's origin with the Build id.
 */
export interface UnconfirmedSend {
  id: number;
  sourceId: number;
  /** Why it is unconfirmed; null while it has no answer yet. */
  reason: string | null;
}

const NONE: readonly UnconfirmedSend[] = [];
const sends = new Map<string, readonly UnconfirmedSend[]>();
/** The core's last refusal per Build, kept for whichever form mounts next. */
const refusals = new Map<string, string>();
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((listener) => listener());
let nextId = 1;

const key = (buildId: number) => `${globalThis.location?.origin ?? ""} ${buildId}`;

function write(buildId: number, next: readonly UnconfirmedSend[]) {
  if (next.length === 0) sends.delete(key(buildId));
  else sends.set(key(buildId), next);
  notify();
}

export function unconfirmedSends(buildId: number): readonly UnconfirmedSend[] {
  return sends.get(key(buildId)) ?? NONE;
}

/** Records a send at the press and returns its id. */
export function recordSend(buildId: number, sourceId: number): number {
  const id = nextId++;
  write(buildId, [...unconfirmedSends(buildId), { id, sourceId, reason: null }]);
  return id;
}

/** The send's answer left it unconfirmed. */
export function sendUnconfirmed(buildId: number, id: number, reason: string) {
  write(
    buildId,
    unconfirmedSends(buildId).map((send) => (send.id === id ? { ...send, reason } : send)),
  );
}

/** The core refused this send, so it wrote nothing. Earlier unconfirmed sends stay. */
export function sendRefused(buildId: number, id: number, reason: string) {
  refusals.set(key(buildId), reason);
  write(
    buildId,
    unconfirmedSends(buildId).filter((send) => send.id !== id),
  );
}

/** The Build has a base: a send was confirmed, or a read shows one. */
export function baseSeen(buildId: number) {
  clearRefusal(buildId);
  if (unconfirmedSends(buildId).length > 0) write(buildId, NONE);
}

/** A new choice, a new press, or a base read ends the refusal. */
export function clearRefusal(buildId: number) {
  if (refusals.delete(key(buildId))) notify();
}

export function useRefusal(buildId: number): string {
  return useSyncExternalStore(subscribe, () => refusals.get(key(buildId)) ?? "");
}

/** Tests start each case with no sends. */
export function forgetAllSends() {
  sends.clear();
  refusals.clear();
  notify();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export function useUnconfirmedSends(buildId: number): readonly UnconfirmedSend[] {
  return useSyncExternalStore(subscribe, () => unconfirmedSends(buildId));
}
