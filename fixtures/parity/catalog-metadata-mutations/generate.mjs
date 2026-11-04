import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../..");
const stale = process.argv.includes("--stale");
const output = join(here, stale ? "stale.json" : "node.json");

if (!process.argv.includes("--probe")) {
  const env = { ...process.env };
  if (stale) env.PP_DESKTOP_REBUILD = "1";
  else delete env.PP_DESKTOP_REBUILD;
  const result = JSON.parse(execFileSync(process.execPath, [
    "--conditions=development", "--import",
    pathToFileURL(join(root, "web/node_modules/tsx/dist/esm/index.mjs")).href,
    fileURLToPath(import.meta.url), "--probe",
    ...(stale ? ["--stale"] : []),
  ], { cwd: join(root, "web/apps/server"), encoding: "utf8", env }).trim());
  const text = `{
  "generated": "fixtures/parity/catalog-metadata-mutations/generate.mjs",
  "seed_sql": ${JSON.stringify(result.seed_sql)},
  "initial": ${JSON.stringify(result.initial)},
  "steps": [
${result.steps.map((step) => `    ${JSON.stringify(step)}`).join(",\n")}
  ]
}\n`;
  if (process.argv.includes("--check")) {
    assert.equal(readFileSync(output, "utf8"), text, "node.json is out of date");
  } else {
    writeFileSync(output, text);
  }
  console.log(JSON.stringify({ steps: result.steps.length, check: process.argv.includes("--check") }));
  process.exit(0);
}

const require = createRequire(join(root, "web/package.json"));
const Database = require("better-sqlite3");
const Fastify = require("fastify");
const { drizzle } = require("drizzle-orm/better-sqlite3");
const server = (path) => import(pathToFileURL(join(root, "web/apps/server/src", path)).href);
const { AppRepository } = await server("db/repository.ts");
const schema = await server("db/schema.ts");
const { registerSettingsRoutes, registerSourceNamingRoutes } = await server("routes/settings.ts");
const { registerSourceRoutes } = await server("routes/sources.ts");
const { DEFAULT_NAMING_PROFILE } = await import(pathToFileURL(join(root, "web/packages/domain/src/index.ts")).href);
const directory = mkdtempSync(join(tmpdir(), "pp-catalog-metadata-"));
let raw;
let apps = {};

