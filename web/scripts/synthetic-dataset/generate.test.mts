import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { getDb, SqliteDatabase } from "../../apps/server/src/db/client.js";
import { AppRepository } from "../../apps/server/src/db/repository.js";
import { listOpenUnattributedPrints } from "../../apps/server/src/services/unattributed-print-store.js";
import {
  installationDigest,
  MANIFEST_FILE,
  measureShape,
  SYNTHETIC_LABEL,
  UNMATCHED_PRINT_COUNT,
} from "./shape.mjs";

const run = promisify(execFile);
const generator = fileURLToPath(new URL("./generate.mts", import.meta.url));
const root = mkdtempSync(join(tmpdir(), "pp-synthetic-dataset-"));
const outputs = {
  a: join(root, "seed-7-a"),
  b: join(root, "seed-7-b"),
  other: join(root, "seed-8"),
};
type Manifest = {
  label: string;
  synthetic: boolean;
  seed: number;
  generator: { name: string; version: number };
  digest: string;
  schemaVersion: number;
  shape: ReturnType<typeof measureShape> & { deletedBuildIds: number[]; buildIdDensity: number };
};
const manifest = (directory: string) =>
  JSON.parse(readFileSync(join(directory, MANIFEST_FILE), "utf8")) as Manifest;

// Each run is its own process: the generator mocks Date and the accept helper
// keeps a module-level command counter.
const generate = (seed: number, output: string) =>
  run(
    process.execPath,
    ["--conditions=development", "--import", "tsx", generator, "--seed", String(seed), output],
    {
      cwd: fileURLToPath(new URL("../..", import.meta.url)),
    },
  );

before(async () => {
  await Promise.all([generate(7, outputs.a), generate(7, outputs.b), generate(8, outputs.other)]);
});
after(() => rmSync(root, { recursive: true, force: true }));

test("the same seed gives the same digest and a different seed does not", () => {
  const [a, b, other] = [manifest(outputs.a), manifest(outputs.b), manifest(outputs.other)];
  assert.equal(a.digest, b.digest);
  assert.equal(installationDigest(outputs.a), a.digest);
  assert.equal(installationDigest(outputs.b), a.digest);
  assert.notEqual(other.digest, a.digest);
  assert.deepEqual(a.shape, b.shape);
});

test("every run is labeled synthetic with its seed and generator version", () => {
  for (const [directory, seed] of [
    [outputs.a, 7],
    [outputs.other, 8],
  ] as const) {
    const label = manifest(directory);
    assert.equal(label.label, SYNTHETIC_LABEL);
    assert.equal(label.label, "synthetic: not the architecture's step-2 rehearsal");
    assert.equal(label.synthetic, true);
    assert.equal(label.seed, seed);
    assert.deepEqual(label.generator, {
      ...label.generator,
      name: "print-partner-synthetic-dataset",
      version: 1,
    });
  }
});

