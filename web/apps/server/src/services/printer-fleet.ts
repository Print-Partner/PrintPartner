import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";
import type { AppRepository } from "../db/repository.js";
import type { PrinterMachine } from "@print-partner/domain";

const FLEET_KEY = "printer.fleet";

const MODULE_DIR = dirname(fileURLToPath(import.meta.url));
/** tsc does not copy JSON into dist/ — fall back to src/data like kit-catalog. */
const PRESETS_CANDIDATES = [
  join(MODULE_DIR, "../data/printer_presets.json"),
  join(MODULE_DIR, "../../src/data/printer_presets.json"),
];

type PrinterPreset = {
  id: string;
  name: string;
  model_slug?: string;
  thumbnail?: string;
  bed_width_mm: number;
  bed_depth_mm: number;
  bed_height_mm: number | null;
  max_filament_slots: number;
};

function ensureSlots(machine: PrinterMachine): PrinterMachine {
  const n = Math.max(1, Math.min(4, machine.max_filament_slots));
  const bySlot = Object.fromEntries(machine.loaded_filaments.map((lf) => [lf.slot, lf]));
  const loaded = [];
  for (let i = 1; i <= n; i++) {
    loaded.push(bySlot[i] ?? { slot: i, filament_color_id: null, label: "" });
  }
  return { ...machine, max_filament_slots: n, loaded_filaments: loaded };
}

export function parsePrinterMachine(data: Record<string, unknown>): PrinterMachine {
  const loaded = Array.isArray(data.loaded_filaments)
    ? data.loaded_filaments.map((x) => {
        const row = x as Record<string, unknown>;
        return {
          slot: Number(row.slot ?? 1),
          filament_color_id: (row.filament_color_id as string | null) ?? null,
          label: String(row.label ?? ""),
        };
      })
    : [];
  const machine: PrinterMachine = {
    id: String(data.id),
    name: String(data.name ?? "Printer"),
    model: String(data.model ?? data.name ?? "Printer"),
    bed_width_mm: Number(data.bed_width_mm ?? 250),
    bed_depth_mm: Number(data.bed_depth_mm ?? 210),
    bed_height_mm: data.bed_height_mm != null ? Number(data.bed_height_mm) : null,
    margin_mm: Number(data.margin_mm ?? 4),
    max_filament_slots: Number(data.max_filament_slots ?? 1),
    loaded_filaments: loaded,
  };
  if (data.integration_id != null && String(data.integration_id).trim()) {
    machine.integration_id = String(data.integration_id).trim();
  } else if (data.integration_id === null) {
    machine.integration_id = null;
  }
  if (data.device_id != null && String(data.device_id).trim()) {
    machine.device_id = String(data.device_id).trim();
  } else if (data.device_id === null) {
    machine.device_id = null;
  }
  if (data.preset_id != null && String(data.preset_id).trim()) {
    machine.preset_id = String(data.preset_id).trim();
  } else if (data.preset_id === null) {
    machine.preset_id = null;
  }
  if (data.preferred_slicer != null) {
    const raw = String(data.preferred_slicer).trim().toLowerCase();
    machine.preferred_slicer = raw === "orca" || raw === "prusa" || raw === "bambu" ? raw : null;
  } else {
    machine.preferred_slicer = null;
  }
  return ensureSlots(machine);
}

export function loadPrinterPresets(): PrinterPreset[] {
  const presetsPath = PRESETS_CANDIDATES.find((p) => existsSync(p));
  if (!presetsPath) {
    return [];
  }
  const raw = JSON.parse(readFileSync(presetsPath, "utf8")) as Array<Record<string, unknown>>;
  return raw.map((item) => ({
    id: String(item.id ?? ""),
    name: String(item.name ?? "Printer"),
    bed_width_mm: Number(item.bed_width_mm ?? 250),
    bed_depth_mm: Number(item.bed_depth_mm ?? 210),
    bed_height_mm: item.bed_height_mm != null ? Number(item.bed_height_mm) : null,
    max_filament_slots: Number(item.max_filament_slots ?? 1),
    ...(item.model_slug ? { model_slug: String(item.model_slug) } : {}),
    ...(item.thumbnail ? { thumbnail: String(item.thumbnail) } : {}),
  }));
}

export function loadFleet(repo: AppRepository): PrinterMachine[] {
  const raw = repo.getSetting(FLEET_KEY);
  if (!raw) return [];
  try {
    const items = JSON.parse(raw) as Array<Record<string, unknown>>;
    return items.map((x) => parsePrinterMachine(x));
  } catch {
    return [];
  }
}

export function saveFleet(repo: AppRepository, fleet: PrinterMachine[]): void {
  const normalized = fleet.map(ensureSlots);
  repo.setSetting(FLEET_KEY, JSON.stringify(normalized, null, 2));
}

/** Clear fleet rows that linked a deleted integration host. */
export function clearFleetIntegrationBinds(repo: AppRepository, integrationId: string): number {
  const fleet = loadFleet(repo);
  let cleared = 0;
  const next = fleet.map((m) => {
    if (m.integration_id !== integrationId) return m;
    cleared += 1;
    return { ...m, integration_id: null, device_id: null };
  });
  if (cleared > 0) saveFleet(repo, next);
  return cleared;
}

export function newMachineFromPreset(preset: PrinterPreset, name?: string): PrinterMachine {
  const slots = Math.max(1, preset.max_filament_slots);
  return ensureSlots({
    id: `printer-${randomBytes(5).toString("hex")}`,
    name: name ?? preset.name,
    model: preset.model_slug ?? preset.name,
    bed_width_mm: preset.bed_width_mm,
    bed_depth_mm: preset.bed_depth_mm,
    bed_height_mm: preset.bed_height_mm,
    margin_mm: 4,
    max_filament_slots: slots,
    loaded_filaments: Array.from({ length: slots }, (_, i) => ({
      slot: i + 1,
      filament_color_id: null,
      label: "",
    })),
    preset_id: preset.id,
  });
}
