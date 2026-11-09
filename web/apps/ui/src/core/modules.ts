import type { Health } from "@print-partner/rust-client";
import { queryOptions, useQuery } from "@tanstack/react-query";
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
  return useHealth().data?.modules?.find((m) => m.module === name);
}

/**
 * GET /health, shared by every screen that needs Module state. It reads again every few
 * seconds while the read failed (the core was down, or retries ran out) or while a Module
 * reports `setting_unreadable`, which docs/desktop-rebuild.md says means "unknown, try again",
 * so Module state returns with the core instead of staying unknown.
 */
export const healthQuery = queryOptions({
  queryKey: ["core", "modules"],
  queryFn: ({ signal }) => readCore("readHealth", {}, signal),
  staleTime: 30_000,
  refetchInterval: (query) =>
    query.state.status === "error" ||
    query.state.data?.modules?.some((m) => m.state === "off" && m.reason === "setting_unreadable")
      ? MODULE_RETRY_MS
      : false,
});

function useHealth() {
  return useQuery(healthQuery);
}
