import { matchRoutes, type RouteObject } from "react-router";
import { isSameOriginPath } from "../core/client";

/** Where the app goes after sign-in or setup when no usable `next` was given. */
export const HOME = "/today";
export const SIGN_IN = "/login";
export const SETUP = "/setup";

const BASE = "http://ui.invalid";

/**
 * The page to open after sign-in: `raw` when it is a same-origin path to a screen this
 * UI knows, otherwise Today. Anything else, such as `//host`, a full URL or an unknown
 * path, is ignored rather than followed.
 */
export function nextPath(raw: string | null, routes: RouteObject[]): string {
  if (!raw || !isSameOriginPath(raw) || raw.includes("\\")) return HOME;
  let url: URL;
  try {
    url = new URL(raw, BASE);
  } catch {
    return HOME;
  }
  if (url.origin !== BASE || url.pathname === SIGN_IN || url.pathname === SETUP) return HOME;
  const leaf = matchRoutes(routes, url.pathname)?.at(-1)?.route;
  if (!leaf || leaf.path === "*") return HOME;
  return `${url.pathname}${url.search}${url.hash}`;
}

/** The sign-in screen's address that returns to this page afterwards. */
export function signInFor({
  pathname,
  search = "",
  hash = "",
}: {
  pathname: string;
  search?: string;
  hash?: string;
}): string {
  if (pathname === "/" || pathname === SIGN_IN || pathname === SETUP) return SIGN_IN;
  return `${SIGN_IN}?${new URLSearchParams({ next: `${pathname}${search}${hash}` })}`;
}
