// Generates the production-setup parity golden: Node's real
// GET and PATCH /plans/:id/production-setup on disposable copies of
// accepted-plate-node.db (Build 1 with an accepted Plan whose included Part
// has Required unit TOKEN). Each phase seeds its own SQL, with triggers
// dropped and foreign keys off, then sends its requests in order. The golden
// records the seed, every request and response, the tables each request
// changed, and the stored production_setup:1 value after it.
// Run from the repo root after npm --prefix web ci:
//   node fixtures/parity/production-setup/generate.mjs          # write
//   node fixtures/parity/production-setup/generate.mjs --check  # verify, no writes
// updated_at is the only clock value; a written timestamp is recorded as
// "<now>".
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../..");
const output = join(here, "node.json");
const database = "rust/crates/pp-storage/tests/fixtures/accepted-plate-node.db";
const sources = [
  "web/apps/server/src/routes/production-setup.ts",
  "web/apps/server/src/services/production-setup-store.ts",
  "web/apps/server/src/services/production-method-lock.ts",
  "web/packages/contracts/src/production-setup.ts",
  "web/packages/domain/src/production-unit-identity.ts",
];
const TOKEN = "ppu_574c8d5f92624f029e21705a8ebb8a1b";
const OTHER = "ppu_827108f7baf3a2f3461d20c24e85822e";
const URL = "/plans/1/production-setup";
const quote = (text) => "'" + text.replaceAll("'", "''") + "'";
const setting = (key, value) =>
  "DELETE FROM app_settings WHERE tenant_id='default' AND key=" + quote(key) + ";" +
  "INSERT INTO app_settings(tenant_id,key,value) VALUES('default'," + quote(key) + "," +
  quote(typeof value === "string" ? value : JSON.stringify(value)) + ");";
const stored = (value) => setting("production_setup:1", value);
const links = (...rows) => setting("printer.checkoff_links", rows);
const link = (objectName, state = "watching", profileId = 1) => ({
  id: "link-" + state, profile_id: profileId, integration_id: "fixture", printer_id: "p1",
  filename: "plate.3mf", state, units: [{ part_id: 2, unit_index: 0, object_name: objectName }],
});
const get = (id, url = URL) => ({ id, method: "GET", url });
const patch = (id, body, url = URL) => ({ id, method: "PATCH", url, body });
const raw = (id, payload, url = URL) => ({ id, method: "PATCH", url, payload });
const route = (value) => ({ kind: "set_route", route: value });
const SETUP = {
  format: "production-setup-v1",
  profile_id: 1,
  preferred_slicer_instance_id: null,
  selection: { mode: "all_incomplete" },
  printer_assignments: [],
  route: null,
  rules: [],
  updated_at: null,
};
const RULES = [
  { id: " by-color ", enabled: true, kind: "separate_by", field: "color", extra: 1 },
  { kind: "keep_together", value: " Base ", field: "part", enabled: false, id: "keep" },
  { id: "assign", enabled: true, kind: "assign_to_printer", field: "role", value: "body", printer_id: " p1 " },
  { id: "pla", enabled: true, kind: "set_material", field: "source_layer", value: "layer", material_type: " PLA " },
];
const long = (n) => "x".repeat(n);
// The fixture records every unit as printed; the lock needs TOKEN incomplete.
const INCOMPLETE = "UPDATE print_progress SET completed=0, assembled=0 WHERE part_id=2;";
const locked = (setup, ...rows) => stored({ ...SETUP, route: "plates", ...setup }) + links(...rows) + INCOMPLETE;

