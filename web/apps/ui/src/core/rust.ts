import {
  routePath,
  routes,
  type RouteName,
  type RouteParams,
  type RouteRequest,
  type RouteResponse,
} from "@print-partner/rust-client";
import { fetchCore } from "./client";

type ReadRoute = {
  [N in RouteName]: (typeof routes)[N]["method"] extends "GET" ? N : never;
}[RouteName];

type WriteRoute = Exclude<RouteName, ReadRoute>;

/** The core answered 200 with a body that does not match the generated schema. */
export class ContractError extends Error {
  constructor(path: string) {
    super(`The core's answer for ${path} does not match the client contract`);
    this.name = "ContractError";
  }
}

/** A write body that fails the generated request schema; it is never sent. */
export class RequestContractError extends Error {
  constructor(path: string) {
    super(`The request for ${path} does not match the client contract, so it was not sent`);
    this.name = "RequestContractError";
  }
}

/**
 * One GET through the generated Rust client: the route's path, filled with its
 * parameters and any query, and its response schema. A body that fails the schema
 * is a ContractError, never a partly typed value.
 */
export async function readCore<N extends ReadRoute>(
  name: N,
  params: RouteParams<N>,
  signal?: AbortSignal,
  query?: Record<string, string>,
): Promise<RouteResponse<N>> {
  const path = routePath(name, params) + (query ? `?${new URLSearchParams(query)}` : "");
  const response = await fetchCore(path, { headers: { accept: "application/json" }, signal });
  const parsed = routes[name].response.safeParse(await response.json().catch(() => undefined));
  if (!parsed.success) throw new ContractError(path);
  return parsed.data as RouteResponse<N>;
}

/** A write's body argument: none for a route that takes no body. */
type WriteBody<N extends WriteRoute> = [RouteRequest<N>] extends [never]
  ? []
  : [body: RouteRequest<N>];

/**
 * One write through the generated Rust client: the body is checked against the
 * route's request schema before it is sent, and the answer against its response
 * schema. A route without a request schema is sent with no body. A route that
 * declares an `Idempotency-Key` header is never sent without one. A failed send
 * throws; the caller decides whether to resend.
 */
export async function writeCore<N extends WriteRoute>(
  name: N,
  params: RouteParams<N>,
  ...args: [...WriteBody<N>, idempotencyKey?: string]
): Promise<RouteResponse<N>> {
  const route = routes[name];
  const path = routePath(name, params);
  const bodyless = route.request === null;
  const body: unknown = bodyless ? undefined : args[0];
  const idempotencyKey = (bodyless ? args[0] : args[1]) as string | undefined;
  const request = bodyless ? null : route.request.safeParse(body);
  const keyed = (route.headers as readonly string[]).includes("Idempotency-Key");
  if ((request && !request.success) || (keyed && !idempotencyKey)) {
    throw new RequestContractError(path);
  }
  const response = await fetchCore(path, {
    method: route.method,
    headers: {
      accept: "application/json",
      ...(request ? { "content-type": "application/json" } : {}),
      ...(keyed && idempotencyKey ? { "idempotency-key": idempotencyKey } : {}),
    },
    body: request ? JSON.stringify(request.data) : undefined,
  });
  const parsed = route.response.safeParse(await response.json().catch(() => undefined));
  if (!parsed.success) throw new ContractError(path);
  return parsed.data as RouteResponse<N>;
}
