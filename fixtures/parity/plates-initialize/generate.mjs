// Generates the plates-initialize parity golden: Node's real
// POST /plans/:id/plates/initialize on disposable copies of
// accepted-plate-node.db (one Required unit on included.stl) and
// required-units/unchanged.db (bracket units 1-3, gear units 5-6).
// The fixtures pin artifact digests whose files do not exist, so each phase
// writes its STL files into the pinned Source snapshot and its seed SQL
// re-pins those Parts to the files' SHA-256 and restamps the revision digest
// with Node's digestPlanRevisionParts. The seed also stores a Printer fleet
// and runs the phase's own SQL, with triggers dropped and foreign keys off.
// The golden records the files, the seed, every response, the tables each
// request changed, and the Plate rows at the end.
// Run from the repo root after npm --prefix web ci:
//   node fixtures/parity/plates-initialize/generate.mjs          # write
//   node fixtures/parity/plates-initialize/generate.mjs --check  # verify, no writes
// created_at is the only clock value; it is recorded as null.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../..");
const output = join(here, "node.json");
const single = "rust/crates/pp-storage/tests/fixtures/accepted-plate-node.db";
const multi = "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db";
const sources = [
  "web/apps/server/src/routes/accepted-plates.ts",
  "web/apps/server/src/routes/accepted-plates-route-model.ts",
  "web/apps/server/src/services/accepted-plate-workspace.ts",
  "web/apps/server/src/services/accepted-artifact-geometry.ts",
  "web/apps/server/src/services/accepted-artifacts.ts",
  "web/apps/server/src/db/accepted-plates.ts",
];
const TOKEN = "ppu_574c8d5f92624f029e21705a8ebb8a1b";
const PLATE_TABLES = ["accepted_plate_revisions", "accepted_plates", "accepted_plate_units", "accepted_plate_heads"];
const ppu = (n) => "ppu_" + String(n).padStart(32, "0");
const SNAPSHOT = { [single]: "repos/1/revisions/accepted", [multi]: "repos/1/revisions/fixture" };

// An ASCII STL box from (0,0,0) to (w,d,h) millimetres: 12 facets.
const box = (w, d, h) => {
  const v = (x, y, z) => [x ? w : 0, y ? d : 0, z ? h : 0].join(" ");
  const quads = [[[0,0,0],[1,0,0],[1,1,0],[0,1,0]], [[0,0,1],[1,0,1],[1,1,1],[0,1,1]], [[0,0,0],[1,0,0],[1,0,1],[0,0,1]],
    [[0,1,0],[1,1,0],[1,1,1],[0,1,1]], [[0,0,0],[0,1,0],[0,1,1],[0,0,1]], [[1,0,0],[1,1,0],[1,1,1],[1,0,1]]];
  const facets = quads.flatMap(([a, b, c, e]) => [[a, b, c], [a, c, e]]).map((tri) =>
    " facet normal 0 0 0\n  outer loop\n" + tri.map((p) => "   vertex " + v(...p) + "\n").join("") + "  endloop\n endfacet\n");
  return "solid box\n" + facets.join("") + "endsolid box\n";
};
const FLEET = [
  { id: "p-a", name: "Printer A", model: "Model A", bed_width_mm: 250, bed_depth_mm: 220, bed_height_mm: 200, margin_mm: 5 },
  { id: "p-b", name: "Printer B", model: "Model B", bed_width_mm: 180, bed_depth_mm: 180, bed_height_mm: 150, margin_mm: 3 },
  { id: "p-tiny", name: "Tiny", model: "Tiny", bed_width_mm: 12, bed_depth_mm: 12, bed_height_mm: 12, margin_mm: 1 },
  { id: "p-nogeo", name: "No height", model: "No height", bed_width_mm: 200, bed_depth_mm: 200, bed_height_mm: null, margin_mm: 2 },
  { id: "p-huge", name: "Huge", model: "Huge", bed_width_mm: 3000000, bed_depth_mm: 210, bed_height_mm: 200, margin_mm: 4 },
];
const setup = (rules) => "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','production_setup:1','" +
  JSON.stringify({ format: "production-setup-v1", profile_id: 1, preferred_slicer_instance_id: null, updated_at: null,
    selection: { mode: "all_incomplete" }, rules }) + "');";
// A phase's files: { relative path: { pin, write } }. pin is the content whose
// SHA-256 the Part is pinned to; write is what lands on disk (null: nothing).
const same = (text) => ({ pin: text, write: text });
const one = (file) => ({ "included.stl": file });
const assign = (...pairs) => pairs.map(([token, printer]) => ({ token, printer_id: printer }));
// A request names the revision it expects: "head" is the newest revision any
// earlier 200 answered (the phase's starting head at first), "start" the
// starting head, or a literal value.
const init = (id, assignments, extra = {}) => ({ id, path: "/plans/1/plates/initialize", assignments, revision: "head", ...extra });
const NO_HEAD = "DELETE FROM accepted_plate_heads;";
const MULTI_ALL = (printer) => assign(...[1, 2, 3, 5, 6].map((n) => [ppu(n), printer]));

