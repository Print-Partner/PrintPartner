import { Link, useLocation } from "react-router";
import { CoreError } from "../core/client";
import { signInFor } from "./next";

/**
 * Mutation `meta` for a write whose edit lives only on screen. When it answers 401 the app
 * stays on the page instead of opening sign-in, and the screen shows `SignedOutNote`.
 */
export const KEEP_EDITS = { keepEditsOnSignOut: true };

/** Whether a mutation opted in with `KEEP_EDITS`. */
export const keepsEdits = (meta: Record<string, unknown> | undefined) =>
  meta?.keepEditsOnSignOut === true;

/** A write the core refused because the session has ended. */
export const signedOut = (error: unknown) => error instanceof CoreError && error.status === 401;

/**
 * Shown where a write failed with 401. The page stays, so the unsaved edit stays on
 * screen; the link leaves for sign-in only when the owner chooses to.
 */
export function SignedOutNote() {
  const location = useLocation();
  return (
    <>
      You were signed out, and this change is not saved. Copy it before you{" "}
      <Link to={signInFor(location)}>sign in</Link>.
    </>
  );
}
