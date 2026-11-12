/**
 * The one way UI code reaches the Rust core. Requests stay on the page's own
 * origin: the Rust core serves this UI in the desktop app and Docker, and the
 * Vite dev server forwards core paths to PP_UI_CORE_URL.
 *
 * Typed reads go through `readCore` in ./rust.ts, which builds on this function
 * with the generated client's routes and schemas.
 */
export class CoreError extends Error {
  constructor(
    readonly status: number,
    path: string,
    readonly detail?: string,
    /** The problem's Node-compatible `code`, such as `draft_changed`. */
    readonly code?: string,
    readonly conflictingBuilds: readonly string[] = [],
  ) {
    super(`Rust core answered ${status} for ${path}${detail ? `: ${detail}` : ""}`);
    this.name = "CoreError";
  }
}

/** The problem code, detail and holding Build names, when present. */
async function problem(
  response: Response,
): Promise<{ detail?: string; code?: string; conflictingBuilds?: string[] }> {
  const body: unknown = await response.json().catch(() => undefined);
  if (typeof body !== "object" || body === null) return {};
  const text = (key: string) => {
    const value: unknown = (body as Record<string, unknown>)[key];
    return typeof value === "string" ? value : undefined;
  };
  const builds: unknown = (body as Record<string, unknown>).builds;
  const conflictingBuilds = Array.isArray(builds)
    ? builds.flatMap((build: unknown) => {
        if (typeof build !== "object" || build === null || !("name" in build)) return [];
        return typeof build.name === "string" && build.name.trim() ? [build.name] : [];
      })
    : [];
  return { detail: text("detail"), code: text("code"), conflictingBuilds };
}

// One leading slash only: "//host" and "/\host" are cross-origin in browsers. Tabs and
// newlines are refused too, because URL parsing strips them and can turn "/\n/host"
// into "//host".
const SAME_ORIGIN_PATH = /^\/(?![/\\])(?![\s\S]*[\t\r\n])/;

/** True for a path on this page's own origin, such as `/builds/3`. */
export const isSameOriginPath = (path: string) => SAME_ORIGIN_PATH.test(path);

/** What a 401 answered: a read (GET or HEAD) or a write. */
export type Unauthorized = { write: boolean };

const unauthorized = new Set<(answer: Unauthorized) => void>();

/**
 * Calls `listener` whenever the core answers 401, from any read or write, so the app can
 * open sign-in. Returns the function that stops the calls.
 */
export function onUnauthorized(listener: (answer: Unauthorized) => void): () => void {
  unauthorized.add(listener);
  return () => void unauthorized.delete(listener);
}

export async function fetchCore(path: string, init?: RequestInit): Promise<Response> {
  if (!SAME_ORIGIN_PATH.test(path)) {
    throw new TypeError(
      `Core requests take a same-origin path such as /health, got ${JSON.stringify(path)}`,
    );
  }
  const response = await fetch(path, { ...init, credentials: "same-origin" });
  if (!response.ok) {
    if (response.status === 401) {
      const write = !["GET", "HEAD"].includes((init?.method ?? "GET").toUpperCase());
      for (const listener of unauthorized) listener({ write });
    }
    const { detail, code, conflictingBuilds } = await problem(response);
    throw new CoreError(response.status, path, detail, code, conflictingBuilds);
  }
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
