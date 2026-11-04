import type { UseQueryResult } from "@tanstack/react-query";
import type { ConnectionStatus } from "../frame/connection";
import type { ScreenState } from "./ScreenTemplate";

/**
 * Maps one TanStack query to a ScreenTemplate state. Offline wins, keeping what
 * was loaded. An error keeps the last successful data and its time. A success
 * that `isEmpty` says holds no records is the empty state.
 */
export function queryScreenState<T>(
  query: UseQueryResult<T>,
  connection: ConnectionStatus,
  isEmpty: (data: T) => boolean = () => false,
): ScreenState<T> {
  const lastData = query.data;
  const updatedAt = query.dataUpdatedAt || undefined;
  if (connection === "offline") return { kind: "offline", lastData, updatedAt };
  if (query.isError) {
    return {
      kind: "error",
      message: query.error.message,
      lastData,
      updatedAt,
      retry: () => void query.refetch(),
    };
  }
  if (lastData === undefined) return { kind: "loading" };
  return isEmpty(lastData) ? { kind: "empty" } : { kind: "ready", data: lastData };
}
