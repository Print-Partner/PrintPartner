import {
  routePath,
  routes,
  type RouteName,
  type RouteParams,
  type RouteResponse,
} from "@print-partner/rust-client";
import { fetchCore } from "./client";

type ReadRoute = {
  [N in RouteName]: (typeof routes)[N]["method"] extends "GET" ? N : never;
}[RouteName];

/** The core answered 200 with a body that does not match the generated schema. */
export class ContractError extends Error {
  constructor(path: string) {
    super(`The core's answer for ${path} does not match the client contract`);
    this.name = "ContractError";
  }
}

/**
 * One GET through the generated Rust client: the route's path, filled with its
 * parameters, and its response schema. A body that fails the schema is a
 * ContractError, never a partly typed value.
 */
export async function readCore<N extends ReadRoute>(
  name: N,
  params: RouteParams<N>,
  signal?: AbortSignal,
): Promise<RouteResponse<N>> {
  const path = routePath(name, params);
  const response = await fetchCore(path, { headers: { accept: "application/json" }, signal });
  const parsed = routes[name].response.safeParse(await response.json().catch(() => undefined));
  if (!parsed.success) throw new ContractError(path);
  return parsed.data as RouteResponse<N>;
}
