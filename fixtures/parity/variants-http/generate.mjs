// Generates the variants-http parity golden: complete HTTP responses from
// Node's real app for GET /plans/:id/variant-dimensions and
// POST /plans/:id/variant-selection on the flat, /api/v1 and /api/v2 mounts,
// plus the database tables each request changed. Node opens a copy of the
// required-units unchanged.db fixture after SEED adds Sources 2-9, Plans 3-12
// and four stored selections. FILES lays out the Source checkouts (manifests
// with variant_dimensions in several shapes, an invalid manifest, a manifest
// whose selections Node refuses, a checkout without a manifest), and CHECKOUTS
// points each Source's local_path at repos/<id> there; Source 3 has no
// local_path and Source 7's checkout does not exist. The Rust test writes the
// same files, sets the same paths and reads them through the production
// SourceReads adapter. Run from the repo root after npm --prefix web ci:
//   node fixtures/parity/variants-http/generate.mjs          # write
//   node fixtures/parity/variants-http/generate.mjs --check  # verify, no writes
// No response carries a clock value or an absolute path.
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
const database = "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db";
const sources = [
  "web/apps/server/src/routes/plans.ts",
  "web/apps/server/src/services/variant-dimensions.ts",
  "web/apps/server/src/services/manifest-apply.ts",
];
const SEED = [
  "INSERT INTO projects (id, tenant_id, name, url, imported_paths) VALUES",
  "(2, 'default', 'Variant Kit', 'https://example.test/variant', '[\"old/\"]'),",
  "(3, 'default', 'No Checkout', 'https://example.test/none', NULL),",
  "(4, 'default', 'Broken Manifest', 'https://example.test/broken', NULL),",
  "(5, 'default', 'Listed Dimensions', 'https://example.test/listed', NULL),",
  "(6, 'default', 'Scalar Dimensions', 'https://example.test/scalar', NULL),",
  "(7, 'default', 'Missing Checkout', 'https://example.test/missing', NULL),",
  "(8, 'default', 'No Manifest', 'https://example.test/bare', NULL),",
  "(9, 'default', 'Refused Selections', 'https://example.test/refused', NULL);",
  "INSERT INTO build_profiles (id, tenant_id, name) VALUES",
  "(3, 'default', 'Variant Plan'), (4, 'default', 'Empty Plan'), (5, 'default', 'Unchecked Plan'),",
  "(6, 'default', 'Broken Plan'), (7, 'default', 'Listed Plan'), (8, 'default', 'Scalar Plan'),",
  "(9, 'default', 'Missing Plan'), (10, 'default', 'Bare Plan'), (11, 'default', 'Refused Plan'),",
  "(12, 'default', 'Addon Plan');",
  "INSERT INTO profile_layers (tenant_id, profile_id, layer_order, layer_type, project_id) VALUES",
  "('default', 3, 0, 'base', 2), ('default', 3, 1, 'addon', 5), ('default', 5, 0, 'base', 3),",
  "('default', 6, 0, 'base', 4), ('default', 7, 0, 'base', 5), ('default', 8, 0, 'base', 6),",
  "('default', 9, 0, 'base', 7), ('default', 10, 0, 'base', 8), ('default', 11, 0, 'base', 9),",
  "('default', 12, 0, 'addon', 2);",
  "INSERT INTO app_settings (tenant_id, key, value) VALUES",
  "('default', 'plan_variant_selection_5', 'not json'),",
  "('default', 'plan_variant_selection_6', '5'),",
  "('default', 'plan_variant_selection_8', ''),",
  "('default', 'plan_variant_selection_9', '{\"b\": \"x\", \"10\": \"y\", \"2\": 1.0, \"n\": {\"z\": 1, \"1\": [2.50]}}');",
].join(" ");
// Each Source's local_path, relative to the data directory.
const CHECKOUTS = { 2: "repos/2", 4: "repos/4", 5: "repos/5", 6: "repos/6", 7: "repos/7", 8: "repos/8", 9: "repos/9" };
const MANIFEST = "print-partner.manifest.yaml";
// Files relative to the data directory.
const FILES = {
  ["repos/2/" + MANIFEST]: [
    "project: Variant Kit",
    "parts:",
    "  - path: \"size/**\"",
    "variant_dimensions:",
    "  size: [small, large, 10]",
    "  \"10\": [a]",
    "  \"2\": [b]",
    "  finish:",
    "    - matte",
    "    - 1.5",
    "    - 2.0",
    "    - true",
    "    - null",
    "    - {x: 1}",
    "    - [nested]",
    "    - .nan",
    "    - .inf",
    "    - 1e21",
    "    - -0",
    "    - \"1.5\"",
    "  notes: plain text",
    "  empty: []",
    "  blank: [\"\", kept]",
    "  paths: [\"sub\\\\dir\", \" spaced \", \"part.STL\", \"/lead\"]",
    "  dup: [large]",
    "  anchored: &opts [red, blue]",
    "  aliased: *opts",
    "",
  ].join("\n"),
  "repos/2/size/large/part.stl": "solid large",
  ["repos/4/" + MANIFEST]: "not: [valid",
  ["repos/5/" + MANIFEST]: "variant_dimensions:\n  - [x, 1]\n  - lone\n  - [y]\n",
  ["repos/6/" + MANIFEST]: "variant_dimensions: just text\n",
  "repos/8/part.stl": "solid bare",
  ["repos/9/" + MANIFEST]: "option_groups:\n  color:\n    variants:\n      - id: red\nselections:\n  color: []\nvariant_dimensions:\n  color: [red]\n",
};

