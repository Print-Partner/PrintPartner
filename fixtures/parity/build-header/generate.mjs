// Generates the build-header parity golden: Node's real PATCH /plans/:id and
// POST /plans/:id/archive on the flat, /api/v1 and /api/v2 mounts, on
// disposable copies of accepted-plate-node.db (Build 1, whose accepted Plan's
// units are all printed). Node runs with PP_DESKTOP_REBUILD=1, the only mode in
// which Rust serves these routes, so the orders feature gate applies. Each
// phase seeds its own SQL, with triggers dropped and foreign keys off, then
// sends its requests in order. The golden records the seed, every request and
// response, and the tables each request changed. archived_at and last_used_at
// are the only clock values; a time written now is recorded as "<now>".
// Run from the repo root after npm --prefix web ci:
//   node fixtures/parity/build-header/generate.mjs          # write
//   node fixtures/parity/build-header/generate.mjs --check  # verify, no writes
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
const sources = [
  "web/apps/server/src/routes/plans.ts",
  "web/apps/server/src/db/accepted-plan-progress.ts",
  "web/apps/server/src/lib/feature-access.ts",
];
const FIXTURE_TIMES = ["2026-10-02T21:06:51.794Z", "2026-10-03T00:00:00.000Z"];
const quote = (text) => "'" + text.replaceAll("'", "''") + "'";
const setting = (key, value) =>
  "INSERT INTO app_settings(tenant_id,key,value) VALUES('default'," + quote(key) + "," + quote(value) + ");";
const ORDERS_ON = setting("feature_toggles", "{\"orders\":true}");
const INCOMPLETE = "UPDATE print_progress SET completed=0, assembled=0 WHERE part_id=2;";
const TAKEN = "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Taken');";
const patch = (id, body, url = "/plans/1") => ({ id, method: "PATCH", url, body });
const raw = (id, payload, url = "/plans/1") => ({ id, method: "PATCH", url, payload });
const archive = (id, url = "/plans/1/archive", payload) => ({ id, method: "POST", url, payload });

const PHASES = [
  { name: "header", requests: [
    patch("rename_flat", { name: "  Renamed  " }),
    patch("rename_same_v2", { name: "Renamed" }, "/api/v2/plans/1"),
    patch("rename_v1", { name: "Legacy" }, "/api/v1/plans/1"),
    patch("rename_blank", { name: "   " }),
    patch("rename_number_ignored", { name: 5 }),
    patch("empty_body", {}),
    raw("array_body", "[]"),
    patch("touch", { touch_last_used: true }),
    patch("touch_false", { touch_last_used: false, extra: 1 }),
    patch("special_request_orders_off", { special_request: "note" }),
    patch("special_request_off_with_name", { name: "Not saved", special_request: null }),
    patch("unarchive", { archived: false }),
    patch("unarchive_missing_build", { archived: false }, "/plans/99"),
    patch("missing_build", {}, "/plans/99"),
    patch("foreign_build_v2", { name: "x" }, "/api/v2/plans/2"),
    patch("text_id", { name: "x" }, "/plans/abc"),
    patch("hex_id", { name: "Hex" }, "/plans/0x1"),
    raw("not_json", "{"),
    raw("null_body", "null"),
  ] },
  { name: "duplicate_name", seed: TAKEN, requests: [
    patch("rename_taken", { name: " Taken " }),
    patch("rename_taken_with_archive", { name: "Taken", archived: true }),
    patch("rename_taken_by_itself", { name: "Taken" }, "/plans/3"),
  ] },
  { name: "orders_on", seed: ORDERS_ON, requests: [
    patch("special_request_trimmed", { special_request: "  rush  " }),
    patch("special_request_blank", { special_request: "   " }),
    patch("special_request_number", { special_request: 5 }),
    patch("special_request_bool_v2", { special_request: true }, "/api/v2/plans/1"),
    patch("special_request_object", { special_request: { a: 1 } }),
    patch("special_request_array_v1", { special_request: ["a", null, 2] }, "/api/v1/plans/1"),
    patch("special_request_null", { special_request: null }),
    patch("name_and_request", { name: "Both", special_request: "x", touch_last_used: true }),
  ] },
  { name: "orders_plan_off", seed: ORDERS_ON + setting("feature_toggles:plan:1", "{\"orders\":false}"), requests: [
    patch("plan_turns_orders_off", { special_request: "x" }),
  ] },
  { name: "toggles_invalid", seed: setting("feature_toggles", "not json"), requests: [
    patch("invalid_toggles", { special_request: "x" }),
    patch("invalid_toggles_unused", { name: "Fine" }),
  ] },
  { name: "archive", requests: [
    archive("archive_not_json", "/plans/1/archive", "{"),
    archive("archive_empty_json", "/plans/1/archive", ""),
    archive("archive_flat"),
    archive("archive_again_v2", "/api/v2/plans/1/archive"),
    patch("patch_archived_again", { archived: true }),
    archive("archive_missing", "/plans/99/archive"),
    archive("archive_foreign_v1", "/api/v1/plans/2/archive"),
    archive("archive_text_id", "/plans/abc/archive"),
  ] },
  { name: "archive_v1", requests: [archive("archive_v1", "/api/v1/plans/1/archive")] },
  { name: "archive_object", requests: [archive("archive_object_body", "/plans/1/archive", "{}")] },
  { name: "patch_archive", requests: [
    patch("rename_and_archive_v2", { name: "Done", archived: true, touch_last_used: true }, "/api/v2/plans/1"),
  ] },
  { name: "remaining", seed: INCOMPLETE, requests: [
    archive("archive_remaining"),
    patch("rename_then_remaining", { name: "Kept", archived: true }),
    patch("blank_name_with_archive", { name: " ", archived: true }),
    patch("rename_remaining_v1", { name: "Legacy remaining" }, "/api/v1/plans/1"),
  ] },
  { name: "uninitialized", seed: "UPDATE build_profiles SET accepted_plan_revision_id=NULL WHERE id=1;", requests: [
    archive("archive_uninitialized"),
    patch("rename_uninitialized_v1", { name: "x" }, "/api/v1/plans/1"),
    patch("rename_uninitialized_v2", { name: "Fresh" }, "/api/v2/plans/1"),
  ] },
  { name: "archived_uninitialized", seed:
    "UPDATE build_profiles SET accepted_plan_revision_id=NULL, archived_at='2026-10-03T00:00:00.000Z' WHERE id=1;", requests: [
    archive("already_archived_skips_state"),
  ] },
  { name: "no_layers", seed: "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Bare');", requests: [
    archive("archive_empty_build", "/plans/3/archive"),
    patch("rename_empty_build_v1", { name: "Bare renamed" }, "/api/v1/plans/3"),
  ] },
];

