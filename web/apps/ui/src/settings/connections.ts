import { readCore } from "../core/rust";

/** `GET /api/v2/integrations`: every connection, without its secrets. */
export const integrationsQuery = {
  queryKey: ["core", "integrations"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listIntegrations", {}, signal),
} as const;

/** A stored address as the screen may show it. `text` is null when it can't be shown. */
export interface ShownAddress {
  text: string | null;
  /** The stored address carried a user name, password, query or fragment, now left out. */
  hidden: boolean;
}

/**
 * The address to show for a stored `base_url`, or null when there is none. The core already
 * leaves out a user name, password, query and fragment; a row stored before that check, or a
 * core that answers one anyway, must still never put a password on screen. So this keeps only
 * the scheme, host, port and path, and shows text that is not a URL as nothing.
 */
export function shownAddress(stored: string | null): ShownAddress | null {
  if (stored === null) return null;
  let url: URL;
  try {
    url = new URL(stored);
  } catch {
    return { text: null, hidden: false };
  }
  if (url.username === "" && url.password === "" && url.search === "" && url.hash === "" && !stored.includes("@")) {
    return { text: stored, hidden: false };
  }
  return { text: `${url.protocol}//${url.host}${url.pathname}`, hidden: true };
}

export const HIDDEN_NOTE = "Sign-in details stored in this address are hidden.";
export const UNSHOWN_ADDRESS = "The stored address can't be shown.";
