import type { AppRepository } from "../db/repository.js";

export type ProfileSourceMode = "assigned" | "auto_match";

export function latestSyncedAt(timestamps: Array<string | null | undefined>): string | null {
  let best: string | null = null;
  let bestMs = -Infinity;
  for (const raw of timestamps) {
    if (!raw) continue;
    const ms = Date.parse(raw);
    if (Number.isNaN(ms)) continue;
    if (ms >= bestMs) {
      bestMs = ms;
      best = raw;
    }
  }
  return best;
}

function namesLooselyMatch(a: string, b: string): boolean {
  const x = a.trim().toLowerCase();
  const y = b.trim().toLowerCase();
  if (!x || !y) return false;
  return x === y || x.includes(y) || y.includes(x);
}

export function processCompatibleWithMachine(
  compatiblePrintersJson: string | null | undefined,
  machineProfileName: string,
): boolean {
  if (!compatiblePrintersJson) return true;
  let list: unknown;
  try {
    list = JSON.parse(compatiblePrintersJson);
  } catch {
    return true;
  }
  if (!Array.isArray(list) || list.length === 0) return true;
  const names = list.map((x) => String(x ?? ""));
  return names.some((n) => namesLooselyMatch(n, machineProfileName));
}

type PrinterProfileAssignmentView = {
  printer_id: string;
  profile_source: ProfileSourceMode;
  machine_profile_id: number | null;
  filament_slots: Array<{ slot_index: number; filament_profile_id: number | null }>;
  last_synced_at: string | null;
  compatible_processes: Array<{ id: number; name: string }>;
};

export function buildAssignmentView(
  repo: AppRepository,
  printerId: string,
  slotCount: number,
): PrinterProfileAssignmentView {
  const header = repo.getPrinterProfileAssignment(printerId);
  const profileSource = header?.profileSource ?? "auto_match";
  const machineProfileId = header?.machineProfileId ?? null;
  const storedSlots = repo.listFilamentSlotAssignments(printerId);
  const bySlot = new Map(storedSlots.map((s) => [s.slotIndex, s.filamentProfileId]));
  const filament_slots = [];
  for (let i = 1; i <= slotCount; i++) {
    filament_slots.push({
      slot_index: i,
      filament_profile_id: bySlot.get(i) ?? null,
    });
  }
  const machine =
    machineProfileId != null ? repo.getSlicerPrinterProfileById(machineProfileId) : null;
  const filamentTs = filament_slots.map((s) =>
    s.filament_profile_id != null
      ? repo.getSlicerFilamentProfileById(s.filament_profile_id)?.lastSyncedAt
      : null,
  );
  const last_synced_at = latestSyncedAt([machine?.lastSyncedAt, ...filamentTs]);
  const machineName = machine?.name ?? "";
  const compatible_processes = machineName
    ? repo
        .listSlicerProcessProfilesDetailed()
        .filter((p) => processCompatibleWithMachine(p.compatiblePrinters, machineName))
        .map((p) => ({ id: p.id, name: p.name }))
    : [];
  return {
    printer_id: printerId,
    profile_source: profileSource,
    machine_profile_id: machineProfileId,
    filament_slots,
    last_synced_at,
    compatible_processes,
  };
}
