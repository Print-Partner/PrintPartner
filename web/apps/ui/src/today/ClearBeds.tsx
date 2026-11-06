import { useState, type KeyboardEvent, type MouseEvent } from "react";
import { MutationButton } from "../screen/ScreenTemplate";
import {
  listWords,
  lowerFirst,
  MAX_BURST_BEDS,
  partName,
  pickWords,
  pickedPrinters,
  plural,
  unitDiff,
  unitKey,
  type Bed,
  type Pick,
  type Unit,
} from "./beds";

/** One pending unit on one bed: the selection's item, in bed order then stored order. */
interface Slot {
  key: string;
  bed: Bed;
  unit: Unit;
}

const slotKey = (bed: Bed, unit: Unit) => `${bed.result.id}|${unitKey(unit)}`;

/** The units a selection confirms, grouped by bed in bed order. */
function picksOf(beds: readonly Bed[], selected: ReadonlySet<string>): Pick[] {
  return beds
    .map((bed) => ({ bed, units: bed.pending.filter((u) => selected.has(slotKey(bed, u))) }))
    .filter((pick) => pick.units.length > 0);
}

/**
 * The burst keys, on the focused unit: P picks every unit on its bed (the plate), F every
 * unit of its Part on every bed, and 0 to 9 pick that many of its Part, lowest unit first
 * (Progress fills from the left), dropping the rest of that Part.
 */
function burstKey(key: string, slot: Slot, slots: readonly Slot[], selected: ReadonlySet<string>) {
  const next = new Set(selected);
  const samePart = slots
    .filter((s) => s.unit.part_id === slot.unit.part_id)
    .sort((a, b) => a.unit.unit_index - b.unit.unit_index);
  if (key === "p") {
    for (const s of slots) if (s.bed === slot.bed) next.add(s.key);
  } else if (key === "f") {
    for (const s of samePart) next.add(s.key);
  } else if (/^[0-9]$/.test(key)) {
    samePart.forEach((s, i) => (i < Number(key) ? next.add(s.key) : next.delete(s.key)));
  } else {
    return null;
  }
  return next;
}

/** Says what the selection will mark before it is confirmed, and which lower units it needs. */
function Preview({
  picks,
  needs,
  printing,
  unconfirmed,
  pickNeeds,
}: {
  picks: readonly Pick[];
  needs: readonly Pick[];
  printing: readonly Pick[];
  unconfirmed: readonly Pick[];
  pickNeeds: () => void;
}) {
  const units = picks.flatMap((p) => p.units);
  return (
    <div className="pp-burst-preview" aria-live="polite">
      {units.length === 0 ? (
        <p className="pp-muted">
          Nothing picked. On a unit: P picks its plate, F fills its Part, 0 to 9 set a count, Space
          ticks it and Shift with Space or a click picks a run.
        </p>
      ) : (
        <>
          <p>
            {`Will mark ${plural(units.length, "unit")} on ${plural(picks.length, "bed")} (${pickedPrinters(picks)}):`}
          </p>
          <ul aria-label="What the picked units mark" className="pp-diff">
            {unitDiff(units).map((line) => (
              <li key={line.id}>{line.text}</li>
            ))}
          </ul>
          {needs.length > 0 && (
            <p className="pp-burst-needs">
              {`Progress fills from the left, so the core refuses this until these lower units are picked too: ${pickWords(needs)}. `}
              <button type="button" className="pp-link" onClick={pickNeeds}>
                Pick them too
              </button>
            </p>
          )}
          {unconfirmed.length > 0 && (
            <p className="pp-burst-needs">{`Lower units of these Parts are not confirmed on any print Today shows: ${pickWords(unconfirmed)}. Progress fills from the left, so the core refuses this unless they were completed another way.`}</p>
          )}
          {printing.length > 0 && (
            <p className="pp-burst-needs">
              {`Lower units of these Parts are still printing: ${pickWords(printing)}. Progress fills from the left, so the core refuses this until they come off and are confirmed, unless they were completed another way.`}
            </p>
          )}
        </>
      )}
    </div>
  );
}

/**
 * The hero task: clear the beds waiting for a person. Each bed confirms alone, or units
 * from several beds are picked with P, F, digits, Space and Shift, previewed, and
 * confirmed as one burst: one request and one Undo.
 */
