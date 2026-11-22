import type { CheckoffPart } from "./checkoff";

// Which Checkoff units a sliced file prints, by Node's send-flow rule (GRE-1896): the object
// names in the file (`apps/web/src/lib/parseSlicedObjects.ts`) proposed onto the Build's
// remaining units (`proposeCheckoffFromObjects.ts`), with the slicer-aware file name match
// of `packages/domain/src/sliced-object-matching.ts`. This package may not import those, so
// the parts the send sheet uses are ported here.

/** A unit the send links, as `addQueuedSend` reads `checkoff_units`. */
export interface SendUnit {
  part_id: number;
  unit_index: number;
  object_name?: string;
}

// ---- Object names in the file ------------------------------------------------------------

const EXCLUDE_LINE = /^\s*EXCLUDE_OBJECT_DEFINE\b([^\n]*)$/gim;
const EXCLUDE_NAME_EQ = /\bNAME\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s,]+))/i;
const EXCLUDE_NAME_JSON = /EXCLUDE_OBJECT_DEFINE\b[^\n]*?"name"\s*:\s*"([^"]+)"/gi;
const M486_A_QUOTED = /\bM486\b[^\n]*?\bA\s*"([^"]+)"/gi;
const M486_A_BARE = /\bM486\b[^\n]*?\bA\s*([^\s;]+)/gi;
const PRINTING_OBJECT = /;\s*printing object\s+(\S+?)(?:\s+id:\d+)?(?:\s+copy\s+\d+)?\s*$/gim;
const CURA_MESH = /^;\s*MESH\s*:\s*(.+?)\s*$/gim;
const OBJECTS_INFO = /^\s*;?\s*objects_info\s*=\s*(\{[^\r\n]*\})\s*$/gm;

/** PrusaSlicer's `objects_info`: one entry per placed instance, copies included. */
function objectsInfo(text: string): string[] {
  for (const match of text.matchAll(OBJECTS_INFO)) {
    let value: unknown;
    try {
      value = JSON.parse(match[1]!);
    } catch {
      continue;
    }
    const objects =
      typeof value === "object" && value !== null && "objects" in value ? value.objects : null;
    if (!Array.isArray(objects)) continue;
    const names = objects
      .map((item: unknown) =>
        typeof item === "object" && item !== null && "name" in item ? item.name : null,
      )
      .filter((name): name is string => typeof name === "string" && name.trim() !== "")
      .map((name) => name.trim());
    if (names.length > 0) return names;
  }
  return [];
}

/** The object labels in G-code text, each name once (case-insensitive), in file order. */
export function objectNames(text: string): string[] {
  const inventory = objectsInfo(text);
  if (inventory.length > 0) return inventory;
  const out: string[] = [];
  const seen = new Set<string>();
  const push = (name: string | undefined) => {
    const trimmed = (name ?? "").trim();
    if (!trimmed || seen.has(trimmed.toLowerCase())) return;
    seen.add(trimmed.toLowerCase());
    out.push(trimmed);
  };
  for (const m of text.matchAll(EXCLUDE_NAME_JSON)) push(m[1]);
  for (const m of text.matchAll(EXCLUDE_LINE)) {
    const name = EXCLUDE_NAME_EQ.exec(m[1] ?? "");
    push(name?.[1] ?? name?.[2] ?? name?.[3]);
  }
  for (const m of text.matchAll(M486_A_QUOTED)) push(m[1]);
  for (const m of text.matchAll(M486_A_BARE)) {
    const raw = (m[1] ?? "").replace(/^"+|"+$/g, "");
    // A bare number is an M486 object index, not a label.
    if (!/^\d+$/.test(raw)) push(raw);
  }
  for (const m of text.matchAll(PRINTING_OBJECT)) push(m[1]);
  for (const m of text.matchAll(CURA_MESH)) if (!/^NONMESH$/i.test(m[1]!.trim())) push(m[1]);
  return out;
}

/** The printable ASCII runs of a binary file such as .bgcode, one per line. */
function asciiChunks(bytes: Uint8Array, minRun = 12): string {
  const decoder = new TextDecoder("utf-8", { fatal: false });
  const parts: string[] = [];
  let start = -1;
  for (let i = 0; i <= bytes.length; i++) {
    const b = bytes[i];
    const text = b !== undefined && (b === 9 || b === 10 || b === 13 || (b >= 32 && b <= 126));
    if (text && start < 0) start = i;
    if (!text && start >= 0) {
      if (i - start >= minRun) parts.push(decoder.decode(bytes.subarray(start, i)));
      start = -1;
    }
  }
  return parts.join("\n");
}

