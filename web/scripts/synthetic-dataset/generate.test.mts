import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, sep } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import Database from "better-sqlite3";
import Fastify from "fastify";
// Type-only: app.ts loads the cookie, multipart and auth modules whose request
// and reply fields routes/sources.ts relies on.
import type {} from "../../apps/server/src/app.js";
import { parseAcceptedStlMesh, stlMeshDimensionsUm } from "@print-partner/domain";
import { getDb, SqliteDatabase } from "../../apps/server/src/db/client.js";
import { AppRepository } from "../../apps/server/src/db/repository.js";
import { registerAcceptedPlateRoutes } from "../../apps/server/src/routes/accepted-plates.js";
import { registerSourceRoutes } from "../../apps/server/src/routes/sources.js";
import { publishSourceManifestRevision } from "../../apps/server/src/services/local-source-revision.js";
import { SOURCE_SNAPSHOT_MANIFEST_FILE } from "../../apps/server/src/services/local-source-snapshot.js";
import { loadFleet } from "../../apps/server/src/services/printer-fleet.js";
import { listOpenUnattributedPrints } from "../../apps/server/src/services/unattributed-print-store.js";
import { copyInstallation } from "./copy.mjs";
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
      version: 2,
    });
  }
});

test("the shape counts match what the invariant suite and rehearsal name", () => {
  for (const directory of Object.values(outputs)) {
    const { shape } = manifest(directory);
    assert.equal(shape.schemaVersion, 34);
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

test("every generated part parses as a 20 x 20 x 10 mm accepted mesh", () => {
  const parts = (readdirSync(outputs.a, { recursive: true }) as string[]).filter((path) =>
    path.endsWith(".stl"),
  );
  assert.ok(parts.length > 0);
  for (const part of parts) {
    const mesh = parseAcceptedStlMesh(readFileSync(join(outputs.a, part)));
    assert.ok(mesh, part);
    assert.equal(mesh.faces.length, 12, part);
    assert.deepEqual(
      stlMeshDimensionsUm(mesh),
      { widthUm: 20_000, depthUm: 20_000, heightUm: 10_000 },
      part,
    );
  }
});

// Opens a disposable copy through Node, as the reader tests above do.
function withRepository<T>(directory: string, work: (repo: AppRepository) => Promise<T>) {
  const database = new SqliteDatabase(directory);
  database.connect();
  const repo = new AppRepository(getDb(database), "default", database.reposDir);
  return work(repo).finally(() => database.close());
}

test("every Source revision is a snapshot written by LocalSourceSnapshotStore", () => {
  const db = new Database(join(outputs.a, "print-partner.db"), { readonly: true });
  try {
    const revisions = db
      .prepare(
        `SELECT r.project_id, r.upstream_revision_key, r.manifest_digest, r.snapshot_locator, p.local_path
           FROM source_revisions r JOIN projects p ON p.current_source_revision_id = r.id
         ORDER BY r.id`,
      )
      .all() as Array<{
      project_id: number;
      upstream_revision_key: string;
      manifest_digest: string;
      snapshot_locator: string;
      local_path: string;
    }>;
    assert.equal(revisions.length, 10);
    for (const revision of revisions) {
      assert.equal(revision.snapshot_locator, `${revision.project_id}/revisions/synthetic-a`);
      const snapshotDir = join(outputs.a, "repos", ...revision.snapshot_locator.split("/"));
      assert.equal(revision.local_path, snapshotDir);
      const stored = JSON.parse(
        readFileSync(join(snapshotDir, SOURCE_SNAPSHOT_MANIFEST_FILE), "utf8"),
      ) as { upstreamRevisionKey: string; manifestDigest: string; files: Array<{ kind: string }> };
      assert.equal(stored.upstreamRevisionKey, revision.upstream_revision_key);
      assert.equal(stored.manifestDigest, revision.manifest_digest);
      assert.equal(stored.files.length >= 3, true);
      assert.equal(
        stored.files.every((file) => file.kind === "stl"),
        true,
      );
    }
  } finally {
    db.close();
  }
});

test("saving a Source manifest on a generated installation publishes a new revision", async () => {
  const copy = join(root, "manifest-copy");
  copyInstallation(outputs.a, copy);
  await withRepository(copy, async (repo) => {
    const [source] = repo.listSources();
    assert(source);
    const manifestYaml = "project: synthetic\nparts: []\n";
    const saved = await publishSourceManifestRevision({ repo, sourceId: source.id, manifestYaml });
    assert(saved.local_path);
    assert.notEqual(saved.local_path, source.local_path);
    assert.equal(
      readFileSync(join(saved.local_path, "print-partner.manifest.yaml"), "utf8"),
      manifestYaml,
    );
    assert.equal(repo.listSourceRevisions(source.id).length, 2);
  });
});

test("a disposable copy reads Source STL trees after the original directory is removed", async () => {
  const original = join(root, "copy-original");
  const copy = join(root, "copy-target");
  cpSync(outputs.a, original, { recursive: true });
  assert.equal(copyInstallation(original, copy), 10);
  rmSync(original, { recursive: true, force: true });
  assert.equal(existsSync(original), false);
  const stored = manifest(outputs.a).shape;
  assert.deepEqual(
    {
      ...measureShape(copy),
      deletedBuildIds: stored.deletedBuildIds,
      buildIdDensity: stored.buildIdDensity,
    },
    stored,
  );
  await withRepository(copy, async (repo) => {
    const app = Fastify();
    try {
      await registerSourceRoutes(app, {
        repo,
        reposDir: repo.reposDir,
        sourcesDir: join(copy, "sources"),
        thumbsDir: join(copy, "thumbs"),
        coversDir: join(copy, "covers"),
        dataDir: copy,
        hostedPlanning: false,
      });
      const sources = repo.listSources();
      assert.equal(sources.length, 10);
      for (const source of sources) {
        assert.equal(source.local_path?.startsWith(`${copy}${sep}`), true, source.name);
        const tree = await app.inject({ method: "GET", url: `/sources/${source.id}/stl-tree` });
        assert.equal(tree.statusCode, 200, source.name);
        const body = JSON.stringify(tree.json());
        assert.match(body, /synthetic-s\d{2}-part-01\.stl/, source.name);
      }
    } finally {
      await app.close();
    }
  });
});

test("the synthetic printers are connection-free fleet entries that Plate setup accepts", async () => {
  const copy = join(root, "fleet-copy");
  copyInstallation(outputs.a, copy);
  await withRepository(copy, async (repo) => {
    const fleet = loadFleet(repo);
    assert.deepEqual(
      fleet.map((printer) => [
        printer.id,
        printer.integration_id ?? null,
        printer.device_id ?? null,
      ]),
      [
        ["synthetic-printer-1", null, null],
        ["synthetic-printer-2", null, null],
      ],
    );
    const anchor = repo
      .listProfileHeaders()
      .find((build) => build.name === "Synthetic Build Anchor 47u6p");
    assert(anchor);
    const app = Fastify();
    try {
      await registerAcceptedPlateRoutes(app, { repo, reposDir: repo.reposDir });
      const current = (
        await app.inject({ method: "GET", url: `/plans/${anchor.id}/plates` })
      ).json() as {
        kind: string;
        basis: unknown;
        plate_revision_id: number;
        printers: Array<{ id: string }>;
        plates: Array<{ units: Array<{ token: string }> }>;
        unassigned: Array<{ token: string }>;
      };
      assert.equal(current.kind, "ready");
      assert.deepEqual(
        current.printers.map((printer) => printer.id),
        ["synthetic-printer-1", "synthetic-printer-2"],
      );
      const tokens = [...current.plates.flatMap((plate) => plate.units), ...current.unassigned].map(
        (unit) => unit.token,
      );
      assert.equal(tokens.length, 47);

      const initialized = await app.inject({
        method: "POST",
        url: `/plans/${anchor.id}/plates/initialize`,
        payload: {
          expected: current.basis,
          expected_plate_revision_id: current.plate_revision_id,
          assignments: tokens.map((token) => ({ token, printer_id: "synthetic-printer-1" })),
        },
      });
      assert.equal(initialized.statusCode, 200, initialized.body);
      const ready = initialized.json() as {
        kind: string;
        plate_revision_id: number;
        plates: Array<{
          plate_id: string;
          printer: { id: string };
          units: Array<{ token: string }>;
        }>;
      };
      assert.equal(ready.kind, "ready");
      assert.equal(
        ready.plates.every((plate) => plate.printer.id === "synthetic-printer-1"),
        true,
      );
      const [plate] = ready.plates;
      assert(plate?.units[0]);

      const transferred = await app.inject({
        method: "POST",
        url: `/plans/${anchor.id}/plates/${plate.plate_id}/units/${plate.units[0].token}/transfer`,
        payload: {
          expected: current.basis,
          expected_plate_revision_id: ready.plate_revision_id,
          target_printer_id: "synthetic-printer-2",
        },
      });
      assert.equal(transferred.statusCode, 200, transferred.body);
      // A transferred unit lands unplaced on a Plate for the target printer.
      const after = (
        await app.inject({ method: "GET", url: `/plans/${anchor.id}/plates` })
      ).json() as { unplaced: Array<{ token: string; printer_id: string }> };
      assert.deepEqual(after.unplaced, [
        { ...after.unplaced[0], token: plate.units[0].token, printer_id: "synthetic-printer-2" },
      ]);
    } finally {
      await app.close();
    }
  });
});
