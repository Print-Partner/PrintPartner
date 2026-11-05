import type { Health } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { readCore } from "./rust";

const MODULE_RETRY_MS = 5_000;

export type ModuleStatus = NonNullable<Health["modules"]>[number];
export type ModuleName = ModuleStatus["module"];

/**
 * A Module's state as the core reports it in GET /health, or undefined while that
 * is unknown: not loaded yet, unreadable, or a core that does not list Modules.
 * Screens never guess a Module's state from a 404.
 */
export function useModule(name: ModuleName): ModuleStatus | undefined {
  const health = useQuery({
    queryKey: ["core", "modules"],
    queryFn: ({ signal }) => readCore("readHealth", {}, signal),
    staleTime: 30_000,
    // A read that failed (the core was down, or retries ran out) tries again, so Module
    // state returns with the core instead of staying unknown.
    refetchInterval: (query) => (query.state.status === "error" ? MODULE_RETRY_MS : false),
  });
  return health.data?.modules?.find((m) => m.module === name);
}