/** The object names in a chosen sliced file, read in the browser; nothing is uploaded. */
export async function readObjectNames(file: File): Promise<string[]> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const text = /\.bgcode$/i.test(file.name)
    ? asciiChunks(bytes)
    : new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  return objectNames(text);
}

// ---- Slicer-aware name keys and file name match ------------------------------------------

const ORCA_COPY = /^(.+)_id_(\d+)_copy_(\d+)$/i;
const PRUSA_INSTANCE = /_+instance_(\d+)_?$/i;
const OBJECTS_INFO_INSTANCE = /\s*\(instance\s+(\d+)\)\s*$/i;
const MESH_EXTENSION = /\.(?:stl|3mf|gcode|gco|bgcode)$/i;
const UNIT_SUFFIX = /_(\d{1,3})$/u;

interface Keys {
  pathKey: string;
  basenameKey: string;
  unitStemKey: string;
}

function unquote(value: string): string {
  const trimmed = value.trim();
  const [first, last] = [trimmed[0], trimmed[trimmed.length - 1]];
  return trimmed.length >= 2 && first === last && (first === "'" || first === '"')
    ? trimmed.slice(1, -1).trim()
    : trimmed;
}

function decodePath(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/** The name without a slicer's copy or instance wrapper. */
function unwrap(raw: string): string {
  const name = unquote(decodePath(raw));
  const orca = ORCA_COPY.exec(name);
  if (orca) return orca[1]!;
  const info = OBJECTS_INFO_INSTANCE.exec(name);
  if (info) return name.slice(0, info.index);
  const prusa = PRUSA_INSTANCE.exec(name);
  return prusa ? name.slice(0, prusa.index).replace(/_+$/u, "") : name;
}

const segment = (value: string) =>
  value
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "");

/** `interpretSlicedObjectName`'s comparison keys. */
export function keys(raw: string): Keys {
  let path = unwrap(raw).replace(/\\/g, "/");
  for (let i = 0; i < 4; i++) {
    const next = path.replace(MESH_EXTENSION, "");
    if (next === path) break;
    path = next;
  }
  const pathKey = path
    .replace(/_stl$/i, "")
    .split("/")
    .map(segment)
    .filter(Boolean)
    .join("/");
  const basenameKey = pathKey.slice(pathKey.lastIndexOf("/") + 1);
  return { pathKey, basenameKey, unitStemKey: basenameKey.replace(UNIT_SUFFIX, "") || basenameKey };
}

function levenshtein(left: string, right: string): number {
  let previous = Array.from({ length: right.length + 1 }, (_, index) => index);
  for (let l = 1; l <= left.length; l++) {
    const current = [l];
    for (let r = 1; r <= right.length; r++) {
      const substitution = previous[r - 1]! + (left[l - 1] === right[r - 1] ? 0 : 1);
      current[r] = Math.min(current[r - 1]! + 1, previous[r]! + 1, substitution);
    }
    previous = current;
  }
  return previous[right.length]!;
}

const CONTRASTS = [
  ["left", "right"],
  ["front", "rear"],
  ["top", "bottom"],
  ["upper", "lower"],
  ["inner", "outer"],
] as const;

/** Numbers, axes and contrast words (left/right, top/bottom, ...) must agree. */
function sameMeaning(leftKey: string, rightKey: string): boolean {
  const left = new Set(leftKey.split("_").filter(Boolean));
  const right = new Set(rightKey.split("_").filter(Boolean));
  const picked = (tokens: Set<string>, keep: (token: string) => boolean) =>
    [...tokens].filter(keep).join(",");
  const numeric = (token: string) => /^\d+$/u.test(token);
  const axis = (token: string) => token === "x" || token === "y" || token === "z";
  if (picked(left, numeric) !== picked(right, numeric)) return false;
  if (picked(left, axis) !== picked(right, axis)) return false;
  return CONTRASTS.every(
    (group) => group.find((t) => left.has(t)) === group.find((t) => right.has(t)),
  );
}

function fuzzy(leftKey: string, rightKey: string) {
  const [left, right] = [leftKey.replace(/_/g, ""), rightKey.replace(/_/g, "")];
  const distance = levenshtein(left, right);
  return { distance, score: 1 - distance / Math.max(left.length, right.length, 1) };
}

