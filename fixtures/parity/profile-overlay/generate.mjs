// Captures Node profile-overlay results for a bounded literal matrix.
// Prototype-name slugs (GRE-697) and custom quantity regexes stay out of this corpus.
// Regenerate with Node 22.23.3 from the repository root:
// node --conditions=development --import ./web/node_modules/tsx/dist/loader.mjs fixtures/parity/profile-overlay/generate.mjs [--check]
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";

const NODE_VERSION = "v22.23.3";
assert.equal(process.version, NODE_VERSION, "profile-overlay capture requires Node 22.23.3");

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../../..");
const server = join(root, "web/apps/server");
const database = join(root, "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db");
const fixturePath = join(here, "node.json");
const sourcePaths = [
  "web/packages/domain/src/merge.ts",
  "web/packages/domain/src/stl-naming.ts",
  "web/packages/domain/src/import-rules.ts",
  "web/packages/domain/src/scanner.ts",
  "web/apps/server/src/services/plan-freshness.ts",
];

const PART_FIELDS = [
  "partKey", "relativePath", "filename", "sourceLayer", "status", "roleInferred", "roleOverride",
  "quantityInferred", "quantityOverride", "quantityEffective", "included", "notes", "artifactDigest",
];
const INPUT_FIELDS = [
  "sourceId", "sourceLayer", "layerOrder", "trackingKind", "sourceRevisionId", "manifestDigest",
  "effectiveNamingDigest",
];

// Later layers win the same part key. Quantity defaults and slug flags are scalar overrides.
// Import rules keep one base part and leave another present but excluded.
const matrix = [
  { name: "base-only", layers: [
    { name: "Base", type: "base", order: 0, files: ["bracket.stl", "clip_x2.stl"] },
  ] },
  { name: "addon-adds", layers: [
    { name: "Base", type: "base", order: 0, files: ["bracket.stl"] },
    { name: "Addon", type: "addon", order: 1, files: ["clip.stl"] },
  ] },
  { name: "addon-replaces", layers: [
    { name: "Base", type: "base", order: 0, files: ["bracket.stl"] },
    { name: "Addon", type: "addon", order: 1, files: ["bracket.stl"] },
  ] },
  { name: "addon-order-alpha-then-beta", layers: [
    { name: "Base", type: "base", order: 0, files: ["bracket.stl"] },
    { name: "Alpha", type: "addon", order: 1, files: ["shared.stl"] },
    { name: "Beta", type: "addon", order: 2, files: ["shared.stl"] },
  ] },
  { name: "addon-order-beta-then-alpha", layers: [
    { name: "Base", type: "base", order: 0, files: ["bracket.stl"] },
    { name: "Beta", type: "addon", order: 1, files: ["shared.stl"] },
    { name: "Alpha", type: "addon", order: 2, files: ["shared.stl"] },
  ] },
  { name: "scalar-quantity-default", layers: [
    { name: "Base", type: "base", order: 0, files: ["stay.stl", "plain.stl"] },
    { name: "Addon", type: "addon", order: 1, files: ["plain.stl", "extra.stl"], naming: { quantity: { default: 4 } } },
  ] },
  { name: "scalar-strip-quantity-off", layers: [
    { name: "Base", type: "base", order: 0, files: ["clip.stl"] },
    { name: "Addon", type: "addon", order: 1, files: ["clip_x2.stl"], naming: { slug: { strip_quantity: false } } },
  ] },
  { name: "default-strip-conflicts", layers: [
    { name: "Base", type: "base", order: 0, files: ["clip.stl"] },
    { name: "Addon", type: "addon", order: 1, files: ["clip_x2.stl"] },
  ] },
  { name: "included-and-excluded", layers: [
    { name: "Base", type: "base", order: 0, files: ["keep.stl", "drop.stl", "shared.stl"], imported: ["keep.stl"] },
    { name: "Addon", type: "addon", order: 1, files: ["shared.stl", "addon.stl"] },
  ] },
];

const quote = (value) => value == null ? "NULL" : `'${String(value).replaceAll("'", "''")}'`;
const project = (row, fields) => Object.fromEntries(fields.map((field) => [field, row[field] ?? null]));
const byName = (cases, name) => cases.find((row) => row.name === name);
const part = (row, key) => row.parts.find((item) => item.partKey === key);

function sourceMetadata(paths) {
  return paths.map((path) => ({
    path,
    sha256: createHash("sha256").update(readFileSync(join(root, path))).digest("hex"),
  }));
}

function render(cases) {
  const document = {
    metadata: {
      node: NODE_VERSION,
      generator: "fixtures/parity/profile-overlay/generate.mjs",
      sources: sourceMetadata(sourcePaths),
    },
    cases,
  };
  return `${JSON.stringify(document, null, 2)}\n`;
}

