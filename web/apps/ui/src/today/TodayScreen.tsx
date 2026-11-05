import type {
  CheckoffResultList,
  FarmPrinterList,
  VerifyBatchReceipt,
} from "@print-partner/rust-client";
import { useMutation, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { Link } from "react-router";
import { CoreError } from "../core/client";
import { useModule } from "../core/modules";
import { readCore, writeCore } from "../core/rust";
import { tasksQuery, type Task } from "../core/workflow";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";

type CheckoffResult = CheckoffResultList["results"][number];
type FarmPrinter = FarmPrinterList["printers"][number];
type Unit = CheckoffResult["units"][number];

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

/** A bed waiting for a person: an awaiting-verify result and the units it has not resolved. */
interface Bed {
  result: CheckoffResult;
  printer: string;
  pending: Unit[];
}

const unitKey = (u: { part_id: number; unit_index: number }) => `${u.part_id}:${u.unit_index}`;

/** Results waiting for a person, oldest first, each with its pending units. */
function bedsToClear(
  results: readonly CheckoffResult[],
  printerName: (id: string) => string,
): Bed[] {
  return results
    .filter((r) => r.state === "awaiting_verify")
    .map((result) => {
      const resolved = new Set(result.resolved_units.map(unitKey));
      return {
        result,
        printer: printerName(result.printer_id),
        pending: result.units.filter((u) => !resolved.has(unitKey(u))),
      };
    })
    .filter((bed) => bed.pending.length > 0)
    .sort((a, b) => a.result.created_at.localeCompare(b.result.created_at));
}

/** "A", "A and B", "A, B and C". */
const listWords = (words: readonly string[]) =>
  words.length < 2 ? words.join("") : `${words.slice(0, -1).join(", ")} and ${words.at(-1)}`;

const plural = (n: number, one: string) => `${n} ${n === 1 ? one : `${one}s`}`;

/** What confirming a bed marks, one line per Part: its file name, the count and the unit numbers. */
function bedDiff(pending: readonly Unit[]) {
  const parts = new Map<number, { name: string; units: number[] }>();
  for (const unit of pending) {
    const part = parts.get(unit.part_id) ?? {
      name: unit.part_filename ?? `Part ${unit.part_id}`,
      units: [],
    };
    part.units.push(unit.unit_index + 1);
    parts.set(unit.part_id, part);
  }
  return [...parts.entries()].map(([id, { name, units }]) => ({
    id,
    text: `${name}: +${units.length} printed (${units.length === 1 ? "unit" : "units"} ${listWords(units.map(String))})`,
  }));
}

/** A confirm in flight: the bed and the idempotency key every resend of it reuses. */
interface Confirm {
  bed: Bed;
  key: string;
}

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
const lostSend = (error: Error) => !(error instanceof CoreError);
const retryLostSend = (failures: number, error: Error) => failures < 2 && lostSend(error);
const resendable = (error: Error) => !(error instanceof CoreError && error.status < 500);
// A resent undo whose first answer was lost finds the burst already undone.
const ALREADY_UNDONE = "This Checkoff burst was already undone";

function useCheckoff() {
  const client = useQueryClient();
  const [tally, setTally] = useState<Tally | null>(null);
  // The key of each bed's confirm that has no answer yet. Confirming that bed again reuses
  // it, so a write that landed while its answer was lost replays its receipt and Undo.
  const unanswered = useRef(new Map<string, string>());
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
    mutationFn: ({ bed, key }: Confirm) =>
      writeCore(
        "verifyCheckoffBurst",
        {},
        {
          idempotency_key: key,
          beds: [
            {
              link_id: bed.result.id,
              decisions: bed.pending.map((u) => ({
                part_id: u.part_id,
                unit_index: u.unit_index,
                result: "confirmed" as const,
              })),
            },
          ],
        },
      ),
    retry: retryLostSend,
    onError: (error, { bed }) => {
      if (lostSend(error)) return;
      unanswered.current.delete(bed.result.id);
      return refresh();
    },
    onSuccess: (receipt, { bed }) => {
      unanswered.current.delete(bed.result.id);
      undo.reset();
      setTally({ kind: "confirmed", undo: { printer: bed.printer, receipt } });
      return refresh();
    },
  });
  const confirmBed = (bed: Bed) => {
    if (confirm.isPending) return;
    const key = unanswered.current.get(bed.result.id) ?? crypto.randomUUID();
    unanswered.current.set(bed.result.id, key);
    confirm.mutate({ bed, key });
  };
  return { confirm, confirmBed, undo, tally };
}