const get = (id, path) => [id, "GET", path];
const post = (id, path, body) => [id, "POST", path, JSON.stringify(body), false];
const raw = (id, path, payload) => [id, "POST", path, payload, true];
// Each request runs in order on one database.
const REQUESTS = [
  get("dimensions_flat", "/plans/3/variant-dimensions"),
  get("dimensions_v1", "/api/v1/plans/3/variant-dimensions"),
  ["dimensions_head_v2", "HEAD", "/api/v2/plans/3/variant-dimensions"],
  get("no_layers_flat", "/plans/4/variant-dimensions"),
  get("no_checkout_not_json_v2", "/api/v2/plans/5/variant-dimensions"),
  get("broken_manifest_number_v1", "/api/v1/plans/6/variant-dimensions"),
  get("listed_flat", "/plans/7/variant-dimensions"),
  get("scalar_empty_setting_v2", "/api/v2/plans/8/variant-dimensions"),
  get("missing_checkout_ordered_v1", "/api/v1/plans/9/variant-dimensions"),
  get("no_manifest_flat", "/plans/10/variant-dimensions"),
  get("refused_selections_v2", "/api/v2/plans/11/variant-dimensions"),
  get("addon_only_flat", "/plans/12/variant-dimensions"),
  get("missing_build_flat", "/plans/99/variant-dimensions"),
  get("foreign_build_v1", "/api/v1/plans/2/variant-dimensions"),
  get("text_id_v2", "/api/v2/plans/abc/variant-dimensions"),
  get("zero_id_flat", "/plans/0/variant-dimensions"),
  post("select_flat", "/plans/3/variant-selection", { selection: { size: "large", finish: "matte" } }),
  get("after_select_v2", "/api/v2/plans/3/variant-dimensions"),
  // Raw bodies keep their key order; Node reads index keys first.
  raw("select_numbers_v1", "/api/v1/plans/3/variant-selection", '{"selection":{"finish":"1.5","10":"a","size":"10","2":"b"}}'),
  post("select_number_text_v2", "/api/v2/plans/3/variant-selection", { selection: { finish: "2" }, source_id: 2, extra: true }),
  post("select_nan_infinity_flat", "/plans/3/variant-selection", { selection: { finish: "NaN" } }),
  post("select_infinity_exponent_v1", "/api/v1/plans/3/variant-selection", { selection: { finish: "Infinity", aliased: "blue" } }),
  post("select_exponent_zero_v2", "/api/v2/plans/3/variant-selection", { selection: { finish: "1e+21", dup: "large", size: "large" } }),
  post("select_zero_flat", "/plans/3/variant-selection", { selection: { finish: "0", blank: "" } }),
  post("select_paths_v1", "/api/v1/plans/3/variant-selection", { selection: { paths: "sub\\dir" } }),
  post("select_spaced_v2", "/api/v2/plans/3/variant-selection", { selection: { paths: " spaced " } }),
  post("select_stl_flat", "/plans/3/variant-selection", { selection: { paths: "part.STL" } }),
  post("select_lead_v1", "/api/v1/plans/3/variant-selection", { selection: { paths: "/lead" } }),
  raw("source_id_float_v2", "/api/v2/plans/3/variant-selection", "{\"selection\":{\"size\":\"small\"},\"source_id\":2.0}"),
  get("after_float_flat", "/plans/3/variant-dimensions"),
  post("unsupported_option_v1", "/api/v1/plans/3/variant-selection", { selection: { size: "huge" } }),
  post("number_text_mismatch_v2", "/api/v2/plans/3/variant-selection", { selection: { finish: "2.0" } }),
  post("object_option_flat", "/plans/3/variant-selection", { selection: { finish: "[object Object]" } }),
  post("bool_option_v1", "/api/v1/plans/3/variant-selection", { selection: { finish: "true" } }),
  post("null_option_v2", "/api/v2/plans/3/variant-selection", { selection: { finish: "null" } }),
  post("empty_dimension_flat", "/plans/3/variant-selection", { selection: { empty: "x" } }),
  post("unknown_dimension_v1", "/api/v1/plans/3/variant-selection", { selection: { zzz: "1", size: "huge" } }),
  raw("index_key_first_v2", "/api/v2/plans/3/variant-selection", '{"selection":{"size":"huge","5":"x"}}'),
  post("not_array_dimension_flat", "/plans/3/variant-selection", { selection: { notes: "plain text" } }),
  post("inherited_name_v1", "/api/v1/plans/3/variant-selection", { selection: { toString: "x" } }),
  post("other_source_v2", "/api/v2/plans/3/variant-selection", { selection: {}, source_id: 5 }),
  post("unknown_source_flat", "/plans/3/variant-selection", { selection: { size: "large" }, source_id: 99 }),
  post("source_id_zero_v1", "/api/v1/plans/3/variant-selection", { source_id: 0 }),
  post("source_id_negative_v2", "/api/v2/plans/3/variant-selection", { source_id: -2 }),
  post("source_id_fraction_flat", "/plans/3/variant-selection", { source_id: 1.5 }),
  post("source_id_text_v1", "/api/v1/plans/3/variant-selection", { source_id: "2" }),
  post("source_id_null_v2", "/api/v2/plans/3/variant-selection", { source_id: null }),
  post("source_id_unsafe_flat", "/plans/3/variant-selection", { source_id: 9007199254740992 }),
  post("selection_value_number_v1", "/api/v1/plans/3/variant-selection", { selection: { size: 10 } }),
  post("selection_array_v2", "/api/v2/plans/3/variant-selection", { selection: ["large"] }),
  post("selection_null_flat", "/plans/3/variant-selection", { selection: null }),
  post("selection_text_v1", "/api/v1/plans/3/variant-selection", { selection: "large" }),
  post("body_array_v2", "/api/v2/plans/3/variant-selection", []),
  post("body_text_flat", "/plans/3/variant-selection", "large"),
  get("after_refusals_v1", "/api/v1/plans/3/variant-dimensions"),
  raw("body_null_v2", "/api/v2/plans/3/variant-selection", "null"),
  raw("duplicate_key_flat", "/plans/3/variant-selection", '{"selection":{"size":"huge","dup":"large","size":"small"}}'),
  get("after_null_flat", "/plans/3/variant-dimensions"),
  ["no_body_v1", "POST", "/api/v1/plans/3/variant-selection"],
  post("no_base_flat", "/plans/4/variant-selection", { selection: {} }),
  post("no_base_with_source_v1", "/api/v1/plans/4/variant-selection", { selection: {}, source_id: 2 }),
  post("addon_only_v2", "/api/v2/plans/12/variant-selection", { source_id: 2 }),
  post("no_checkout_empty_flat", "/plans/5/variant-selection", { selection: {} }),
  post("no_checkout_unknown_v1", "/api/v1/plans/5/variant-selection", { selection: { a: "b" } }),
  post("listed_index_v2", "/api/v2/plans/7/variant-selection", { selection: { 0: "1", 2: "y" } }),
  post("refused_manifest_flat", "/plans/11/variant-selection", { selection: { color: "red" } }),
  post("missing_build_v1", "/api/v1/plans/99/variant-selection", { selection: {} }),
  post("foreign_build_bad_body_v2", "/api/v2/plans/2/variant-selection", []),
  post("text_id_flat", "/plans/abc/variant-selection", { selection: {} }),
];

