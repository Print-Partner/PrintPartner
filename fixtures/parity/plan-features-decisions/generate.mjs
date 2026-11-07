// Generates the plan-features-decisions parity golden: complete HTTP responses
// from Node's real app for GET and PATCH /plans/:id/features and
// POST /plans/:id/decisions on the flat, /api/v1 and /api/v2 mounts, plus the
// tables each request changed and the text Node stored. Node opens a copy of
// the required-units unchanged.db fixture (Plan 1 in tenant default, Plan 2 in
// tenant foreign) after SEED adds Plans 3-12 and stored feature layers: a
// tenant layer with an unknown key, a Plan layer whose unknown keys hold
// nested values, index keys and number text Node keeps as written, and Plan
// layers Node refuses (not JSON, an array, null and number choices, empty
// text). Requests run in order on one database. A case with "sql" runs that
// SQL on the database before its request; a case with "rebuild": false runs
// with PP_DESKTOP_REBUILD unset, every other with PP_DESKTOP_REBUILD=1.
// A decision's created_at is the clock, recorded as "<now>".
// Run from the repo root after npm --prefix web ci:
//   node fixtures/parity/plan-features-decisions/generate.mjs          # write
//   node fixtures/parity/plan-features-decisions/generate.mjs --check  # verify, no writes
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
const database = "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db";
const sources = [
  "web/apps/server/src/routes/plans.ts",
  "web/apps/server/src/services/plan-features.ts",
  "web/apps/server/src/services/plan-decisions.ts",
];
const SEED = [
  "INSERT INTO build_profiles (id, tenant_id, name) VALUES",
  "(3, 'default', 'Feature Plan'), (4, 'default', 'Raw Plan'), (5, 'default', 'Not JSON Plan'),",
  "(6, 'default', 'Array Plan'), (7, 'default', 'Null Choice Plan'), (8, 'default', 'Number Choice Plan'),",
  "(9, 'default', 'Duplicate Plan'), (10, 'default', 'Empty Text Plan'), (11, 'default', 'Surrogate Plan'),",
  "(12, 'default', 'Decision Plan');",
  "INSERT INTO app_settings (tenant_id, key, value) VALUES",
  "('default', 'feature_toggles', '{\"bambu\":true,\"extra\":2.50}'),",
  "('default', 'feature_toggles:plan:4', '{\"zeta\": 1.50, \"10\": {\"b\": 1e2, \"a\": [1.0, \"\\u0041\\/\"]}, \"farm\": true, \"2\": null, \"orders\": false, \"big\": 12345678901234567890, \"neg\": -0, \"exp\": 1E+2}'),",
  "('default', 'feature_toggles:plan:5', 'not json'),",
  "('default', 'feature_toggles:plan:6', '[]'),",
  "('default', 'feature_toggles:plan:7', '{\"farm\":null}'),",
  "('default', 'feature_toggles:plan:8', '{\"orders\":1}'),",
  "('default', 'feature_toggles:plan:9', ' {\"farm\": true, \"spoolman\": false, \"farm\": false} '),",
  "('default', 'feature_toggles:plan:10', ''),",
  "('default', 'feature_toggles:plan:11', '{\"\\ud83d\":\"\\ud83d\\ude00\",\"farm\":true}'),",
  "('foreign', 'feature_toggles:plan:2', '{\"farm\":true}');",
].join(" ");
const USER_INVALID = "UPDATE app_settings SET value = '{\"farm\":\"yes\"}' WHERE tenant_id = 'default' AND key = 'feature_toggles';";
const USER_MISSING = "DELETE FROM app_settings WHERE tenant_id = 'default' AND key = 'feature_toggles';";

