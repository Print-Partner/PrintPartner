import type { BuildHistoryPage } from "@print-partner/rust-client";
import { useInfiniteQuery } from "@tanstack/react-query";
import { useRef, type RefObject } from "react";
import { useOutletContext } from "react-router";
import { readCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { OlderEntries, pagedScreenState } from "../screen/OlderEntries";
import { ScreenTemplate } from "../screen/ScreenTemplate";
import { BuildTabs, type WorkspaceContext } from "./BuildWorkspace";
import { dateText } from "./format";

type Entry = BuildHistoryPage["entries"][number];
type UndoReason = NonNullable<NonNullable<Entry["undo"]>["reason"]>;

const TYPE_WORDS: Record<Entry["type"], string> = {
  revision: "Accepted revision",
  snapshot: "Snapshot",
  checkoff_burst: "Checkoff burst",
  send: "Send",
  decision: "Decision",
};

const ACTOR_WORDS: Record<Entry["actor"], string> = { user: "You", assistant: "Assistant" };

/** Why History cannot undo an entry, in the words docs/build-history.md uses. */
const HISTORY_UNDO_REASONS: Record<UndoReason, string> = {
  printer_command: "A printer command cannot be undone",
  no_undo_token: "History cannot undo checkoff bursts yet. Undo a recent one from the Farm log",
};

/**
 * `GET /api/v2/plans/{id}/history`, newest first, one page at a time. Older pages load
 * from the previous page's `next_cursor`.
 */
const historyQuery = (id: number) => ({
  queryKey: ["core", "history", id],
  queryFn: ({ pageParam, signal }: { pageParam: string | null; signal: AbortSignal }) =>
    readCore("readBuildHistory", { id }, signal, pageParam ? { cursor: pageParam } : undefined),
  initialPageParam: null as string | null,
  getNextPageParam: (page: BuildHistoryPage) => page.next_cursor,
});

function UndoCell({ undo }: { undo: Entry["undo"] }) {
  if (!undo) return null;
  if (undo.undoable) return <>Undoable</>;
  return (
    <>
      Not undoable
      {undo.reason && <span className="pp-fact-detail">{HISTORY_UNDO_REASONS[undo.reason]}</span>}
    </>
  );
}

function HistoryTable({
  entries,
  table,
}: {
  entries: Entry[];
  table: RefObject<HTMLTableElement | null>;
}) {
  return (
    <table ref={table} className="pp-farm-table" aria-label="History">
      <thead>
        <tr>
          <th scope="col">When</th>
          <th scope="col">What</th>
          <th scope="col">By</th>
          <th scope="col">Undo</th>
        </tr>
      </thead>
      <tbody>
        {entries.map((entry) => (
          <tr key={entry.id} data-entry={entry.id} tabIndex={-1}>
            <td>{dateText(entry.at)}</td>
            <th scope="row">
              {TYPE_WORDS[entry.type]}
              <span className="pp-fact-detail">{entry.summary}</span>
            </th>
            <td>{ACTOR_WORDS[entry.actor]}</td>
            <td>
              <UndoCell undo={entry.undo} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * The History tab: the Build's accepted revisions, snapshots, checkoff bursts, sends and
 * assistant decisions from one read, newest first, each with whether it can be undone.
 */
export function BuildHistory() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const query = useInfiniteQuery(historyQuery(id));
  const table = useRef<HTMLTableElement>(null);
  const state = pagedScreenState(query, connection, (data) => data.pages[0]?.entries.length === 0);
  return (
    <ScreenTemplate
      title="History"
      subnav={<BuildTabs id={id} />}
      state={state}
      renderData={(data) => (
        <>
          <HistoryTable entries={data.pages.flatMap((page) => page.entries)} table={table} />
          <OlderEntries query={query} table={table} />
        </>
      )}
      empty={{ message: "Nothing has happened to this Build yet." }}
    />
  );
}
