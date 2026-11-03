import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import Database from "better-sqlite3";

export const GENERATOR_NAME = "print-partner-synthetic-dataset";
export const GENERATOR_VERSION = 1;
export const SYNTHETIC_LABEL = "synthetic: not the architecture's step-2 rehearsal";
export const MANIFEST_FILE = "SYNTHETIC-DATASET.json";
const DATABASE_FILE = "print-partner.db";
export const UNMATCHED_PRINT_COUNT = 18;
const UNATTRIBUTED_SETTING = "printer.unattributed_prints";

const sha256 = (bytes: string | Buffer) => createHash("sha256").update(bytes).digest("hex");

function files(root: string): Array<{ path: string; sha256: string }> {
  const found: string[] = [];
  const walk = (directory: string) => {
    for (const entry of readdirSync(directory).sort()) {
      const path = join(directory, entry);
      if (statSync(path).isDirectory()) walk(path);
      else found.push(path);
    }
  };
  walk(root);
  return found
    .map((path) => relative(root, path).split("\\").join("/"))
    .filter((path) => path !== MANIFEST_FILE && !path.startsWith(DATABASE_FILE))
    .sort()
    .map((path) => ({ path, sha256: sha256(readFileSync(join(root, path))) }));
}

// Logical digest: every table's schema and rows, plus every managed file except
// the manifest. Node stores absolute snapshot paths, so the data directory
// prefix becomes $DATA. SQLite page layout is not part of the digest.
export function installationDigest(root: string): string {
  const db = new Database(join(root, DATABASE_FILE), { readonly: true, fileMustExist: true });
  try {
    const tables = db
      .prepare(
        "SELECT name, sql FROM sqlite_master WHERE type IN ('table','index','trigger','view') AND sql IS NOT NULL ORDER BY type, name",
      )
      .all() as Array<{ name: string; sql: string }>;
    const rows = tables
      .filter((table) => /^CREATE TABLE/i.test(table.sql))
      .map((table) => {
        const width = db
          .prepare(`SELECT COUNT(*) FROM pragma_table_info(?)`)
          .pluck()
          .get(table.name) as number;
        const order = Array.from({ length: width }, (_, index) => index + 1).join(", ");
        return [
          table.name,
          db.prepare(`SELECT * FROM "${table.name}" ORDER BY ${order}`).raw().all(),
        ];
      });
    return sha256(
      JSON.stringify({ schema: tables, rows, files: files(root) })
        .split(JSON.stringify(root).slice(1, -1))
        .join("$DATA"),
    );
  } finally {
    db.close();
  }
}

export type MeasuredShape = {
  schemaVersion: number;
  sources: number;
  buildIds: number[];
  layers: { total: number; maxPerBuild: number };
  acceptedPlanRevisions: { total: number; maxPerBuild: number };
  requiredUnits: number;
  acceptedPlateRevisions: { total: number; maxPerBuild: number; onEarlierPlanRevision: number };
  // Current Plate head per Build name: units placed and Plate count.
  currentPlates: Record<string, { units: number; plates: number }>;
  unmatchedPrints: number;
};

// Counts read straight from SQLite, so the generator, its tests and a reader on
// a copy all measure the same thing.
export function measureShape(root: string): MeasuredShape {
  const db = new Database(join(root, DATABASE_FILE), { readonly: true, fileMustExist: true });
  const one = (sql: string) => db.prepare(sql).pluck().get() as number;
  const maxPer = (table: string) =>
    one(`SELECT COALESCE(MAX(n), 0) FROM (SELECT COUNT(*) n FROM ${table} GROUP BY profile_id)`);
  try {
    const heads = db
      .prepare(
        `SELECT b.name, r.expected_unit_count, r.expected_plate_count FROM build_profiles b
           JOIN accepted_plate_heads h ON h.profile_id = b.id
           JOIN accepted_plate_revisions r ON r.id = h.current_revision_id
         ORDER BY b.id`,
      )
      .raw()
      .all() as Array<[string, number, number]>;
    const unattributed = db
      .prepare("SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = ?")
      .pluck()
      .get(UNATTRIBUTED_SETTING) as string | undefined;
    const prints = JSON.parse(unattributed ?? "[]") as Array<{
      claimed_at?: string;
      dismissed?: boolean;
    }>;
    return {
      schemaVersion: Number(
        db
          .prepare(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = 'schema_version'",
          )
          .pluck()
          .get(),
      ),
      sources: one("SELECT COUNT(*) FROM projects"),
      buildIds: db.prepare("SELECT id FROM build_profiles ORDER BY id").pluck().all() as number[],
      layers: {
        total: one("SELECT COUNT(*) FROM profile_layers"),
        maxPerBuild: maxPer("profile_layers"),
      },
      acceptedPlanRevisions: {
        total: one("SELECT COUNT(*) FROM plan_revisions"),
        maxPerBuild: maxPer("plan_revisions"),
      },
      requiredUnits: one("SELECT COUNT(*) FROM required_units"),
      acceptedPlateRevisions: {
        total: one("SELECT COUNT(*) FROM accepted_plate_revisions"),
        maxPerBuild: maxPer("accepted_plate_revisions"),
        onEarlierPlanRevision: one(
          `SELECT COUNT(*) FROM accepted_plate_revisions r JOIN build_profiles b ON b.id = r.profile_id
           WHERE r.plan_revision_id <> b.accepted_plan_revision_id`,
        ),
      },
      currentPlates: Object.fromEntries(
        heads.map(([name, units, plates]) => [name, { units, plates }]),
      ),
      unmatchedPrints: prints.filter((print) => !print.claimed_at && !print.dismissed).length,
    };
  } finally {
    db.close();
  }
}