const quote = (value) => `'${String(value).replaceAll("'", "''")}'`;
const json = (value) => quote(JSON.stringify(value));
const global = structuredClone(DEFAULT_NAMING_PROFILE);
global.quantity.default = 2;
const changedGlobal = structuredClone(global);
changedGlobal.quantity.default = 6;
changedGlobal.slug.strip_markers = false;
changedGlobal.slug.strip_quantity = false;
const override = structuredClone(global);
override.quantity.default = 4;
override.slug.strip_markers = false;
override.slug.strip_quantity = false;
const naming = (id, settings) => ({ operation: "save_naming", id, settings });
const defaults = { kind: "use_defaults" };
const custom = { kind: "override", profile: override };
const categories = (paths, replacements = {}) => ({ operation: "save_categories_with_tree", categories: paths, replacements });
const assign = (selectors, category) => ({ operation: "apply_numeric_category", command: { selectors, category } });
const commands = [
  { name: "create category tree", request: categories([" Hardware / Frame ", "Hardware/Panels", "Mods"]) },
  { name: "assign category and deduplicate selectors", request: assign([7, 9, 7], " Hardware / Panels ") },
  { name: "rename ancestor and move assigned descendants", request: categories(["Printer parts/Frame", "Printer parts/Panels", "Mods"], { Hardware: "Printer parts" }) },
  { name: "set global scalar naming defaults", request: { operation: "save_global_naming", profile: changedGlobal } },
  { name: "read changed global naming defaults", request: { operation: "get_global_naming" } },
  { name: "Source inherits changed global naming defaults", request: { operation: "get_naming", id: 7 } },
  { name: "set scalar naming override", request: naming(7, custom) },
  { name: "read scalar naming override", request: { operation: "get_naming", id: 7 } },
  { name: "reopen with override persisted", request: { operation: "get_naming", id: 7 }, reopen: true },
  { name: "read global naming defaults after reopen", request: { operation: "get_global_naming" } },
  { name: "read categories after reopen", request: { operation: "get_categories_with_tree" } },
  { name: "read complete Source after reopen", request: { operation: "get", id: 7 } },
  { name: "read sentinel activity after reopen", request: { operation: "source_activity", limit: 100 } },
  { name: "foreign tenant category assignment preserves state", actor: "foreign", request: assign([7], "Mods"), preservesState: true },
  { name: "foreign tenant naming set preserves state", actor: "foreign", request: naming(7, custom), status: 404, preservesState: true },
  { name: "foreign tenant naming clear preserves state", actor: "foreign", request: naming(7, defaults), status: 404, preservesState: true },
  { name: "foreign tenant can read its own Source naming", actor: "foreign", request: { operation: "get_naming", id: 8 } },
  { name: "delete child category and fall back to ancestor", request: categories(["Printer parts/Frame", "Mods"]) },
  { name: "delete ancestor category with explicit uncategorise", request: categories(["Mods"], { "Printer parts": null }) },
  { name: "clear scalar naming override using defaults", request: naming(7, defaults) },
  { name: "reset global scalar naming defaults", request: { operation: "save_global_naming", profile: DEFAULT_NAMING_PROFILE } },
  { name: "Source inherits reset global naming defaults", request: { operation: "get_naming", id: 7 } },
  { name: "reassign category", request: assign([7], "Mods") },
  { name: "clear category assignment", request: assign([7], null) },
  { name: "delete disposable Source", request: { operation: "delete", id: 9 }, status: 204 },
  { name: "deleted Source ID category edit preserves state", request: assign([9], "Mods"), preservesState: true },
  { name: "deleted Source ID naming set preserves state", request: naming(9, custom), status: 404, preservesState: true },
  { name: "deleted Source ID naming clear preserves state", request: naming(9, defaults), status: 404, preservesState: true },
  { name: "reopen after clears and rejected edits", request: { operation: "get_naming", id: 7 }, reopen: true },
  { name: "read reset global naming defaults after reopen", request: { operation: "get_global_naming" } },
  { name: "read final complete Source", request: { operation: "get", id: 7 } },
  { name: "read final categories", request: { operation: "get_categories_with_tree" } },
  { name: "read final sentinel activity", request: { operation: "source_activity", limit: 100 } },
];
const conflictBody = {
  code: "catalog_metadata_conflict",
  detail: "Catalog metadata changed; retry from the current version",
};
const staleCommands = [
  {
    name: "read implicit catalog metadata version",
    request: { operation: "get", id: 7 },
    assertStep(outcome) {
      assert.equal(outcome.body.catalog_metadata_version, 0);
      assert.equal(Object.hasOwn(outcome.body.metadata, "catalog_metadata_version"), false);
    },
  },
  {
    name: "stale token on a name edit writes nothing",
    request: { operation: "update", id: 7, patch: { name: "Renamed fixture", metadata_version: 4 } },
    status: 409,
    preservesState: true,
    assertStep(outcome) {
      assert.deepEqual(outcome.body, conflictBody);
    },
  },
  {
    name: "matching token metadata edit bumps version",
    request: { operation: "update", id: 7, patch: { metadata: { custom_name: "Renamed once" }, metadata_version: 0 } },
    assertStep(outcome, state) {
      assert.equal(outcome.body.catalog_metadata_version, 1);
      assert.equal(outcome.body.metadata.custom_name, "Renamed once");
      assert.equal(Object.hasOwn(outcome.body.metadata, "catalog_metadata_version"), false);
      assert.equal(state.projects.find((row) => row.id === 7).metadata_json.catalog_metadata_version, 1);
    },
  },
  {
    name: "naming save bumps catalog metadata version",
    request: naming(7, custom),
    assertStep(_outcome, state) {
      assert.equal(state.projects.find((row) => row.id === 7).metadata_json.catalog_metadata_version, 2);
    },
  },
  {
    name: "stale token metadata edit preserves rows",
    request: { operation: "update", id: 7, patch: { metadata: { custom_name: "Should not land" }, metadata_version: 0 } },
    status: 409,
    preservesState: true,
    assertStep(outcome) {
      assert.deepEqual(outcome.body, conflictBody);
    },
  },
  {
    name: "tokenless metadata edit still overwrites",
    request: { operation: "update", id: 7, patch: { metadata: { custom_name: "Tokenless rename" } } },
    assertStep(outcome, state) {
      assert.equal(outcome.body.metadata.custom_name, "Tokenless rename");
      assert.equal(outcome.body.catalog_metadata_version, 3);
      const row = state.projects.find((project) => project.id === 7);
      assert.equal(row.metadata_json.catalog_metadata_version, 3);
      assert.equal(row.metadata_json.custom_name, "Tokenless rename");
      assert.equal(Object.hasOwn(outcome.body.metadata, "catalog_metadata_version"), false);
    },
  },
];

