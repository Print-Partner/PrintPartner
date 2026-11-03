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
  const configure = originBridge(url.origin);
  return Object.fromEntries(
    CORE_PATHS.map((prefix) => [
      prefix,
      { target: url.origin, changeOrigin: true, ws: prefix === "/ws", configure },
    ]),
  );
}

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "[::1]"]);

/** Whether origin is the dev server's own kind of origin: http(s) on loopback. */
export function isLoopbackOrigin(origin: string): boolean {
  try {
    const parsed = new URL(origin);
    return (
      (parsed.protocol === "http:" || parsed.protocol === "https:") &&
      LOOPBACK_HOSTS.has(parsed.hostname) &&
      parsed.origin === origin
    );
  } catch {
    return false;
  }
}

type ProxyRequest = { setHeader(name: string, value: string): void };
type IncomingRequest = { headers: Record<string, string | string[] | undefined> };
type ProxyEvents = {
  on(event: "proxyReq" | "proxyReqWs", listener: (proxyReq: ProxyRequest, req: IncomingRequest) => void): void;
};

/**
 * The Rust core admits only its own Origin (pp-api loopback checks), and
 * changeOrigin rewrites Host but not Origin. For a browser on a loopback dev
 * server, present the core's origin upstream. Any other Origin is forwarded
 * unchanged, so the core still rejects it.
 */
export function originBridge(coreOrigin: string) {
  return (proxy: ProxyEvents) => {
    const adapt = (proxyReq: ProxyRequest, req: IncomingRequest) => {
      const origin = req.headers.origin;
      if (typeof origin === "string" && isLoopbackOrigin(origin)) {
        proxyReq.setHeader("origin", coreOrigin);
      }
    };
    proxy.on("proxyReq", adapt);
    proxy.on("proxyReqWs", adapt);
  };
}
