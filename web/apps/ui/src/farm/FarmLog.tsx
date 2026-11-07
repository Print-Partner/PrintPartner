import type { FarmLogPage } from "@print-partner/rust-client";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router";
import { useModule } from "../core/modules";
import { readCore, writeCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { MutationButton, ScreenTemplate, type ScreenState } from "../screen/ScreenTemplate";
import { dateText } from "../builds/format";

type Entry = FarmLogPage["entries"][number];
type UndoReason = NonNullable<Entry["undo"]["reason"]>;

const KIND_WORDS: Record<Entry["kind"], string> = {
  checkoff_burst: "Checkoff burst",
  checkoff_burst_undo: "Undo of a checkoff burst",
  send: "Send",
};

/** Why an entry cannot be undone, as docs/farm-log.md lists them. */
const UNDO_REASONS: Record<UndoReason, string> = {
  already_undone: "Already undone",
  receipt_expired: "Too old: its record was dropped",
  state_changed: "Checkoff changed since",
  archived: "A Build it touched is archived",
  printer_command: "A printer command cannot be undone",
  undo_entry: "An undo cannot be undone",
};

/**
 * `GET /api/v2/farm-log`, newest first, one page at a time. Older pages load from the
 * previous page's `next_cursor`.
 */
const farmLogQuery = {
  queryKey: ["core", "farm-log"],
  queryFn: ({ pageParam, signal }: { pageParam: string | null; signal: AbortSignal }) =>
    readCore("listFarmLog", {}, signal, pageParam ? { cursor: pageParam } : undefined),
  initialPageParam: null as string | null,
  getNextPageParam: (page: FarmLogPage) => page.next_cursor,
};

function UndoCell({ entry, onUndo }: { entry: Entry; onUndo: (entry: Entry) => void }) {
  const { undo } = entry;
  if (undo.undoable) return <MutationButton onClick={() => onUndo(entry)}>Undo</MutationButton>;
  return (
    <>
      Not undoable
      {undo.reason && <span className="pp-fact-detail">{UNDO_REASONS[undo.reason]}</span>}
      {undo.reason === "state_changed" && undo.detail && (
        <span className="pp-fact-detail">{undo.detail}</span>
      )}
    </>
  );
}

function LogTable({ entries, onUndo }: { entries: Entry[]; onUndo: (entry: Entry) => void }) {
  return (
    <table className="pp-farm-table" aria-label="Farm log">
      <thead>
        <tr>
          <th scope="col">When</th>
          <th scope="col">Action</th>
          <th scope="col">Builds</th>
          <th scope="col">Printers</th>
          <th scope="col">Undo</th>
        </tr>
      </thead>
      <tbody>
        {entries.map((entry) => (
          <tr key={entry.id} data-entry={entry.id}>
            <td>{dateText(entry.at)}</td>
            <th scope="row">
              {KIND_WORDS[entry.kind]}
              <span className="pp-fact-detail">{entry.summary}</span>
            </th>
            <td>
              {entry.build_ids.map((build, index) => (
                <span key={build}>
                  {index > 0 && ", "}
                  <Link to={`/builds/${build}/history`} className="pp-link num">
                    Build {build}
                  </Link>
                </span>
              ))}
            </td>
            <td>{entry.printer_ids.join(", ")}</td>
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
 * The farm log: printer and queue actions across the fleet, newest first, with Undo on
 * each entry the core says it can undo now. Sends never offer Undo.
 */
export function FarmLog() {
  const connection = useConnection();
  const farm = useModule("farm");
  const client = useQueryClient();
  const query = useInfiniteQuery(farmLogQuery);
  const undo = useMutation({
    mutationFn: (entry: Entry) => writeCore("undoFarmLogEntry", { id: entry.id }),
    // An undo changes Checkoff progress and History too.
    onSettled: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: farmLogQuery.queryKey }),
        client.invalidateQueries({ queryKey: ["core", "history"] }),
      ]),
  });
  const state: ScreenState<{ pages: FarmLogPage[] }> =
    farm?.state === "off"
      ? { kind: "module-off", module: "Farm", wouldShow: "the farm log" }
      : queryScreenState(query, connection, (data) => data.pages[0]?.entries.length === 0);
  return (
    <ScreenTemplate
      title="Farm log"
      state={state}
      renderData={(data) => (
        <>
          {undo.isError && (
            <div role="alert" className="pp-banner" data-kind="error">
              <p>
                Could not undo {undo.variables && KIND_WORDS[undo.variables.kind].toLowerCase()}.
              </p>
              <p className="pp-banner-detail">{undo.error.message}</p>
            </div>
          )}
          {undo.isSuccess && <p role="status">{undo.data.summary}</p>}
          <LogTable
            entries={data.pages.flatMap((page) => page.entries)}
            onUndo={(entry) => undo.mutate(entry)}
          />
          {query.hasNextPage && (
            <button
              type="button"
              className="pp-button"
              disabled={query.isFetchingNextPage}
              onClick={() => void query.fetchNextPage()}
            >
              {query.isFetchingNextPage ? "Loading older entries" : "Show older entries"}
            </button>
          )}
        </>
      )}
      empty={{ message: "No farm actions yet. Checkoff bursts and sends appear here." }}
    />
  );
}