const metadata = {
  custom_name: "Keep this name", category: "Hardware/Frame",
  unrelated: { label: "keep", nested: [1, false, null] },
  remote_update_status: "up_to_date", remote_checked_at: "2025-01-02T03:04:05.000Z",
};
const seed_sql = [
  `INSERT INTO users(id,email,display_name,is_admin,created_at) VALUES('tenant-b','foreign@example.invalid','Foreign',0,'2025-01-01T00:00:00.000Z')`,
  `INSERT INTO sessions(id,user_id,expires_at,provider) VALUES('${createHash("sha256").update("catalog-metadata-foreign-session").digest("hex")}','tenant-b','2099-01-01T00:00:00.000Z','email')`,
  ...[[7, "default", "Local fixture", metadata], [8, "tenant-b", "Foreign fixture", { ...metadata, category: "Foreign/Keep" }], [9, "default", "Deleted fixture", metadata]].map(([id, tenant, name, data]) =>
    `INSERT INTO projects(id,tenant_id,name,url,source_type,source_kind,role,local_path,last_synced_at,last_commit_sha,docs_url,imported_paths,manifest_community_slug,metadata_json) VALUES(${id},${quote(tenant)},${quote(name)},'','local','local','unassigned','@DATA_DIR@/local/${id}','2025-01-02T03:04:05.000Z','fixture-sha','https://example.invalid/docs','["parts/"]','fixture-community',${json(data)})`),
  ...["default", "tenant-b"].flatMap((tenant) => [
    `INSERT INTO app_settings(tenant_id,key,value) VALUES(${quote(tenant)},'source_categories',${json(tenant === "default" ? ["Hardware", "Hardware/Frame", "Mods"] : ["Foreign", "Foreign/Keep"])})`,
    `INSERT INTO app_settings(tenant_id,key,value) VALUES(${quote(tenant)},'stl_naming_defaults',${json(global)})`,
    `INSERT INTO app_settings(tenant_id,key,value) VALUES(${quote(tenant)},'catalog_metadata_sentinel','keep-setting')`,
    `INSERT INTO app_events(id,tenant_id,at,kind,actor_type,actor_id,payload_json) VALUES(${tenant === "default" ? 41 : 42},${quote(tenant)},'2025-01-02T03:04:05.000Z','source.updated','fixture','sentinel',${json({ source_id: tenant === "default" ? 7 : 8, source_name: "Sentinel", note: "keep-activity" })})`,
  ]),
];

async function open() {
  raw = new Database(join(directory, "print-partner.db"), { fileMustExist: true });
  raw.pragma("foreign_keys = ON");
  const db = drizzle(raw, { schema });
  for (const [actor, tenant] of [["owner", "default"], ["foreign", "tenant-b"]]) {
    const app = Fastify();
    const deps = { repo: new AppRepository(db, tenant, join(directory, "repos")), dataDir: directory,
      reposDir: join(directory, "repos"), sourcesDir: join(directory, "sources"),
      thumbsDir: join(directory, "thumbs"), coversDir: join(directory, "covers"), hostedPlanning: false };
    await registerSettingsRoutes(app, deps);
    await registerSourceNamingRoutes(app, deps);
    await registerSourceRoutes(app, deps);
    await app.ready();
    apps[actor] = app;
  }
}

async function close() {
  for (const app of Object.values(apps)) await app.close();
  apps = {};
  raw?.close();
  raw = undefined;
}

function normalize(value, key = "") {
  if (Array.isArray(value)) return value.map((item) => normalize(item));
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([field, item]) => [field, normalize(item, field)]));
  }
  if (key === "local_path" && value !== null) {
    assert.equal(typeof value, "string");
    assert.ok([7, 8, 9].some((id) => value === join(directory, "local", String(id))), `unexpected local path: ${value}`);
    assert.ok(existsSync(value), `local path must exist: ${value}`);
    return value.replace(directory, "@DATA_DIR@");
  }
  return value;
}

