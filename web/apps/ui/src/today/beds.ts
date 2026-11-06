import type { CheckoffResultList } from "@print-partner/rust-client";

type CheckoffResult = CheckoffResultList["results"][number];
export type Unit = CheckoffResult["units"][number];

/** A bed waiting for a person: an awaiting-verify result and the units it has not resolved. */
export interface Bed {
  result: CheckoffResult;
  printer: string;
  pending: Unit[];
}

/** The most beds one verify-batch request may carry (the generated request schema). */
export const MAX_BURST_BEDS = 50;

/** Units of one bed that a burst confirms. */
export interface Pick {
  bed: Bed;
  units: Unit[];
}

export const unitKey = (u: { part_id: number; unit_index: number }) =>
  `${u.part_id}:${u.unit_index}`;

/** Results in one state, oldest first, each with the units it has not resolved. */
function bedsIn(
  results: readonly CheckoffResult[],
  printerName: (id: string) => string,
  state: CheckoffResult["state"],
): Bed[] {
  return results
    .filter((r) => r.state === state)
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

/** Results waiting for a person, oldest first, each with its pending units. */
export const bedsToClear = (
  results: readonly CheckoffResult[],
  printerName: (id: string) => string,
) => bedsIn(results, printerName, "awaiting_verify");

/**
 * Units on loaded results that no bed can confirm now: on a print still running, or failed,
 * dismissed or rejected and neither confirmed nor reprinting on another result, each named once. A lower one of these makes
 * the core refuse a confirm (Progress fills from the left) unless it was completed elsewhere.
 */
export function bedsBlocking(
  results: readonly CheckoffResult[],
  printerName: (id: string) => string,
  beds: readonly Bed[],
): { printing: Bed[]; unconfirmed: Bed[] } {
  const pickable = new Set(beds.flatMap((bed) => bed.pending.map(unitKey)));
  const confirmed = new Set(
    results.flatMap((r) => r.resolved_units.filter((u) => u.result === "confirmed").map(unitKey)),
  );
  const blocked = (result: CheckoffResult): Bed => ({
    result,
    printer: printerName(result.printer_id),
    pending: result.units.filter((u) => !pickable.has(unitKey(u)) && !confirmed.has(unitKey(u))),
  });
  const all = [...results]
    .sort((a, b) => a.created_at.localeCompare(b.created_at))
    .map(blocked)
    .filter((bed) => bed.pending.length > 0);
  const printing = all.filter((bed) => bed.result.state === "watching");
  const seen = new Set(printing.flatMap((bed) => bed.pending.map(unitKey)));
  const unconfirmed = all
    .filter((bed) => bed.result.state !== "watching")
    .map((bed) => {
      const pending = bed.pending.filter((u) => !seen.has(unitKey(u)));
      for (const u of pending) seen.add(unitKey(u));
      return { ...bed, pending };
    })
    .filter((bed) => bed.pending.length > 0);
  return { printing, unconfirmed };
}

/** "A", "A and B", "A, B and C". */
export const listWords = (words: readonly string[]) =>
  words.length < 2 ? words.join("") : `${words.slice(0, -1).join(", ")} and ${words.at(-1)}`;

export const plural = (n: number, one: string) => `${n} ${n === 1 ? one : `${one}s`}`;

export const partName = (u: Unit) => u.part_filename ?? `Part ${u.part_id}`;

/** What confirming these units marks, one line per Part: its file name, the count and the unit numbers. */
export function unitDiff(units: readonly Unit[]) {
  const parts = new Map<number, { name: string; units: number[] }>();
  for (const unit of units) {
    const part = parts.get(unit.part_id) ?? { name: partName(unit), units: [] };
    part.units.push(unit.unit_index + 1);
    parts.set(unit.part_id, part);
  }
  return [...parts.entries()].map(([id, { name, units: numbers }]) => ({
    id,
    text: `${name}: +${numbers.length} printed (${numbers.length === 1 ? "unit" : "units"} ${listWords(numbers.map(String))})`,
  }));
}

/** The printers a set of picks frees, in words. */
export const pickedPrinters = (picks: readonly Pick[]) =>
  listWords([...new Set(picks.map((p) => p.bed.printer))]);

/**
 * Units on these beds (or running prints) that confirming the picks also needs. Progress
 * fills from the left, so the core refuses a Part's unit while a lower one is incomplete.
 */
export function lowerFirst(beds: readonly Bed[], picks: readonly Pick[]): Pick[] {
  const units = picks.flatMap((p) => p.units);
  const chosen = new Set(units.map(unitKey));
  const top = new Map<number, number>();
  for (const u of units) top.set(u.part_id, Math.max(top.get(u.part_id) ?? -1, u.unit_index));
  return beds
    .map((bed) => ({
      bed,
      units: bed.pending.filter(
        (u) => u.unit_index < (top.get(u.part_id) ?? -1) && !chosen.has(unitKey(u)),
      ),
    }))
    .filter((pick) => pick.units.length > 0);
}

/** "Trident: frame.stl units 1 and 2 and gear.stl unit 3", one entry per bed. */
export function pickWords(picks: readonly Pick[]) {
  return listWords(
    picks.map((p) => {
      const parts = new Map<number, { name: string; numbers: number[] }>();
      for (const u of p.units) {
        const part = parts.get(u.part_id) ?? { name: partName(u), numbers: [] };
        part.numbers.push(u.unit_index + 1);
        parts.set(u.part_id, part);
      }
      const words = [...parts.values()].map(
        ({ name, numbers }) =>
          `${name} ${numbers.length === 1 ? "unit" : "units"} ${listWords(numbers.map(String))}`,
      );
      return `${p.bed.printer}: ${listWords(words)}`;
    }),
  );
}
