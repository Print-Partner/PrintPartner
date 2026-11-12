import { CoreError } from "../core/client";
import { ContractError, RequestContractError } from "../core/rust";

export const UNREACHABLE =
  "Print Partner can't be reached. Check that it is running, then try again.";
export const TOO_MANY = "Too many attempts. Try again in a minute.";
export const WRONG = "That email and password don't match. Try again.";
export const LINK_ENDED =
  "This browser is no longer connected to Print Partner. Open a new sign-in link from Print Partner.";
export const OWNER_EXISTS = "Print Partner already has an owner account. Sign in instead.";
export const MISMATCH = "The two passwords don't match.";
export const BUSY = "Print Partner is busy or stopping. Try again in a moment.";

/** No answer at all, or a proxy that says the core behind it is gone. */
export function unreachable(error: unknown): boolean {
  if (error instanceof CoreError) return error.status === 502 || error.status === 504;
  return !(error instanceof ContractError) && !(error instanceof RequestContractError);
}

/** A running core that cannot take the request now: a full queue, or it is stopping. */
const busy = (error: unknown) => error instanceof CoreError && error.status === 503;

/** What the sign-in screen says about a failed sign-in. It names no account detail. */
export function signInProblem(error: unknown): string {
  if (unreachable(error)) return UNREACHABLE;
  if (busy(error)) return BUSY;
  if (error instanceof CoreError && error.status === 401) return WRONG;
  if (error instanceof CoreError && error.status === 429) return TOO_MANY;
  return "Print Partner could not sign you in. Try again.";
}

/** What the setup screen says about a failed account creation. */
export function setupProblem(error: unknown): string {
  if (unreachable(error)) return UNREACHABLE;
  if (busy(error)) return BUSY;
  if (error instanceof CoreError && error.status === 429) return TOO_MANY;
  if (error instanceof CoreError && (error.status === 403 || error.status === 409)) {
    return OWNER_EXISTS;
  }
  // The core's own reason, such as a password that is too short.
  if (error instanceof CoreError && error.status === 400 && error.detail) return error.detail;
  return "Print Partner could not create the account. Try again.";
}

/** What either screen says when it cannot read whether setup is needed. */
export function statusProblem(error: unknown): string {
  if (error instanceof CoreError && error.status === 401) return LINK_ENDED;
  if (unreachable(error)) return UNREACHABLE;
  if (busy(error)) return BUSY;
  return "Print Partner answered in a way this screen does not understand. Try again.";
}
