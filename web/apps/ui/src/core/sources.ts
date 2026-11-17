import { readCore } from "./rust";

/** The Library's Sources. Library lists them, and Builds offers them as a base Source. */
export const sourcesQuery = {
  queryKey: ["core", "sources"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listSources", {}, signal),
} as const;