const stamp = (value) => {
  if (Array.isArray(value)) return value.map(stamp);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key,
    (key === "archived_at" || key === "last_used_at") && typeof entry === "string" && !FIXTURE_TIMES.includes(entry)
      ? "<now>" : stamp(entry)]));
};

if (process.argv.includes("--probe")) {
  const load = (source) => import(pathToFileURL(join(root, source)).href);
  const require = createRequire(join(root, "web/apps/server/package.json"));
  const Database = require("better-sqlite3");
  delete process.env.PRINT_PARTNER_API_KEY;
  process.env.PP_DESKTOP_REBUILD = "1";
  const { createSelfHostPorts } = await load("web/apps/server/src/adapters/self-host/index.ts");
  const { buildApp } = await load("web/apps/server/src/app.ts");
  const { loadConfig } = await load("web/apps/server/src/config.ts");
  const phases = [];
  for (const phase of PHASES) {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-parity-build-header-"));
    try {
      const file = join(dataDir, "print-partner.db");
      copyFileSync(join(root, database), file);
      mkdirSync(join(dataDir, "repos/1/revisions/accepted"), { recursive: true });
      const seed = phase.seed ?? "";
      {
        const db = new Database(file);
        db.pragma("foreign_keys = OFF");
        for (const { name } of db.prepare("SELECT name FROM sqlite_schema WHERE type = 'trigger'").all()) {
          db.exec("DROP TRIGGER \"" + name + "\"");
        }
        db.exec(seed);
        db.close();
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
      process.env.PRINT_PARTNER_DATA_DIR = dataDir;
      const ports = createSelfHostPorts(dataDir);
      await ports.db.connect();
      const app = await buildApp(loadConfig(), ports);
      await app.ready();
      const cases = [];
      for (const request of phase.requests) {
        const before = graph();
        const payload = request.payload ?? (request.body === undefined ? undefined : JSON.stringify(request.body));
        const response = await app.inject({
          method: request.method,
          url: request.url,
          ...(payload === undefined ? {} : { payload, headers: { "content-type": "application/json" } }),
        });
        const after = graph();
        cases.push({
          id: request.id,
          method: request.method,
          url: request.url,
          ...(request.body === undefined ? {} : { body: request.body }),
          ...(request.payload === undefined ? {} : { payload: request.payload }),
          node: { status: response.statusCode, body: stamp(response.body === "" ? null : JSON.parse(response.body)) },
          changed: tables.filter((table) => before.get(table) !== after.get(table)),
        });
      }
      await app.close();
      await ports.db.close?.();
      phases.push({ name: phase.name, seed, cases });
    } finally {
      rmSync(dataDir, { recursive: true, force: true });
    }
  }
  writeFileSync(process.argv.at(-1), JSON.stringify(phases));
  process.exit(0);
}

const tsx = join(root, "web/node_modules/tsx/dist/esm/index.mjs");
const scratch = mkdtempSync(join(tmpdir(), "pp-parity-build-header-probe-"));
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
const line = (value) => JSON.stringify(value);
const text = "{\n" +
  " \"generated\": " + line("@generated by fixtures/parity/build-header/generate.mjs. Do not edit.") + ",\n" +
  " \"sources\": " + line(sources) + ",\n" +
  " \"phases\": [\n" +
  phases.map((phase) => "  {\"name\": " + line(phase.name) + ", \"seed\": " + line(phase.seed) + ", \"cases\": [\n" +
    phase.cases.map((entry) => "   " + line(entry)).join(",\n") + "\n  ]}").join(",\n") +
  "\n ]\n}\n";
if (process.argv.includes("--check")) {
  assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
} else {
  writeFileSync(output, text);
}
console.log(JSON.stringify({ phases: phases.length, cases: phases.reduce((n, phase) => n + phase.cases.length, 0) }));
