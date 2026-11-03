import type { ProxyOptions } from "vite";

/** Names the Rust core that `npm run dev` forwards to, as an http(s) origin. */
export const CORE_URL_ENV = "PP_UI_CORE_URL";

/** Path prefixes the dev server forwards to the Rust core. Vite serves the rest. */
export const CORE_PATHS = ["/api", "/ws", "/health"] as const;

/**
 * Builds the dev server proxy. There is no default target: without
 * PP_UI_CORE_URL the dev server forwards nothing, so it can never reach the
 * Node server by accident.
 */
export function coreProxy(env: Record<string, string | undefined>): Record<string, ProxyOptions> {
  const raw = env[CORE_URL_ENV]?.trim();
  if (!raw) return {};
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new Error(`${CORE_URL_ENV} must be an http(s) origin, got ${JSON.stringify(raw)}`);
  }
  const originOnly =
    url.pathname === "/" && !url.search && !url.hash && !url.username && !url.password;
  if ((url.protocol !== "http:" && url.protocol !== "https:") || !originOnly) {
    throw new Error(`${CORE_URL_ENV} must be an http(s) origin, got ${JSON.stringify(raw)}`);
  }
  return Object.fromEntries(
    CORE_PATHS.map((prefix) => [
      prefix,
      { target: url.origin, changeOrigin: true, ws: prefix === "/ws" },
    ]),
  );
}