test("the shape counts match what the invariant suite and rehearsal name", () => {
  for (const directory of Object.values(outputs)) {
    const { shape, schemaVersion } = manifest(directory);
    assert.equal(schemaVersion, 34);
    assert.deepEqual(measureShape(directory), {
      schemaVersion: shape.schemaVersion,
      sources: shape.sources,
      buildIds: shape.buildIds,
      layers: shape.layers,
      acceptedPlanRevisions: shape.acceptedPlanRevisions,
      requiredUnits: shape.requiredUnits,
      acceptedPlateRevisions: shape.acceptedPlateRevisions,
      currentPlates: shape.currentPlates,
      unmatchedPrints: shape.unmatchedPrints,
    });
    // ID density: 22 live Builds with 7 deleted IDs between them.
    assert.equal(shape.buildIds.length, 22);
    assert.equal(shape.deletedBuildIds.length, 7);
    assert.equal(new Set([...shape.buildIds, ...shape.deletedBuildIds]).size, 29);
    assert.deepEqual(
      [...shape.buildIds, ...shape.deletedBuildIds].sort((x, y) => x - y),
      Array.from({ length: 29 }, (_, index) => index + 1),
    );
    // Plan and Plate sizes: many layers, accepted history, Plate history.
    assert.equal(shape.layers.maxPerBuild >= 4, true);
    assert.equal(shape.acceptedPlanRevisions.maxPerBuild, 4);
    assert.equal(shape.acceptedPlanRevisions.total > shape.buildIds.length, true);
    assert.equal(shape.acceptedPlateRevisions.onEarlierPlanRevision > 0, true);
    assert.equal(shape.acceptedPlateRevisions.maxPerBuild >= 2, true);
    assert.deepEqual(shape.currentPlates["Synthetic Build Anchor 100u15p"], {
      units: 100,
      plates: 15,
    });
    assert.deepEqual(shape.currentPlates["Synthetic Build Anchor 47u6p"], { units: 47, plates: 6 });
    assert.equal(shape.unmatchedPrints, UNMATCHED_PRINT_COUNT);
    assert.equal(UNMATCHED_PRINT_COUNT, 18);
  }
});

// Node connect re-seeds starter slicer profiles with INSERT OR IGNORE, which
// advances their sqlite_sequence rows on every open. Nothing else may change.
test("Node opens a disposable copy and reads it without changing its shape", () => {
  const copy = join(root, "node-reader-copy");
  cpSync(outputs.a, copy, { recursive: true });
  const before = measureShape(copy);
  const database = new SqliteDatabase(copy);
  database.connect();
  try {
    const repo = new AppRepository(getDb(database), "default", database.reposDir);
    const builds = repo.listProfileHeaders();
    assert.equal(builds.length, 22);
    for (const build of builds) {
      const accepted = repo.getAcceptedPlanRevision(build.id);
      assert(accepted, `${build.name} has an accepted Plan`);
      assert.equal(accepted.parts.length > 0, true);
    }
    const anchor = builds.find((build) => build.name === "Synthetic Build Anchor 100u15p");
    assert(anchor);
    const plates = repo.readAcceptedPlates(anchor.id);
    assert.equal(plates.kind, "ready");
    assert.equal(plates.plates.length, 15);
    assert.equal(plates.plates.flatMap((plate) => plate.units).length, 100);
    const prints = listOpenUnattributedPrints(repo);
    assert.equal(prints.length, 18);
    assert.equal(
      prints.every((print) => print.filename.startsWith("synthetic-")),
      true,
    );
  } finally {
    database.close();
  }
  assert.deepEqual(measureShape(copy), before);
});

// Rust storage opens a copy through WriterOwner and migrates it to its schema
// ceiling. Build the binary with
// `cargo build --manifest-path rust/Cargo.toml --locked -p pp-storage --bin storage-fixture`.
const rustOpener = process.env.PP_STORAGE_FIXTURE_BIN;
test(
  "Rust storage opens a disposable copy",
  { skip: rustOpener ? false : "set PP_STORAGE_FIXTURE_BIN" },
  async () => {
    const copy = join(root, "rust-reader-copy");
    cpSync(outputs.a, copy, { recursive: true });
    const before = measureShape(copy);
    const { stdout } = await run(rustOpener!, ["initialize", copy]);
    const ready = JSON.parse(stdout) as { version: number; previous_version: number };
    assert.equal(ready.previous_version, 34);
    assert.equal(ready.version >= 35, true);
    const after = measureShape(copy);
    assert.deepEqual({ ...after, schemaVersion: before.schemaVersion }, before);
  },
);

test("the generator refuses a non-empty output directory", async () => {
  const occupied = join(root, "occupied");
  cpSync(outputs.a, occupied, { recursive: true });
  writeFileSync(join(occupied, "keep.txt"), "not generator output\n");
  await assert.rejects(generate(9, occupied), /refusing to write into non-empty directory/);
  assert.equal(readFileSync(join(occupied, "keep.txt"), "utf8"), "not generator output\n");
});