export function ClearBeds({
  beds,
  blocking,
  linkNote,
  confirm,
}: {
  beds: Bed[];
  blocking: { printing: Bed[]; unconfirmed: Bed[] };
  linkNote: (printerId: string) => string | null;
  confirm: (picks: Pick[]) => boolean;
}) {
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const slots: Slot[] = beds.flatMap((bed) =>
    bed.pending.map((unit) => ({ key: slotKey(bed, unit), bed, unit })),
  );
  const selected = new Set([...picked].filter((key) => slots.some((s) => s.key === key)));
  const picks = picksOf(beds, selected);
  const needs = lowerFirst(beds, picks);
  const stillPrinting = lowerFirst(blocking.printing, picks);
  const unconfirmed = lowerFirst(blocking.unconfirmed, picks);
  const pickNeeds = () =>
    setPicked(
      new Set([...selected, ...needs.flatMap((p) => p.units.map((u) => slotKey(p.bed, u)))]),
    );

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const slot = slots.find((s) => s.key === (event.target as HTMLElement).dataset.slot);
    if (!slot) return;
    const next = burstKey(event.key.toLowerCase(), slot, slots, selected);
    if (!next) return;
    event.preventDefault();
    event.stopPropagation();
    setPicked(next);
  };

  // Space and click toggle one unit; with Shift, every unit from the last one toggled
  // takes this unit's new state.
  const onUnitClick = (event: MouseEvent<HTMLInputElement>, slot: Slot) => {
    const on = event.currentTarget.checked;
    const next = new Set(selected);
    const from = slots.findIndex((s) => s.key === anchor);
    const to = slots.indexOf(slot);
    const run =
      event.shiftKey && from >= 0
        ? slots.slice(Math.min(from, to), Math.max(from, to) + 1)
        : [slot];
    for (const s of run) {
      if (on) next.add(s.key);
      else next.delete(s.key);
    }
    setPicked(next);
    setAnchor(slot.key);
  };

  // A send that did not start (one already in flight) keeps the picks.
  const confirmPicked = () => {
    if (confirm(picks)) setPicked(new Set());
  };
  const tooMany = picks.length > MAX_BURST_BEDS;

  return (
    <section className="pp-hero" aria-labelledby="pp-hero-title" onKeyDown={onKeyDown}>
      <h2 id="pp-hero-title">{`Clear ${plural(beds.length, "bed")} and confirm what came off`}</h2>
      <p className="pp-hero-reason">{`Frees ${listWords([...new Set(beds.map((b) => b.printer))])}.`}</p>
      <ul className="pp-beds">
        {beds.map((bed) => {
          const first = lowerFirst(beds, [{ bed, units: bed.pending }]);
          const running = lowerFirst(blocking.printing, [{ bed, units: bed.pending }]);
          const elsewhere = lowerFirst(blocking.unconfirmed, [{ bed, units: bed.pending }]);
          return (
            <li key={bed.result.id} className="pp-bed" data-link={bed.result.id}>
              <h3>{bed.printer}</h3>
              <p className="pp-muted">
                {bed.result.filename}
                {linkNote(bed.result.printer_id) &&
                  `. ${linkNote(bed.result.printer_id)}, so confirm by hand what came off.`}
              </p>
              <ul aria-label={`What confirming ${bed.printer} marks`} className="pp-diff">
                {unitDiff(bed.pending).map((line) => (
                  <li key={line.id}>{line.text}</li>
                ))}
              </ul>
              {first.length > 0 && (
                <p className="pp-muted">
                  {`Progress fills from the left, so confirm ${pickWords(first)} first, or pick those units with this bed below.`}
                </p>
              )}
              {elsewhere.length > 0 && (
                <p className="pp-muted">{`Lower units of these Parts are not confirmed on any print Today shows: ${pickWords(elsewhere)}. Progress fills from the left, so the core refuses this unless they were completed another way.`}</p>
              )}
              {running.length > 0 && (
                <p className="pp-muted">
                  {`Lower units of these Parts are still printing: ${pickWords(running)}. Progress fills from the left, so the core refuses this until they come off and are confirmed, unless they were completed another way.`}
                </p>
              )}
              <MutationButton primary onClick={() => confirm([{ bed, units: bed.pending }])}>
                {`Confirm ${bed.printer} bed`}
              </MutationButton>
              <fieldset className="pp-units">
                <legend>{`Pick units on ${bed.printer}`}</legend>
                {slots
                  .filter((s) => s.bed === bed)
                  .map((slot) => (
                    <label key={slot.key} className="pp-unit">
                      <input
                        type="checkbox"
                        data-slot={slot.key}
                        checked={selected.has(slot.key)}
                        onChange={() => {}}
                        onClick={(event) => onUnitClick(event, slot)}
                      />
                      {`${partName(slot.unit)} unit ${slot.unit.unit_index + 1}`}
                    </label>
                  ))}
              </fieldset>
            </li>
          );
        })}
      </ul>
      <div className="pp-burst">
        <Preview
          picks={picks}
          needs={needs}
          printing={stillPrinting}
          unconfirmed={unconfirmed}
          pickNeeds={pickNeeds}
        />
        {tooMany && (
          <p className="pp-burst-needs">
            {`One burst holds at most ${MAX_BURST_BEDS} beds, and ${picks.length} are picked. Untick some beds and confirm them in a second burst.`}
          </p>
        )}
        {picks.length > 0 && !tooMany && (
          <MutationButton primary onClick={confirmPicked}>
            {`Confirm ${plural(
              picks.reduce((n, p) => n + p.units.length, 0),
              "picked unit",
            )}`}
          </MutationButton>
        )}
      </div>
    </section>
  );
}
