import { matchRoutes, type RouteObject } from "react-router";
import { isSameOriginPath } from "../core/client";

/** Where the app goes after sign-in or setup when no usable `next` was given. */
export const HOME = "/today";
export const SIGN_IN = "/login";
export const SETUP = "/setup";
export const FORGOT_PASSWORD = "/forgot-password";
export const RESET_PASSWORD = "/reset-password";

const BASE = "http://ui.invalid";

/** Public account screens are never post-sign-in destinations. */
export const isAuthPath = (pathname: string) =>
  [SIGN_IN, SETUP, FORGOT_PASSWORD, RESET_PASSWORD].includes(
    pathname.toLowerCase().replace(/\/+$/, ""),
  );

/**
 * The page to open after sign-in: `raw` when it is a same-origin path to a screen this
 * UI knows, otherwise Today. Anything else, such as `//host`, a full URL or an unknown
 * path, is ignored rather than followed. The check runs on the parsed path too, since
 * parsing turns `/.//` into `//`.
 */
export function nextPath(raw: string | null, routes: RouteObject[]): string {
  if (!raw || !isSameOriginPath(raw) || raw.includes("\\")) return HOME;
  let url: URL;
  try {
    url = new URL(raw, BASE);
  } catch {
    return HOME;
  }
  const path = `${url.pathname}${url.search}${url.hash}`;
  if (url.origin !== BASE || !isSameOriginPath(path) || isAuthPath(url.pathname)) return HOME;
  const leaf = matchRoutes(routes, url.pathname)?.at(-1)?.route;
  if (!leaf || leaf.path === "*" || (leaf.path && isAuthPath(leaf.path))) return HOME;
  return path;
}

export function withNext(pathname: string, next?: string): string {
  return next ? `${pathname}?${new URLSearchParams({ next })}` : pathname;
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
  if (pathname === "/" || isAuthPath(pathname)) return SIGN_IN;
  return withNext(SIGN_IN, `${pathname}${search}${hash}`);
}
