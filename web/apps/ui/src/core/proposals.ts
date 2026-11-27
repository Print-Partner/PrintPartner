import { queryOptions } from "@tanstack/react-query";
import { readCore } from "./rust";

export const proposalsQuery = queryOptions({
  queryKey: ["core", "proposals"],
  queryFn: ({ signal }) => readCore("listProposals", {}, signal),
  refetchInterval: 2_000,
  staleTime: 0,
  retry: false,
});
