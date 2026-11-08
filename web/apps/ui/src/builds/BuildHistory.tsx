import type { BuildHistoryPage, RestoreSnapshotRequest } from "@print-partner/rust-client";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useRef, type KeyboardEvent, type RefObject } from "react";
import { useNavigate, useOutletContext } from "react-router";
import { CoreError } from "../core/client";
import { readCore, writeCore } from "../core/rust";
import { lostSend, newKey } from "../core/writes";
import { useConnection } from "../frame/connection";
import { OlderEntries, pagedScreenState } from "../screen/OlderEntries";
import { MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
import { NotFound } from "../screen/screens";
import { planKey, planQuery, type RestoredState } from "./BuildPlan";
import { BuildTabs, type WorkspaceContext } from "./BuildWorkspace";
import { dateText } from "./format";
import { identityOf } from "./plan";

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
  no_undo_token:
    "History cannot undo checkoff bursts yet. Undo a printer burst from the Farm log, or your last manual burst on the Checkoff tab",
};

/** Each restore refusal in docs/build-history.md, in words. */
const RESTORE_REFUSALS: Record<string, string> = {
  snapshot_not_restorable:
    "This snapshot holds no accepted Plan of this Build, so it cannot be restored. Snapshots the old app saved hold only Source names and kit choices.",
  snapshot_not_found: "This snapshot no longer exists.",
  inputs_changed:
    "The Build's Sources changed since this snapshot was saved, so it cannot be restored.",
  printed_units_affected:
    "Restoring would leave fewer units than are already printed, so nothing was restored. Restore never rewinds printed units.",
  base_changed: "The accepted Plan changed while restoring. Nothing was restored; try again.",
  draft_changed: "The open draft changed while restoring. Nothing was restored; try again.",
  accepted_baseline_required:
    "This Build has no accepted Plan yet, so there is nothing to restore over.",
  required_unit_set_unavailable:
    "The accepted Plan has no Required units yet, so restore cannot keep printed units in place. Nothing was restored.",
  invalid_header:
    "The name, order number, customer or due date saved in this snapshot is no longer valid, so it cannot be restored.",
  build_archived: "This Build is archived, so its Plan cannot change.",
  idempotency_conflict: "A different restore was already sent with this request. Try again.",
};

const restoreRefusal = (error: Error) =>
  (error instanceof CoreError && error.code && RESTORE_REFUSALS[error.code]) ||
  (error instanceof CoreError && error.detail) ||
  error.message;

/** A restore's request, kept with its key so a resend repeats both. */
interface PendingRestore {
  key: string;
  body: RestoreSnapshotRequest;
}

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
  // A missing Build answers 404 for good, so it is not retried.
  retry: (failures: number, error: Error) =>
    !(error instanceof CoreError && error.status === 404) && failures < 3,
});

