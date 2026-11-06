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
/** One burst: the picks it confirms and its idempotency key. */
interface Burst {
  picks: Pick[];
  key: string;
}

/**
 * One confirm. It sends one burst, or resends every lost burst a reread shows had landed,
 * so each one's receipt and Undo come back.
 */
interface Confirm {
  bursts: Burst[];
  /** Runs only when the core confirms these picks, never on a replayed undone receipt. */
  onConfirmed?: () => void;
}

/** Names a burst by the units it confirms, so the same burst finds its unanswered key. */
const burstId = (picks: readonly Pick[]) =>
  picks.map((p) => `${p.bed.result.id}=${p.units.map(unitKey).join(",")}`).join("|");

/** One pending unit of one bed. */
const pendingKey = (bed: Bed, u: Unit) => `${bed.result.id}=${unitKey(u)}`;

interface Undo {
  printer: string;
  receipt: VerifyBatchReceipt;
}

/** The last thing a confirm or undo did, for the live region and the Undo button. */
type Tally =
  | { kind: "confirmed"; undos: Undo[] }
  | { kind: "undone"; printer: string; units: number }
  | { kind: "replayed"; printer: string };

/**
 * A new idempotency key. `crypto.randomUUID` exists only in secure contexts, and a shared
 * core may be opened over plain HTTP on the LAN, so random bytes build the key there.
 */
const newKey = () =>
  typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) =>
        b.toString(16).padStart(2, "0"),
      ).join("");

const printersOf = (undos: readonly Undo[]) => undos.map((u) => u.printer).join(" and ");