const PHASES = [
  { name: "reads", requests: [
    get("default"),
    get("api_v2_alias", "/api/v2" + URL),
    get("text_id", "/plans/abc/production-setup"),
    get("zero_id", "/plans/0/production-setup"),
    get("negative_id", "/plans/-1/production-setup"),
    get("fraction_id", "/plans/1.5/production-setup"),
    get("hex_id", "/plans/0x1/production-setup"),
    get("exponent_id", "/plans/1e0/production-setup"),
    get("padded_id", "/plans/%201%20/production-setup"),
    get("missing_build", "/plans/99/production-setup"),
    patch("patch_text_id", route("plates"), "/plans/abc/production-setup"),
    patch("patch_missing_build", { kind: "nope" }, "/plans/99/production-setup"),
  ] },
  { name: "stored_valid", seed: stored({
    rules: RULES, extra: true, updated_at: "2026-10-01T00:00:00.000Z", profile_id: 1,
    selection: { selected_unit_tokens: [" " + TOKEN + " ", "unit:a"], mode: "custom", junk: 1 },
    preferred_slicer_instance_id: " orca ", format: "production-setup-v1",
  }), requests: [get("normalized")] },
  { name: "stored_full", seed: stored({ ...SETUP, route: "stl", printer_assignments: [{ printer_id: " p1 ", token: " t " }] }),
    requests: [get("full")] },
  { name: "stored_not_json", seed: stored("not json"), requests: [get("ignored"), patch("replaces", route("plates"))] },
  { name: "stored_other_build", seed: stored({ ...SETUP, profile_id: 2 }), requests: [get("ignored")] },
  { name: "stored_bad_rule", seed: stored({ ...SETUP, rules: [{ id: "r", enabled: true, kind: "unknown" }] }),
    requests: [get("ignored")] },
  { name: "stored_bad_time", seed: stored({ ...SETUP, updated_at: "2026-02-30T00:00:00Z" }), requests: [get("ignored")] },
  { name: "stored_empty", seed: stored(""), requests: [get("ignored")] },
  { name: "foreign", seed: "UPDATE build_profiles SET tenant_id='foreign' WHERE id=1;", requests: [
    get("read_refused"), patch("patch_refused", route("plates")),
  ] },
  { name: "archived", seed: "UPDATE build_profiles SET archived_at='2026-10-03T00:00:00.000Z' WHERE id=1;", requests: [
    patch("archived_still_saves", route("plates")),
  ] },
  { name: "commands", requests: [
    patch("slicer", { kind: "set_preferred_slicer_instance", preferred_slicer_instance_id: " orca-main " }),
    patch("slicer_cleared", { kind: "set_preferred_slicer_instance", preferred_slicer_instance_id: null }),
    patch("selection_custom", { kind: "set_selection", selection: { mode: "custom", selected_unit_tokens: [TOKEN, " x "], extra: 1 } }),
    patch("assignments", { kind: "replace_printer_assignments", printer_assignments: [{ token: TOKEN, printer_id: " p1 ", extra: 1 }] }),
    patch("rules", { kind: "replace_rules", rules: RULES }),
    patch("route_first", route("plates")),
    patch("route_same", route("plates")),
    patch("route_cleared_unlocked", route(null)),
    patch("selection_all", { kind: "set_selection", selection: { mode: "all_incomplete" } }),
    get("after_commands"),
    patch("api_v2_alias", route("external"), "/api/v2" + URL),
  ] },
  { name: "validation", requests: [
    patch("empty_object", {}),
    raw("null_body", "null"),
    patch("array_body", []),
    patch("unknown_kind", { kind: "nope" }),
    patch("kind_number", { kind: 1 }),
    patch("route_missing", { kind: "set_route" }),
    patch("route_unknown", route("plate")),
    patch("route_number", route(3)),
    patch("extra_key", { kind: "set_route", route: "plates", selection: { mode: "all_incomplete" } }),
    patch("two_extra_keys", { kind: "set_route", route: "plates", b: 1, a: 2 }),
    patch("extra_key_and_bad_route", { x: 1, kind: "set_route", route: "plate" }),
    patch("slicer_blank", { kind: "set_preferred_slicer_instance", preferred_slicer_instance_id: "   " }),
    patch("slicer_too_long", { kind: "set_preferred_slicer_instance", preferred_slicer_instance_id: long(201) }),
    patch("slicer_number", { kind: "set_preferred_slicer_instance", preferred_slicer_instance_id: 5 }),
    patch("slicer_missing", { kind: "set_preferred_slicer_instance" }),
    patch("selection_mode", { kind: "set_selection", selection: { mode: "some" } }),
    patch("selection_missing", { kind: "set_selection" }),
    patch("selection_token_blank", { kind: "set_selection", selection: { mode: "custom", selected_unit_tokens: ["ok", ""] } }),
    patch("selection_tokens_missing", { kind: "set_selection", selection: { mode: "custom" } }),
    patch("selection_too_many", { kind: "set_selection", selection: { mode: "custom", selected_unit_tokens: Array(20_001).fill("t") } }),
    patch("assignment_printer_missing", { kind: "replace_printer_assignments", printer_assignments: [{ token: "t" }] }),
    patch("assignment_not_object", { kind: "replace_printer_assignments", printer_assignments: ["t"] }),
    patch("assignments_not_array", { kind: "replace_printer_assignments", printer_assignments: {} }),
    patch("too_many_with_blank", { kind: "set_selection", selection: { mode: "custom", selected_unit_tokens: ["", ...Array(20_000).fill("t")] } }),
    patch("rules_too_many_and_invalid", { kind: "replace_rules", rules: [{ id: "", enabled: true, kind: "separate_by", field: "part" },
      ...Array(200).fill({ id: "r", enabled: true, kind: "separate_by", field: "part" })] }),
    patch("rules_too_many", { kind: "replace_rules", rules: Array(201).fill({ id: "r", enabled: true, kind: "separate_by", field: "part" }) }),
    patch("rule_unknown_kind", { kind: "replace_rules", rules: [{ id: "r", enabled: true, kind: "unknown" }] }),
    patch("rule_bad_field", { kind: "replace_rules", rules: [{ id: "r", enabled: true, kind: "set_material", field: "material", value: "v", material_type: "m" }] }),
    patch("rule_value_long", { kind: "replace_rules", rules: [{ id: "r", enabled: true, kind: "keep_together", field: "part", value: long(501) }] }),
    patch("rule_enabled_text", { kind: "replace_rules", rules: [{ id: " ", enabled: "yes", kind: "separate_by", field: "part" }] }),
    get("unchanged"),
  ] },
  { name: "lock", seed: locked({}, link("bracket__" + TOKEN)), requests: [
    patch("locked", route("stl")),
    patch("cleared_locked", route(null)),
    patch("same_route_allowed", route("plates")),
    patch("other_commands_allowed", { kind: "replace_rules", rules: [] }),
    patch("custom_without_token", { kind: "set_selection", selection: { mode: "custom", selected_unit_tokens: [OTHER] } }),
    patch("unlocked_by_selection", route("stl")),
  ] },
  { name: "lock_slicer_suffix", seed: locked({}, link("bracket__" + TOKEN + "_id_0_copy_0")), requests: [
    patch("locked", route("external")),
  ] },
  { name: "lock_completed_unit", seed: stored({ ...SETUP, route: "plates" }) + links(link("bracket__" + TOKEN)), requests: [
    patch("allowed", route("stl")),
  ] },
  { name: "lock_dismissed", seed: locked({}, link("bracket__" + TOKEN, "dismissed")), requests: [
    patch("allowed", route("stl")),
  ] },
  { name: "lock_other_build", seed: locked({}, link("bracket__" + TOKEN, "watching", 2)), requests: [
    patch("allowed", route("stl")),
  ] },
  { name: "lock_token_inside_name", seed: locked({}, link(TOKEN + "__x")), requests: [
    patch("allowed", route("stl")),
  ] },
  { name: "lock_unselected_unit", seed: locked({ selection: { mode: "custom", selected_unit_tokens: [OTHER] } }, link("bracket__" + OTHER)), requests: [
    patch("allowed_excluded_part", route("stl")),
  ] },
  { name: "lock_custom_selection", seed: locked({ selection: { mode: "custom", selected_unit_tokens: [TOKEN] } }, link("bracket__" + TOKEN)), requests: [
    patch("locked", route("stl")),
  ] },
  { name: "lock_unavailable", seed: locked({}, link("bracket__" + TOKEN)) +
    "UPDATE build_profiles SET accepted_plan_revision_id=NULL WHERE id=1;", requests: [
    patch("unavailable", route("stl")),
    patch("same_route_skips_check", route("plates")),
  ] },
  { name: "bodies", requests: [
    raw("over_eight_mib", JSON.stringify({ kind: "replace_rules", rules: [], pad: long(8 * 1024 * 1024) })),
    raw("not_json", "{"),
  ] },
];

