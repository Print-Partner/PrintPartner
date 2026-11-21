// The installations of a daily-loop run (GRE-1515). Step 1 starts the product container on
// a new volume. Step 2 imports the synthetic Node installation into a second new volume with
// a one-off `pp-server import` container, because the import refuses a folder that holds
// data, then replaces the product container on that volume under the same name and network.
// Step 12 restores a backup on a second product container, `restore`, on a third new volume.
// run.mjs and daily-loop.spec.ts both start containers from these arguments.
import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, readlinkSync } from "node:fs";
import { join } from "node:path";

/** The seed of the synthetic Node installation (docs/storage.md#synthetic-installations). */
export const SYNTHETIC_SEED = 1138;

/** The names one run gives what it creates, all from its unique `name`. */
export function runNames(name) {
  return {
    container: name,
    simulator: `${name}-sim`,
    importer: `${name}-import`,
    network: name,
    volume: name,
    importVolume: `${name}-imported`,
    restore: `${name}-restore`,
    restoreVolume: `${name}-restored`,
    image: `${name}:test`,
  };
}

/**
 * The run command from docs/docker.md, on the run's network, except that Docker picks the
 * host port, so two runs on one machine do not collide; the tests read it back with
 * `docker port`.
 */
export function productRunArgs({ container, network, volume, image }) {
  return [
    ...["run", "-d", "--name", container, "--network", network, "-p", "127.0.0.1::8080"],
    ...["--restart", "unless-stopped", "--read-only", "--tmpfs", "/tmp:size=1g,mode=1777"],
    ...["--cap-drop", "ALL", "--security-opt", "no-new-privileges"],
    ...["-v", `${volume}:/data`, image],
  ];
}

/**
 * `docker run` arguments for the one-off import. The Node installation is mounted
 * read-only at its host path, because its rows store absolute Source paths under that
 * folder and the import rebases only paths under `--from`. The new volume is at /data,
 * where the product container mounts it, so the rebased paths lead into the same folder.
 * The import needs no network.
 */
export function importerRunArgs({ container, volume, image, nodeDir }) {
  return [
    ...["run", "--rm", "--name", container, "--network", "none"],
    ...["--read-only", "--tmpfs", "/tmp:size=1g,mode=1777"],
    ...["--cap-drop", "ALL", "--security-opt", "no-new-privileges"],
    ...["-v", `${nodeDir}:${nodeDir}:ro`, "-v", `${volume}:/data`, image],
    ...["pp-server", "import", "--from", nodeDir, "--data", "/data"],
  ];
}

/**
 * A SHA-256 over every entry under `dir`: its path, type, mode, size, modification time
 * and bytes, or a link's target. Two equal digests mean nothing under `dir` changed.
 */
export function treeDigest(dir) {
  const hash = createHash("sha256");
  const walk = (relative) => {
    const names = readdirSync(join(dir, relative)).sort();
    for (const name of names) {
      const path = relative ? `${relative}/${name}` : name;
      const absolute = join(dir, path);
      const stat = lstatSync(absolute);
      const kind = stat.isDirectory() ? "dir" : stat.isSymbolicLink() ? "link" : "file";
      hash.update(`${path}\0${kind}\0${stat.mode}\0${stat.size}\0${stat.mtimeMs}\0`);
      if (kind === "dir") walk(path);
      else hash.update(kind === "link" ? readlinkSync(absolute) : readFileSync(absolute));
      hash.update("\0");
    }
  };
  walk("");
  return hash.digest("hex");
}

/**
 * The `docker` commands that remove what a run created, in order: containers before the
 * network and the volumes they use. Each names only this run's things.
 */
export function cleanupCommands(names) {
  const { container, simulator, importer, restore, network, volume, importVolume, restoreVolume, image } = names;
  return [
    ["rm", "-f", container, importer, restore, simulator],
    ["network", "rm", network],
    ["volume", "rm", "-f", volume, importVolume, restoreVolume],
    ["rmi", image],
  ];
}

/**
 * The `docker` list commands that find what is left of a run. A name filter matches part
 * of a name, so `name=<run>` also finds the simulator, the importer, the restore container
 * and the other volumes.
 */
export function leftoverLists(name) {
  return ["ps -aq", "volume ls -q", "network ls -q", "images -q"].map((list) =>
    `${list} --filter ${list.startsWith("images") ? "reference" : "name"}=${name}`.split(" "),
  );
}