if (process.argv.includes("--probe")) {
  const load = (source) => import(pathToFileURL(join(root, source)).href);
  const require = createRequire(join(root, "web/apps/server/package.json"));
  const Database = require("better-sqlite3");
  const dataDir = mkdtempSync(join(tmpdir(), "pp-parity-variants-http-"));
  try {
    copyFileSync(join(root, database), join(dataDir, "print-partner.db"));
    for (const [path, body] of Object.entries(FILES)) {
      mkdirSync(dirname(join(dataDir, path)), { recursive: true });
      writeFileSync(join(dataDir, path), body);
    }
    const seeder = new Database(join(dataDir, "print-partner.db"));
    seeder.exec(SEED);
    const setPath = seeder.prepare("UPDATE projects SET local_path = ? WHERE id = ?");
    for (const [id, path] of Object.entries(CHECKOUTS)) setPath.run(join(dataDir, path), Number(id));
    seeder.close();
    process.env.PRINT_PARTNER_DATA_DIR = dataDir;
    delete process.env.PRINT_PARTNER_API_KEY;
    const { createSelfHostPorts } = await load("web/apps/server/src/adapters/self-host/index.ts");
    const { buildApp } = await load("web/apps/server/src/app.ts");
    const { loadConfig } = await load("web/apps/server/src/config.ts");
    const ports = createSelfHostPorts(dataDir);
    await ports.db.connect();
    const app = await buildApp(loadConfig(), ports);
    await app.ready();

    // Every row of every table, so a case records which tables it changed.
    const reader = new Database(join(dataDir, "print-partner.db"), { readonly: true });
    const tables = reader
      .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
      .all()
      .map((row) => row.name);
    const graph = () =>
      new Map(tables.map((table) => [table, JSON.stringify(reader.prepare("SELECT * FROM \"" + table + "\"").raw().all())]));
    // The stored selection text and the Source's import rules after a save.
    const stored = (plan, source) => ({
      selection: reader.prepare("SELECT value FROM app_settings WHERE tenant_id='default' AND key=?")
        .get("plan_variant_selection_" + plan).value,
      imported_paths: reader.prepare("SELECT imported_paths FROM projects WHERE id=?").get(source).imported_paths,
    });

    const cases = [];
    for (const [id, method, path, payload, verbatim] of REQUESTS) {
      const before = graph();
      const response = await app.inject({
        method,
        url: path,
        ...(payload === undefined ? {} : { payload, headers: { "content-type": "application/json" } }),
      });
      const after = graph();
      const changed = tables.filter((table) => before.get(table) !== after.get(table));
      cases.push({
        id,
        method,
        path,
        ...(payload === undefined ? {} : verbatim ? { rawBody: payload } : { body: JSON.parse(payload) }),
        node: { status: response.statusCode, body: response.body === "" ? null : response.json(), setCookie: [] },
        changed,
        ...(changed.length ? { stored: stored(path.split("/").at(-2), response.json().source_id) } : {}),
      });
    }
    reader.close();
    await app.close();
    await ports.db.close?.();

    const byId = Object.fromEntries(cases.map((item) => [item.id, item]));
    // The corpus is only useful if Node really read the checkouts.
    assert.deepEqual(Object.keys(byId.dimensions_flat.node.body.dimensions).slice(0, 3), ["2", "10", "size"]);
    assert.deepEqual(Object.keys(byId.listed_flat.node.body.dimensions), ["0", "2"]);
    assert.equal(byId.select_flat.node.status, 200);
    for (const item of cases) {
      if (item.method !== "POST" || item.node.status !== 200) {
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
const scratch = mkdtempSync(join(tmpdir(), "pp-parity-variants-http-probe-"));
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
  "  \"generated\": " + line("@generated by fixtures/parity/variants-http/generate.mjs. Do not edit.") + ",\n" +
  "  \"sources\": " + line(sources) + ",\n" +
  "  \"database\": " + line(database) + ",\n" +
  "  \"seed\": " + line(SEED) + ",\n" +
  "  \"checkouts\": " + line(CHECKOUTS) + ",\n" +
  "  \"files\": " + line(FILES) + ",\n" +
  "  \"cases\": [\n" + cases.map((item) => "    " + line(item)).join(",\n") + "\n  ]\n}\n";
const check = process.argv.includes("--check");
if (check) {
  assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
} else {
  writeFileSync(output, text);
}
console.log(JSON.stringify({ cases: cases.length, check }));