const stamp = (value) => {
  if (value && typeof value === "object" && typeof value.updated_at === "string" && value.updated_at.length === 24 &&
    !value.updated_at.startsWith("2026-10-01")) return { ...value, updated_at: "<now>" };
  return value;
};

if (process.argv.includes("--probe")) {
  const load = (source) => import(pathToFileURL(join(root, source)).href);
  const require = createRequire(join(root, "web/apps/server/package.json"));
  const Database = require("better-sqlite3");
  delete process.env.PRINT_PARTNER_API_KEY;
  const { createSelfHostPorts } = await load("web/apps/server/src/adapters/self-host/index.ts");
  const { buildApp } = await load("web/apps/server/src/app.ts");
  const { loadConfig } = await load("web/apps/server/src/config.ts");
  const phases = [];
  for (const phase of PHASES) {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-parity-production-setup-"));
    try {
      const file = join(dataDir, "print-partner.db");
      copyFileSync(join(root, database), file);
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
        const row = db.prepare("SELECT value FROM app_settings WHERE tenant_id='default' AND key='production_setup:1'").get();
        db.close();
        return { state, setup: row ? row.value : null };
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
        const json = response.body === "" ? null : JSON.parse(response.body);
        let setup = after.setup;
        try { setup = stamp(JSON.parse(setup)); } catch { /* raw text */ }
        cases.push({
          id: request.id,
          method: request.method,
          url: request.url,
          ...(request.body === undefined ? {} : { body: request.body }),
          ...(request.payload === undefined ? {} : { payload: request.payload.length > 4096 ? { repeat: request.payload.length } : request.payload }),
          node: { status: response.statusCode, body: stamp(json) },
          changed: tables.filter((table) => before.state.get(table) !== after.state.get(table)),
          setup: before.setup === after.setup ? "unchanged" : setup,
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
const scratch = mkdtempSync(join(tmpdir(), "pp-parity-production-setup-probe-"));
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
  generated: "@generated by fixtures/parity/production-setup/generate.mjs. Do not edit.",
  sources,
  phases,
}, null, 1) + "\n";
if (process.argv.includes("--check")) {
  assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
} else {
  writeFileSync(output, text);
}
console.log(JSON.stringify({ phases: phases.length, cases: phases.reduce((n, phase) => n + phase.cases.length, 0) }));