const get = (id, path, extra = {}) => ({ id, method: "GET", path, ...extra });
const patch = (id, path, body, extra = {}) => ({ id, method: "PATCH", path, payload: JSON.stringify(body), ...extra });
const rawPatch = (id, path, payload, extra = {}) => ({ id, method: "PATCH", path, payload, verbatim: true, ...extra });
const post = (id, path, body, extra = {}) => ({ id, method: "POST", path, payload: JSON.stringify(body), ...extra });
const rawPost = (id, path, payload, extra = {}) => ({ id, method: "POST", path, payload, verbatim: true, ...extra });
const decisions = (id) => "/plans/" + id + "/decisions";
const features = (id) => "/plans/" + id + "/features";
const REQUESTS = [
  get("read_no_layer_flat", features(1)),
  get("read_raw_v1", "/api/v1" + features(4)),
  get("read_duplicate_key_v2", "/api/v2" + features(9)),
  get("read_surrogate_flat", features(11)),
  get("read_percent_id_flat", "/plans/%31/features"),
  get("read_zero_id_flat", features(0)),
  get("read_leading_zero_v1", "/api/v1/plans/01/features"),
  get("read_text_id_v2", "/api/v2/plans/abc/features"),
  get("read_hex_id_flat", "/plans/0x1/features"),
  get("read_decimal_id_v1", "/api/v1/plans/1.0/features"),
  get("read_plus_id_v2", "/api/v2/plans/+1/features"),
  get("read_unsafe_id_flat", features("9007199254740992")),
  get("read_max_safe_id_v1", "/api/v1" + features("9007199254740991")),
  get("read_missing_v2", "/api/v2" + features(99)),
  get("read_foreign_flat", features(2)),
  get("read_not_json_v1", "/api/v1" + features(5)),
  get("read_array_v2", "/api/v2" + features(6)),
  get("read_null_choice_flat", features(7)),
  get("read_number_choice_v1", "/api/v1" + features(8)),
  get("read_empty_text_v2", "/api/v2" + features(10)),
  // The patch applies in Node's feature order whatever the body's order.
  rawPatch("patch_several_flat", features(1), "{\"spoolman\":false,\"farm\":true,\"orders\":true}"),
  patch("patch_null_clear_v1", "/api/v1" + features(1), { farm: null }),
  patch("patch_clear_rest_v2", "/api/v2" + features(1), { orders: null, spoolman: null }),
  patch("patch_clear_absent_flat", features(1), { bambu: null }),
  patch("patch_empty_new_layer_flat", features(3), {}),
  patch("patch_empty_again_v1", "/api/v1" + features(3), {}),
  patch("patch_raw_kept_v2", "/api/v2" + features(4), { bambu: true, farm: null }),
  patch("patch_raw_replace_flat", features(4), { orders: true }),
  patch("patch_duplicate_layer_v1", "/api/v1" + features(9), { orders: false }),
  patch("patch_surrogate_layer_v2", "/api/v2" + features(11), { farm: false }),
  rawPatch("patch_duplicate_body_key_flat", features(3), "{\"farm\":true,\"farm\":false}"),
  patch("patch_unknown_key_v1", "/api/v1" + features(3), { farm: true, extra: true }),
  patch("patch_text_value_v2", "/api/v2" + features(3), { farm: "yes" }),
  patch("patch_number_value_flat", features(3), { farm: 1 }),
  patch("patch_array_body_v1", "/api/v1" + features(3), [true]),
  patch("patch_text_body_v2", "/api/v2" + features(3), "farm"),
  rawPatch("patch_null_body_flat", features(3), "null"),
  { id: "patch_no_body_v1", method: "PATCH", path: "/api/v1" + features(3) },
  rawPatch("patch_surrogate_key_v2", "/api/v2" + features(3), "{\"\\ud800\":true}"),
  patch("patch_zero_id_flat", features(0), { farm: true }),
  patch("patch_text_id_bad_body_v1", "/api/v1/plans/abc/features", { farm: "x" }),
  patch("patch_missing_v2", "/api/v2" + features(99), { farm: true }),
  patch("patch_missing_bad_body_flat", features(99), { farm: 1 }),
  patch("patch_foreign_v1", "/api/v1" + features(2), { farm: false }),
  patch("patch_not_json_layer_v2", "/api/v2" + features(5), { farm: true }),
  patch("patch_array_layer_flat", features(6), {}),
  patch("patch_null_choice_layer_v1", "/api/v1" + features(7), { farm: true }),
  patch("patch_number_choice_layer_v2", "/api/v2" + features(8), { orders: null }),
  patch("patch_empty_text_layer_flat", features(10), { farm: true }),
  // Fastify's parser answers before the route runs.
  rawPatch("patch_prototype_flat", features(3), "{\"__proto__\":{\"farm\":true}}"),
  rawPatch("patch_constructor_v1", "/api/v1" + features(3), "{\"constructor\":{\"prototype\":{}}}"),
  rawPatch("patch_not_json_v2", "/api/v2" + features(3), "{"),
  rawPatch("patch_empty_json_flat", features(3), ""),
  // A corrupt tenant layer refuses every Plan once its input is valid.
  get("user_invalid_read_flat", features(1), { sql: USER_INVALID }),
  patch("user_invalid_patch_v1", "/api/v1" + features(3), { farm: true }),
  patch("user_invalid_missing_v2", "/api/v2" + features(99), { farm: true }),
  patch("user_invalid_bad_body_flat", features(3), { farm: 2 }),
  get("user_missing_read_v1", "/api/v1" + features(4), { sql: USER_MISSING }),
  patch("user_missing_patch_v2", "/api/v2" + features(4), { spoolman: true }),
  // With the flag off Node keeps the routes and answers not found.
  get("flag_off_read_flat", features(1), { rebuild: false }),
  patch("flag_off_patch_v1", "/api/v1" + features(1), { farm: true }, { rebuild: false }),
  patch("flag_off_bad_body_v2", "/api/v2" + features(1), { farm: 1 }, { rebuild: false }),
  post("decision_defaults_flat", decisions(12), {}),
  post("decision_applied_v1", "/api/v1" + decisions(12), {
    kind: "applied_action", actor: "assistant", action_type: "set_layers",
    params: { b: 1, a: [1.5, "x"], 10: true }, label: "Apply", summary: "Applied it",
    rationale: "Because", result: { ok: true, count: 2 },
  }),
  post("decision_dismissed_v2", "/api/v2" + decisions(12), {
    kind: "dismissed_action", actor: "user", action_type: "archive", label: "Archive", summary: "Not now",
  }),
  post("decision_choice_flat", decisions(12), { kind: "choice", actor: "robot", summary: "Picked red", params: { color: "red" } }),
  post("decision_user_note_v1", "/api/v1" + decisions(1), { kind: "user_note", label: "Note", rationale: null, result: null }),
  rawPost("decision_raw_numbers_v2", "/api/v2" + decisions(12), "{\"params\":{\"z\":1.50,\"2\":1e2,\"1\":[2.0,-0,1E400],\"s\":\"\\u0041\\/\"},\"result\":{\"n\":12345678901234567890}}"),
  post("decision_kind_null_flat", decisions(12), { kind: null, actor: null, action_type: null, params: null, label: null, summary: null }),
  post("decision_params_array_v1", "/api/v1" + decisions(12), { params: [1, 2] }),
  post("decision_params_text_v2", "/api/v2" + decisions(12), { params: "text", result: false }),
  post("decision_result_zero_flat", decisions(12), { params: 7, result: 0 }),
  post("decision_label_number_v1", "/api/v1" + decisions(12), { label: 5, summary: 1.5, rationale: 1e21, action_type: -0 }),
  post("decision_label_bool_v2", "/api/v2" + decisions(12), { label: true }),
  post("decision_label_object_flat", decisions(12), { summary: { a: 1 } }),
  post("decision_action_array_v1", "/api/v1" + decisions(12), { action_type: ["x"] }),
  rawPost("decision_surrogate_v2", "/api/v2" + decisions(12), "{\"label\":\"a\\ud800b\",\"summary\":\"\\ud83d\\ude00\"}"),
  // The driver spreads an array into its parameters and reads an object as named parameters.
  post("decision_label_pair_flat", decisions(12), { label: ["a", "b"] }),
  post("decision_summary_empty_array_v1", "/api/v1" + decisions(12), { summary: [] }),
  post("decision_label_number_array_v2", "/api/v2" + decisions(12), { label: [5], rationale: [null] }),
  post("decision_label_bool_array_flat", decisions(12), { label: [true] }),
  post("decision_two_objects_v1", "/api/v1" + decisions(12), { label: {}, summary: {} }),
  post("decision_object_then_bool_v2", "/api/v2" + decisions(12), { label: {}, summary: true }),
  post("decision_nested_array_flat", decisions(12), { action_type: [["x"]] }),
  post("decision_shifted_v1", "/api/v1" + decisions(12), { action_type: [], label: ["a", "b"] }),
  post("decision_overflow_then_bool_flat", decisions(12), { label: ["a", "b", "c", "d", "e"], summary: true }),
  post("decision_overflow_then_objects_v2", "/api/v2" + decisions(12), { label: ["a", "b", "c", "d", "e"], summary: {}, rationale: {} }),
  rawPost("decision_duplicate_key_flat", decisions(12), "{\"kind\":\"bogus\",\"kind\":\"choice\"}"),
  post("decision_invalid_kind_v1", "/api/v1" + decisions(12), { kind: "bogus" }),
  post("decision_empty_kind_v2", "/api/v2" + decisions(12), { kind: "" }),
  post("decision_number_kind_flat", decisions(12), { kind: 5 }),
  post("decision_array_body_v1", "/api/v1" + decisions(12), ["kind"]),
  post("decision_text_body_v2", "/api/v2" + decisions(12), "choice"),
  rawPost("decision_null_body_flat", decisions(12), "null"),
  { id: "decision_no_body_v1", method: "POST", path: "/api/v1" + decisions(12) },
  post("decision_missing_v2", "/api/v2" + decisions(99), {}),
  post("decision_missing_bad_kind_flat", decisions(99), { kind: "bogus" }),
  post("decision_foreign_v1", "/api/v1" + decisions(2), {}),
  post("decision_text_id_v2", "/api/v2/plans/abc/decisions", {}),
  post("decision_zero_id_flat", decisions(0), {}),
  post("decision_hex_id_v1", "/api/v1/plans/0xc/decisions", { summary: "hex" }),
  post("decision_decimal_id_v2", "/api/v2/plans/12.0/decisions", { summary: "decimal" }),
  rawPost("decision_prototype_flat", decisions(12), "{\"params\":{\"__proto__\":{\"x\":1}}}"),
  rawPost("decision_not_json_v1", "/api/v1" + decisions(12), "{"),
  rawPost("decision_empty_json_v2", "/api/v2" + decisions(12), ""),
  rawPost("decision_prototype_missing_flat", decisions(99), "{\"__proto__\":{}}"),
  get("decision_list_v1", "/api/v1" + decisions(12)),
  // Node serves the append with the flag off too; Rust then mounts nothing.
  post("flag_off_decision_flat", decisions(12), { summary: "flag off" }, { rebuild: false }),
];

