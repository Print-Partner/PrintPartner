import { readCore } from "./rust";

/** The Library's Sources. Library lists them, and a Build offers them as its base Source. */
export const sourcesQuery = {
  queryKey: ["core", "sources"],
  queryFn: ({ signal }: { signal: AbortSignal }) =>
    readCore("listSources", {}, signal, undefined, { rejectRedirect: true }),
} as const;
