import type { BuildHistoryPage } from "@print-partner/rust-client";
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, type RefObject } from "react";
import { useOutletContext } from "react-router";
import { readCore, writeCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { UndoOutcome, useBurstUndo } from "../screen/BurstUndo";
import { OlderEntries, pagedScreenState } from "../screen/OlderEntries";
import { MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
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
  no_undo_token: "No undo is kept for this burst",
  already_undone: "Already undone",
  state_changed: "Checkoff changed since",
  archived: "A Build it touched is archived",
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

function UndoCell({ entry, onUndo }: { entry: Entry; onUndo: (entry: Entry) => void }) {
  const { undo } = entry;
  if (!undo) return null;
  if (undo.undoable) return <MutationButton onClick={() => onUndo(entry)}>Undo</MutationButton>;
  return (
    <>
      Not undoable
      {undo.reason && <span className="pp-fact-detail">{HISTORY_UNDO_REASONS[undo.reason]}</span>}
      {undo.reason === "state_changed" && undo.detail && (
        <span className="pp-fact-detail">{undo.detail}</span>
      )}
    </>
  );
}

function HistoryTable({
  entries,
  onUndo,
  table,
}: {
  entries: Entry[];
  onUndo: (entry: Entry) => void;
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
              <UndoCell entry={entry} onUndo={onUndo} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * The History tab: the Build's accepted revisions, snapshots, checkoff bursts, sends and
 * assistant decisions from one read, newest first, with Undo on each checkoff burst the
 * core says it can undo now.
 */
export function BuildHistory() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const client = useQueryClient();
  const query = useInfiniteQuery(historyQuery(id));
  const table = useRef<HTMLTableElement>(null);
  const { undo, start, message } = useBurstUndo(
    (entry: Entry) => writeCore("undoBuildHistoryEntry", { id, entry_id: entry.id }),
    // An undo changes the Build's Checkoff progress and the farm log too.
    () =>
      Promise.all([
        client.invalidateQueries({ queryKey: ["core", "history"] }),
        client.invalidateQueries({ queryKey: ["core", "farm-log"] }),
        client.invalidateQueries({ queryKey: ["core", "checkoff-units", id] }),
      ]),
  );
  const state = pagedScreenState(query, connection, (data) => data.pages[0]?.entries.length === 0);
  const what = undo.variables && TYPE_WORDS[undo.variables.type].toLowerCase();
  return (
    <ScreenTemplate
      title="History"
      subnav={<BuildTabs id={id} />}
      state={state}
      renderData={(data) => (
        <>
          <UndoOutcome
            undo={undo}
            message={message}
            what={what}
            done={`Undid the ${what}: ${undo.variables?.summary}.`}
            reloaded="History"
          />
          <HistoryTable
            entries={data.pages.flatMap((page) => page.entries)}
            onUndo={start}
            table={table}
          />
          <OlderEntries query={query} table={table} />
        </>
      )}
      empty={{ message: "Nothing has happened to this Build yet." }}
    />
  );
}