const ISO = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/;
const stamp = (value) => {
  if (Array.isArray(value)) return value.map(stamp);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(Object.entries(value).map(([key, entry]) =>
    [key, key === "created_at" && typeof entry === "string" && ISO.test(entry) ? "<now>" : stamp(entry)]));
};

if (process.argv.includes("--probe")) {
  const load = (source) => import(pathToFileURL(join(root, source)).href);
  const require = createRequire(join(root, "web/apps/server/package.json"));
  const Database = require("better-sqlite3");
  const dataDir = mkdtempSync(join(tmpdir(), "pp-parity-plan-features-decisions-"));
  try {
    copyFileSync(join(root, database), join(dataDir, "print-partner.db"));
    const seeder = new Database(join(dataDir, "print-partner.db"));
    seeder.exec(SEED);
    seeder.close();
    process.env.PRINT_PARTNER_DATA_DIR = dataDir;
    process.env.PP_DESKTOP_REBUILD = "1";
    delete process.env.PRINT_PARTNER_API_KEY;
    const { createSelfHostPorts } = await load("web/apps/server/src/adapters/self-host/index.ts");
    const { buildApp } = await load("web/apps/server/src/app.ts");
    const { loadConfig } = await load("web/apps/server/src/config.ts");
    const ports = createSelfHostPorts(dataDir);
    await ports.db.connect();
    const app = await buildApp(loadConfig(), ports);
    await app.ready();

    const writer = new Database(join(dataDir, "print-partner.db"));
    const reader = new Database(join(dataDir, "print-partner.db"), { readonly: true });
    const tables = reader
      .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
      .all()
      .map((row) => row.name);
    const graph = () =>
      new Map(tables.map((table) => [table, JSON.stringify(reader.prepare("SELECT * FROM \"" + table + "\"").raw().all())]));
    const lastDecision = () => reader.prepare("SELECT max(id) AS id FROM plan_decisions").get().id ?? 0;
    // The feature layer text a Plan holds, or the decision rows a request added.
    const stored = (path, since) => {
      if (path.endsWith("/features")) {
        const id = path.split("/").at(-2);
        const row = reader.prepare("SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = ?")
          .get("feature_toggles:plan:" + id);
        return { value: row ? row.value : null };
      }
      return {
        decisions: reader.prepare(
          "SELECT id, tenant_id, profile_id, actor, kind, action_type, params_json, label, summary, rationale, result_json FROM plan_decisions WHERE id > ? ORDER BY id",
        ).all(since),
      };
    };

    const cases = [];
    for (const request of REQUESTS) {
      if (request.sql) writer.exec(request.sql);
      if (request.rebuild === false) delete process.env.PP_DESKTOP_REBUILD;
      else process.env.PP_DESKTOP_REBUILD = "1";
      const before = graph();
      const since = lastDecision();
      const response = await app.inject({
        method: request.method,
        url: request.path,
        ...(request.payload === undefined ? {} : { payload: request.payload, headers: { "content-type": "application/json" } }),
      });
      const after = graph();
      const changed = tables.filter((table) => before.get(table) !== after.get(table));
      cases.push({
        id: request.id,
        method: request.method,
        path: request.path,
        ...(request.payload === undefined ? {} : request.verbatim ? { rawBody: request.payload } : { body: JSON.parse(request.payload) }),
        ...(request.sql ? { sql: request.sql } : {}),
        ...(request.rebuild === false ? { rebuild: false } : {}),
        node: { status: response.statusCode, body: stamp(response.body === "" ? null : response.json()), setCookie: [] },
        changed,
        ...(changed.length ? { stored: stored(request.path, since) } : {}),
      });
    }
    writer.close();
    reader.close();
    await app.close();
    await ports.db.close?.();

    const byId = Object.fromEntries(cases.map((item) => [item.id, item]));
    assert.equal(byId.patch_several_flat.stored.value, "{\"farm\":true,\"orders\":true,\"spoolman\":false}");
    assert.equal(byId.decision_defaults_flat.node.body.kind, "user_note");
    assert.equal(byId.flag_off_read_flat.node.status, 404);
    for (const item of cases) {
      if (item.method === "GET" || item.node.status !== 200) {
        assert.deepEqual(item.changed, [], item.id + " wrote state");
      }
    }
    // The app logs to stdout, so the probe hands its result over in a file.
    writeFileSync(process.argv.at(-1), JSON.stringify(cases));
  } finally {
    rmSync(dataDir, { recursive: true, force: true });
  }
  process.exit(0);
}

