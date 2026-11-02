import type { PrinterMachine } from "@print-partner/domain";
import { newMachineFromPreset } from "../services/printer-fleet.js";
import { isRecord } from "../lib/guards.js";

export type PrinterDetailsInput = Pick<
  PrinterMachine,
  | "name"
  | "model"
  | "bed_width_mm"
  | "bed_depth_mm"
  | "bed_height_mm"
  | "margin_mm"
  | "max_filament_slots"
> & { preset_id: string | null };

function requiredText(value: unknown, field: "name" | "model"): string {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`${field} is required`);
  }
  return value.trim();
}

function requiredNumber(
  value: unknown,
  field: "bed_width_mm" | "bed_depth_mm" | "margin_mm",
  allowZero: boolean,
): number {
  if (
    typeof value !== "number" ||
    !Number.isFinite(value) ||
    value < 0 ||
    (!allowZero && value === 0)
  ) {
    const requirement = allowZero ? "0 or greater" : "greater than 0";
    throw new Error(`${field} must be ${requirement}`);
  }
  return value;
}

export function parsePrinterDetails(value: unknown): PrinterDetailsInput {
  if (!isRecord(value)) throw new Error("Printer details are required");
  const bedHeight = value.bed_height_mm;
  if (
    bedHeight !== null &&
    (typeof bedHeight !== "number" || !Number.isFinite(bedHeight) || bedHeight <= 0)
  ) {
    throw new Error("bed_height_mm must be null or greater than 0");
  }
  const maxFilamentSlots = value.max_filament_slots;
  if (
    typeof maxFilamentSlots !== "number" ||
    !Number.isInteger(maxFilamentSlots) ||
    maxFilamentSlots < 1 ||
    maxFilamentSlots > 4
  ) {
    throw new Error("max_filament_slots must be an integer from 1 to 4");
  }
  const rawPresetId = value.preset_id;
  if (rawPresetId !== null && (typeof rawPresetId !== "string" || !rawPresetId.trim())) {
    throw new Error("preset_id must be null or a non-empty string");
  }
  const bedWidth = requiredNumber(value.bed_width_mm, "bed_width_mm", false);
  const bedDepth = requiredNumber(value.bed_depth_mm, "bed_depth_mm", false);
  const margin = requiredNumber(value.margin_mm, "margin_mm", true);
  return {
    name: requiredText(value.name, "name"),
    model: requiredText(value.model, "model"),
    bed_width_mm: bedWidth,
    bed_depth_mm: bedDepth,
    bed_height_mm: bedHeight,
    margin_mm: margin,
    max_filament_slots: maxFilamentSlots,
    preset_id: rawPresetId === null ? null : rawPresetId.trim(),
  };
}

export function positiveMm(value: unknown, fallback: number, field: string): number {
  if (value === undefined) return fallback;
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    throw new Error(`${field} must be greater than 0`);
  }
  return value;
}

export function nullablePositiveMm(value: unknown, fallback: number, field: string): number | null {
  if (value === undefined) return fallback;
  if (value === null) return null;
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    throw new Error(`${field} must be null or greater than 0`);
  }
  return value;
}

export function samePresetSnapshot(
  left: PrinterDetailsInput,
  right: PrinterDetailsInput | PrinterMachine,
): boolean {
  return (
    left.model === right.model &&
    left.bed_width_mm === right.bed_width_mm &&
    left.bed_depth_mm === right.bed_depth_mm &&
    left.bed_height_mm === right.bed_height_mm &&
    left.margin_mm === right.margin_mm &&
    left.max_filament_slots === right.max_filament_slots
  );
}

export function currentPresetDetails(
  preset: Parameters<typeof newMachineFromPreset>[0],
  name: string,
): PrinterDetailsInput {
  const machine = newMachineFromPreset(preset, name);
  return {
    name,
    model: machine.model,
    bed_width_mm: machine.bed_width_mm,
    bed_depth_mm: machine.bed_depth_mm,
    bed_height_mm: machine.bed_height_mm,
    margin_mm: machine.margin_mm,
    max_filament_slots: machine.max_filament_slots,
    preset_id: machine.preset_id ?? preset.id,
  };
}
