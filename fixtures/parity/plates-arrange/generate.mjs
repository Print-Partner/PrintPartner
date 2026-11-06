// Generates the plates-arrange parity golden: Node's real
// POST /plans/:id/plates/arrange (and the restore an arrange of every unit
// enables) on disposable copies of accepted-plate-node.db (one Required unit)
// and required-units/unchanged.db (five, on two Parts). Each phase starts
// from a fresh copy, publishes one valid layout through Node's own
// publishAcceptedPlates, runs its optional SQL (with triggers dropped and
// foreign keys off), then sends its requests in order.
// The golden records the published rows each phase starts from, every
// response, the tables each request changed, and the Plate rows at the end.
// Run from the repo root after npm --prefix web ci:
//   node fixtures/parity/plates-arrange/generate.mjs          # write
//   node fixtures/parity/plates-arrange/generate.mjs --check  # verify, no writes
// created_at is the only clock value; it is recorded as null.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../..");
const output = join(here, "node.json");
const database = "rust/crates/pp-storage/tests/fixtures/accepted-plate-node.db";
const multi = "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db";
const sources = [
  "web/apps/server/src/routes/accepted-plates.ts",
  "web/apps/server/src/services/accepted-plate-workspace.ts",
  "web/apps/server/src/db/accepted-plates.ts",
  "web/packages/domain/src/accepted-plate-packer.ts",
];
const TOKEN = "ppu_574c8d5f92624f029e21705a8ebb8a1b";
const PLATE = "plate_abababababababababababababababab";
const BASIS = {
  profile_id: 1,
  plan_version: 1,
  plan_revision_id: 1,
  plan_revision_digest: "bac2028fe05e303f3c0ff3b2af25b0ebc978bad83e7644320c381e9febadc876",
  required_unit_mapping_digest: "a26b59e92961a70d709090b6dc1742df84dc1cfaed94ee0a2a77a57f9ac0b952",
};
const PLATE_TABLES = ["accepted_plate_revisions", "accepted_plates", "accepted_plate_units", "accepted_plate_heads"];

const printer = (id, width, depth, margin) => ({
  printerId: id, printerName: id.toUpperCase(), printerModel: "Fixture", bedWidthUm: width, bedDepthUm: depth, bedHeightUm: 200000, marginUm: margin,
});
const placed = (token, placement, pinned, xUm, yUm, size = [50000, 40000, 30000]) => ({
  token, xUm, yUm, widthUm: size[0], depthUm: size[1], heightUm: size[2], placement, pinned,
});
const unit = (placement, pinned, x, y) => [{
  plateId: PLATE, ...printer("fixture-printer", 250000, 220000, 5000), printerName: "Fixture Printer", printerModel: "Fixture Model", bedHeightUm: 220000,
  units: [placed(TOKEN, placement, pinned, x, y)],
}];
const ppu = (n) => "ppu_" + String(n).padStart(32, "0");
const small = [40000, 30000, 20000];
// Plan 1 of unchanged.db: bracket units 1-3 and gear units 5-6, on two Printers.
const MULTI = [
  { plateId: "plate_" + "a".repeat(32), ...printer("p-a", 250000, 220000, 5000), units: [
    placed(ppu(1), "auto", false, 5000, 5000, small),
    placed(ppu(2), "manual", false, 60000, 5000, small),
    placed(ppu(5), "auto", true, 120000, 5000, small),
  ] },
  { plateId: "plate_" + "b".repeat(32), ...printer("p-b", 180000, 180000, 3000), units: [
    placed(ppu(3), "auto", false, 100000, 100000, small),
    placed(ppu(6), "unplaced", false, 3000, 3000, small),
  ] },
];
const setup = (rules, profile = 1) => "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','production_setup:1','" +
  JSON.stringify({ format: "production-setup-v1", profile_id: profile, preferred_slicer_instance_id: null, updated_at: null,
    selection: { mode: "all_incomplete" }, rules }) + "');";
const rule = (id, kind, field, extra = {}) => ({ id, enabled: true, kind, field, ...extra });
// A request names the revision it expects relative to the phase: "head" is the
// newest revision any earlier 200 answered (the published one at first),
// "previous" the one before it.
const arrange = (id, mode, extra = {}) => ({ id, path: "/plans/1/plates/arrange", mode, ...extra });
const restore = (id) => ({ id, path: "/plans/1/plates/restore", restore: "previous" });

