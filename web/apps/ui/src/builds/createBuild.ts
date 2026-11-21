import type { CreateBuildRequest, ProfileList, ProfileSummary } from "@print-partner/rust-client";
import type { QueryClient } from "@tanstack/react-query";
import { buildsQuery } from "../core/builds";
import { CoreError } from "../core/client";
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
 * Sends `createBuild` once, unless a create is already in flight or being checked. Only an exact
 * 200, not redirected, confirms the Build. Any other answer that is not the core's refusal is
 * checked against a Builds list read after it, never resent.
 */
export async function startCreate(client: QueryClient, body: CreateBuildRequest): Promise<void> {
  if (busy(state)) return;
  const { name } = body;
  const listed = client.getQueryData<ProfileList>(buildsQuery.queryKey);
  // No loaded list before the press: a Build found later cannot be shown to be new.
  const before = listed ? new Set(listed.profiles.map((build) => build.id)) : null;
  set({ kind: "sending", name });
  let answer: Awaited<ReturnType<typeof writeCoreAnswer<"createBuild">>>;
  try {
    answer = await writeCoreAnswer("createBuild", {}, body);
  } catch (error: unknown) {
    const detail = refusal(error);
    if (detail === null) return check(client, name, before);
    if (detail.startsWith("Profile already exists")) return check(client, name, "taken");
    return set({ kind: "refused", name, text: refusalCopy(detail) });
  } finally {
    void client.invalidateQueries({ queryKey: ["core", "tasks"] });
  }
  if (answer.status !== 200 || answer.redirected) return check(client, name, before);
  void client.invalidateQueries({ queryKey: buildsQuery.queryKey });
  set(dialogs > 0 ? { kind: "opened", name, build: answer.data } : IDLE);
}

/**
 * Reads the Builds list fresh, after the create's answer, and looks for the exact name, as
 * docs/gre-792-builds-http.md says. A list read already in flight may have started before the
 * create, so it is cancelled rather than joined.
 */
async function check(
  client: QueryClient,
  name: string,
  before: ReadonlySet<number> | null | "taken",
): Promise<void> {
  set({ kind: "checking", name });
  await client.cancelQueries({ queryKey: buildsQuery.queryKey });
  let list: ProfileList;
  try {
    list = await readCore("listBuilds", {});
  } catch {
    return set(
      before === "taken" ? { kind: "refused", name, text: TAKEN } : { kind: "unread", name },
    );
  }
  client.setQueryData(buildsQuery.queryKey, list);
  const build = list.profiles.find((listedBuild) => listedBuild.name === name);
  if (!build) {
    return set(
      before === "taken" ? { kind: "refused", name, text: TAKEN } : { kind: "absent", name },
    );
  }
  const why =
    before === "taken"
      ? "taken"
      : before !== null && !before.has(build.id)
        ? "created"
        : "unproven";
  set({ kind: "listed", name, build, why });
}