const PHASES = [
  { name: "stale_head", database: single, files: one(same(box(20, 30, 10))), requests: [
    init("publish_over_stale_head", assign([TOKEN, "p-a"])),
    init("replay_same_assignment", assign([TOKEN, "p-a"]), { revision: "start" }),
    init("replay_other_printer_refused", assign([TOKEN, "p-b"]), { revision: "start" }),
    init("same_head_unchanged", assign([TOKEN, "p-a"])),
    init("same_head_other_printer", assign([TOKEN, "p-b"])),
  ] },
  { name: "no_head", database: single, files: one(same(box(20, 30, 10))), seed: NO_HEAD, requests: [
    init("expects_missing_head", assign([TOKEN, "p-a"]), { revision: 1 }),
    init("creates_head", assign([TOKEN, "p-a"]), { revision: null }),
    init("replay_without_head", assign([TOKEN, "p-a"]), { revision: null }),
  ] },
  { name: "refusals", database: single, files: one(same(box(20, 30, 10))), seed: NO_HEAD, requests: [
    init("missing_assignment", [], { revision: null }),
    init("unknown_unit_token", assign([ppu(9), "p-a"]), { revision: null }),
    init("duplicate_assignment", assign([TOKEN, "p-a"], [TOKEN, "p-b"]), { revision: null }),
    init("unassigned_units", assign([TOKEN, null]), { revision: null }),
    init("printer_not_found", assign([TOKEN, "p-zzz"]), { revision: null }),
    init("missing_printer_geometry", assign([TOKEN, "p-nogeo"]), { revision: null }),
    init("unit_too_large", assign([TOKEN, "p-tiny"]), { revision: null }),
    init("outside_build_area", assign([TOKEN, "p-huge"]), { revision: null }),
    init("stale_revision_before_assignments", assign([ppu(9), "p-a"]), { revision: 5 }),
    init("stale_basis", assign([TOKEN, "p-a"]), { revision: null, basis: { plan_revision_digest: "0".repeat(64) } }),
    init("revision_missing", assign([TOKEN, "p-a"]), { revision: undefined }),
    init("revision_zero", assign([TOKEN, "p-a"]), { revision: 0 }),
    init("printer_blank", assign([TOKEN, "  "]), { revision: null }),
    init("printer_trimmed", assign([TOKEN, " p-a "]), { revision: null }),
    init("token_invalid", assign(["ppu_nope", "p-a"]), { revision: null }),
    init("assignments_not_array", { token: TOKEN }, { revision: null }),
    init("basis_other_profile", assign([TOKEN, "p-a"]), { revision: null, basis: { profile_id: 2 } }),
    init("path_text", assign([TOKEN, "p-a"]), { revision: null, path: "/plans/abc/plates/initialize" }),
    init("missing_plan", assign([TOKEN, "p-a"]), { revision: null, path: "/plans/2/plates/initialize", basis: { profile_id: 2 } }),
  ] },
  { name: "archived", database: single, files: one(same(box(20, 30, 10))),
    seed: NO_HEAD + "UPDATE build_profiles SET archived_at='2026-10-03T00:00:00.000Z' WHERE id=1;", requests: [
    init("archived_refused", assign([TOKEN, "p-a"]), { revision: null }),
    init("archived_before_outside_bed", assign([TOKEN, "p-huge"]), { revision: null }),
  ] },
  { name: "foreign_plan", database: single, files: one(same(box(20, 30, 10))),
    seed: NO_HEAD + "UPDATE build_profiles SET tenant_id='foreign' WHERE id=1;", requests: [
    init("foreign_refused", assign([TOKEN, "p-a"]), { revision: null }),
  ] },
  { name: "missing_file", database: single, files: one({ pin: box(20, 30, 10), write: null }), seed: NO_HEAD, requests: [
    init("artifact_missing", assign([TOKEN, "p-a"]), { revision: null }),
  ] },
  { name: "digest_mismatch", database: single, files: one({ pin: box(20, 30, 10), write: box(20, 30, 11) }), seed: NO_HEAD, requests: [
    init("artifact_digest_mismatch", assign([TOKEN, "p-a"]), { revision: null }),
  ] },
  { name: "invalid_stl", database: single, files: one(same("solid nope\nendsolid nope\n")), seed: NO_HEAD, requests: [
    init("artifact_invalid_stl", assign([TOKEN, "p-a"]), { revision: null }),
  ] },
  { name: "degenerate", database: single, files: one(same(box(20, 30, 0))), seed: NO_HEAD, requests: [
    init("artifact_degenerate", assign([TOKEN, "p-a"]), { revision: null }),
  ] },
  { name: "case_insensitive", database: single, files: { "INCLUDED.STL": { pin: box(20, 30, 10), write: box(20, 30, 10), part: "included.stl" } }, seed: NO_HEAD, requests: [
    init("artifact_matched_ignoring_case", assign([TOKEN, "p-a"]), { revision: null }),
  ] },
  { name: "multi_two_printers", database: multi, files: { "bracket.stl": same(box(40, 30, 20)), "gear.stl": same(box(25, 25, 10)) }, requests: [
    init("multi_splits_by_printer", assign(...[1, 2, 3].map((n) => [ppu(n), "p-a"]), ...[5, 6].map((n) => [ppu(n), "p-b"])), { revision: null }),
  ] },
  { name: "multi_rules", database: multi, files: { "bracket.stl": same(box(40, 30, 20)), "gear.stl": same(box(25, 25, 10)) },
    seed: setup([{ id: "by-part", enabled: true, kind: "separate_by", field: "part" }]), requests: [
    init("multi_separates_parts", MULTI_ALL("p-a"), { revision: null }),
  ] },
  { name: "multi_partial", database: multi, files: { "bracket.stl": same(box(40, 30, 20)), "gear.stl": same(box(25, 25, 10)) }, requests: [
    init("multi_leaves_unassigned", assign(...[1, 2, 3].map((n) => [ppu(n), "p-b"])), { revision: null }),
  ] },
  { name: "multi_overflow", database: multi, files: { "bracket.stl": same(box(100, 100, 20)), "gear.stl": same(box(100, 90, 10)) }, requests: [
    init("multi_overflows_to_new_plates", MULTI_ALL("p-b"), { revision: null }),
  ] },
  { name: "multi_outside_bed", database: multi, files: { "bracket.stl": same(box(40, 30, 20)), "gear.stl": same(box(25, 25, 10)) }, requests: [
    init("multi_too_large_after_outside_bed", assign(...[1, 2, 3].map((n) => [ppu(n), "p-huge"]), ...[5, 6].map((n) => [ppu(n), "p-tiny"])), { revision: null }),
    init("multi_outside_bed_refused", assign(...[1, 2, 3].map((n) => [ppu(n), "p-huge"]), ...[5, 6].map((n) => [ppu(n), "p-a"])), { revision: null }),
  ] },
];