const tsx = join(root, "web/node_modules/tsx/dist/esm/index.mjs");
const scratch = mkdtempSync(join(tmpdir(), "pp-parity-plan-features-decisions-probe-"));
let cases;
try {
  const result = join(scratch, "probe.json");
  execFileSync(
    process.execPath,
    ["--conditions=development", "--import", pathToFileURL(tsx).href, fileURLToPath(import.meta.url), "--probe", result],
    { cwd: join(root, "web/apps/server"), stdio: ["ignore", "ignore", "inherit"] },
  );
  cases = JSON.parse(readFileSync(result, "utf8"));
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
const line = (value) => JSON.stringify(value);
const text = "{\n" +
  "  \"generated\": " + line("@generated by fixtures/parity/plan-features-decisions/generate.mjs. Do not edit.") + ",\n" +
  "  \"sources\": " + line(sources) + ",\n" +
  "  \"database\": " + line(database) + ",\n" +
  "  \"seed\": " + line(SEED) + ",\n" +
  "  \"cases\": [\n" + cases.map((item) => "    " + line(item)).join(",\n") + "\n  ]\n}\n";
const check = process.argv.includes("--check");
if (check) {
  assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
} else {
  writeFileSync(output, text);
}
console.log(JSON.stringify({ cases: cases.length, check }));
