import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";

export const JOBS_KEY = "pp.library.jobs";

type JobMap = Record<string, string>;

function readStored(): JobMap | null {
  try {
    const raw = localStorage.getItem(JOBS_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    return Object.fromEntries(
      Object.entries(parsed).filter(([, job]) => typeof job === "string" && job !== ""),
    );
  } catch {
    return null;
  }
}

export function readJobs(): JobMap {
  return readStored() ?? {};
}

function writeStored(jobs: JobMap): boolean {
  try {
    localStorage.setItem(JOBS_KEY, JSON.stringify(jobs));
    return true;
  } catch {
    return false;
  }
}

function sameJobs(a: JobMap, b: JobMap): boolean {
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((key) => a[key] === b[key]);
}

function createLibraryJobs() {
  let active = false;
  let snapshot = {
    jobs: readJobs(),
    starting: new Set<number>(),
    refusals: {} as Partial<Record<number, string>>,
  };
  const listeners = new Set<() => void>();
  const unsaved = new Map<string, string>();
  const ended = new Set<string>();

  const publish = (next: typeof snapshot) => {
    if (next === snapshot) return;
    snapshot = next;
    listeners.forEach((listener) => listener());
  };

  // Retry on the next reconciliation against the current shared map. Only unpersisted
  // starts and observed endings change that map. If reading fails, keep following our
  // last known jobs in memory and wait for a readable map before trying to write.
  const reconcile = (state = snapshot) => {
    const stored = readStored();
    const jobs = { ...(stored ?? snapshot.jobs) };
    for (const key of Object.keys(jobs)) {
      if (ended.has(jobs[key])) delete jobs[key];
    }
    for (const [key, job] of unsaved) {
      // Another tab already published a different job for this Source. Follow that one.
      if (stored && jobs[key] && jobs[key] !== job) unsaved.delete(key);
      else jobs[key] = job;
    }
    if (stored && (sameJobs(stored, jobs) || writeStored(jobs))) unsaved.clear();
    publish(sameJobs(state.jobs, jobs) ? state : { ...state, jobs });
  };

  return {
    getSnapshot: () => snapshot,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    connect() {
      active = true;
      const follow = (event: StorageEvent) => {
        if (event.key === null || event.key === JOBS_KEY) reconcile();
      };
      window.addEventListener("storage", follow);
      return () => {
        active = false;
        window.removeEventListener("storage", follow);
      };
    },
    refresh: () => reconcile(),
    async start(
      sourceId: number,
      send: () => Promise<{ job_id: string }>,
      failure: (error: unknown) => string,
    ) {
      if (!active || snapshot.starting.has(sourceId)) return;
      reconcile();
      if (snapshot.jobs[sourceId]) return;
      const refusals = { ...snapshot.refusals };
      delete refusals[sourceId];
      publish({ ...snapshot, refusals, starting: new Set(snapshot.starting).add(sourceId) });

      let message: string | undefined;
      try {
        const { job_id } = await send();
        if (!active) return;
        if (!ended.has(job_id)) unsaved.set(String(sourceId), job_id);
      } catch (error: unknown) {
        if (!active) return;
        message = failure(error);
      }
      const starting = new Set(snapshot.starting);
      starting.delete(sourceId);
      reconcile({
        ...snapshot,
        starting,
        refusals: message ? { ...snapshot.refusals, [sourceId]: message } : snapshot.refusals,
      });
    },
    forget(sourceId: number, jobId: string) {
      if (!active) return;
      ended.add(jobId);
      const key = String(sourceId);
      if (unsaved.get(key) === jobId) unsaved.delete(key);
      reconcile();
    },
  };
}

type LibraryJobs = ReturnType<typeof createLibraryJobs>;
export type StartLibraryJob = LibraryJobs["start"];

const LibraryJobsContext = createContext<LibraryJobs | null>(null);

/** The authenticated frame owns the tracker, so it outlives Library routes. */
export function LibraryJobsProvider({ children }: { children: ReactNode }) {
  const [store] = useState(createLibraryJobs);
  useLayoutEffect(() => store.connect(), [store]);
  return <LibraryJobsContext.Provider value={store}>{children}</LibraryJobsContext.Provider>;
}

export function useLibraryJobs() {
  const store = useContext(LibraryJobsContext);
  if (!store) throw new Error("LibraryJobsProvider is missing");
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  useEffect(() => store.refresh(), [store]);
  return { ...snapshot, start: store.start, forget: store.forget };
}
