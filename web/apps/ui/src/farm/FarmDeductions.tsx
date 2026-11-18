import type { DeductionList, DeductionView } from "@print-partner/rust-client";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useRef } from "react";
import { Link } from "react-router";
import { DEDUCTION_STATE, SKIP_REASON, deductionsQuery } from "../core/deductions";
import { useModule } from "../core/modules";
import { readCore, writeCore } from "../core/rust";
import { lostSend, reason } from "../core/writes";
import { ModuleOff, MutationButton } from "../screen/ScreenTemplate";

type Answer = "deducted" | "retry" | "dismiss";
type Asked = { row: DeductionView; answer: Answer };
type Grams = { kind: "known"; grams: number } | { kind: "unknown" };

/**
 * The answers `POST /api/v2/spoolman/deductions/{id}/reconcile` takes for each state.
 * `deducted` is for `unknown` only: a `failed` send never reached Spoolman's stock.
 */
const ANSWERS: Record<string, Answer[]> = {
  unknown: ["deducted", "retry", "dismiss"],
  failed: ["retry", "dismiss"],
};

const BUTTON: Record<Answer, string> = {
  deducted: "Deducted",
  retry: "Retry",
  dismiss: "Dismiss",
};

/** Why a send ended `unknown` or `failed`, from the row's `failure`. */
function failureText(row: DeductionView) {
  switch (row.failure) {
    case "reply_lost":
      return "Spoolman's reply never came back";
    case "interrupted":
      return "Print Partner stopped during the send";
    case "unreachable":
      return "Spoolman could not be reached";
    case "refused":
      return row.failure_status === null
        ? "Spoolman refused it"
        : `Spoolman refused it with HTTP ${row.failure_status}`;
    default:
      return row.failure;
  }
}

const gramsText = (grams: number | null) => (grams === null ? "grams not known" : `${grams} g`);
const spoolText = (row: DeductionView) =>
  row.spool_id === null ? "no spool" : `spool ${row.spool_id}`;
const reportedText = (grams: Grams) => (grams.kind === "known" ? `${grams.grams} g` : "not known");

/** What the screen says after the core took an answer. */
function doneText({ row, answer }: Asked) {
  const what = `${gramsText(row.grams)} from ${spoolText(row)}`;
  if (answer === "deducted") return `Recorded the ${what} as deducted. Nothing was sent.`;
  if (answer === "retry") return `The ${what} will be sent to Spoolman once more.`;
  return `Dismissed the ${what}. Nothing was sent.`;
}

const detailKey = (id: number) => ["core", "spoolman-deduction", id];

/**
 * `GET /api/v2/spoolman/deductions/{id}`: the grams Spoolman reported for the spool before
 * the send, and what it reports now.
 */
function BeforeAndNow({ id }: { id: number }) {
  const detail = useQuery({
    queryKey: detailKey(id),
    queryFn: ({ signal }) => readCore("readSpoolmanDeduction", { id }, signal),
    refetchOnReconnect: false,
  });
  if (detail.isPending) return <span className="pp-fact-detail">Reading Spoolman</span>;
  if (detail.isError) {
    return (
      <span className="pp-fact-detail">
        Could not read the spool's grams: {detail.error.message}.
      </span>
    );
  }
  return (
    <span className="pp-fact-detail">
      Spool before the send: {reportedText(detail.data.grams_before)}. Now:{" "}
      {reportedText(detail.data.grams_now)}.
    </span>
  );
}

function StateCell({ row }: { row: DeductionView }) {
  const words = DEDUCTION_STATE[row.state] ?? row.state;
  if (row.state === "skipped") {
    const why = (row.reason && SKIP_REASON[row.reason]) ?? row.reason;
    return (
      <>
        {words}
        {why && <span className="pp-fact-detail">Because {why}</span>}
      </>
    );
  }
  return (
    <>
      {words}
      {row.failure && <span className="pp-fact-detail">{failureText(row)}</span>}
      {ANSWERS[row.state] && <BeforeAndNow id={row.id} />}
    </>
  );
}

