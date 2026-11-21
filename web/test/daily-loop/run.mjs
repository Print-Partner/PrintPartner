#!/usr/bin/env node
// Runs the daily-loop suite (docs/product/daily-loop.md) the same way locally and in
// daily-loop.yml:
//
//   npm --prefix web run test:daily-loop
//
// It checks the test titles against the steps table and writes the synthetic Node
// installation (`npm run fixture:synthetic`, seed 1138) into a new temporary folder. It
// builds the Rust-only image from Dockerfile.rust at HEAD and creates a Docker network for
// the run. On that network it starts the Moonraker simulator as `pp-sim` (simulator.mjs)
// and the image on a new volume with the run command from docs/docker.md. It creates a
// second, empty volume for Step 2's import (installation.mjs). It checks that the container
// reaches the simulator and that both publish ports on host loopback only, then runs the
// twelve Playwright tests. Whether the run passed, failed or was stopped, it removes the
// containers (product, importer and simulator), both volumes, the image tag, the network,
// the Node installation's folder and the simulator's API key folder it created.
//
// The tests read the simulator from these variables:
//   PP_DAILY_LOOP_SIMULATOR        its container; `docker port` names its control port
//   PP_DAILY_LOOP_PRINTER_URL      the printer's address inside the product container
//   PP_DAILY_LOOP_PRINTER_KEY_FILE a host file holding the printer's API key
// and Step 2's import from these:
//   PP_DAILY_LOOP_NODE_INSTALLATION the synthetic Node installation's host folder
//   PP_DAILY_LOOP_IMAGE, PP_DAILY_LOOP_NETWORK, PP_DAILY_LOOP_IMPORTER and
//   PP_DAILY_LOOP_IMPORT_VOLUME     what Step 2 starts the importer and the new container from
import { execFileSync, spawn, spawnSync } from "node:child_process";
import console from "node:console";
import { randomBytes } from "node:crypto";
import { chmodSync, existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { setInterval } from "node:timers";
import { setTimeout as delay } from "node:timers/promises";
import {
  PRINTER_PORT,
  PRINTER_URL,
  SIM_ALIAS,
  isPrivateIPv4,
  nodeImage,
  parseAnswer,
  publishedOffLoopback,
  simulatorRunArgs,
  withKeyFile,
} from "./simulator.mjs";
import {
  cleanupCommands,
  leftoverLists,
  productRunArgs,
  runNames,
  SYNTHETIC_SEED,
} from "./installation.mjs";

const here = import.meta.dirname;
const root = resolve(here, "../../..");
const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
// Unique per run: another run on this machine keeps its own containers, volume, network and tag.
const name = `pp-daily-loop-${randomBytes(4).toString("hex")}`;
const names = runNames(name);
const { image, simulator } = names;

// A stop ends the running command, starts no other, and still runs the cleanup below.
let child;
let stopped = false;
const stop = (signal) => {
  stopped = true;
  if (!child?.group) child?.kill(signal);
  else {
    try {
      process.kill(-child.pid, signal);
    } catch {
      // The group has already exited.
    }
  }
};
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => stop(signal));
// npm runs this with `exec`, so a signal sent to npm reaches this process. If the parent
// dies without passing one on (killed with SIGKILL, say), this process is reparented:
// stop as if it sent SIGTERM.
const parent = process.ppid;
setInterval(() => process.ppid !== parent && !stopped && stop("SIGTERM"), 1000).unref();

/**
 * Runs a command with this terminal's output; resolves to its exit code. With `group`, the
 * command leads a new process group and a stop signals the whole group: `npm run` does not
 * pass a signal on to the script it started.
 */
const run = (command, args, { group = false, ...options } = {}) =>
  new Promise((done) => {
    child = spawn(command, args, { stdio: "inherit", cwd: root, detached: group, ...options });
    child.group = group;
    child.on("error", () => done(1));
    child.on("close", (code) => done(code ?? 1));
  });

const docker = (args, options = {}) =>
  spawnSync("docker", args, { encoding: "utf8", timeout: 30_000, ...options });
const inspect = (container, format) => docker(["inspect", "--format", format, container]).stdout.trim();

