import type { CheckoffUnits } from "@print-partner/rust-client";
import { listWords } from "../today/beds";

export type CheckoffPart = CheckoffUnits["parts"][number];
export type CheckoffUnit = CheckoffPart["units"][number];

/** Which flag a burst sets: printed, or assembled on units already printed. */
export type Field = "printed" | "assembled";

/** The most units one manual burst may carry (the generated request schema). */
export const MAX_BURST_UNITS = 1000;

/** One unit in tab order with its Part. */
export interface Slot {
  part: CheckoffPart;
  unit: CheckoffUnit;
}

/** Each unit's wanted flag, by token. Only units that differ from the stored state count. */
export type Picks = ReadonlyMap<string, boolean>;

export const slotsOf = (parts: readonly CheckoffPart[]): Slot[] =>
  parts.flatMap((part) => part.units.map((unit) => ({ part, unit })));

/** A unit can take part in an assembled burst only once it is printed. */
export const pickable = (field: Field, unit: CheckoffUnit) => field === "printed" || unit.printed;

/**
 * The state each picked Part ends in, as the core computes it. Printed units fill from the
 * left: printing a unit prints every lower one, and clearing one clears every higher one and
 * its assembly. `conflict` names a Part where the picks both print and clear across it.
 */
export function outcome(parts: readonly CheckoffPart[], field: Field, picks: Picks) {
  const finals = new Map<number, { printed: boolean; assembled: boolean }[]>();
  const conflicts: string[] = [];
  for (const part of parts) {
    const picked = part.units.filter((u) => picks.has(u.token));
    if (picked.length === 0) continue;
    const state = part.units.map((u) => ({ printed: u.printed, assembled: u.assembled }));
    if (field === "assembled") {
      part.units.forEach((u, i) => {
        const want = picks.get(u.token);
        if (want !== undefined) state[i]!.assembled = want;
      });
    } else {
      const high = Math.max(
        -1,
        ...picked.filter((u) => picks.get(u.token)).map((u) => u.unit_index),
      );
      const low = Math.min(
        Infinity,
        ...picked.filter((u) => !picks.get(u.token)).map((u) => u.unit_index),
      );
      if (high >= low) conflicts.push(part.filename);
      part.units.forEach((u, i) => {
        if (u.unit_index <= high) state[i]!.printed = true;
        if (u.unit_index >= low) state[i] = { printed: false, assembled: false };
      });
    }
    finals.set(part.part_id, state);
  }
  return { finals, conflicts };
}

/** One line per Part the burst changes: "bracket.stl: +2 printed (units 3 and 4)". */
export function diffLines(parts: readonly CheckoffPart[], field: Field, picks: Picks) {
  const { finals } = outcome(parts, field, picks);
  const lines: { id: string; text: string }[] = [];
  for (const part of parts) {
    const state = finals.get(part.part_id);
    if (!state) continue;
    for (const flag of ["printed", "assembled"] as const) {
      const added: number[] = [];
      const removed: number[] = [];
      part.units.forEach((u, i) => {
        if (state[i]![flag] && !u[flag]) added.push(u.unit_index + 1);
        if (!state[i]![flag] && u[flag]) removed.push(u.unit_index + 1);
      });
      for (const [sign, numbers] of [
        ["+", added],
        ["\u2212", removed],
      ] as const) {
        if (numbers.length === 0) continue;
        lines.push({
          id: `${part.part_id}:${flag}:${sign}`,
          text: `${part.filename}: ${sign}${numbers.length} ${flag} (${numbers.length === 1 ? "unit" : "units"} ${listWords(numbers.map(String))})`,
        });
      }
    }
  }
  return lines;
}

/** Picks that would change nothing are dropped, so a toggle back to the stored state un-picks. */
function settle(slots: readonly Slot[], field: Field, picks: Map<string, boolean>) {
  for (const { unit } of slots) {
    if (picks.get(unit.token) === unit[field]) picks.delete(unit.token);
  }
  return picks;
}

/** What a unit shows now: its pick, else its stored flag. */
export const shown = (field: Field, picks: Picks, unit: CheckoffUnit) =>
  picks.get(unit.token) ?? unit[field];

/**
 * The burst keys, on the focused unit. P sets every unit on its head Plate, F every unit of
 * its Part, and 0 to 9 set that many units of its Part from the lowest, clearing the rest.
 * Returns null for any other key, and for P on a unit that is on no Plate.
 */
export function burstKey(
  key: string,
  slot: Slot,
  slots: readonly Slot[],
  field: Field,
  picks: Picks,
): Map<string, boolean> | null {
  const next = new Map(picks);
  const set = (targets: readonly Slot[], on: (s: Slot, i: number) => boolean) =>
    targets.forEach((s, i) => {
      if (pickable(field, s.unit)) next.set(s.unit.token, on(s, i));
    });
  const samePart = slots
    .filter((s) => s.part.part_id === slot.part.part_id)
    .sort((a, b) => a.unit.unit_index - b.unit.unit_index);
  if (key === "p") {
    const plate = slot.unit.plate?.plate_id;
    if (plate === undefined) return null;
    set(
      slots.filter((s) => s.unit.plate?.plate_id === plate),
      () => true,
    );
  } else if (key === "f") {
    set(samePart, () => true);
  } else if (/^[0-9]$/.test(key)) {
    set(
      samePart.filter((s) => pickable(field, s.unit)),
      (_, i) => i < Number(key),
    );
  } else {
    return null;
  }
  return settle(slots, field, next);
}

/** Space or click flips one unit; with Shift, every unit from the anchor takes its new state. */
export function toggle(
  slot: Slot,
  range: { anchor: Slot | null; shift: boolean },
  slots: readonly Slot[],
  field: Field,
  picks: Picks,
) {
  const { anchor, shift } = range;
  const on = !shown(field, picks, slot.unit);
  const from = anchor ? slots.indexOf(anchor) : -1;
  const to = slots.indexOf(slot);
  const run = shift && from >= 0 ? slots.slice(Math.min(from, to), Math.max(from, to) + 1) : [slot];
  const next = new Map(picks);
  for (const s of run) if (pickable(field, s.unit)) next.set(s.unit.token, on);
  return settle(slots, field, next);
}

/** "Plate 2 on Voron 2.4" for a unit's head Plate. */
export const plateName = (plate: NonNullable<CheckoffUnit["plate"]>) =>
  `Plate ${plate.ordinal} on ${plate.printer_name}`;
