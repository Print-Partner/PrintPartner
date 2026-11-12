import type { IncomingMessage } from "node:http";
import type { ProxyOptions } from "vite";

/** Names the Rust core that `npm run dev` forwards to, as an http(s) origin. */
export const CORE_URL_ENV = "PP_UI_CORE_URL";

/** Path prefixes the dev server forwards to the Rust core. Vite serves the rest. */
export const CORE_PATHS = ["/api", "/ws", "/health", "/auth"] as const;

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
  const configure = originBridge(url.origin);
  return Object.fromEntries(
    CORE_PATHS.map((prefix) => [
      prefix,
      { target: url.origin, changeOrigin: true, ws: prefix === "/ws", configure },
    ]),
  );
}

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "[::1]"]);

/**
 * Whether origin is this dev server's own page: an http(s) loopback origin whose
 * host and port equal the Host the browser sent to the dev server. A page on any
 * other port (another local service) does not qualify.
 */
function isDevServerOrigin(origin: string, host: string | undefined): boolean {
  if (!host) return false;
  try {
    const parsed = new URL(origin);
    return (
      (parsed.protocol === "http:" || parsed.protocol === "https:") &&
      parsed.origin === origin &&
      LOOPBACK_HOSTS.has(parsed.hostname) &&
      parsed.host === host
    );
  } catch {
    return false;
  }
}

/**
 * The Rust core admits only its own Origin (pp-api loopback checks), and
 * changeOrigin rewrites Host but not Origin. For the dev server's own page,
 * present the core's origin upstream. Any other Origin, including a loopback
 * page on another port, is forwarded unchanged, so the core still rejects it.
 */
export function originBridge(coreOrigin: string): NonNullable<ProxyOptions["configure"]> {
  return (proxy) => {
    const adapt = (proxyReq: { setHeader(name: string, value: string): void }, req: IncomingMessage) => {
      const origin = req.headers.origin;
      if (typeof origin === "string" && isDevServerOrigin(origin, req.headers.host)) {
        proxyReq.setHeader("origin", coreOrigin);
      }
    };
    proxy.on("proxyReq", adapt);
    proxy.on("proxyReqWs", adapt);
  };
}
