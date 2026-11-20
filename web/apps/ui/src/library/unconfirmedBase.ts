import { skipToken, useQuery, type QueryClient } from "@tanstack/react-query";

/**
 * A `setBuildBase` the UI sent but never saw confirmed: the answer was a 5xx or never came.
 * The core may still apply it later, and a second send would then replace one base with the
 * other. It is kept per Build in the query client, never garbage-collected, so leaving the
 * tab or the send settling does not forget it; a page reload does.
 */
export interface UnconfirmedBase {
  sourceId: number;
  /** Why the send is unconfirmed; null while it is still in flight. */
  reason: string | null;
}

const PREFIX = ["ui", "unconfirmedBase"] as const;
const key = (buildId: number) => [...PREFIX, buildId] as const;

export function markUnconfirmed(client: QueryClient, buildId: number, mark: UnconfirmedBase) {
  client.setQueryDefaults(PREFIX, { gcTime: Infinity, staleTime: Infinity });
  client.setQueryData<UnconfirmedBase | null>(key(buildId), mark);
}

/** After a read shows a base, a confirmed or refused send, or the owner's Set base anyway. */
export function clearUnconfirmed(client: QueryClient, buildId: number) {
  if (client.getQueryData(key(buildId))) {
    client.setQueryData<UnconfirmedBase | null>(key(buildId), null);
  }
}

export function useUnconfirmedBase(buildId: number): UnconfirmedBase | null {
  const query = useQuery<UnconfirmedBase | null>({
    queryKey: key(buildId),
    queryFn: skipToken,
    gcTime: Infinity,
    staleTime: Infinity,
  });
  return query.data ?? null;
}