/**
 * Checks what the tests rely on before they start: the simulator is up, the product
 * container reaches it at PRINTER_URL with the key, and no port is published off loopback.
 * Returns the first problem, or null.
 */
async function checkSimulator(keyFile) {
  let ready = false;
  for (let tries = 0; tries < 60 && !ready && !stopped; tries++) {
    if (inspect(simulator, "{{.State.Running}}") !== "true") return "the simulator stopped";
    ready = /"control_port":\d+/.test(docker(["logs", simulator]).stdout);
    if (!ready) await delay(500);
  }
  if (!ready) return "the simulator did not start in 30 s";

  for (const container of [simulator, name]) {
    const off = publishedOffLoopback(JSON.parse(inspect(container, "{{json .NetworkSettings.Ports}}")));
    if (off.length > 0) return `${container} publishes off host loopback: ${off.join(", ")}`;
  }

  // pp-net's outbound rule accepts an RFC 1918 printer address.
  const address = inspect(simulator, `{{(index .NetworkSettings.Networks "${name}").IPAddress}}`);
  if (!isPrivateIPv4(address)) return `${SIM_ALIAS} has ${address || "no"} address, not RFC 1918`;
  const resolved = docker(["exec", name, "getent", "hosts", SIM_ALIAS]).stdout.split(/\s+/)[0];
  if (resolved !== address) return `the container resolves ${SIM_ALIAS} to ${resolved || "nothing"}, not ${address}`;

  // From inside the product container, with the key on stdin so no command line holds it.
  const probe = docker(["exec", "-i", name, "bash", "-c", [
    "read -r key",
    `exec 3<>/dev/tcp/${SIM_ALIAS}/${PRINTER_PORT}`,
    String.raw`printf 'GET /server/info HTTP/1.0\r\nHost: ${SIM_ALIAS}\r\nX-Api-Key: %s\r\n\r\n' "$key" >&3`,
    "cat <&3",
  ].join("\n")], { input: readFileSync(keyFile) });
  const answer = parseAnswer(probe.stdout);
  if (answer.status !== 200 || !answer.body.includes('"klippy_state":"ready"')) {
    return `the container did not reach ${PRINTER_URL}/server/info: ${answer.status ?? probe.stderr.trim()}`;
  }
  console.log(`The container reached ${PRINTER_URL} (${address}); ports are published on 127.0.0.1 only.`);
  return null;
}

/**
 * The Node generator opens its database with better-sqlite3. `npm ci --ignore-scripts`, as
 * daily-loop.yml runs it, installs no binding the package does not ship prebuilt, so build
 * one here when it is missing. The package's own build script runs where `npm rebuild`
 * would skip its install script under npm's `allowScripts`.
 */
async function ensureBetterSqlite() {
  const web = resolve(root, "web");
  const probe = ["-e", "new (require('better-sqlite3'))(':memory:').close()"];
  if (spawnSync(process.execPath, probe, { cwd: web, stdio: "ignore" }).status === 0) return 0;
  console.log("better-sqlite3 has no native binding; building it.");
  const build = ["run", "build-release", "--prefix", "node_modules/better-sqlite3"];
  const code = await run("npm", build, { cwd: web, group: true });
  if (code !== 0 || stopped) return code || 1;
  return spawnSync(process.execPath, probe, { cwd: web, stdio: "inherit" }).status ?? 1;
}

/**
 * Writes the synthetic Node installation into `dir`, which is empty. Its files are made
 * readable to the image's user (uid 1000), which may not be the host's: the folder above
 * stays mode 0700, so no other host user reaches them.
 */
async function writeNodeInstallation(dir) {
  const code = await ensureBetterSqlite();
  if (code !== 0 || stopped) return code || 1;
  const args = ["run", "fixture:synthetic", "--", "--seed", String(SYNTHETIC_SEED), dir];
  const written = await run("npm", args, { cwd: resolve(root, "web"), group: true });
  if (written !== 0 || stopped) return written || 1;
  const open = (path) => {
    const stat = statSync(path);
    chmodSync(path, stat.mode | (stat.isDirectory() ? 0o055 : 0o044));
    if (stat.isDirectory()) for (const entry of readdirSync(path)) open(join(path, entry));
  };
  open(dir);
  return 0;
}

