import type { CreateBuildRequest, ProfileList, ProfileSummary } from "@print-partner/rust-client";
import { MutationObserver, type QueryClient } from "@tanstack/react-query";
import { buildsQuery } from "../core/builds";
import { CoreError, notifyUnauthorized } from "../core/client";
import { readCore, RequestContractError, writeCoreAnswer } from "../core/rust";
import { refusalText } from "../screen/SourceSelect";

/**
 * The one Create Build in this page, outside any dialog, so closing and reopening the dialog
 * can neither send a second create nor lose the first one's answer (GRE-1401). `name` is the
 * trimmed name that was sent.
 */
export type CreateState =
  | { kind: "idle" }
  | { kind: "sending"; name: string }
  | { kind: "checking"; name: string }
  | { kind: "opened"; name: string; build: ProfileSummary }
  | { kind: "refused"; name: string; text: string }
  /**
   * The Builds list holds the name. `created`: it was not listed before the press. `unproven`:
   * the list before the press was not loaded, or already held it. `taken`: the core refused the
   * name as in use.
   */
  | { kind: "listed"; name: string; build: ProfileSummary; why: "created" | "unproven" | "taken" }
  | { kind: "absent"; name: string }
  | { kind: "unread"; name: string };

const IDLE: CreateState = { kind: "idle" };
const TAKEN = "A Build with this name already exists. Choose another name.";

let state: CreateState = IDLE;
let dialogs = 0;
/** The latest list read after an answer; an older one is aborted and never written. */
let listRead = 0;
let listAbort = new AbortController();
const listeners = new Set<() => void>();

function set(next: CreateState) {
  state = next;
  for (const listener of listeners) listener();
}

export const createStore = {
  subscribe(listener: () => void) {
    listeners.add(listener);
    return () => void listeners.delete(listener);
  },
  get: () => state,
  /** A Create Build dialog mounted; returns its unmount. An answer with no dialog opens nothing. */
  mount() {
    dialogs += 1;
    return () => {
      dialogs -= 1;
      if (dialogs === 0 && state.kind === "opened") set(IDLE);
    };
  },
  /** Forgets a settled answer, as when the name changes or the dialog closes. Never a busy one. */
  clear() {
    if (!busy(state)) set(IDLE);
  },
};

/** Forgets everything; tests only. */
export function resetCreateBuild() {
  state = IDLE;
  dialogs = 0;
  listRead += 1;
  listAbort.abort();
}

export function busy(current: CreateState): boolean {
  return current.kind === "sending" || current.kind === "checking";
}

/**
 * Statuses the create handler refuses with before or inside its one write transaction, so
 * nothing of that request commits later. Any other answer may hide a created Build.
 */
const REFUSALS = new Set([400, 401, 403, 409, 413]);

function refusal(error: unknown): string | null {
  if (error instanceof RequestContractError) return error.message;
  if (!(error instanceof CoreError) || !error.problem || !REFUSALS.has(error.status)) return null;
  return error.detail || error.message;
}

/** The core's create refusals in the UI's words; the core still says "Profile" for a Build. */
function refusalCopy(detail: string): string {
  if (detail.startsWith("Profile name is")) return detail.replace("Profile", "Build");
  return refusalText(detail);
}

/**
 * One `createBuild` POST. A redirect throws `RedirectedWrite` in `fetchCore`, which is checked
 * like a lost answer. It runs in the mutation cache, so the core's own 401 opens sign-in
 * (`App.tsx`), and lets go of the cache once settled, since nothing reads a settled Create there.
 */
async function post(client: QueryClient, body: CreateBuildRequest) {
  const observer = new MutationObserver(client, {
    retry: false,
    networkMode: "always",
    gcTime: 0,
    mutationFn: (request: CreateBuildRequest) => writeCoreAnswer("createBuild", {}, request),
  });
  try {
    return await observer.mutate(body);
  } finally {
    observer.reset();
  }
}

/**
 * Sends `createBuild` once, unless a create is already in flight or being checked. Only an exact
 * 200 confirms the Build. Any other answer that is not the core's refusal is checked against a
 * Builds list read after it, never resent.
 */