function snapshot() {
  return normalize({
    projects: raw.prepare("SELECT * FROM projects ORDER BY id").all().map((row) => ({ ...row, metadata_json: JSON.parse(row.metadata_json) })),
    settings: raw.prepare("SELECT * FROM app_settings ORDER BY tenant_id,key").all().map((row) => ({ ...row, value: ["source_categories", "stl_naming_defaults"].includes(row.key) ? JSON.parse(row.value) : row.value })),
    activity: raw.prepare("SELECT * FROM app_events ORDER BY id").all().map((row) => ({ ...row, payload_json: JSON.parse(row.payload_json) })),
  });
}

function route(request) {
  switch (request.operation) {
    case "save_categories_with_tree": return { method: "PUT", url: "/settings/source-categories", payload: { categories: request.categories, replacements: request.replacements } };
    case "get_categories_with_tree": return { method: "GET", url: "/settings/source-categories" };
    case "apply_numeric_category": return { method: "POST", url: "/sources/bulk-category", payload: { source_ids: request.command.selectors, category: request.command.category } };
    case "save_naming": return { method: "PUT", url: `/sources/${request.id}/naming`, payload: request.settings.kind === "use_defaults" ? { use_defaults: true } : { use_defaults: false, override: request.settings.profile } };
    case "get_naming": return { method: "GET", url: `/sources/${request.id}/naming` };
    case "save_global_naming": return { method: "PUT", url: "/settings/stl-naming", payload: { profile: request.profile } };
    case "get_global_naming": return { method: "GET", url: "/settings/stl-naming" };
    case "get": return { method: "GET", url: `/sources/${request.id}` };
    case "update": return { method: "PATCH", url: `/sources/${request.id}`, payload: request.patch };
    case "delete": return { method: "DELETE", url: `/sources/${request.id}` };
    case "source_activity": return { method: "GET", url: `/sources/activity?limit=${request.limit}` };
    default: throw new Error(`Unsupported fixture command: ${request.operation}`);
  }
}

try {
  for (const id of [7, 8, 9]) mkdirSync(join(directory, "local", String(id)), { recursive: true });
  execFileSync("cargo", [
    "run", "--locked", "--quiet", "--manifest-path", join(root, "rust/Cargo.toml"), "-p", "pp-storage",
    // test-unmarked-writer: the disposable directory has local/ and no ownership marker.
    "--features", "desktop_rebuild,test-unmarked-writer", "--bin", "storage-fixture", "--", "initialize", directory,
    "2025-01-02T03:04:05.000Z",
  ], { cwd: root, stdio: ["ignore", "pipe", "inherit"], env: {
    ...process.env, CARGO_BUILD_JOBS: "3",
  } });
  await open();
  for (const sql of seed_sql) raw.exec(sql.replaceAll("@DATA_DIR@", directory.replaceAll("'", "''")));
  const initial = snapshot();
  assert.equal(initial.projects.length, 3);
  assert.equal(initial.activity.length, 2);
  const steps = [];
  for (const { name, actor = "owner", request, reopen = false, status = 200, preservesState = false, assertStep } of (stale ? staleCommands : commands)) {
    if (reopen) { await close(); await open(); }
    const response = await apps[actor].inject(route(request));
    const outcome = { status: response.statusCode, body: response.body === "" ? null : response.json() };
    assert.equal(outcome.status, status, `${name}: HTTP ${outcome.status} ${response.body}`);
    const state = snapshot();
    if (assertStep) assertStep(outcome, state);
    assert.deepEqual(state.activity, initial.activity, `${name}: activity must stay intact`);
    assert.deepEqual(state.projects.filter((row) => row.tenant_id === "tenant-b"), initial.projects.filter((row) => row.tenant_id === "tenant-b"), `${name}: foreign Source must stay intact`);
    assert.deepEqual(state.settings.filter((row) => row.tenant_id === "tenant-b"), initial.settings.filter((row) => row.tenant_id === "tenant-b"), `${name}: foreign settings must stay intact`);
    if (preservesState) assert.deepEqual(state, steps.at(-1).snapshot, name);
    steps.push({ name, actor, request, reopen, outcome: normalize(outcome), snapshot: state });
  }
  process.stdout.write(JSON.stringify({ seed_sql, initial, steps }));
} finally {
  await close();
  rmSync(directory, { recursive: true, force: true });
}