async function suite(keyFile, nodeDir) {
  const tests = ["steps", "simulator", "installation"].map((test) => resolve(here, `${test}.test.mjs`));
  const steps = [
    [process.execPath, ["--test", ...tests]],
    [null, () => writeNodeInstallation(nodeDir)],
    ["docker", ["build", "-f", "Dockerfile.rust", "--build-arg", `PP_COMMIT=${commit}`, "-t", image, "."]],
    ["docker", ["network", "create", name]],
    [
      "docker",
      simulatorRunArgs({
        container: simulator,
        network: name,
        image: nodeImage(),
        keyDir: dirname(keyFile),
        user: `${process.getuid()}:${process.getgid()}`,
      }),
    ],
    ["docker", ["volume", "create", names.volume]],
    // Step 2 imports into this one; the import accepts only an empty folder.
    ["docker", ["volume", "create", names.importVolume]],
    ["docker", productRunArgs(names)],
  ];
  for (const [command, args] of steps) {
    const code = command ? await run(command, args) : await args();
    if (code !== 0 || stopped) return code || 1;
  }
  const problem = await checkSimulator(keyFile);
  if (problem || stopped) {
    if (problem) console.error(`Harness: ${problem}.`);
    return 1;
  }
  // Playwright's own CLI, not `npx playwright`: npx runs it under a shell that does not
  // pass a signal on, so a stopped run would leave the browser tests running.
  const playwright = resolve(root, "web/node_modules/@playwright/test/cli.js");
  return run(process.execPath, [playwright, "test", "--config", "test/daily-loop/playwright.config.ts"], {
    cwd: resolve(root, "web"),
    env: {
      ...process.env,
      PP_DAILY_LOOP_CONTAINER: name,
      PP_DAILY_LOOP_COMMIT: commit,
      PP_DAILY_LOOP_SIMULATOR: simulator,
      PP_DAILY_LOOP_PRINTER_URL: PRINTER_URL,
      PP_DAILY_LOOP_PRINTER_KEY_FILE: keyFile,
      PP_DAILY_LOOP_NODE_INSTALLATION: nodeDir,
      PP_DAILY_LOOP_IMAGE: image,
      PP_DAILY_LOOP_NETWORK: names.network,
      PP_DAILY_LOOP_IMPORTER: names.importer,
      PP_DAILY_LOOP_IMPORT_VOLUME: names.importVolume,
    },
  });
}

let code = 1;
// The synthetic Node installation, in a new 0700 folder; Step 2 imports it.
const nodeRoot = mkdtempSync(join(tmpdir(), `${name}-node-`));
try {
  // A run that stops before the tests must not leave an older run's score in place.
  rmSync(resolve(root, "web/test-results/daily-loop/daily-loop-score.json"), { force: true });
  // The printer's API key, new for each run; only the simulator and the tests read it.
  code = await withKeyFile(`${name}-`, (keyFile) => suite(keyFile, join(nodeRoot, "node")));
  if (stopped) code ||= 1;
} finally {
  // Only what this run created, by its unique name. A docker command the tests started just
  // before a stop can still create its container after the first pass, so look again.
  let left = [];
  for (let pass = 0; pass < 3; pass++) {
    if (pass > 0) await delay(2000);
    for (const args of cleanupCommands(names)) spawnSync("docker", args, { stdio: "ignore" });
    rmSync(nodeRoot, { recursive: true, force: true });
    left = leftoverLists(name)
      .filter((list) => {
        const listed = spawnSync("docker", list, { encoding: "utf8" });
        return listed.status !== 0 || listed.stdout.trim() !== "";
      })
      .map((list) => `docker ${list.join(" ")}`);
    if (existsSync(nodeRoot)) left.push(`ls ${nodeRoot}`);
    if (left.length === 0) break;
  }
  if (left.length > 0) {
    console.error(`Not removed, check by hand: ${left.join("; ")}`);
    code ||= 1;
  } else {
    const { importer, importVolume } = names;
    console.log(
      `Removed containers ${name}, ${importer} and ${simulator}, volumes ${name} and ${importVolume}, network ${name}, image tag ${image}, and the Node installation's folder.`,
    );
  }
}
process.exit(code);
