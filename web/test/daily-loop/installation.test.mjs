import assert from "node:assert/strict";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  cleanupCommands,
  importerRunArgs,
  leftoverLists,
  productRunArgs,
  runNames,
  treeDigest,
} from "./installation.mjs";

const RUN = "pp-daily-loop-0a1b2c3d";

test("every container, volume, network and tag of a run carries the run's name", () => {
  const names = runNames(RUN);
  assert.deepEqual(names, {
    container: RUN,
    simulator: `${RUN}-sim`,
    importer: `${RUN}-import`,
    network: RUN,
    volume: RUN,
    importVolume: `${RUN}-imported`,
    restore: `${RUN}-restore`,
    restoreVolume: `${RUN}-restored`,
    image: `${RUN}:test`,
  });
  // The leftover lists filter on the run's name, so each one must contain it.
  for (const value of Object.values(names)) assert.ok(value.startsWith(RUN), value);
  const volumes = [names.volume, names.importVolume, names.restoreVolume];
  assert.equal(new Set(volumes).size, 3, "three different volumes");
  assert.equal(new Set([names.container, names.importer, names.restore, names.simulator]).size, 4);
});

test("the cleanup removes every container, the three volumes, the network and the image tag", () => {
  const names = runNames(RUN);
  assert.deepEqual(cleanupCommands(names), [
    ["rm", "-f", RUN, `${RUN}-import`, `${RUN}-restore`, `${RUN}-sim`],
    ["network", "rm", RUN],
    ["volume", "rm", "-f", RUN, `${RUN}-imported`, `${RUN}-restored`],
    ["rmi", `${RUN}:test`],
  ]);
  assert.deepEqual(leftoverLists(RUN), [
    ["ps", "-aq", "--filter", `name=${RUN}`],
    ["volume", "ls", "-q", "--filter", `name=${RUN}`],
    ["network", "ls", "-q", "--filter", `name=${RUN}`],
    ["images", "-q", "--filter", `reference=${RUN}`],
  ]);
});

test("the product container runs as docs/docker.md says, on the volume it is given", () => {
  const names = runNames(RUN);
  const args = productRunArgs({ ...names, volume: names.importVolume });
  assert.deepEqual(args.slice(0, 5), ["run", "-d", "--name", RUN, "--network"]);
  assert.equal(args[args.indexOf("--network") + 1], RUN);
  assert.equal(args[args.indexOf("-p") + 1], "127.0.0.1::8080");
  assert.equal(args[args.indexOf("-v") + 1], `${RUN}-imported:/data`);
  for (const flag of ["--read-only", "no-new-privileges", "ALL"]) assert.ok(args.includes(flag), flag);
  assert.equal(args.at(-1), `${RUN}:test`);
});

test("the restore container runs as the product container does, on loopback only, on its own volume", () => {
  const names = runNames(RUN);
  const args = productRunArgs({ ...names, container: names.restore, volume: names.restoreVolume });
  assert.equal(args[args.indexOf("--name") + 1], `${RUN}-restore`);
  assert.equal(args[args.indexOf("-v") + 1], `${RUN}-restored:/data`);
  // Every published port is bound to host loopback.
  const published = args.flatMap((arg, at) => (args[at - 1] === "-p" ? [arg] : []));
  assert.deepEqual(published, ["127.0.0.1::8080"]);
});

test("the importer reads the Node installation read-only at its own path and writes the new volume at /data", () => {
  const names = runNames(RUN);
  const args = importerRunArgs({
    container: names.importer,
    volume: names.importVolume,
    image: names.image,
    nodeDir: "/tmp/run-node/node",
  });
  assert.deepEqual(args.slice(0, 6), ["run", "--rm", "--name", `${RUN}-import`, "--network", "none"]);
  const mounts = args.flatMap((arg, at) => (args[at - 1] === "-v" ? [arg] : []));
  assert.deepEqual(mounts, ["/tmp/run-node/node:/tmp/run-node/node:ro", `${RUN}-imported:/data`]);
  assert.ok(!args.includes("-p"), "the importer publishes no port");
  assert.deepEqual(args.slice(args.indexOf(`${RUN}:test`)), [
    `${RUN}:test`,
    ...["pp-server", "import", "--from", "/tmp/run-node/node", "--data", "/data"],
  ]);
});

test("the tree digest changes with any byte, name, mode or time under the folder, and only then", () => {
  const dir = mkdtempSync(join(tmpdir(), "pp-daily-loop-digest-"));
  try {
    mkdirSync(join(dir, "repos/s01"), { recursive: true });
    writeFileSync(join(dir, "print-partner.db"), "rows");
    writeFileSync(join(dir, "repos/s01/part.stl"), "solid part\n");
    const first = treeDigest(dir);
    assert.equal(treeDigest(dir), first, "reading the folder changes nothing");

    const changes = [
      () => writeFileSync(join(dir, "repos/s01/part.stl"), "solid PART\n"),
      () => utimesSync(join(dir, "print-partner.db"), 1, 1),
      () => chmodSync(join(dir, "print-partner.db"), 0o604),
      () => writeFileSync(join(dir, "print-partner.db-shm"), ""),
    ];
    let previous = first;
    for (const change of changes) {
      change();
      const next = treeDigest(dir);
      assert.notEqual(next, previous, change.toString());
      previous = next;
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
