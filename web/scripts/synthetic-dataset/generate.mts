// Builds a synthetic, production-shaped Node schema-34 installation through the
// Node server's own connect, repository and accept code. Output is a function
// of the seed. See docs/storage.md#synthetic-installations.
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mock } from "node:test";
import { getDb, SqliteDatabase } from "../../apps/server/src/db/client.js";
import { acceptedPlanBasis } from "../../apps/server/src/db/accepted-plan-progress.js";
import type { AcceptedPlateInput } from "../../apps/server/src/db/accepted-plates.js";
import { AppRepository } from "../../apps/server/src/db/repository.js";
import { currentSchemaVersion } from "../../apps/server/src/db/schema.js";
import { saveUnattributedPrint } from "../../apps/server/src/services/unattributed-print-store.js";
import {
  acceptPlanForTest,
  editAcceptedPartsForTest,
} from "../../apps/server/src/test/accept-plan.js";
import {
  GENERATOR_NAME,
  GENERATOR_VERSION,
  installationDigest,
  MANIFEST_FILE,
  measureShape,
  SYNTHETIC_LABEL,
  UNMATCHED_PRINT_COUNT,
} from "./shape.mjs";

const CLOCK = "2026-10-02T00:00:00.000Z";
const SOURCE_COUNT = 10;
const BUILD_COUNT = 22;
const DELETED_BUILD_COUNT = 7;
// Plan and Plate sizes the UX target and the cutover rehearsal name.
const ANCHORS = [
  { name: "Synthetic Build Anchor 100u15p", layers: 4, revisions: 4, units: 100, plates: 15 },
  { name: "Synthetic Build Anchor 47u6p", layers: 3, revisions: 3, units: 47, plates: 6 },
] as const;
const BED = { bedWidthUm: 250_000, bedDepthUm: 220_000, bedHeightUm: 220_000, marginUm: 5_000 };
const UNIT = { widthUm: 20_000, depthUm: 20_000, heightUm: 10_000 };
const PITCH_UM = 30_000;
const COLUMNS = 8;
const ROWS = 7;
const UNITS_PER_PLATE = COLUMNS * ROWS;
const PRINTERS = [1, 2].map((n) => ({
  printerId: `synthetic-printer-${n}`,
  printerName: `Synthetic Printer ${n}`,
  printerModel: "Synthetic Model 250",
}));

function parseArgs(argv: readonly string[]): { seed: number; output: string } {
  let seed: number | undefined;
  const rest: string[] = [];
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === "--seed") seed = Number(argv[++index]);
    else rest.push(argv[index]!);
  }
  if (
    seed === undefined ||
    !Number.isInteger(seed) ||
    seed < 0 ||
    seed > 0xffff_ffff ||
    rest.length !== 1
  ) {
    throw new Error("usage: generate.mts --seed <0..4294967295> OUTPUT_DIR");
  }
  return { seed, output: resolve(rest[0]!) };
}

// mulberry32: small, fast and stable across Node versions.
function prng(seed: number) {
  let state = seed >>> 0;
  const next = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4_294_967_296;
  };
  const int = (min: number, max: number) => min + Math.floor(next() * (max - min + 1));
  const hex = (length: number) => Array.from({ length }, () => int(0, 15).toString(16)).join("");
  const shuffle = <T,>(items: readonly T[]): T[] => {
    const copy = [...items];
    for (let index = copy.length - 1; index > 0; index -= 1) {
      const other = int(0, index);
      [copy[index], copy[other]] = [copy[other]!, copy[index]!];
    }
    return copy;
  };
  return { next, int, hex, shuffle };
}

const pad = (n: number) => String(n).padStart(2, "0");
const sha256 = (text: string) => createHash("sha256").update(text).digest("hex");
const stl = (name: string) =>
  `solid ${name}\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid ${name}\n`;