const { createSelfHostPorts } = await import(pathToFileURL(join(server, "src/adapters/self-host/index.ts")).href);
const cases = [];
for (const test of matrix) {
  const directory = mkdtempSync(join(tmpdir(), "pp-profile-overlay-"));
  const databasePath = join(directory, "print-partner.db");
  copyFileSync(database, databasePath);
  const ports = createSelfHostPorts(directory);
  await ports.db.connect();
  const raw = new DatabaseSync(databasePath);
  const setup = [];
  const files = {};
  try {
    const apply = (sql) => {
      raw.exec(sql);
      setup.push(sql.replaceAll(directory, "/overlay-root"));
    };
    apply("INSERT INTO build_profiles(id,tenant_id,name,accepted_plan_version) VALUES(3,'default','Overlay Build',0)");
    let sourceId = 10;
    for (const layer of test.layers) {
      const id = sourceId;
      sourceId += 1;
      const physical = join(directory, "sources", String(id));
      mkdirSync(physical, { recursive: true });
      for (const file of layer.files) {
        writeFileSync(join(physical, file), file);
      }
      files[String(id)] = [...layer.files];
      const metadata = layer.naming
        ? JSON.stringify({ naming: { use_defaults: false, override: layer.naming } })
        : null;
      const imported = layer.imported ? JSON.stringify(layer.imported) : null;
      apply(`INSERT INTO projects(id,tenant_id,name,url,local_path,metadata_json,imported_paths,current_source_revision_id) VALUES(${id},'default',${quote(layer.name)},${quote(`https://overlay.invalid/${layer.name}`)},${quote(physical)},${quote(metadata)},${quote(imported)},NULL)`);
      apply(`INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES('default',3,${layer.order},${quote(layer.type)},${id})`);
    }
    const prepared = ports.repository.preparePlanDraft(3, { baseRevisionId: null, basePlanVersion: 0 }, {
      applyManifest: true,
      preferAccepted: false,
    });
    assert.equal(prepared.kind, "prepared", test.name);
    cases.push({
      name: test.name,
      setup,
      files,
      parts: prepared.value.parts.map((row) => project(row, PART_FIELDS)),
      inputs: prepared.value.inputs.map((row) => project(row, INPUT_FIELDS)),
    });
  } finally {
    raw.close();
    await ports.db.close();
    rmSync(directory, { recursive: true, force: true });
  }
}

const alpha = byName(cases, "addon-order-alpha-then-beta");
const beta = byName(cases, "addon-order-beta-then-alpha");
assert.equal(part(alpha, "shared.stl").sourceLayer, "addon:Beta");
assert.equal(part(beta, "shared.stl").sourceLayer, "addon:Alpha");
assert.notDeepEqual(alpha.parts, beta.parts);
assert.notDeepEqual(alpha.inputs, beta.inputs);
const strip = (row) => row.parts.map(({ sourceLayer: _source, ...rest }) => rest);
assert.deepEqual(strip(alpha), strip(beta));
const quantities = byName(cases, "scalar-quantity-default");
assert.equal(part(quantities, "stay.stl").quantityEffective, 1);
assert.equal(part(quantities, "plain.stl").quantityEffective, 4);
assert.equal(part(quantities, "extra.stl").quantityEffective, 4);
assert.notEqual(quantities.inputs[0].effectiveNamingDigest, quantities.inputs[1].effectiveNamingDigest);
const stripOff = byName(cases, "scalar-strip-quantity-off");
assert.equal(stripOff.parts.every((row) => row.status !== "conflict"), true);
const conflicts = byName(cases, "default-strip-conflicts");
assert.equal(conflicts.parts.every((row) => row.status === "conflict"), true);
const inclusion = byName(cases, "included-and-excluded");
assert.equal(inclusion.parts.some((row) => row.included), true);
assert.equal(inclusion.parts.some((row) => !row.included), true);
assert.equal(part(inclusion, "drop.stl").included, false);
assert.equal(part(inclusion, "shared.stl").sourceLayer, "addon:Addon");
for (const row of cases) {
  assert.equal(row.parts.length > 0, true, row.name);
  for (const item of row.parts) assert.match(item.sourceLayer, /^(base|addon):/);
  for (const input of row.inputs) assert.match(input.effectiveNamingDigest, /^[0-9a-f]{64}$/);
}

const output = render(cases);
const check = process.argv.includes("--check");
if (check) assert.equal(readFileSync(fixturePath, "utf8"), output);
else writeFileSync(fixturePath, output);
console.log(`profile-overlay: ${cases.length} Node cases ${check ? "verified" : "written"}`);
