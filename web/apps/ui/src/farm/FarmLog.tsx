import type { FarmLogPage } from "@print-partner/rust-client";
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, type RefObject } from "react";
import { Link } from "react-router";
import { useModule } from "../core/modules";
import { readCore, writeCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { UndoOutcome, useBurstUndo } from "../screen/BurstUndo";
import { OlderEntries, pagedScreenState } from "../screen/OlderEntries";
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

function LogTable({
  entries,
  onUndo,
  table,
}: {
  entries: Entry[];
  onUndo: (entry: Entry) => void;
  table: RefObject<HTMLTableElement | null>;
}) {
  return (
    <table ref={table} className="pp-farm-table" aria-label="Farm log">
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
          <tr key={entry.id} data-entry={entry.id} tabIndex={-1}>
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
  const query = useInfiniteQuery({
    ...farmLogQuery,
    // Not read until /health says the Farm module is on or degraded, so an off module is
    // never asked.
    enabled: farm !== undefined && farm.state !== "off",
  });
  const table = useRef<HTMLTableElement>(null);
  const { undo, start, message } = useBurstUndo(
    (entry: Entry) => writeCore("undoFarmLogEntry", { id: entry.id }),
    // An undo refreshes the Farm log and every Build's History. Checkoff progress
    // reloads when that screen is opened again.
    () =>
      Promise.all([
        client.invalidateQueries({ queryKey: farmLogQuery.queryKey }),
        client.invalidateQueries({ queryKey: ["core", "history"] }),
      ]),
  );
  const state: ScreenState<{ pages: FarmLogPage[] }> =
    farm?.state === "off"
      ? { kind: "module-off", module: "Farm", wouldShow: "the farm log" }
      : pagedScreenState(query, connection, (data) => data.pages[0]?.entries.length === 0);
  const what = undo.variables && KIND_WORDS[undo.variables.kind].toLowerCase();
  return (
    <ScreenTemplate
      title="Farm log"
      state={state}
      renderData={(data) => (
        <>
          <UndoOutcome
            undo={undo}
            message={message}
            what={what}
            done={undo.data?.summary ?? `Undid the ${what}.`}
            reloaded="The log"
          />
          <LogTable
            entries={data.pages.flatMap((page) => page.entries)}
            onUndo={start}
            table={table}
          />
          <OlderEntries query={query} table={table} />
        </>
      )}
      empty={{
        message: "No farm actions yet. Checkoff bursts and sends appear here.",
        action: <Link to="/farm" className="pp-link">View Farm</Link>,
      }}
    />
  );
}
