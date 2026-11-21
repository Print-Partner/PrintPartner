import { CoreError } from "../core/client";
import { RequestContractError } from "../core/rust";

/** What one restore request got back, reduced to what decides whether it may be sent again. */
export type RestoreAnswer =
  /** The request was refused in this page and never left it. */
  | { kind: "not-sent" }
  /** The request left, and no answer the page could read came back. */
  | { kind: "no-answer" }
  /**
   * A non-2xx answer. `problem` is true only for the core's own problem body: read whole,
   * `application/problem+json`, valid against the generated `Problem` schema, and its `status`
   * equal to the answer's. A `code` that is present but not a string fails that schema.
   */
  | { kind: "answered"; status: number; problem: boolean; code?: string };

/**
 * `unchanged`: the core refused before the restore was queued; the same check may be sent
 * again. `choose-again`: unchanged, but the core no longer holds the check. `unknown`: the
 * installation may have been replaced; nothing is sent again until the page reloads.
 */
export type RestoreVerdict = "unchanged" | "choose-again" | "unknown";

/**
 * Code-less statuses the restore route and its gate decide before the restore is queued:
 * 400 an unreadable or invalid body or a wrong confirmation, 401 no or a bad credential,
 * 403 Host, Origin or owner mapping, 409 an installation without the Rust marker, 422 a staged
 * database this release cannot open. No answer after the queue push is one of these.
 */
const UNCHANGED_STATUSES: ReadonlySet<number> = new Set([400, 401, 403, 409, 422]);

/** Coded answers that prove nothing changed, each only with the status the core sends it with. */
const CODED: ReadonlyMap<string, { status: number; verdict: Exclude<RestoreVerdict, "unknown"> }> = new Map([
  // The stage id is unknown; checked before the queue push.
  ["restore_not_found", { status: 404, verdict: "choose-again" }],
  // The route, the activity gate, or the writer before it swaps any file (readers did not drain).
  ["restore_busy", { status: 503, verdict: "unchanged" }],
  // The writer put the previous installation back and removed the stage.
  ["restore_rolled_back", { status: 500, verdict: "choose-again" }],
]);

/**
 * The one decision on a failed restore. Only the answers above are a refusal. Everything else is
 * `unknown`: every 1xx, 2xx, 3xx, other 4xx and 5xx, a body that is not the core's problem
 * (including one whose `code` is `null`, a number or an object), an empty or unknown code string,
 * a code on the wrong status, and a request that left without a readable answer.
 */
export function restoreVerdict(answer: RestoreAnswer): RestoreVerdict {
  if (answer.kind === "not-sent") return "unchanged";
  if (answer.kind === "no-answer" || !answer.problem) return "unknown";
  if (answer.code === undefined) return UNCHANGED_STATUSES.has(answer.status) ? "unchanged" : "unknown";
  const coded = CODED.get(answer.code);
  return coded !== undefined && coded.status === answer.status ? coded.verdict : "unknown";
}

/** Reads what the restore request threw as a `RestoreAnswer`. */
export function restoreAnswer(failure: unknown): RestoreAnswer {
  if (failure instanceof RequestContractError) return { kind: "not-sent" };
  if (failure instanceof CoreError) {
    return { kind: "answered", status: failure.status, problem: failure.problem, code: failure.code };
  }
  // A network error, an abort, a refused redirect, or a 2xx body that fails the contract.
  return { kind: "no-answer" };
}