// Same identity format the Node Plate workspace uses for packed Plates.
function plateId(
  basis: ReturnType<typeof acceptedPlanBasis>,
  printerId: string,
  tokens: readonly string[],
) {
  const digest = sha256(
    JSON.stringify([
      "accepted-plate-v1",
      basis.revisionDigest,
      basis.requiredUnitMappingDigest,
      printerId,
      [...tokens].sort(),
    ]),
  );
  return `plate_${digest.slice(0, 32)}`;
}

function chunk<T>(items: readonly T[], count: number): T[][] {
  return Array.from({ length: count }, (_, index) =>
    items.slice(
      Math.floor((index * items.length) / count),
      Math.floor(((index + 1) * items.length) / count),
    ),
  );
}

type Random = ReturnType<typeof prng>;
type BuildSpec = {
  name: string;
  layers: number;
  revisions: number;
  units: number;
  plates: number;
  deleted: boolean;
};

function buildSpecs(random: Random): BuildSpec[] {
  const others = Array.from({ length: BUILD_COUNT - ANCHORS.length }, () => ({
    name: "",
    layers: random.int(1, 5),
    revisions: random.int(1, 4),
    units: 0,
    plates: random.next() < 0.3 ? 0 : random.int(1, 8),
    deleted: false,
  }));
  const deleted = Array.from({ length: DELETED_BUILD_COUNT }, () => ({
    name: "",
    layers: 1,
    revisions: 1,
    units: 0,
    plates: 0,
    deleted: true,
  }));
  return random.shuffle([
    ...ANCHORS.map((anchor) => ({ ...anchor, deleted: false })),
    ...others,
    ...deleted,
  ]);
}

// Final per-part quantities: every part keeps at least one unit and the total
// is the target. The result always differs from the current revision.
function finalQuantities(
  random: Random,
  current: readonly number[],
  target: number,
  fixedTarget: boolean,
): number[] {
  const next = current.map(() => 1);
  for (let extra = target - current.length; extra > 0; extra -= 1)
    next[random.int(0, next.length - 1)]! += 1;
  if (!next.every((quantity, index) => quantity === current[index])) return next;
  const from = next.findIndex((quantity) => quantity > 1);
  if (next.length > 1 && from >= 0) {
    next[from]! -= 1;
    next[(from + 1) % next.length]! += 1;
  } else if (fixedTarget) {
    throw new Error("final revision would be unchanged");
  } else {
    next[0]! += 1;
  }
  return next;
}

function sourceFiles(root: string, label: string, count: number): string[] {
  mkdirSync(root, { recursive: true });
  const names = Array.from(
    { length: count },
    (_, part) => `synthetic-s${label}-part-${pad(part + 1)}.stl`,
  );
  for (const name of names) {
    writeFileSync(join(root, name), stl(name));
    utimesSync(join(root, name), new Date(), new Date());
  }
  return names;
}

