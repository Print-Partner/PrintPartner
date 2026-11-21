/**
 * The one way UI code reaches the Rust core. Requests stay on the page's own
 * origin: the Rust core serves this UI in the desktop app and Docker, and the
 * Vite dev server forwards core paths to PP_UI_CORE_URL.
 *
 * Typed reads go through `readCore` in ./rust.ts, which builds on this function
 * with the generated client's routes and schemas.
 */
import { problemSchema } from "@print-partner/rust-client";

/** One field the core refused: a JSON Pointer into the request body, and the core's reason. */
export type FieldProblem = { pointer: string; detail: string };

/**
 * One Source file a download refused (`source_files_unavailable`): the Source, the file's
 * path in it, and the core's reason code, such as `missing` or `digest_mismatch`.
 */
export type UnavailableSourceFile = { source: string; relative_path: string; reason: string };

export class CoreError extends Error {
  /** The problem's `errors` list: each refused field, in the core's order. */
  errors: readonly FieldProblem[] = [];
  /** The problem's `files` list: each Source file a download refused, in the core's order. */
  files: readonly UnavailableSourceFile[] = [];
  /**
   * True when the body is the core's own problem: `application/problem+json`, valid against the
   * generated `Problem` schema, with a `status` equal to this answer's. A proxy page, a cut-off
   * body or a malformed member such as `"code": null` leaves it false.
   */
  problem = false;
  /** True when `fetch` followed a redirect, so the answer is not the requested route's own. */
  redirected = false;

  constructor(
    readonly status: number,
    path: string,
    readonly detail?: string,
    /** The problem's Node-compatible `code`, such as `draft_changed`. */
    readonly code?: string,
    readonly conflictingBuilds: readonly HoldingBuild[] = [],
  ) {
    super(`Rust core answered ${status} for ${path}${detail ? `: ${detail}` : ""}`);
    this.name = "CoreError";
  }
}

/** A Build a refusal names, such as the one holding an order. `profileId` is null when absent. */
export interface HoldingBuild {
  name: string;
  profileId: number | null;
}

/** The problem code, detail, holding Builds and refused fields, when present. */
async function problem(response: Response): Promise<{
  own?: boolean;
  detail?: string;
  code?: string;
  conflictingBuilds?: HoldingBuild[];
  errors?: FieldProblem[];
  files?: UnavailableSourceFile[];
}> {
  const body: unknown = await response.json().catch(() => undefined);
  if (typeof body !== "object" || body === null) return {};
  const type = response.headers.get("content-type")?.split(";")[0]?.trim().toLowerCase();
  const own =
    type === "application/problem+json" &&
    problemSchema.safeParse(body).success &&
    (body as Record<string, unknown>).status === response.status;
  const text = (key: string) => {
    const value: unknown = (body as Record<string, unknown>)[key];
    return typeof value === "string" ? value : undefined;
  };
  const builds: unknown = (body as Record<string, unknown>).builds;
  const conflictingBuilds = Array.isArray(builds)
    ? builds.flatMap((build: unknown): HoldingBuild[] => {
        if (typeof build !== "object" || build === null) return [];
        const { name, profile_id } = build as Record<string, unknown>;
        if (typeof name !== "string" || !name.trim()) return [];
        const profileId = Number.isSafeInteger(profile_id) && (profile_id as number) > 0;
        return [{ name, profileId: profileId ? (profile_id as number) : null }];
      })
    : [];
  const listed: unknown = (body as Record<string, unknown>).errors;
  const errors = Array.isArray(listed)
    ? listed.flatMap((entry: unknown) => {
        const { pointer, detail } = (
          typeof entry === "object" && entry !== null ? entry : {}
        ) as Record<string, unknown>;
        return typeof pointer === "string" && typeof detail === "string"
          ? [{ pointer, detail }]
          : [];
      })
    : [];
  const refused: unknown = (body as Record<string, unknown>).files;
  const files = Array.isArray(refused)
    ? refused.flatMap((entry: unknown) => {
        const { source, relative_path, reason } = (
          typeof entry === "object" && entry !== null ? entry : {}
        ) as Record<string, unknown>;
        return typeof source === "string" &&
          typeof relative_path === "string" &&
          typeof reason === "string"
          ? [{ source, relative_path, reason }]
          : [];
      })
    : [];
  return { own, detail: text("detail"), code: text("code"), conflictingBuilds, errors, files };
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

/** Tells every listener the core answered 401, as `fetchCore` does unless told not to. */
export function notifyUnauthorized(write: boolean): void {
  for (const listener of unauthorized) listener({ write });
}

export async function fetchCore(
  path: string,
  init?: RequestInit,
  options?: { notifyUnauthorized?: boolean },
): Promise<Response> {
  if (!SAME_ORIGIN_PATH.test(path)) {
    throw new TypeError(
      `Core requests take a same-origin path such as /health, got ${JSON.stringify(path)}`,
    );
  }
  const response = await fetch(path, { ...init, credentials: "same-origin" });
  if (!response.ok) {
    if (response.status === 401 && options?.notifyUnauthorized !== false) {
      notifyUnauthorized(!["GET", "HEAD"].includes((init?.method ?? "GET").toUpperCase()));
    }
    const { own, detail, code, conflictingBuilds, errors, files } = await problem(response);
    const error = new CoreError(response.status, path, detail, code, conflictingBuilds);
    error.errors = errors ?? [];
    error.files = files ?? [];
    error.problem = own ?? false;
    error.redirected = response.redirected;
    throw error;
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