function UndoCell({ entry, onRestore }: { entry: Entry; onRestore: (entry: Entry) => void }) {
  const { undo } = entry;
  if (entry.type === "snapshot" && entry.restorable === true) {
    return <MutationButton onClick={() => onRestore(entry)}>Restore as draft</MutationButton>;
  }
  if (entry.type === "snapshot" && entry.restorable === false) {
    return (
      <>
        Not restorable
        <span className="pp-fact-detail">It holds no accepted Plan of this Build</span>
      </>
    );
  }
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
  onRestore,
  table,
}: {
  entries: Entry[];
  onRestore: (entry: Entry) => void;
  table: RefObject<HTMLTableElement | null>;
}) {
  const hint = useId();
  // `R` anywhere in a restorable snapshot's row, its button included, restores it.
  const onKeyDown = (event: KeyboardEvent<HTMLTableRowElement>, entry: Entry) => {
    if (event.ctrlKey || event.metaKey || event.altKey || event.key.toLowerCase() !== "r") return;
    if (entry.type !== "snapshot" || entry.restorable !== true) return;
    event.preventDefault();
    onRestore(entry);
  };
  return (
    <>
      <p id={hint} className="pp-muted">
        Restore as draft (<kbd className="pp-kbd">R</kbd> on a snapshot) starts a new draft from the
        snapshot and opens Accept. Any open draft is set aside. Nothing changes until you accept,
        and printed units and printer commands are never rewound.
      </p>
      <table ref={table} className="pp-farm-table" aria-label="History" aria-describedby={hint}>
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
            <tr
              key={entry.id}
              data-entry={entry.id}
              tabIndex={-1}
              onKeyDown={(event) => onKeyDown(event, entry)}
            >
              <td>{dateText(entry.at)}</td>
              <th scope="row">
                {TYPE_WORDS[entry.type]}
                <span className="pp-fact-detail">{entry.summary}</span>
              </th>
              <td>{ACTOR_WORDS[entry.actor]}</td>
              <td>
                <UndoCell entry={entry} onRestore={onRestore} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}

/**
 * Restore as draft: one request at a time, each snapshot's key and body kept until the core
 * answers, so a resend after a lost answer replays the first restore instead of making a
 * second. A restored draft opens in the Plan tab's Accept gate.
 */
function useRestore(id: number) {
  const client = useQueryClient();
  const navigate = useNavigate();
  const inFlight = useRef(false);
  const pending = useRef(new Map<string, PendingRestore>());
  const mutation = useMutation({
    mutationFn: async (entry: Entry) => {
      let restore = pending.current.get(entry.link.id);
      if (!restore) {
        // The base and draft the restore names are read now, not from an older screen.
        const plan = await client.fetchQuery({ ...planQuery(id), staleTime: 0 });
        restore = {
          key: newKey(),
          body: {
            mode: "draft",
            expected_base: plan.base,
            expected_draft: plan.draft ? identityOf(plan.draft.workspace.draft) : null,
          },
        };
        pending.current.set(entry.link.id, restore);
      }
      const snapshot = { id, snapshot_id: Number(entry.link.id) };
      return writeCore("restoreSnapshotAsDraft", snapshot, restore.body, restore.key);
    },
    retry: (failures: number, error: Error) => failures < 2 && lostSend(error),
    onSuccess: (workspace, entry) => {
      pending.current.delete(entry.link.id);
      // The Plan tab reads the restored draft fresh rather than the draft it last showed.
      client.removeQueries({ queryKey: planKey(id) });
      const state: RestoredState = {
        restored: { draftId: workspace.draft.draft_id, summary: entry.summary },
      };
      void navigate(`/builds/${id}/plan`, { state });
    },
    onError: (error, entry) => {
      // A refusal ends the key; a lost answer keeps it, so Restore again replays it.
      if (!lostSend(error)) pending.current.delete(entry.link.id);
    },
    onSettled: () => {
      inFlight.current = false;
    },
  });
  const start = (entry: Entry) => {
    if (inFlight.current) return;
    inFlight.current = true;
    mutation.mutate(entry);
  };
  return { mutation, start };
}

/**
 * The History tab: the Build's accepted revisions, snapshots, checkoff bursts, sends and
 * assistant decisions from one read, newest first, each with whether it can be undone or
 * restored as a draft.
 */
export function BuildHistory() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const offline = connection === "offline";
  const query = useInfiniteQuery(historyQuery(id));
  const table = useRef<HTMLTableElement>(null);
  const message = useRef<HTMLDivElement>(null);
  const { mutation: restore, start } = useRestore(id);
  // A refused restore leaves the button in place, so focus moves to what happened.
  useEffect(() => {
    if (restore.isError) message.current?.focus();
  }, [restore.isError, restore.submittedAt]);
  const state = pagedScreenState(query, connection, (data) => data.pages[0]?.entries.length === 0);
  const missing = (error: Error | null) =>
    error instanceof CoreError && error.status === 404 && error.code !== "snapshot_not_found";
  if ((missing(query.error) && !query.data) || missing(restore.error)) {
    return <NotFound />;
  }
  return (
    <ScreenTemplate
      title="History"
      subnav={<BuildTabs id={id} />}
      state={state}
      renderData={(data) => (
        <>
          <div ref={message} tabIndex={-1}>
            {restore.isPending && <p role="status">Restoring {restore.variables.summary}.</p>}
            {restore.isError &&
              (lostSend(restore.error) ? (
                <div role="alert" className="pp-banner" data-kind="stale">
                  <p>
                    The restore of {restore.variables?.summary} may not have reached the core.
                    Restore it again to check; it will not make a second draft.
                  </p>
                  <p className="pp-banner-detail">{restore.error.message}</p>
                </div>
              ) : (
                <div role="alert" className="pp-banner" data-kind="error">
                  <p>Could not restore {restore.variables?.summary}.</p>
                  <p className="pp-banner-detail">{restoreRefusal(restore.error)}</p>
                </div>
              ))}
          </div>
          <HistoryTable
            entries={data.pages.flatMap((page) => page.entries)}
            onRestore={(entry) => !offline && start(entry)}
            table={table}
          />
          <OlderEntries query={query} table={table} />
        </>
      )}
      empty={{ message: "Nothing has happened to this Build yet." }}
    />
  );
}
