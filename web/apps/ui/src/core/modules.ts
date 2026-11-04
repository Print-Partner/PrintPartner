import type { Health } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { readCore } from "./rust";

export type ModuleName = Health["modules"][number]["module"];
export type ModuleStatus = Health["modules"][number];

/** Words for each Module, as the screens name them. */
export const MODULE_LABELS: Record<ModuleName, string> = {
  farm: "Farm",
  spoolman: "Spoolman",
  orders: "Orders",
  bambu: "Bambu",
  assembly_tracking: "Assembly tracking",
};

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
  });
  return health.data?.modules.find((m) => m.module === name);
}
