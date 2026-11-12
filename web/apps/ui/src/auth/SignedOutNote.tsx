import { Link, useLocation } from "react-router";
import { CoreError } from "../core/client";
import { signInFor } from "./next";

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
      You were signed out. <Link to={signInFor(location)}>Sign in to save.</Link>
    </>
  );
}
