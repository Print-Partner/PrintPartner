/**
 * The one way UI code reaches the Rust core. Requests stay on the page's own
 * origin: the Rust core serves this UI in the desktop app and Docker, and the
 * Vite dev server forwards core paths to PP_UI_CORE_URL.
 *
 * Seam: the generated Rust client package replaces hand-written calls built on
 * this function. Do not add typed endpoint wrappers here before it lands.
 */
export class CoreError extends Error {
  constructor(status: number, path: string) {
    super(`Rust core answered ${status} for ${path}`);
    this.name = "CoreError";
  }
}

// One leading slash only: "//host" and "/\host" are cross-origin in browsers. Tabs and
// newlines are refused too, because URL parsing strips them and can turn "/\n/host"
// into "//host".
const SAME_ORIGIN_PATH = /^\/(?![/\\])(?![\s\S]*[\t\r\n])/;

export async function fetchCore(path: string, init?: RequestInit): Promise<Response> {
  if (!SAME_ORIGIN_PATH.test(path)) {
    throw new TypeError(
      `Core requests take a same-origin path such as /health, got ${JSON.stringify(path)}`,
    );
  }
  const response = await fetch(path, { ...init, credentials: "same-origin" });
  if (!response.ok) throw new CoreError(response.status, path);
  return response;
}

/**
 * Resolves when the Rust core answers GET /health with JSON. A static server or
 * SPA fallback answers that path with index.html, which must not count.
 */
export async function checkCoreHealth(signal?: AbortSignal): Promise<void> {
  const response = await fetchCore("/health", { headers: { accept: "application/json" }, signal });
  if (!response.headers.get("content-type")?.includes("application/json")) {
    throw new CoreError(response.status, "/health");
  }
}