/** The one file whose rows all share `key`, or undefined when none or several do. */
function only(rows: readonly { filename: string; keys: Keys }[], key: (k: Keys) => boolean) {
  const found = rows.filter((row) => key(row.keys));
  return found.length === 0 ? undefined : found.length === 1 ? found[0]!.filename : null;
}

/**
 * `matchSlicedObjectName`: the one file `raw` names, by path, file name, file name with a
 * unit suffix, then a close spelling. Null when it names none or several.
 */
export function matchFilename(raw: string, filenames: readonly string[]): string | null {
  const observed = keys(raw);
  if (!observed.basenameKey) return null;
  const rows = filenames.map((filename) => ({ filename, keys: keys(filename) }));
  const exact = [
    observed.pathKey.includes("/") ? only(rows, (k) => k.pathKey === observed.pathKey) : undefined,
    only(rows, (k) => k.basenameKey === observed.basenameKey),
    observed.unitStemKey !== observed.basenameKey
      ? only(rows, (k) => k.basenameKey === observed.unitStemKey)
      : undefined,
  ].find((outcome) => outcome !== undefined);
  if (exact !== undefined) return exact;

  const ranked = rows
    .map((row) => ({
      ...row,
      ...fuzzy(observed.unitStemKey, row.keys.basenameKey),
      meaning: sameMeaning(observed.unitStemKey, row.keys.basenameKey),
    }))
    .sort((a, b) => b.score - a.score || a.filename.localeCompare(b.filename));
  const [best, runnerUp] = ranked;
  if (!best) return null;
  const length = observed.unitStemKey.replace(/_/g, "").length;
  const close =
    length >= 8 && best.meaning && best.distance <= (length >= 12 ? 2 : 1) && best.score >= 0.9;
  const clear = runnerUp === undefined || best.score - runnerUp.score >= 0.08;
  return close && clear ? best.filename : null;
}

// ---- Proposal ----------------------------------------------------------------------------

export interface Proposal {
  units: SendUnit[];
  /** Units proposed per Part file name, in the order first matched. */
  perPart: { filename: string; count: number }[];
  /** Names that matched no remaining unit. */
  unmatched: string[];
}

interface Slot {
  part: CheckoffPart;
  unitIndex: number;
  exportKey: string;
}

/**
 * `proposeCheckoffFromObjects`: each object name takes one remaining (unprinted) unit.
 * First a unique export-style name (`bracket_02` is unit 2 of bracket.stl), then the file
 * name match for the rest. Names that are ambiguous or match nothing link no unit.
 */
export function proposeUnits(names: readonly string[], parts: readonly CheckoffPart[]): Proposal {
  const slots: Slot[] = parts.flatMap((part) =>
    part.units
      .filter((unit) => !unit.printed)
      .map((unit) => ({
        part,
        unitIndex: unit.unit_index,
        exportKey: keys(
          `${keys(part.filename).basenameKey}_${String(unit.unit_index + 1).padStart(2, "0")}`,
        ).basenameKey,
      })),
  );
  const used = new Set<Slot>();
  const taken: { slot: Slot; name: string }[] = [];
  const unmatched: string[] = [];
  const pending: string[] = [];
  for (const name of names) {
    const key = keys(name).basenameKey;
    const exact = slots.filter((slot) => !used.has(slot) && slot.exportKey === key);
    if (key && exact.length === 1) {
      used.add(exact[0]!);
      taken.push({ slot: exact[0]!, name });
    } else if (!key || exact.length > 1) unmatched.push(name);
    else pending.push(name);
  }
  for (const name of pending) {
    const free = slots.filter((slot) => !used.has(slot));
    const filename = matchFilename(name, [...new Set(free.map((slot) => slot.part.filename))]);
    const candidates = free.filter((slot) => slot.part.filename === filename);
    if (candidates.length === 0 || new Set(candidates.map((s) => s.part.part_id)).size !== 1) {
      unmatched.push(name);
      continue;
    }
    used.add(candidates[0]!);
    taken.push({ slot: candidates[0]!, name });
  }

  const perPart = new Map<string, number>();
  for (const { slot } of taken)
    perPart.set(slot.part.filename, (perPart.get(slot.part.filename) ?? 0) + 1);
  return {
    units: taken.map(({ slot, name }) => {
      const objectName = name.trim().slice(0, 200);
      const unit = { part_id: slot.part.part_id, unit_index: slot.unitIndex };
      return objectName ? { ...unit, object_name: objectName } : unit;
    }),
    perPart: [...perPart].map(([filename, count]) => ({ filename, count })),
    unmatched,
  };
}