const sha = (text) => createHash("sha256").update(text).digest("hex");
const quote = (text) => "'" + text.replaceAll("'", "''") + "'";

if (process.argv.includes("--probe")) {
  const load = (source) => import(pathToFileURL(join(root, source)).href);
  const require = createRequire(join(root, "web/apps/server/package.json"));
  const Database = require("better-sqlite3");
  delete process.env.PRINT_PARTNER_API_KEY;
  const { createSelfHostPorts } = await load("web/apps/server/src/adapters/self-host/index.ts");
  const { buildApp } = await load("web/apps/server/src/app.ts");
  const { loadConfig } = await load("web/apps/server/src/config.ts");
  const { digestPlanRevisionParts } = await load("web/apps/server/src/services/plan-publication.ts");
  const rows = (db, table) =>
    db.prepare("SELECT * FROM \"" + table + "\" ORDER BY rowid").all().map((row) =>
      "created_at" in row ? { ...row, created_at: null } : row);
  const camel = (name) => name.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
  const DIGEST_FIELDS = ["part_key", "relative_path", "filename", "source_layer", "status", "role_inferred", "role_override",
    "filament_color_id", "filament_custom_hex", "spoolman_spool_id", "quantity_inferred", "quantity_override", "quantity_effective",
    "included", "notes", "github_blob_url", "geometry_same", "requirement", "option_group_id", "manifest_source", "artifact_digest"];

  const phases = [];
  for (const phase of PHASES) {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-parity-plates-initialize-"));
    try {
      const file = join(dataDir, "print-partner.db");
      copyFileSync(join(root, phase.database), file);
      for (const revision of ["accepted", "fixture"]) {
        mkdirSync(join(dataDir, "repos/1/revisions", revision), { recursive: true, mode: 0o700 });
      }
      const files = [];
      for (const [name, spec] of Object.entries(phase.files)) {
        if (spec.write !== null) files.push({ path: SNAPSHOT[phase.database] + "/" + name, text: spec.write });
      }
      // Re-pin each Part to its file and restamp the revision digest, on a
      // scratch copy, to learn the SQL the phase seeds.
      const pins = Object.entries(phase.files).map(([name, spec]) =>
        "UPDATE plan_revision_parts SET artifact_digest=" + quote(sha(spec.pin)) + " WHERE revision_id=1 AND relative_path=" + quote(spec.part ?? name) + ";");
      const scratch = new Database(":memory:");
      scratch.exec("ATTACH " + quote(join(root, phase.database)) + " AS src; CREATE TABLE plan_revision_parts AS SELECT * FROM src.plan_revision_parts WHERE revision_id=1;");
      scratch.exec(pins.join(""));
      const parts = scratch.prepare("SELECT * FROM plan_revision_parts ORDER BY id").all().map((row) =>
        Object.fromEntries(DIGEST_FIELDS.map((field) => {
          const value = row[field];
          const boolean = field === "included" || field === "geometry_same";
          return [camel(field), boolean && value !== null ? value === 1 : value];
        })));
      scratch.close();
      const digest = digestPlanRevisionParts(parts);
      const seed = pins.join("") +
        "UPDATE plan_revisions SET snapshot_digest=" + quote(digest) + " WHERE id=1;" +
        "UPDATE plan_apply_requests SET revision_digest=" + quote(digest) + " WHERE revision_id=1;" +
        "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.fleet'," + quote(JSON.stringify(FLEET)) + ");" +
        (phase.seed ?? "");
      for (const { path, text } of files) writeFileSync(join(dataDir, path), text);
      {
        const db = new Database(file);
        db.pragma("foreign_keys = OFF");
        for (const { name } of db.prepare("SELECT name FROM sqlite_schema WHERE type = 'trigger'").all()) {
          db.exec("DROP TRIGGER \"" + name + "\"");
        }
        db.exec(seed);
        db.close();
      }
      const start = (() => {
        const db = new Database(file, { readonly: true });
        const head = db.prepare("SELECT current_revision_id AS id FROM accepted_plate_heads WHERE profile_id=1").get();
        const base = db.prepare("SELECT b.accepted_plan_version AS planVersion, r.id AS revisionId, r.snapshot_digest AS revisionDigest, s.mapping_digest AS mappingDigest FROM build_profiles b JOIN plan_revisions r ON r.id=b.accepted_plan_revision_id LEFT JOIN plan_revision_required_unit_sets s ON s.revision_id=r.id WHERE b.id=1").get();
        db.close();
        return { head: head?.id ?? null, base };
      })();
      const basis = {
        profile_id: 1,
        plan_version: start.base.planVersion,
        plan_revision_id: start.base.revisionId,
        plan_revision_digest: start.base.revisionDigest,
        required_unit_mapping_digest: start.base.mappingDigest,
      };
      process.env.PRINT_PARTNER_DATA_DIR = dataDir;
      const ports = createSelfHostPorts(dataDir);
      await ports.db.connect();
      const app = await buildApp(loadConfig(), ports);
      await app.ready();
      if (!phase.seed?.includes("tenant_id='foreign'")) {
        const read = await app.inject({ method: "GET", url: "/plans/1/plates" });
        assert.equal(read.statusCode, 200, phase.name + ": " + read.body);
        assert.ok(["setup", "ready"].includes(read.json().kind), phase.name + ": " + read.body);
      }
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
      let head = start.head;
      const cases = [];
      for (const request of phase.requests) {
        const revision = request.revision === "head" ? head : request.revision === "start" ? start.head : request.revision;
        const body = {
          expected: { ...basis, ...request.basis },
          ...(revision === undefined ? {} : { expected_plate_revision_id: revision }),
          assignments: request.assignments,
        };
        const before = graph();
        const response = await app.inject({ method: "POST", url: request.path, payload: body });
        const after = graph();
        const json = response.body === "" ? null : response.json();
        if (response.statusCode === 200 && typeof json?.plate_revision_id === "number") head = json.plate_revision_id;
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
      phases.push({ name: phase.name, database: phase.database, files, seed, seedRows: {}, cases, plateRows });
    } finally {
      rmSync(dataDir, { recursive: true, force: true });
    }
  }
  writeFileSync(process.argv.at(-1), JSON.stringify(phases));
  process.exit(0);
}

const tsx = join(root, "web/node_modules/tsx/dist/esm/index.mjs");
const scratch = mkdtempSync(join(tmpdir(), "pp-parity-plates-initialize-probe-"));
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
  generated: "@generated by fixtures/parity/plates-initialize/generate.mjs. Do not edit.",
  sources,
  phases,
}, null, 1) + "\n";
if (process.argv.includes("--check")) {
  assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
} else {
  writeFileSync(output, text);
}
console.log(JSON.stringify({ phases: phases.length, cases: phases.reduce((n, phase) => n + phase.cases.length, 0) }));