const tallyText = (t: Tally) =>
  t.kind === "confirmed"
    ? `Confirmed ${plural(
        t.undos.reduce((n, u) => n + u.receipt.units_confirmed, 0),
        "unit",
      )} from ${printersOf(t.undos)}.`
    : t.kind === "replayed"
      ? `Nothing was marked on ${t.printer}: that confirm had already been undone elsewhere. Confirm the bed again.`
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
  // Why a Confirm sent nothing: the reread failed, or no picked unit is still waiting.
  const [notice, setNotice] = useState<string | null>(null);
  // The key of each burst that has no answer yet. Confirming the same units again reuses
  // it, so a write that landed while its answer was lost replays its receipt and Undo.
  const unanswered = useRef(new Map<string, { key: string; picks: Pick[] }>());
  // A saved key lasts only while a reread still lists every unit of its burst as pending.
  // Once one is gone, confirming the same units later is a new write with a new key, so a
  // burst that was undone elsewhere is never answered with its old receipt.
  const listed = useRef(new Set<string>());
  const keepWaiting = useCallback((beds: readonly Bed[]) => {
    const waiting = new Set(beds.flatMap((bed) => bed.pending.map((u) => pendingKey(bed, u))));
    listed.current = waiting;
    for (const [id, { picks }] of unanswered.current) {
      const stillPending = picks.every(({ bed, units }) =>
        units.every((u) => waiting.has(pendingKey(bed, u))),
      );
      if (!stillPending) unanswered.current.delete(id);
    }
  }, []);
  const refresh = () =>
    Promise.all([
      client.invalidateQueries({ queryKey: checkoffQuery.queryKey }),
      client.invalidateQueries({ queryKey: tasksQuery.queryKey }),
    ]);
  const undo = useMutation({
    mutationFn: async (undos: Undo[]) => {
      for (const { receipt } of undos) {
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
      }
    },
    retry: retryLostSend,
    onError: () => refresh(),
    onSuccess: (_, undos) => {
      setTally({
        kind: "undone",
        printer: printersOf(undos),
        units: undos.reduce((n, u) => n + u.receipt.units_confirmed + u.receipt.units_rejected, 0),
      });
      return refresh();
    },
  });
  const confirm = useMutation({
    mutationFn: async ({ bursts }: Confirm) => {
      const receipts: VerifyBatchReceipt[] = [];
      for (const { picks, key } of bursts) {
        receipts.push(
          await writeCore(
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
        );
      }
      return receipts;
    },
    retry: retryLostSend,
    onMutate: () => {
      setNotice(null);
      undo.reset();
    },
    onError: (error, { bursts }) => {
      if (lostSend(error)) return;
      for (const { picks } of bursts) unanswered.current.delete(burstId(picks));
      return refresh();
    },
    onSuccess: async (receipts, { bursts, onConfirmed }) => {
      for (const { picks } of bursts) unanswered.current.delete(burstId(picks));
      undo.reset();
      const answeredAt = Date.now();
      await refresh();
      // A resent key whose burst was undone elsewhere gets its old receipt and writes
      // nothing; a reread made after the answer still lists every sent unit as pending.
      const read = client.getQueryState<CheckoffResultList>(checkoffQuery.queryKey);
      const pending =
        read?.data !== undefined && read.status === "success" && read.dataUpdatedAt >= answeredAt
          ? new Set(
              bedsToClear(read.data.results, String).flatMap((bed) =>
                bed.pending.map((u) => pendingKey(bed, u)),
              ),
            )
          : null;
      const replayed = ({ picks }: Burst) =>
        pending !== null &&
        picks.every(({ bed, units }) => units.every((u) => pending.has(pendingKey(bed, u))));
      const undos = bursts.flatMap((burst, i) =>
        replayed(burst) ? [] : [{ printer: pickedPrinters(burst.picks), receipt: receipts[i]! }],
      );
      setTally(
        undos.length === 0
          ? { kind: "replayed", printer: pickedPrinters(bursts.flatMap((b) => b.picks)) }
          : { kind: "confirmed", undos },
      );
      if (undos.length > 0) onConfirmed?.();
    },
  });
  // Set while a confirm rereads before it decides what to send, so a second click waits.
  const deciding = useRef(false);
  /**
   * Sends the picks as one burst, and calls `onConfirmed` once the core confirms it. The
   * same picks as a burst that got no answer reuse its key. Picks that only overlap such a
   * burst reread the results first: if one of those bursts landed, it is resent with its key
   * so the core replays its receipt and Undo; if none did, the picks the reread still lists as
   * pending go as a new burst. If the reread fails, nothing is sent and those bursts keep their
   * keys, so their receipts and Undo stay reachable.
   */
  const confirmPicks = async (picks: Pick[], onConfirmed?: () => void) => {
    // One write at a time: a confirm never resets a pending Undo, and the reverse.
    if (confirm.isPending || undo.isPending || deciding.current) return;
    if (picks.length === 0 || picks.length > MAX_BURST_BEDS) return;
    const id = burstId(picks);
    const saved = unanswered.current.get(id);
    if (saved) {
      confirm.mutate({ bursts: [{ picks, key: saved.key }], onConfirmed });
      return;
    }
    const beds = new Set(picks.map((p) => p.bed.result.id));
    const overlaps = [...unanswered.current.entries()].filter(([, u]) =>
      u.picks.some((p) => beds.has(p.bed.result.id)),
    );
    let sending = picks;
    if (overlaps.length > 0) {
      deciding.current = true;
      setNotice(null);
      try {
        const fresh = await client.fetchQuery({ ...checkoffQuery, staleTime: 0 });
        keepWaiting(bedsToClear(fresh.results, String));
      } catch {
        setNotice(
          `Nothing was sent: could not reread the results to check the earlier confirm on ${pickedPrinters(overlaps.flatMap(([, lost]) => lost.picks))}. Confirm again once the core answers.`,
        );
        return;
      } finally {
        deciding.current = false;
      }
      const landed = overlaps.filter(([lostId]) => !unanswered.current.has(lostId));
      if (landed.length > 0) {
        confirm.mutate({ bursts: landed.map(([, burst]) => burst) });
        return;
      }
      sending = picks
        .map(({ bed, units }) => ({
          bed,
          units: units.filter((u) => listed.current.has(pendingKey(bed, u))),
        }))
        .filter(({ units }) => units.length > 0);
      if (sending.length === 0) {
        setNotice("Nothing was sent: the picked units are no longer waiting.");
        return;
      }
      // A lost burst that never landed and whose units all go again is replaced by this one.
      const resent = new Set(
        sending.flatMap(({ bed, units }) => units.map((u) => pendingKey(bed, u))),
      );
      for (const [lostId, lost] of overlaps) {
        const covered = lost.picks.every(({ bed, units }) =>
          units.every((u) => resent.has(pendingKey(bed, u))),
        );
        if (covered) unanswered.current.delete(lostId);
      }
    }
    const key = newKey();
    unanswered.current.set(burstId(sending), { key, picks: sending });
    confirm.mutate({ bursts: [{ picks: sending, key }], onConfirmed });
  };
  // Retry of a confirm that got no answer. It rereads the results first. While every sent
  // unit is listed as pending, it goes through `confirmPicks` and its saved-key rule, so a
  // burst undone elsewhere becomes a new write. Once one is gone, the write landed, so the
  // same request and key go again and the core replays the receipt.
  const retryConfirm = async () => {
    const sent = confirm.variables!;
    try {
      const fresh = await client.fetchQuery({ ...checkoffQuery, staleTime: 0 });
      keepWaiting(bedsToClear(fresh.results, String));
    } catch {
      // An unreadable reread keeps the last one.
    }
    const [only] = sent.bursts;
    const waiting =
      sent.bursts.length === 1 &&
      only!.picks.every(({ bed, units }) =>
        units.every((u) => listed.current.has(pendingKey(bed, u))),
      );
    if (waiting) await confirmPicks(only!.picks, sent.onConfirmed);
    else confirm.mutate(sent);
  };
  // Only the newest write's failure shows, so an old confirm error never hides a failed Undo.
  const undoBurst = (tallied: Undo[]) => {
    if (undo.isPending || confirm.isPending || deciding.current) return;
    confirm.reset();
    undo.mutate(tallied);
  };
  return { confirm, confirmPicks, keepWaiting, retryConfirm, undo, undoBurst, tally, notice };
}

type Checkoff = ReturnType<typeof useCheckoff>;

/**
 * The live tally, Undo, and a Retry (`retryConfirm` for a confirm, the same request and
 * key for an Undo).
 */
function CheckoffStatus({ checkoff }: { checkoff: Checkoff }) {
  const { confirm, retryConfirm, undo, undoBurst, tally, notice } = checkoff;
  const failure = confirm.isError
    ? {
        what: "Could not confirm what came off.",
        error: confirm.error,
        resend: retryConfirm,
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
              <MutationButton onClick={() => undoBurst(tally.undos)}>Undo</MutationButton>
            )}
          </p>
        )}
      </div>
      {notice && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>{notice}</p>
        </div>
      )}
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
