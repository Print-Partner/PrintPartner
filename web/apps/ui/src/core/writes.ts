import { CoreError } from "./client";
import { RequestContractError } from "./rust";

/**
 * A new idempotency key. `crypto.randomUUID` exists only in secure contexts, and a shared
 * core may be opened over plain HTTP on the LAN, so random bytes build the key there.
 */
export const newKey = () =>
  typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) =>
        b.toString(16).padStart(2, "0"),
      ).join("");

// A send that never reached the core, or whose answer was lost, is resent with the same key;
// the core answers a repeat with the first receipt and writes nothing. A refusal is not.
export const lostSend = (error: Error) =>
  !(error instanceof CoreError) && !(error instanceof RequestContractError);
export const resendable = (error: Error) =>
  !(error instanceof RequestContractError) && !(error instanceof CoreError && error.status < 500);
export const reason = (error: Error) =>
  (error instanceof CoreError && error.detail) || error.message;
