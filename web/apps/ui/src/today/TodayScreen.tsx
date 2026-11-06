import type {
  CheckoffResultList,
  FarmPrinterList,
  VerifyBatchReceipt,
} from "@print-partner/rust-client";
import { useMutation, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { CoreError } from "../core/client";
import { useModule } from "../core/modules";
import { readCore, RequestContractError, writeCore } from "../core/rust";
import { tasksQuery, type Task } from "../core/workflow";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
import {
  bedsBlocking,
  bedsToClear,
  MAX_BURST_BEDS,
  pickedPrinters,
  plural,
  unitKey,
  type Bed,
  type Pick,
  type Unit,
} from "./beds";
import { ClearBeds } from "./ClearBeds";

type CheckoffResult = CheckoffResultList["results"][number];
type FarmPrinter = FarmPrinterList["printers"][number];

/** Today rereads results and printer links this often, so a print that ends or a link that drops shows up. */
const REFRESH_MS = 30_000;

const checkoffQuery = {
  queryKey: ["core", "checkoff"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listPrinterCheckoff", {}, signal),
  refetchInterval: REFRESH_MS,
} as const;

const printersQuery = {
  queryKey: ["core", "printers"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listPrinters", {}, signal),
  refetchInterval: REFRESH_MS,
} as const;

/** Execution states in which a printer's link cannot report that a print ended. */
const LINK_DOWN = new Set<FarmPrinter["state"]["execution"]>([
  "unreachable",
  "not_set_up",
  "manual",
  "unknown",
]);

/** A burst in flight: the picked units and the idempotency key every resend of it reuses. */
interface Confirm {
  picks: Pick[];
  key: string;
}

/** Names a burst by the units it confirms, so the same burst finds its unanswered key. */
const burstId = (picks: readonly Pick[]) =>
  picks.map((p) => `${p.bed.result.id}=${p.units.map(unitKey).join(",")}`).join("|");

interface Undo {
  printer: string;
  receipt: VerifyBatchReceipt;
}

/** The last thing a confirm or undo did, for the live region and the Undo button. */
type Tally = { kind: "confirmed"; undo: Undo } | { kind: "undone"; printer: string; units: number };

const tallyText = (t: Tally) =>
  t.kind === "confirmed"
    ? `Confirmed ${plural(t.undo.receipt.units_confirmed, "unit")} from ${t.undo.printer}.`
    : `Undid the ${t.printer} confirmation. ${plural(t.units, "unit")} ${t.units === 1 ? "is" : "are"} waiting again.`;

// A send that never reached the core, or whose answer was lost, is resent with the same
// key; the core answers a repeat with the first receipt and writes nothing.
// A body the client refused to send was not lost; resending it fails the same way.
const lostSend = (error: Error) =>
  !(error instanceof CoreError) && !(error instanceof RequestContractError);
const retryLostSend = (failures: number, error: Error) => failures < 2 && lostSend(error);
const resendable = (error: Error) =>
  !(error instanceof RequestContractError) && !(error instanceof CoreError && error.status < 500);
// A resent undo whose first answer was lost finds the burst already undone.
const ALREADY_UNDONE = "This Checkoff burst was already undone";

function useCheckoff() {
  const client = useQueryClient();
  const [tally, setTally] = useState<Tally | null>(null);
  // The key of each burst that has no answer yet. Confirming the same units again reuses
  // it, so a write that landed while its answer was lost replays its receipt and Undo.
  const unanswered = useRef(new Map<string, { key: string; picks: Pick[] }>());
  // A saved key lasts only while a reread still lists every unit of its burst as pending.
  // Once one is gone, confirming the same units later is a new write with a new key, so a
  // burst that was undone elsewhere is never answered with its old receipt.
  const keepWaiting = useCallback((beds: readonly Bed[]) => {
    const pendingKey = (bed: Bed, u: Unit) => `${bed.result.id}=${unitKey(u)}`;
    const waiting = new Set(beds.flatMap((bed) => bed.pending.map((u) => pendingKey(bed, u))));
    for (const [id, { picks }] of unanswered.current) {
      const listed = picks.every(({ bed, units }) =>
        units.every((u) => waiting.has(pendingKey(bed, u))),
      );
      if (!listed) unanswered.current.delete(id);
    }
  }, []);
  const refresh = () =>
    Promise.all([
      client.invalidateQueries({ queryKey: checkoffQuery.queryKey }),
      client.invalidateQueries({ queryKey: tasksQuery.queryKey }),
    ]);
  const undo = useMutation({
    mutationFn: async ({ receipt }: Undo) => {
      try {
        await writeCore("undoCheckoffBurst", {}, { undo_token: receipt.undo_token });
      } catch (error) {
        if (!(
          error instanceof CoreError &&
          error.status === 409 &&
          error.detail === ALREADY_UNDONE
        )) {
          throw error;
        }
      }
    },
    retry: retryLostSend,
    onError: () => refresh(),
    onSuccess: (_, { printer, receipt }) => {
      setTally({
        kind: "undone",
        printer,
        units: receipt.units_confirmed + receipt.units_rejected,
      });
      return refresh();
    },
  });
  const confirm = useMutation({
    mutationFn: ({ picks, key }: Confirm) =>
      writeCore(
        "verifyCheckoffBurst",
        {},
        {
          idempotency_key: key,
          beds: picks.map(({ bed, units }) => ({
            link_id: bed.result.id,
            decisions: units.map((u) => ({
              part_id: u.part_id,
              unit_index: u.unit_index,
              result: "confirmed" as const,
            })),
          })),
        },
      ),
    retry: retryLostSend,
    onMutate: () => undo.reset(),
    onError: (error, { picks }) => {
      if (lostSend(error)) return;
      unanswered.current.delete(burstId(picks));
      return refresh();
    },
    onSuccess: (receipt, { picks }) => {
      unanswered.current.delete(burstId(picks));
      undo.reset();
      setTally({ kind: "confirmed", undo: { printer: pickedPrinters(picks), receipt } });
      return refresh();
    },
  });
  /**
   * Sends the picks as one burst, and calls `onConfirmed` once the core confirms it. Picks
   * that touch a bed of a burst that got no answer resend that burst with its key instead,
   * so a write that landed replays its receipt and Undo rather than being refused.
   */
  const confirmPicks = (picks: Pick[], onConfirmed?: () => void) => {
    if (confirm.isPending || picks.length === 0 || picks.length > MAX_BURST_BEDS) return;
    const beds = new Set(picks.map((p) => p.bed.result.id));
    const lost = [...unanswered.current.entries()].find(([, u]) =>
      u.picks.some((p) => beds.has(p.bed.result.id)),
    );
    if (lost) {
      const [id, { key, picks: sent }] = lost;
      confirm.mutate(
        { picks: sent, key },
        { onSuccess: id === burstId(picks) ? onConfirmed : undefined },
      );
      return;
    }
    const key = crypto.randomUUID();
    unanswered.current.set(burstId(picks), { key, picks });
    confirm.mutate({ picks, key }, { onSuccess: onConfirmed });
  };
  // Only the newest write's failure shows, so an old confirm error never hides a failed Undo.
  const undoBurst = (tallied: Undo) => {
    if (undo.isPending) return;
    confirm.reset();
    undo.mutate(tallied);
  };
  return { confirm, confirmPicks, keepWaiting, undo, undoBurst, tally };
}

type Checkoff = ReturnType<typeof useCheckoff>;

/**
 * The live tally, Undo, and a Retry. A confirm's Retry goes through `confirmPicks`, so it
 * reuses the saved key only while a reread still lists that bed waiting.
 */
function CheckoffStatus({ checkoff }: { checkoff: Checkoff }) {
  const { confirm, confirmPicks, undo, undoBurst, tally } = checkoff;
  const failure = confirm.isError
    ? {
        what: "Could not confirm what came off.",
        error: confirm.error,
        resend: () => confirmPicks(confirm.variables!.picks),
      }
    : undo.isError
      ? {
          what: "Could not undo the confirmation.",
          error: undo.error,
          resend: () => undo.mutate(undo.variables),
        }
      : null;
  return (
    <>
      <div role="status" aria-label="Checkoff tally" className="pp-tally">
        {tally && (
          <p>
            {tallyText(tally)}{" "}
            {tally.kind === "confirmed" && (
              <MutationButton onClick={() => undoBurst(tally.undo)}>Undo</MutationButton>
            )}
          </p>
        )}
      </div>
      {failure && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>{failure.what}</p>
          <p className="pp-banner-detail">{failure.error.message}</p>
          {resendable(failure.error) && (
            <MutationButton onClick={failure.resend}>Retry</MutationButton>
          )}
        </div>
      )}
    </>
  );
}

/** The hero when no bed waits: the top-ranked task, its reason and what it covers. */
function TopTask({ task }: { task: Task }) {
  const { unit_count, item_count, issue_count } = task.next_action;
  const counts = [
    unit_count !== null && plural(unit_count, "unit"),
    item_count !== null && plural(item_count, "item"),
    issue_count !== null && plural(issue_count, "issue"),
  ].filter((c) => c !== false);
  return (
    <section className="pp-hero" aria-labelledby="pp-hero-title">
      <h2 id="pp-hero-title">{task.next_action.label}</h2>
      <p className="pp-hero-reason">
        <Link to={`/builds/${task.plan_id}`} className="pp-link">
          {task.build_name}
        </Link>
        {`: ${task.next_action.reason}`}
      </p>
      {counts.length > 0 && <p className="pp-diff">{counts.join(", ")}</p>}
    </section>
  );
}

/** Prints a down printer was watching: kept on Today so no job drops out of sight. */
function UnseenPrints({
  results,
  printerName,
  linkNote,
}: {
  results: CheckoffResult[];
  printerName: (id: string) => string;
  linkNote: (printerId: string) => string | null;
}) {
  if (results.length === 0) return null;
  return (
    <section aria-labelledby="pp-unseen-title">
      <h2 id="pp-unseen-title">Prints the app cannot see finish</h2>
      <ul className="pp-unseen">
        {results.map((r) => (
          <li key={r.id}>
            {`${printerName(r.printer_id)}: ${r.filename}. ${linkNote(r.printer_id)}, so check it off by hand in the `}
            <Link to={`/builds/${r.profile_id}/checkoff`} className="pp-link">
              Build&apos;s Checkoff tab
            </Link>
            .
          </li>
        ))}
      </ul>
    </section>
  );
}

/** The ranked list. A failed reread keeps the last list beside the error and its Retry. */
function RankedTasks({
  query,
  skipTop,
}: {
  query: UseQueryResult<{ tasks: Task[] }>;
  skipTop: boolean;
}) {
  const notice = query.isError && (
    <div role="alert" className="pp-banner" data-kind="error">
      <p>
        {query.data
          ? "Could not refresh ranked tasks. Showing the last list."
          : "Could not load ranked tasks."}
      </p>
      <p className="pp-banner-detail">{query.error.message}</p>
      <button type="button" className="pp-button" onClick={() => void query.refetch()}>
        Retry
      </button>
    </div>
  );
  if (!query.data) return notice || <p className="pp-muted">Loading ranked tasks.</p>;
  const tasks = skipTop ? query.data.tasks.slice(1) : query.data.tasks;
  return (
    <>
      {notice}
      {tasks.length === 0 ? (
        <p className="pp-muted">No other open tasks.</p>
      ) : (
        <ol className="pp-tasks" aria-label="Ranked tasks">
          {tasks.map((task) => (
            <li key={task.plan_id}>
              <Link to={`/builds/${task.plan_id}`} className="pp-link">
                {task.build_name}
              </Link>{" "}
              {task.next_action.label}
              {task.attention && <span className="pp-tag">Needs you</span>}
              <span className="pp-fact-detail">{task.rank_reason}</span>
            </li>
          ))}
        </ol>
      )}
    </>
  );
}

/**
 * Today: the hero task and the ranked list. Beds waiting for a person come first, and
 * each confirms with one verify-batch request and an Undo. With printer links down the
 * same confirm is the manual checkoff, and prints a down printer was watching stay listed.
 */
export function TodayScreen() {
  const connection = useConnection();
  const farm = useModule("farm");
  const results = useQuery(checkoffQuery);
  const tasks = useQuery({ ...tasksQuery, refetchInterval: REFRESH_MS });
  const printers = useQuery({ ...printersQuery, enabled: farm?.state === "on" });
  const checkoff = useCheckoff();
  const byId = new Map((printers.data?.printers ?? []).map((p) => [p.id, p]));
  const printerName = (id: string) => byId.get(id)?.name ?? id;
  const { keepWaiting } = checkoff;
  useEffect(() => {
    if (results.data) keepWaiting(bedsToClear(results.data.results, String));
  }, [results.data, keepWaiting]);
  // Why a printer's link cannot report a finished print, or null when it can. A failed,
  // pending or switched-off farm read says nothing about links, so it never counts as up
  // or as down.
  const linkNote = (id: string) => {
    if (farm && farm.state !== "on") return "The Farm module is off";
    if (printers.isError) return "Printer links could not be read";
    if (!printers.data) return "Printer links not read yet";
    const printer = byId.get(id);
    return !printer || LINK_DOWN.has(printer.state.execution) ? "Printer link down" : null;
  };
  return (
    <ScreenTemplate
      title="Today"
      state={queryScreenState(results, connection)}
      renderData={(data) => {
        const beds = bedsToClear(data.results, printerName);
        const unseen = data.results.filter(
          (r) => r.state === "watching" && linkNote(r.printer_id) !== null,
        );
        const top = tasks.data?.tasks[0];
        return (
          <>
            <CheckoffStatus checkoff={checkoff} />
            {beds.length > 0 ? (
              <ClearBeds
                beds={beds}
                blocking={bedsBlocking(data.results, printerName, beds)}
                linkNote={linkNote}
                confirm={checkoff.confirmPicks}
              />
            ) : (
              top && <TopTask task={top} />
            )}
            <UnseenPrints results={unseen} printerName={printerName} linkNote={linkNote} />
            <h2>Ranked tasks</h2>
            <RankedTasks query={tasks} skipTop={beds.length === 0} />
          </>
        );
      }}
      empty={{ message: "Nothing waits for you." }}
    />
  );
}
