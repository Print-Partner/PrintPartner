import type {
  CheckoffUnits,
  ManualBurstReceipt,
  ManualBurstRequest,
} from "@print-partner/rust-client";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { useOutletContext } from "react-router";
import { CoreError } from "../core/client";
import { readCore, RequestContractError, writeCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { NotFound } from "../screen/screens";
import { MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
import { plural } from "../today/beds";
import { BuildTabs, type WorkspaceContext } from "./BuildWorkspace";
import {
  burstKey,
  diffLines,
  MAX_BURST_UNITS,
  outcome,
  pickable,
  plateName,
  shown,
  slotsOf,
  toggle,
  type CheckoffPart,
  type Field,
  type Picks,
  type Slot,
} from "./checkoff";

const checkoffUnitsQuery = (id: number) => ({
  queryKey: ["core", "checkoff-units", id],
  queryFn: ({ signal }: { signal: AbortSignal }) =>
    readCore("readBuildCheckoffUnits", { id }, signal),
  retry: (failures: number, error: Error) =>
    !(error instanceof CoreError && error.status === 404) && failures < 3,
});

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

// A send that never reached the core, or whose answer was lost, is resent with the same key;
// the core answers a repeat with the first receipt and writes nothing. A refusal is not.
const lostSend = (error: Error) =>
  !(error instanceof CoreError) && !(error instanceof RequestContractError);
const resendable = (error: Error) =>
  !(error instanceof RequestContractError) && !(error instanceof CoreError && error.status < 500);
const reason = (error: Error) => (error instanceof CoreError && error.detail) || error.message;
// A resent undo whose first answer was lost finds the burst already undone.
const ALREADY_UNDONE = "This Checkoff burst was already undone";

/** The last write, for the live tally and the Undo button. */
type Tally =
  | { kind: "recorded"; receipt: ManualBurstReceipt; lines: string[] }
  | { kind: "undone"; lines: string[] };

const receiptText = (r: ManualBurstReceipt) =>
  [
    r.printed_added && `+${r.printed_added} printed`,
    r.printed_removed && `\u2212${r.printed_removed} printed`,
    r.assembled_added && `+${r.assembled_added} assembled`,
    r.assembled_removed && `\u2212${r.assembled_removed} assembled`,
  ]
    .filter(Boolean)
    .join(", ") || "nothing changed";

function useBursts(id: number, onRecorded: (sent: ManualBurstRequest) => void) {
  const client = useQueryClient();
  const [tally, setTally] = useState<Tally | null>(null);
  const reread = () => client.invalidateQueries({ queryKey: checkoffUnitsQuery(id).queryKey });
  const record = useMutation({
    mutationFn: (burst: { body: ManualBurstRequest; lines: string[] }) =>
      writeCore("recordManualCheckoffBurst", { id }, burst.body),
    retry: (failures: number, error: Error) => failures < 2 && lostSend(error),
    onSuccess: (receipt, { body, lines }) => {
      setTally({ kind: "recorded", receipt, lines });
      onRecorded(body);
    },
    onSettled: reread,
  });
  // Undo tokens whose send was lost: the core may have undone the burst already.
  const lostUndos = useRef(new Set<string>());
  const undo = useMutation({
    mutationFn: async ({ undo_token }: ManualBurstReceipt) => {
      try {
        await writeCore("undoManualCheckoffBurst", { id }, { undo_token });
      } catch (error) {
        if (error instanceof Error && lostSend(error)) lostUndos.current.add(undo_token);
        const landed =
          error instanceof CoreError &&
          error.detail === ALREADY_UNDONE &&
          lostUndos.current.has(undo_token);
        if (!landed) throw error;
      }
    },
    // Only the burst this undo named turns undone; a newer burst keeps its Undo.
    onSuccess: (_, { undo_token }) =>
      setTally((t) =>
        t?.kind === "recorded" && t.receipt.undo_token === undo_token
          ? { kind: "undone", lines: t.lines }
          : t,
      ),
    onSettled: reread,
  });
  return { record, undo, tally };
}

/** The picker: Parts and their units, with the burst keys on each unit. */
function UnitPicker({
  parts,
  field,
  picks,
  setPicks,
}: {
  parts: CheckoffPart[];
  field: Field;
  picks: Picks;
  setPicks: (next: Picks) => void;
}) {
  const [anchor, setAnchor] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const slots = slotsOf(parts);
  const find = (token: string | undefined) => slots.find((s) => s.unit.token === token) ?? null;

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const slot = find((event.target as HTMLElement).dataset.token);
    if (!slot) return;
    const key = event.key.toLowerCase();
    if (key === "p" && !slot.unit.plate) {
      event.preventDefault();
      setNotice("This unit is on no Plate of the accepted layout, so P has nothing to mark.");
      return;
    }
    const next = burstKey(key, slot, slots, field, picks);
    if (!next) return;
    event.preventDefault();
    setNotice(null);
    setPicks(next);
  };

  const onClick = (event: MouseEvent<HTMLInputElement>, slot: Slot) => {
    setPicks(
      toggle(
        slot,
        { anchor: find(anchor ?? undefined), shift: event.shiftKey },
        slots,
        field,
        picks,
      ),
    );
    setAnchor(slot.unit.token);
  };

  return (
    <div className="pp-checkoff-parts" onKeyDown={onKeyDown}>
      {notice && <p className="pp-burst-needs">{notice}</p>}
      {parts.map((part) => (
        <fieldset key={part.part_id} className="pp-units">
          <legend>
            {part.filename}{" "}
            <span className="pp-muted">
              <span className="num">{part.printed_count}</span> of{" "}
              <span className="num">{part.quantity}</span> printed,{" "}
              <span className="num">{part.assembled_count}</span> assembled
            </span>
          </legend>
          {part.units.map((unit) => {
            const slot = slots.find((s) => s.unit === unit)!;
            const disabled = !pickable(field, unit);
            return (
              <label
                key={unit.token}
                className="pp-unit"
                data-picked={picks.has(unit.token) || undefined}
              >
                <input
                  type="checkbox"
                  data-token={unit.token}
                  aria-label={`${part.filename} unit ${unit.unit_index + 1}`}
                  checked={shown(field, picks, unit)}
                  disabled={disabled}
                  onChange={() => {}}
                  onClick={(event) => onClick(event, slot)}
                />
                {`Unit ${unit.unit_index + 1}`}
                {unit.plate && <span className="pp-muted"> {plateName(unit.plate)}</span>}
                {disabled && <span className="pp-muted"> not printed yet</span>}
              </label>
            );
          })}
        </fieldset>
      ))}
    </div>
  );
}

function CheckoffBody({ id, data }: { id: number; data: CheckoffUnits }) {
  const [field, setField] = useState<Field>("printed");
  const [picks, setPicks] = useState<Picks>(new Map());
  // The key of the burst whose answer has not come back; a Retry resends it unchanged.
  const [pending, setPending] = useState<{ body: ManualBurstRequest; lines: string[] } | null>(
    null,
  );
  // A recorded burst drops only the picks it sent; a pick added or changed while it was in
  // flight stays for the next burst.
  const { record, undo, tally } = useBursts(id, (sent) => {
    setPending(null);
    setPicks((now) => {
      const next = new Map(now);
      for (const u of sent.units)
        if (next.get(u.token) === (u.printed ?? u.assembled)) next.delete(u.token);
      return next;
    });
  });
  // A reread drops picks that now match the stored state, so a burst that landed while its
  // answer was lost is never picked twice.
  const stored = JSON.stringify(data.parts);
  useEffect(() => {
    const units = new Map(
      (JSON.parse(stored) as CheckoffPart[]).flatMap((p) => p.units.map((u) => [u.token, u])),
    );
    setPicks((now) => {
      const kept = [...now].filter(([token, want]) => {
        const unit = units.get(token);
        return unit !== undefined && unit[field] !== want;
      });
      return kept.length === now.size ? now : new Map(kept);
    });
  }, [stored, field]);

  if (!data.basis) {
    return <p>This Build has no accepted Plan yet, so there are no units to check off.</p>;
  }
  const basis = data.basis;
  const lines = diffLines(data.parts, field, picks);
  const { conflicts } = outcome(data.parts, field, picks);
  const tooMany = picks.size > MAX_BURST_UNITS;
  const busy = record.isPending || undo.isPending;
  const send = () => {
    if (busy || picks.size === 0 || conflicts.length > 0 || tooMany) return;
    const burst = {
      body: {
        idempotency_key: newKey(),
        expected: basis,
        units: [...picks].map(([token, want]) => ({ token, [field]: want })),
      },
      lines: lines.map((l) => l.text),
    };
    setPending(burst);
    // A new burst retires an old undo's failure, so its Retry cannot act on a newer tally.
    undo.reset();
    record.mutate(burst);
  };
  const switchField = (next: Field) => {
    setField(next);
    setPicks(new Map());
  };
  const failure = record.isError
    ? {
        what: "Could not record the burst.",
        error: record.error,
        resend: () => pending && record.mutate(pending),
      }
    : undo.isError
      ? {
          what: "Could not undo the burst.",
          error: undo.error,
          resend: () => undo.mutate(undo.variables),
        }
      : null;

  return (
    <>
      <div role="radiogroup" aria-label="What a burst marks" className="pp-segmented">
        {(["printed", "assembled"] as const).map((f) => (
          <button
            key={f}
            type="button"
            role="radio"
            aria-checked={field === f}
            className="pp-button"
            onClick={() => switchField(f)}
          >
            {f === "printed" ? "Mark printed" : "Mark assembled"}
          </button>
        ))}
      </div>
      {data.plate_revision_id === null && (
        <p className="pp-muted">No accepted Plate layout for this Plan, so P is unavailable.</p>
      )}
      <UnitPicker parts={data.parts} field={field} picks={picks} setPicks={setPicks} />
      <div className="pp-burst">
        <div className="pp-burst-preview" aria-live="polite">
          {lines.length === 0 ? (
            <p className="pp-muted">
              Nothing picked. On a unit: P marks its Plate, F fills its Part, 0 to 9 set a count,
              Space ticks it and Shift with Space or a click picks a run.
            </p>
          ) : (
            <>
              <p>Will record one burst:</p>
              <ul aria-label="What the burst marks" className="pp-diff">
                {lines.map((line) => (
                  <li key={line.id}>{line.text}</li>
                ))}
              </ul>
            </>
          )}
          {conflicts.length > 0 && (
            <p className="pp-burst-needs">{`The picks both print and clear units of ${conflicts.join(", ")}. Printed units fill from the left, so record the clear and the print as two bursts.`}</p>
          )}
          {tooMany && (
            <p className="pp-burst-needs">{`One burst holds at most ${MAX_BURST_UNITS} units, and ${picks.size} are picked.`}</p>
          )}
        </div>
        {lines.length > 0 && conflicts.length === 0 && !tooMany && (
          <MutationButton primary onClick={send}>
            {`Record ${plural(picks.size, "picked unit")}`}
          </MutationButton>
        )}
      </div>
      <div role="status" aria-label="Checkoff tally" className="pp-tally">
        {tally?.kind === "recorded" && (
          <p>
            {`Recorded: ${receiptText(tally.receipt)}.`}{" "}
            <MutationButton onClick={() => !busy && undo.mutate(tally.receipt)}>
              Undo
            </MutationButton>
          </p>
        )}
        {tally?.kind === "undone" && <p>Undid the last burst. The units are back as they were.</p>}
      </div>
      {failure && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>{failure.what}</p>
          <p className="pp-banner-detail">{reason(failure.error)}</p>
          {resendable(failure.error) && (
            <MutationButton onClick={failure.resend}>Retry</MutationButton>
          )}
        </div>
      )}
    </>
  );
}

/**
 * The Checkoff tab: every accepted Required unit with its durable printed and assembled
 * state, picked with P, F, digits, Space and Shift, previewed, and recorded as one burst
 * with one Undo. It needs no printer link and no Spoolman.
 */
export function BuildCheckoff() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const query = useQuery(checkoffUnitsQuery(id));
  if (query.error instanceof CoreError && query.error.status === 404 && !query.data) {
    return <NotFound />;
  }
  return (
    <ScreenTemplate
      title="Checkoff"
      subnav={<BuildTabs id={id} />}
      state={queryScreenState(query, connection)}
      renderData={(data) => <CheckoffBody id={id} data={data} />}
      empty={{ message: "" }}
    />
  );
}