function DeductionTable({
  rows,
  onAnswer,
}: {
  rows: DeductionView[];
  onAnswer: (asked: Asked) => void;
}) {
  return (
    <table className="pp-farm-table" aria-label="Filament deductions">
      <thead>
        <tr>
          <th scope="col">When</th>
          <th scope="col">Spool</th>
          <th scope="col">Grams</th>
          <th scope="col">Build</th>
          <th scope="col">State</th>
          <th scope="col">Answer</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => {
          const at = new Date(row.updated_at);
          return (
            <tr key={row.id} data-deduction={row.id}>
              <td>
                <time dateTime={at.toISOString()}>
                  {new Intl.DateTimeFormat(undefined, {
                    dateStyle: "medium",
                    timeStyle: "short",
                  }).format(at)}
                </time>
              </td>
              <th scope="row">{row.spool_id === null ? "No spool" : `Spool ${row.spool_id}`}</th>
              <td className="num">{row.grams === null ? "Not known" : `${row.grams} g`}</td>
              <td>
                <Link to={`/builds/${row.profile_id}`} className="pp-link num">
                  Build {row.profile_id}
                </Link>
              </td>
              <td>
                <StateCell row={row} />
              </td>
              <td>
                {ANSWERS[row.state]?.map((answer) => (
                  <MutationButton key={answer} onClick={() => onAnswer({ row, answer })}>
                    {BUTTON[answer]}
                  </MutationButton>
                ))}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/**
 * The Farm log's "Filament deductions" list (GRE-411): the newest 100 Spoolman deductions
 * of every Build. An `unknown` or `failed` row shows the spool's grams before the send and
 * now, and takes an answer. The core's answer replaces the row in place, in the cache the
 * Build Overview's Filament section also reads, so nothing reloads.
 */
export function FarmDeductions() {
  const headingId = useId();
  const spoolman = useModule("spoolman");
  const client = useQueryClient();
  // Set before the first send and cleared when it settles, so clicks between renders send
  // nothing: `isPending` is not true until the next render.
  const inFlight = useRef(false);
  // Not read until /health says the Spoolman module is on or degraded.
  const list = useQuery({
    ...deductionsQuery,
    enabled: spoolman !== undefined && spoolman.state !== "off",
  });
  const answer = useMutation({
    mutationFn: ({ row, answer }: Asked) =>
      writeCore("reconcileSpoolmanDeduction", { id: row.id }, { answer }),
    onSuccess: (updated) => {
      client.setQueryData<DeductionList>(
        deductionsQuery.queryKey,
        (old) =>
          old && {
            ...old,
            deductions: old.deductions.map((row) => (row.id === updated.id ? updated : row)),
          },
      );
      client.removeQueries({ queryKey: detailKey(updated.id) });
    },
    // A refused answer means the row changed elsewhere, and a lost one may have landed:
    // either way the list is read again so each row shows the core's state.
    onError: () => client.invalidateQueries({ queryKey: deductionsQuery.queryKey }),
    onSettled: () => {
      inFlight.current = false;
    },
  });
  const start = (asked: Asked) => {
    if (inFlight.current) return;
    inFlight.current = true;
    answer.mutate(asked);
  };
  let content;
  if (spoolman?.state === "off") {
    content = (
      <ModuleOff
        module="Spoolman"
        wouldShow="filament deductions and their answers"
        where="the Farm log"
      />
    );
  } else if (list.isError) {
    content = (
      <p className="pp-muted">
        Could not read deductions: {list.error.message}.{" "}
        <button type="button" className="pp-button" onClick={() => void list.refetch()}>
          Retry
        </button>
      </p>
    );
  } else if (!list.data) {
    content = <p className="pp-muted">Reading deductions</p>;
  } else if (list.data.deductions.length === 0) {
    content = (
      <p className="pp-muted">
        No filament deductions yet. Checking off a linked print records one for each pinned spool.
      </p>
    );
  } else {
    content = (
      <>
        <p className="pp-muted">
          For an unknown or failed deduction, compare the spool's grams before the send with now.
          Deducted records it as applied without calling Spoolman. Retry sends it once more. Dismiss
          leaves Spoolman as it is.
        </p>
        <DeductionTable rows={list.data.deductions} onAnswer={start} />
      </>
    );
  }
  const asked = answer.variables;
  return (
    <section aria-labelledby={headingId}>
      <h2 id={headingId}>Filament deductions</h2>
      {answer.isSuccess && asked && <p role="status">{doneText(asked)}</p>}
      {answer.isError && asked && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>
            {lostSend(answer.error)
              ? `The answer for the ${gramsText(asked.row.grams)} from ${spoolText(asked.row)} may not have reached the core. The list was read again.`
              : `Could not answer the deduction from ${spoolText(asked.row)}.`}
          </p>
          {!lostSend(answer.error) && <p className="pp-banner-detail">{reason(answer.error)}</p>}
        </div>
      )}
      {content}
    </section>
  );
}
