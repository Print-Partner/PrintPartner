import type { BuildHistoryPage, RestoreSnapshotRequest } from "@print-partner/rust-client";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import { useNavigate, useOutletContext } from "react-router";
import { CoreError } from "../core/client";
import { readCore, writeCore } from "../core/rust";
import { lostSend, newKey } from "../core/writes";
import { useConnection } from "../frame/connection";
import { UndoOutcome, useBurstUndo } from "../screen/BurstUndo";
import { OlderEntries, pagedScreenState } from "../screen/OlderEntries";
import { MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
import { NotFound } from "../screen/screens";
import { planKey, planQuery, type RestoredState } from "./BuildPlan";
import { BuildTabs, type WorkspaceContext } from "./BuildWorkspace";
import { dateText } from "./format";
import { identityOf } from "./plan";
import { HISTORY_EMPTY } from "./planCopy";

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

/** The Plan could not be read before a restore, so nothing was sent. */
class PlanReadError extends Error {
  constructor(cause: Error) {
    super(`Could not read the Plan before restoring: ${cause.message}`);
    this.name = "PlanReadError";
  }
}

/** A sent restore whose answer was lost: it may have landed. */
const restoreLost = (error: Error) => lostSend(error) && !(error instanceof PlanReadError);

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

function UndoCell({
  entry,
  onUndo,
  onRestore,
}: {
  entry: Entry;
  onUndo: (entry: Entry) => void;
  onRestore: (entry: Entry) => void;
}) {
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
  if (undo.undoable) {
    return (
      <>
        <MutationButton onClick={() => onUndo(entry)}>Undo</MutationButton>
        {undo.scope && <span className="pp-fact-detail">{undo.scope}</span>}
      </>
    );
  }
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
  onRestore,
  table,
}: {
  entries: Entry[];
  onUndo: (entry: Entry) => void;
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
                <UndoCell entry={entry} onUndo={onUndo} onRestore={onRestore} />
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
        const plan = await client
          .fetchQuery({ ...planQuery(id), staleTime: 0 })
          .catch((error: Error) => {
            // A missing Build still answers 404, so the tab shows Not found.
            throw error instanceof CoreError ? error : new PlanReadError(error);
          });
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
    retry: (failures: number, error: Error) => failures < 2 && restoreLost(error),
    onSuccess: (workspace, entry) => {
      pending.current.delete(entry.link.id);
      // A replay answers the draft the first restore made, which may since be accepted or
      // set aside. Then there is nothing to accept.
      if (workspace.draft.state !== "open") {
        setClosed(entry.summary);
        return;
      }
      // The Plan tab reads the restored draft fresh rather than the draft it last showed.
      client.removeQueries({ queryKey: planKey(id) });
      const state: RestoredState = {
        restored: { draftId: workspace.draft.draft_id, summary: entry.summary },
      };
      void navigate(`/builds/${id}/plan`, { state });
    },
    onError: (error, entry) => {
      // A refusal ends the key and may mean the snapshot changed, so History reloads. A lost
      // answer keeps the key, so Restore again replays it.
      if (restoreLost(error)) return;
      pending.current.delete(entry.link.id);
      void client.invalidateQueries({ queryKey: ["core", "history", id] });
    },
    onSettled: () => {
      inFlight.current = false;
    },
  });
  const [closed, setClosed] = useState<string | null>(null);
  const start = (entry: Entry) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setClosed(null);
    mutation.mutate(entry);
  };
  return { mutation, start, closed };
}

/**
 * The History tab: the Build's accepted revisions, snapshots, checkoff bursts, sends and
 * assistant decisions from one read, newest first, with Undo on each checkoff burst the
 * core says it can undo now and Restore as draft on each restorable snapshot.
 */
export function BuildHistory() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const client = useQueryClient();
  const offline = connection === "offline";
  const query = useInfiniteQuery(historyQuery(id));
  const table = useRef<HTMLTableElement>(null);
  const { undo, start, message } = useBurstUndo(
    (entry: Entry) => writeCore("undoBuildHistoryEntry", { id, entry_id: entry.id }),
    // An undo changes Checkoff progress on every Build a printer burst touched, so the
    // Build headers, Checkoff, Today and the farm log reload with History.
    () => client.invalidateQueries({ queryKey: ["core"] }),
    // A printer burst's undo deletes its entry, so a resend after a lost answer finds none.
    // A manual burst stays listed after its undo, so its 404 proves nothing.
    (error, entry) =>
      entry.type === "checkoff_burst" &&
      !entry.id.startsWith("checkoff_burst:manual-") &&
      error.status === 404 &&
      error.detail === "History entry not found",
  );
  const restoreMessage = useRef<HTMLDivElement>(null);
  const { mutation: restore, start: startRestore, closed } = useRestore(id);
  // A refused restore leaves the button in place, so focus moves to what happened.
  useEffect(() => {
    if (restore.isError || closed) restoreMessage.current?.focus();
  }, [restore.isError, closed, restore.submittedAt]);
  const state = pagedScreenState(query, connection, (data) => data.pages[0]?.entries.length === 0);
  const missing = (error: Error | null) =>
    error instanceof CoreError && error.status === 404 && error.code !== "snapshot_not_found";
  if ((missing(query.error) && !query.data) || missing(restore.error)) {
    return <NotFound />;
  }
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
          <div ref={restoreMessage} tabIndex={-1}>
            {restore.isPending && <p role="status">Restoring {restore.variables.summary}.</p>}
            {closed && (
              <p role="status">
                {closed} was already restored, and that draft is no longer open. Open the Plan tab
                to see the current draft.
              </p>
            )}
            {restore.isError &&
              (restoreLost(restore.error) ? (
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
            onUndo={start}
            onRestore={(entry) => !offline && startRestore(entry)}
            table={table}
          />
          <OlderEntries query={query} table={table} />
        </>
      )}
      empty={{ message: HISTORY_EMPTY }}
    />
  );
}