const PHASES = [
  { name: "multi_unplaced", database: multi, plates: MULTI, requests: [
    arrange("multi_unplaced_packs_around_fixed", "unplaced"),
  ] },
  { name: "multi_all", database: multi, plates: MULTI, requests: [
    arrange("multi_all_merges_per_printer", "all"),
  ] },
  { name: "multi_rules", database: multi, plates: MULTI,
    seed: setup([rule("by-part", "separate_by", "part"), rule("off", "keep_together", "color", { value: "#FF0000", enabled: false })]), requests: [
    arrange("multi_all_separates_parts", "all"),
  ] },
  { name: "multi_keep_together", database: multi, plates: MULTI,
    seed: setup([rule("brackets", "keep_together", "part", { value: "bracket__" + ppu(1) }), rule("paint", "set_material", "part", { value: "gear__" + ppu(5), material_type: "PETG" }), rule("by-material", "separate_by", "material")]), requests: [
    arrange("multi_all_keep_together_and_material", "all"),
  ] },
  { name: "multi_other_profile_setup", database: multi, plates: MULTI,
    seed: setup([rule("by-part", "separate_by", "part")], 2), requests: [
    arrange("multi_setup_for_another_plan_is_ignored", "all"),
  ] },
  { name: "auto_offset", unit: unit("auto", false, 100000, 80000), requests: [
    arrange("all_records_undo", "all"),
    restore("restore_undoes_all"),
    arrange("unplaced_packs_auto", "unplaced"),
    arrange("unplaced_again_unchanged", "unplaced"),
  ] },
  { name: "pinned", unit: unit("auto", true, 100000, 80000), requests: [
    arrange("unplaced_keeps_pinned", "unplaced"),
    arrange("all_unpins", "all"),
    arrange("all_again_unchanged", "all"),
  ] },
  { name: "manual", unit: unit("manual", false, 100000, 80000), requests: [
    arrange("unplaced_keeps_manual", "unplaced"),
    arrange("all_resets_manual", "all"),
  ] },
  { name: "unplaced_unit", unit: unit("unplaced", false, 5000, 5000), requests: [
    arrange("unplaced_places_unit", "unplaced"),
  ] },
  { name: "refusals", unit: unit("auto", false, 100000, 80000), requests: [
    arrange("bad_mode", "everything"),
    arrange("missing_mode", undefined),
    arrange("basis_other_profile", "all", { basis: { ...BASIS, profile_id: 2 } }),
    arrange("path_text", "all", { path: "/plans/abc/plates/arrange" }),
    arrange("stale_digest", "all", { basis: { ...BASIS, plan_revision_digest: "0".repeat(64) } }),
    arrange("stale_version", "all", { basis: { ...BASIS, plan_version: 2 } }),
    arrange("wrong_revision", "all", { revision: 999 }),
    arrange("revision_zero", "all", { revision: 0 }),
    arrange("missing_plan", "all", { path: "/plans/2/plates/arrange", basis: { ...BASIS, profile_id: 2 } }),
  ] },
  { name: "archived", unit: unit("auto", false, 100000, 80000),
    seed: "UPDATE build_profiles SET archived_at='2026-10-03T00:00:00.000Z' WHERE id=1;", requests: [
    arrange("archived_changed", "all"),
    arrange("archived_unchanged", "unplaced"),
  ] },
  { name: "no_head", unit: unit("auto", false, 100000, 80000),
    seed: "DELETE FROM accepted_plate_heads;", requests: [
    arrange("setup_refuses", "all", { revision: 1 }),
  ] },
  { name: "foreign_plan", unit: unit("auto", false, 100000, 80000),
    seed: "UPDATE build_profiles SET tenant_id='foreign' WHERE id=1;", requests: [
    arrange("foreign_refuses", "all"),
  ] },
];

