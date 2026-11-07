import type { InfiniteData, UseInfiniteQueryResult } from "@tanstack/react-query";
import { useEffect, useRef, type RefObject } from "react";
import type { ConnectionStatus } from "../frame/connection";
import { queryScreenState } from "./queryState";
import type { ScreenState } from "./ScreenTemplate";

type Paged<P> = UseInfiniteQueryResult<InfiniteData<P>>;

/**
 * queryScreenState for a paged read. A failed older page keeps the loaded pages on screen
 * without the refresh banner, because OlderEntries reports it beside its button.
 */
export function pagedScreenState<P>(
  query: Paged<P>,
  connection: ConnectionStatus,
  isEmpty: (data: InfiniteData<P>) => boolean,
): ScreenState<InfiniteData<P>> {
  if (query.isFetchNextPageError && query.data && connection !== "offline") {
    return { kind: "ready", data: query.data };
  }
  return queryScreenState(query, connection, isEmpty);
}

/**
 * Show older entries under a paged table. It loads the next page and moves focus to that
 * page's first row, so focus never falls to the page when the button goes away after the
 * last page. A failed load is reported here, with a Retry that loads the same page again.
 */
export function OlderEntries<P>({
  query,
  table,
}: {
  query: Paged<P>;
  table: RefObject<HTMLTableElement | null>;
}) {
  const focusRow = useRef<number | null>(null);
  const loaded = query.data?.pages.length ?? 0;
  useEffect(() => {
    if (focusRow.current === null) return;
    const row = table.current?.tBodies[0]?.rows[focusRow.current];
    if (!row) return;
    focusRow.current = null;
    row.focus();
  }, [loaded, table]);
  if (!query.hasNextPage) return null;
  const load = () => {
    focusRow.current = table.current?.tBodies[0]?.rows.length ?? null;
    void query.fetchNextPage();
  };
  if (query.isFetchNextPageError) {
    return (
      <div role="alert" className="pp-banner" data-kind="error">
        <p>Could not load older entries.</p>
        <p className="pp-banner-detail">{query.error?.message}</p>
        <button type="button" className="pp-button" onClick={load}>
          Retry
        </button>
      </div>
    );
  }
  return (
    <button type="button" className="pp-button" disabled={query.isFetchingNextPage} onClick={load}>
      {query.isFetchingNextPage ? "Loading older entries" : "Show older entries"}
    </button>
  );
}