export async function startCreate(client: QueryClient, body: CreateBuildRequest): Promise<void> {
  if (busy(state)) return;
  const { name } = body;
  const listed = profileList(client.getQueryData(buildsQuery.queryKey));
  // No loaded list before the press: a Build found later cannot be shown to be new.
  const before = listed ? new Set(listed.profiles.map((build) => build.id)) : null;
  set({ kind: "sending", name });
  let answer: Awaited<ReturnType<typeof post>>;
  try {
    answer = await post(client, body);
  } catch (error: unknown) {
    const detail = refusal(error);
    if (detail === null) return check(client, name, before);
    if (detail.startsWith("Profile already exists")) return check(client, name, "taken");
    return set({ kind: "refused", name, text: refusalCopy(detail) });
  } finally {
    void client.invalidateQueries({ queryKey: ["core", "tasks"] });
  }
  if (answer.status !== 200) return check(client, name, before);
  set(dialogs > 0 ? { kind: "opened", name, build: answer.data } : IDLE);
  // The list must show the Build even when no dialog opens it. A later Create cancels this read.
  await freshList(client).catch(() => client.invalidateQueries({ queryKey: buildsQuery.queryKey }));
}

/**
 * The Builds list read after the create's answer, or null when a newer one replaced it. A list
 * read already in flight may have started before the create, so it is cancelled rather than
 * joined. The read is not the Builds query's own fetch, so leaving Builds cannot cancel it or
 * put the list from before the create back; a newer read aborts it. A list the cache got while
 * this read ran was read after the create too, and may be newer, so that list wins; a query that
 * was removed, failed or holds no list since gives no answer and is never written.
 */
async function freshList(client: QueryClient): Promise<ProfileList | null> {
  const read = ++listRead;
  listAbort.abort();
  const abort = (listAbort = new AbortController());
  await client.cancelQueries({ queryKey: buildsQuery.queryKey });
  const cache = client.getQueryCache();
  const before = cache.find({ queryKey: buildsQuery.queryKey, exact: true });
  // A count, not a time, so two writes in one millisecond differ; sign-out makes a new query.
  const writes = before?.state.dataUpdateCount ?? 0;
  let list: ProfileList | undefined;
  let failure: unknown;
  try {
    const quiet = { notifyUnauthorized: false };
    list = await readCore("listBuilds", {}, abort.signal, undefined, quiet);
  } catch (error: unknown) {
    failure = error;
  }
  if (read !== listRead) return null;
  // A moved cache answers even when this read failed: its 401 is not this session's.
  const after = cache.find({ queryKey: buildsQuery.queryKey, exact: true });
  const afterWrites = after?.state.dataUpdateCount ?? 0;
  if (after === before ? afterWrites > writes : afterWrites > 0 || before !== undefined) {
    return after?.state.status === "success" ? profileList(after.state.data) : null;
  }
  if (!list) {
    if (failure instanceof CoreError && failure.status === 401) notifyUnauthorized(false);
    throw failure;
  }
  client.setQueryData(buildsQuery.queryKey, list);
  return list;
}

/** The cache's Builds list, or null when the entry holds anything else. */
function profileList(value: unknown): ProfileList | null {
  const profiles: unknown = (value as { profiles?: unknown } | null)?.profiles;
  return Array.isArray(profiles) ? (value as ProfileList) : null;
}

/** Looks for the exact name in a fresh Builds list, as docs/gre-792-builds-http.md says. */
async function check(
  client: QueryClient,
  name: string,
  before: ReadonlySet<number> | null | "taken",
): Promise<void> {
  set({ kind: "checking", name });
  // No list, from a failed or replaced read, is "unread".
  const list = await freshList(client).catch(() => null);
  const build = list?.profiles.find((listedBuild) => listedBuild.name === name);
  if (!build) {
    if (before === "taken") return set({ kind: "refused", name, text: TAKEN });
    return set({ kind: list ? "absent" : "unread", name });
  }
  const why =
    before === "taken"
      ? "taken"
      : before !== null && !before.has(build.id)
        ? "created"
        : "unproven";
  set({ kind: "listed", name, build, why });
}