function populate(repo: AppRepository, reposDir: string, random: Random): number[] {
  const tick = () => mock.timers.tick(60_000);
  const sources = Array.from({ length: SOURCE_COUNT }, (_, index) => {
    const label = pad(index + 1);
    const source = repo.createSource({
      name: `Synthetic Source ${label}`,
      url: `https://example.invalid/synthetic/source-${label}`,
      source_kind: "github",
    });
    const observed = repo.getProjectRow(source.id);
    if (!observed) throw new Error("synthetic Source row is missing");
    const locator = `${source.id}/revisions/synthetic-a`;
    const names = sourceFiles(join(reposDir, locator), label, random.int(3, 8));
    const revision = repo.recordSourceRevision({
      sourceId: source.id,
      upstreamRevisionKey: "synthetic-a",
      manifestDigest: sha256(names.join("\n")),
      snapshotLocator: locator,
      syncedAt: new Date().toISOString(),
      completeness: "complete",
    });
    repo.activateSourceRevision({
      sourceId: source.id,
      revisionId: revision.id,
      observed,
      sourceVersion: "synthetic-a",
    });
    tick();
    return source.id;
  });

  const createBuild = (name: string, layerCount: number) => {
    const [base, ...addons] = random.shuffle(sources).slice(0, layerCount);
    const profile = repo.createProfile(name, base);
    for (const addon of addons) repo.addAddonLayer(profile.id, addon!);
    const accepted = acceptPlanForTest(repo, profile.id);
    if (!accepted.merged) throw new Error(`${name}: initial accept failed (${accepted.reason})`);
    tick();
    return profile.id;
  };
  const acceptedParts = (profileId: number) => {
    const revision = repo.getAcceptedPlanRevision(profileId);
    if (!revision) throw new Error(`Build ${profileId} has no accepted Plan`);
    return revision.parts.filter((part) => part.included && part.projectionPartId != null);
  };
  const setQuantities = (profileId: number, quantities: readonly number[]) => {
    const edits = acceptedParts(profileId).map((part, index) => ({
      projectionPartId: part.projectionPartId!,
      quantityOverride: quantities[index]!,
    }));
    editAcceptedPartsForTest(repo, profileId, edits);
    tick();
  };
  const publishPlates = (
    profileId: number,
    plateCount: number,
    reverse: boolean,
    expectedPlateRevisionId: number | null,
  ) => {
    const accepted = repo.readAcceptedPlanOperationalSnapshot(profileId);
    if (accepted.kind !== "ready")
      throw new Error(`Build ${profileId}: accepted snapshot ${accepted.kind}`);
    const basis = acceptedPlanBasis(accepted.snapshot);
    const units = accepted.snapshot.parts
      .filter((part) => part.included)
      .flatMap((part) => part.units.map((unit) => unit.token));
    if (reverse) units.reverse();
    const count = Math.max(
      Math.ceil(units.length / UNITS_PER_PLATE),
      Math.min(plateCount, units.length),
    );
    const plates: AcceptedPlateInput[] = chunk(units, count).map((group, index) => {
      const printer = PRINTERS[index % PRINTERS.length]!;
      return {
        plateId: plateId(basis, printer.printerId, group),
        ...printer,
        ...BED,
        units: group.map((token, slot) => ({
          token,
          xUm: BED.marginUm + (slot % COLUMNS) * PITCH_UM,
          yUm: BED.marginUm + Math.floor(slot / COLUMNS) * PITCH_UM,
          ...UNIT,
        })),
      };
    });
    const result = repo.publishAcceptedPlates({
      profileId,
      expected: basis,
      expectedPlateRevisionId,
      plates,
    });
    if (result.kind !== "published")
      throw new Error(`Build ${profileId}: Plate publication ${result.kind}`);
    tick();
    return result.plateRevisionId;
  };

  const populateBuild = (name: string, spec: BuildSpec) => {
    const profileId = createBuild(name, spec.layers);
    const partCount = acceptedParts(profileId).length;
    const target =
      spec.units || (spec.revisions === 1 ? partCount : partCount + random.int(0, partCount * 2));
    if (target < partCount)
      throw new Error(`${name}: ${target} units cannot cover ${partCount} parts`);
    let current = Array.from({ length: partCount }, () => 1);
    for (let revision = 2; revision < spec.revisions; revision += 1) {
      current = current.map((quantity) => (random.next() < 0.3 ? quantity + 1 : quantity));
      current[(revision - 2) % partCount]! += 1;
      setQuantities(profileId, current);
    }
    if (spec.revisions > 1) {
      // An older Plate revision stays in history, linked to the prior Plan revision.
      if (spec.plates > 0 && spec.revisions >= 3)
        publishPlates(profileId, spec.plates, false, null);
      setQuantities(profileId, finalQuantities(random, current, target, spec.units > 0));
    }
    if (spec.plates > 0) {
      const first = publishPlates(profileId, spec.plates, false, null);
      if (spec.units) publishPlates(profileId, spec.plates, true, first);
    }
  };

  const deletedBuildIds: number[] = [];
  let kept = 0;
  for (const spec of buildSpecs(random)) {
    if (spec.deleted) {
      // Deleted Builds leave the ID gaps a long-lived installation has.
      const id = createBuild(
        `Synthetic Deleted Build ${pad(deletedBuildIds.length + 1)}`,
        spec.layers,
      );
      repo.deleteProfile(id);
      deletedBuildIds.push(id);
      tick();
    } else {
      populateBuild(spec.name || `Synthetic Build ${pad(++kept)}`, spec);
    }
  }
  return deletedBuildIds.sort((a, b) => a - b);
}

