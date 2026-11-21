import { readCore } from "./rust";

/** Every Build the core holds. The Builds list shows them, and Create Build checks them. */
export const buildsQuery = {
  queryKey: ["core", "builds"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listBuilds", {}, signal),
} as const;