type Checkoff = ReturnType<typeof useCheckoff>;

/** The live tally, Undo, and a Retry that resends the same request with the same key. */
function CheckoffStatus({ checkoff }: { checkoff: Checkoff }) {
  const { confirm, undo, tally } = checkoff;
  const failure = confirm.isError
    ? {
        what: "Could not confirm the bed.",
        error: confirm.error,
        resend: () => confirm.mutate(confirm.variables),
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
              <MutationButton onClick={() => !undo.isPending && undo.mutate(tally.undo)}>
                Undo
              </MutationButton>
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

/** The hero task: clear the beds waiting for a person, with the reason and what each confirm marks. */
function ClearBeds({
  beds,
  linkDown,
  checkoff,
}: {
  beds: Bed[];
  linkDown: (printerId: string) => boolean;
  checkoff: Checkoff;
}) {
  return (
    <section className="pp-hero" aria-labelledby="pp-hero-title">
      <h2 id="pp-hero-title">{`Clear ${plural(beds.length, "bed")} and confirm what came off`}</h2>
      <p className="pp-hero-reason">{`Frees ${listWords([...new Set(beds.map((b) => b.printer))])}.`}</p>
      <ul className="pp-beds">
        {beds.map((bed) => (
          <li key={bed.result.id} className="pp-bed" data-link={bed.result.id}>
            <h3>{bed.printer}</h3>
            <p className="pp-muted">
              {bed.result.filename}
              {linkDown(bed.result.printer_id) &&
                ". Printer link down, so confirm by hand what came off."}
            </p>
            <ul aria-label={`What confirming ${bed.printer} marks`} className="pp-diff">
              {bedDiff(bed.pending).map((line) => (
                <li key={line.id}>{line.text}</li>
              ))}
            </ul>
            <MutationButton primary onClick={() => checkoff.confirmBed(bed)}>
              {`Confirm ${bed.printer} bed`}
            </MutationButton>
          </li>
        ))}
      </ul>
    </section>
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
}: {
  results: CheckoffResult[];
  printerName: (id: string) => string;
}) {
  if (results.length === 0) return null;
  return (
    <section aria-labelledby="pp-unseen-title">
      <h2 id="pp-unseen-title">Prints the app cannot see finish</h2>
      <ul className="pp-unseen">
        {results.map((r) => (
          <li key={r.id}>
            {`${printerName(r.printer_id)}: ${r.filename}. Its printer link is down, so check it off by hand in the `}
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

function RankedTasks({
  query,
  skipTop,
}: {
  query: UseQueryResult<{ tasks: Task[] }>;
  skipTop: boolean;
}) {
  if (query.isError) {
    return (
      <div role="alert" className="pp-banner" data-kind="error">
        <p>Could not load ranked tasks.</p>
        <p className="pp-banner-detail">{query.error.message}</p>
        <button type="button" className="pp-button" onClick={() => void query.refetch()}>
          Retry
        </button>
      </div>
    );
  }
  if (!query.data) return <p className="pp-muted">Loading ranked tasks.</p>;
  const tasks = skipTop ? query.data.tasks.slice(1) : query.data.tasks;
  if (tasks.length === 0) return <p className="pp-muted">No other open tasks.</p>;
  return (
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
  // Without a farm read that names the printer, nothing shows its link is up.
  const linkDown = (id: string) => {
    const printer = byId.get(id);
    return !printer || LINK_DOWN.has(printer.state.execution);
  };
  return (
    <ScreenTemplate
      title="Today"
      state={queryScreenState(results, connection)}
      renderData={(data) => {
        const beds = bedsToClear(data.results, printerName);
        const unseen = data.results.filter((r) => r.state === "watching" && linkDown(r.printer_id));
        const top = tasks.data?.tasks[0];
        return (
          <>
            <CheckoffStatus checkoff={checkoff} />
            {beds.length > 0 ? (
              <ClearBeds beds={beds} linkDown={linkDown} checkoff={checkoff} />
            ) : (
              top && <TopTask task={top} />
            )}
            <UnseenPrints results={unseen} printerName={printerName} />
            <h2>Ranked tasks</h2>
            <RankedTasks query={tasks} skipTop={beds.length === 0} />
          </>
        );
      }}
      empty={{ message: "Nothing waits for you." }}
    />
  );
}