function addUnmatchedPrints(repo: AppRepository, reposDir: string, random: Random) {
  const filenames = readdirSync(reposDir, { recursive: true, encoding: "utf8" })
    .filter((path) => path.endsWith(".stl"))
    .map((path) => path.split(/[\\/]/).pop()!)
    .sort();
  for (let index = 0; index < UNMATCHED_PRINT_COUNT; index += 1) {
    const base = filenames[random.int(0, filenames.length - 1)]!.replace(/\.stl$/, "");
    const printer = PRINTERS[index % PRINTERS.length]!;
    const uuid = random.hex(32);
    saveUnattributedPrint(repo, {
      id: `${uuid.slice(0, 8)}-${uuid.slice(8, 12)}-4${uuid.slice(13, 16)}-a${uuid.slice(17, 20)}-${uuid.slice(20)}`,
      integration_id: "synthetic-integration",
      printer_id: printer.printerId,
      host_name: `${printer.printerId}.invalid`,
      filename: `${base}-run-${pad(index + 1)}.gcode`,
      completed_at: new Date(
        Date.now() - (UNMATCHED_PRINT_COUNT - index) * 3_600_000,
      ).toISOString(),
      gcode_objects: [`${base}.stl_id_0_copy_0`],
      candidates: [],
    });
  }
}

function generate(seed: number, output: string) {
  if (currentSchemaVersion !== 34)
    throw new Error(`generator targets Node schema 34, found ${currentSchemaVersion}`);
  if (existsSync(output) && readdirSync(output).length > 0) {
    throw new Error(
      `refusing to write into non-empty directory ${output}; synthetic output needs a fresh directory`,
    );
  }
  mkdirSync(output, { recursive: true });
  const random = prng(seed);
  const tokens = prng(seed ^ 0x9e37_79b9);
  mock.timers.enable({ apis: ["Date"], now: Date.parse(CLOCK) });
  const database = new SqliteDatabase(output);
  database.connect();
  let deletedBuildIds: number[];
  try {
    const repo = new AppRepository(getDb(database), "default", database.reposDir, undefined, {
      clock: () => new Date(),
      tokenFactory: () => `ppu_${tokens.hex(32)}`,
    });
    deletedBuildIds = populate(repo, database.reposDir, random);
    addUnmatchedPrints(repo, database.reposDir, random);
  } finally {
    database.close();
    mock.timers.reset();
  }

  const measured = measureShape(output);
  const maxId = Math.max(...measured.buildIds, ...deletedBuildIds);
  const manifest = {
    label: SYNTHETIC_LABEL,
    synthetic: true,
    generator: {
      name: GENERATOR_NAME,
      version: GENERATOR_VERSION,
      script: "web/scripts/synthetic-dataset/generate.mts",
      sha256: createHash("sha256")
        .update(readFileSync(fileURLToPath(import.meta.url)))
        .digest("hex"),
    },
    seed,
    clock: CLOCK,
    node: process.version,
    schemaVersion: measured.schemaVersion,
    digest: installationDigest(output),
    shape: {
      ...measured,
      deletedBuildIds,
      buildIdDensity: Number((measured.buildIds.length / maxId).toFixed(4)),
      anchors: ANCHORS.map(({ name, units, plates }) => ({ name, units, plates })),
    },
  };
  writeFileSync(join(output, MANIFEST_FILE), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}

const { seed, output } = parseArgs(process.argv.slice(2));
const manifest = generate(seed, output);
process.stdout.write(
  `${JSON.stringify({ output, seed, digest: manifest.digest, label: manifest.label })}\n`,
);