if (process.argv.includes("--probe")) {
  const load = (source) => import(pathToFileURL(join(root, source)).href);
  const require = createRequire(join(root, "web/apps/server/package.json"));
  const Database = require("better-sqlite3");
  delete process.env.PRINT_PARTNER_API_KEY;
  const { createSelfHostPorts } = await load("web/apps/server/src/adapters/self-host/index.ts");
  const { buildApp } = await load("web/apps/server/src/app.ts");
  const { loadConfig } = await load("web/apps/server/src/config.ts");
  const expected = {
    profileId: BASIS.profile_id,
    planVersion: BASIS.plan_version,
    revisionId: BASIS.plan_revision_id,
    revisionDigest: BASIS.plan_revision_digest,
    requiredUnitMappingDigest: BASIS.required_unit_mapping_digest,
  };
  const rows = (db, table, where = "") =>
    db.prepare("SELECT * FROM \"" + table + "\" " + where + " ORDER BY rowid").all().map((row) =>
      "created_at" in row ? { ...row, created_at: null } : row);

  const phases = [];
  for (const phase of PHASES) {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-parity-plates-arrange-"));
    try {
      const file = join(dataDir, "print-partner.db");
      copyFileSync(join(root, phase.database ?? database), file);
      mkdirSync(join(dataDir, "repos/1/revisions/accepted"), { recursive: true, mode: 0o700 });
      process.env.PRINT_PARTNER_DATA_DIR = dataDir;
      const ports = createSelfHostPorts(dataDir);
      await ports.db.connect();
      const initial = (() => {
        const db = new Database(file, { readonly: true });
        const head = db.prepare("SELECT current_revision_id AS id FROM accepted_plate_heads WHERE profile_id=1").get();
        const base = db.prepare("SELECT b.accepted_plan_version AS planVersion, r.id AS revisionId, r.snapshot_digest AS revisionDigest, s.mapping_digest AS mappingDigest FROM build_profiles b JOIN plan_revisions r ON r.id=b.accepted_plan_revision_id LEFT JOIN plan_revision_required_unit_sets s ON s.revision_id=r.id WHERE b.id=1").get();
        db.close();
        return { head: head?.id ?? null, base };
      })();
      const basis = phase.database ? {
        profile_id: 1,
        plan_version: initial.base.planVersion,
        plan_revision_id: initial.base.revisionId,
        plan_revision_digest: initial.base.revisionDigest,
        required_unit_mapping_digest: initial.base.mappingDigest,
      } : BASIS;
      const published = ports.repository.publishAcceptedPlates({
        profileId: 1,
        expected: {
          profileId: 1,
          planVersion: basis.plan_version,
          revisionId: basis.plan_revision_id,
          revisionDigest: basis.plan_revision_digest,
          requiredUnitMappingDigest: basis.required_unit_mapping_digest,
        },
        expectedPlateRevisionId: initial.head,
        plates: phase.plates ?? phase.unit,
      });
      assert.equal(published.kind, "published", phase.name);
      const reader = new Database(file, { readonly: true });
      const seedRows = Object.fromEntries(PLATE_TABLES.map((table) => [
        table,
        rows(reader, table, table === "accepted_plate_heads" ? "" : (table === "accepted_plate_revisions" ? "WHERE id>" + (initial.head ?? 0) : "WHERE revision_id>" + (initial.head ?? 0))),
      ]));
      reader.close();
      if (phase.seed) {
        const db = new Database(file);
        db.pragma("foreign_keys = OFF");
        for (const { name } of db.prepare("SELECT name FROM sqlite_schema WHERE type = 'trigger'").all()) {
          db.exec("DROP TRIGGER \"" + name + "\"");
        }
        db.exec(phase.seed);
        db.close();
      }
      const app = await buildApp(loadConfig(), ports);
      await app.ready();
      const tables = (() => {
        const db = new Database(file, { readonly: true });
        const names = db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name").all().map((row) => row.name);
        db.close();
        return names;
      })();
      const graph = () => {
        const db = new Database(file, { readonly: true });
        const state = new Map(tables.map((table) => [table, JSON.stringify(db.prepare("SELECT * FROM \"" + table + "\"").raw().all())]));
        db.close();
        return state;
      };
      const revisions = [published.plateRevisionId];
      const cases = [];
      for (const request of phase.requests) {
        const head = revisions.at(-1);
        const body = {
          expected: request.basis ?? basis,
          expected_plate_revision_id: request.revision ?? head,
          ...(request.mode === undefined ? {} : { mode: request.mode }),
          ...(request.restore ? { restore_plate_revision_id: revisions.at(-2) } : {}),
        };
        const before = graph();
        const response = await app.inject({ method: "POST", url: request.path, payload: body });
        const after = graph();
        const json = response.body === "" ? null : response.json();
        if (response.statusCode === 200 && typeof json?.plate_revision_id === "number" && json.plate_revision_id !== head) {
          revisions.push(json.plate_revision_id);
        }
        cases.push({
          id: request.id,
          path: request.path,
          body,
          node: { status: response.statusCode, body: json },
          changed: tables.filter((table) => before.get(table) !== after.get(table)),
        });
      }
      await app.close();
      await ports.db.close?.();
      const final = new Database(file, { readonly: true });
      const plateRows = Object.fromEntries(PLATE_TABLES.map((table) => [table, rows(final, table)]));
      final.close();
      phases.push({ name: phase.name, database: phase.database ?? database, ...(phase.seed ? { seed: phase.seed } : {}), seedRows, cases, plateRows });
    } finally {
      rmSync(dataDir, { recursive: true, force: true });
    }
  }
  writeFileSync(process.argv.at(-1), JSON.stringify(phases));
  process.exit(0);
}

const tsx = join(root, "web/node_modules/tsx/dist/esm/index.mjs");
const scratch = mkdtempSync(join(tmpdir(), "pp-parity-plates-arrange-probe-"));
let phases;
try {
  const result = join(scratch, "probe.json");
  execFileSync(
    process.execPath,
    ["--conditions=development", "--import", pathToFileURL(tsx).href, fileURLToPath(import.meta.url), "--probe", result],
    { cwd: join(root, "web/apps/server"), stdio: ["ignore", "ignore", "inherit"] },
  );
  phases = JSON.parse(readFileSync(result, "utf8"));
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
const text = JSON.stringify({
  generated: "@generated by fixtures/parity/plates-arrange/generate.mjs. Do not edit.",
  sources,
  phases,
}, null, 1) + "\n";
if (process.argv.includes("--check")) {
  assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
} else {
  writeFileSync(output, text);
}
console.log(JSON.stringify({ phases: phases.length, cases: phases.reduce((n, phase) => n + phase.cases.length, 0) }));
