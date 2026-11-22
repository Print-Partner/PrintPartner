#!/usr/bin/env node
// Runs the daily-loop suite (docs/product/daily-loop.md) the same way locally and in
// daily-loop.yml:
//
//   npm --prefix web run test:daily-loop
//
// It checks the test titles against the steps table and writes the synthetic Node
// installation (`npm run fixture:synthetic`, seed 1138) into a new temporary folder. It
// builds the Rust-only image from Dockerfile.rust at HEAD and creates a Docker network for
// the run. On that network it starts the Moonraker simulator as `pp-sim` (simulator.mjs),
// the Spoolman stand-in as `pp-spoolman` (spoolman.mjs) and the image on a new volume with
// the run command from docs/docker.md. It creates a second, empty volume for Step 2's
// import and a third for Step 12's restore (installation.mjs). It checks that the container
// reaches the simulator and the stand-in and that all three publish ports on host loopback
// only, then runs the twelve Playwright tests. Whether the run passed, failed or was
// stopped, it removes the containers (product, importer, restore, simulator and stand-in),
// the three volumes, the image tag, the network, the run's folder (the Node installation and
// the browser's files and downloads) and the simulator's API key folder it created. Before it
// removes them it waits for every process the tests started (runner.mjs, teardown.mjs), so a
// `docker run` still in flight cannot create a container afterwards.
//
// The tests read the simulator from these variables:
//   PP_DAILY_LOOP_SIMULATOR        its container; `docker port` names its control port
//   PP_DAILY_LOOP_PRINTER_URL      the printer's address inside the product container
//   PP_DAILY_LOOP_PRINTER_KEY_FILE a host file holding the printer's API key
// Step 11 reads the Spoolman stand-in from these:
//   PP_DAILY_LOOP_SPOOLMAN         its container; `docker port` names its control port
//   PP_DAILY_LOOP_SPOOLMAN_URL     its address inside the product container
// and Step 2's import from these:
//   PP_DAILY_LOOP_NODE_INSTALLATION the synthetic Node installation's host folder
//   PP_DAILY_LOOP_IMAGE, PP_DAILY_LOOP_NETWORK, PP_DAILY_LOOP_IMPORTER and
//   PP_DAILY_LOOP_IMPORT_VOLUME     what Step 2 starts the importer and the new container from
// and Step 12's restore from these:
//   PP_DAILY_LOOP_RESTORE           the second product container Step 12 starts
//   PP_DAILY_LOOP_RESTORE_VOLUME    the empty volume it restores the backup into
import { execFileSync, spawnSync } from "node:child_process";
import console from "node:console";
import { randomBytes } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
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
import { createRunner, teardown } from "./runner.mjs";
import { SPOOLMAN_ALIAS, SPOOLMAN_PORT, SPOOLMAN_URL, spoolmanRunArgs } from "./spoolman.mjs";
import { stopProcessesNaming } from "./teardown.mjs";
import {
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
const { image, simulator, spoolman } = names;

// A stop ends the running command, starts no other, and still runs the cleanup below.
// runner.mjs says how: a command that asks Docker to create something is left to finish.
const runner = createRunner({ cwd: root });
const { run } = runner;
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => runner.stop(signal));
// npm runs this with `exec`, so a signal sent to npm reaches this process. If the parent
// dies without passing one on (killed with SIGKILL, say), this process is reparented:
// stop as if it sent SIGTERM.
const parent = process.ppid;
setInterval(() => process.ppid !== parent && !runner.stopped && runner.stop("SIGTERM"), 1000).unref();

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
  for (let tries = 0; tries < 60 && !ready && !runner.stopped; tries++) {
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
 * The same checks for the Spoolman stand-in: it is up, it publishes on host loopback only,
 * and the product container reaches SPOOLMAN_URL. Returns the first problem, or null.
 */
async function checkSpoolman() {
  let ready = false;
  for (let tries = 0; tries < 60 && !ready && !runner.stopped; tries++) {
    if (inspect(spoolman, "{{.State.Running}}") !== "true") return "the Spoolman stand-in stopped";
    ready = /"control_port":\d+/.test(docker(["logs", spoolman]).stdout);
    if (!ready) await delay(500);
  }
  if (!ready) return "the Spoolman stand-in did not start in 30 s";
  const off = publishedOffLoopback(JSON.parse(inspect(spoolman, "{{json .NetworkSettings.Ports}}")));
  if (off.length > 0) return `${spoolman} publishes off host loopback: ${off.join(", ")}`;
  const address = inspect(spoolman, `{{(index .NetworkSettings.Networks "${name}").IPAddress}}`);
  if (!isPrivateIPv4(address)) return `${SPOOLMAN_ALIAS} has ${address || "no"} address, not RFC 1918`;
  const probe = docker(["exec", name, "bash", "-c", [
    `exec 3<>/dev/tcp/${SPOOLMAN_ALIAS}/${SPOOLMAN_PORT}`,
    String.raw`printf 'GET /api/v1/info HTTP/1.0\r\nHost: ${SPOOLMAN_ALIAS}\r\n\r\n' >&3`,
    "cat <&3",
  ].join("\n")]);
  const answer = parseAnswer(probe.stdout);
  if (answer.status !== 200) {
    return `the container did not reach ${SPOOLMAN_URL}/api/v1/info: ${answer.status ?? probe.stderr.trim()}`;
  }
  console.log(`The container reached ${SPOOLMAN_URL} (${address}); its port is published on 127.0.0.1 only.`);
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
  if (code !== 0 || runner.stopped) return code || 1;
  return spawnSync(process.execPath, probe, { cwd: web, stdio: "inherit" }).status ?? 1;
}

/**
 * Writes the synthetic Node installation into `dir`, which is empty. Its files are made
 * readable to the image's user (uid 1000), which may not be the host's: the folder above
 * stays mode 0700, so no other host user reaches them.
 */
async function writeNodeInstallation(dir) {
  const code = await ensureBetterSqlite();
  if (code !== 0 || runner.stopped) return code || 1;
  const args = ["run", "fixture:synthetic", "--", "--seed", String(SYNTHETIC_SEED), dir];
  const written = await run("npm", args, { cwd: resolve(root, "web"), group: true });
  if (written !== 0 || runner.stopped) return written || 1;
  const open = (path) => {
    const stat = statSync(path);
    chmodSync(path, stat.mode | (stat.isDirectory() ? 0o055 : 0o044));
    if (stat.isDirectory()) for (const entry of readdirSync(path)) open(join(path, entry));
  };
  open(dir);
  return 0;
}

async function suite(keyFile, nodeDir, browserDir) {
  const tests = ["steps", "simulator", "spoolman", "runner", "installation", "teardown"].map((test) =>
    resolve(here, `${test}.test.mjs`),
  );
  const steps = [
    // Exits when the tests end, so a listener a test leaves open fails the test, not the run.
    // In its own group, like the browser tests: a stop reaches each test file's process.
    [null, () => run(process.execPath, ["--test", "--test-force-exit", ...tests], { group: true })],
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
    [
      "docker",
      spoolmanRunArgs({
        container: spoolman,
        network: name,
        image: nodeImage(),
        user: `${process.getuid()}:${process.getgid()}`,
      }),
    ],
    ["docker", ["volume", "create", names.volume]],
    // Step 2 imports into this one; the import accepts only an empty folder.
    ["docker", ["volume", "create", names.importVolume]],
    // Step 12 restores the backup on a second container on this one.
    ["docker", ["volume", "create", names.restoreVolume]],
    ["docker", productRunArgs(names)],
  ];
  for (const [command, args] of steps) {
    // Each `docker` command here except the build asks Docker to create something.
    const settle = command === "docker" && args[0] !== "build";
    const code = command ? await run(command, args, { settle }) : await args();
    if (code !== 0 || runner.stopped) return code || 1;
  }
  const problem = (await checkSimulator(keyFile)) ?? (runner.stopped ? null : await checkSpoolman());
  if (problem || runner.stopped) {
    if (problem) console.error(`Harness: ${problem}.`);
    return 1;
  }
  // Playwright's own CLI, not `npx playwright`: npx runs it under a shell that does not
  // pass a signal on, so a stopped run would leave the browser tests running. In its own
  // group, so a stop reaches its worker and the docker commands the tests run.
  const playwright = resolve(root, "web/node_modules/@playwright/test/cli.js");
  return run(process.execPath, [playwright, "test", "--config", "test/daily-loop/playwright.config.ts"], {
    cwd: resolve(root, "web"),
    group: true,
    env: {
      ...process.env,
      // Chromium's profile and Playwright's downloads and output go in the run's folder,
      // which the cleanup removes, rather than the host's temporary folder or the checkout.
      TMPDIR: browserDir,
      PP_DAILY_LOOP_OUTPUT_DIR: join(browserDir, "results"),
      PP_DAILY_LOOP_CONTAINER: name,
      PP_DAILY_LOOP_COMMIT: commit,
      PP_DAILY_LOOP_SIMULATOR: simulator,
      PP_DAILY_LOOP_PRINTER_URL: PRINTER_URL,
      PP_DAILY_LOOP_PRINTER_KEY_FILE: keyFile,
      PP_DAILY_LOOP_SPOOLMAN: spoolman,
      PP_DAILY_LOOP_SPOOLMAN_URL: SPOOLMAN_URL,
      PP_DAILY_LOOP_NODE_INSTALLATION: nodeDir,
      PP_DAILY_LOOP_IMAGE: image,
      PP_DAILY_LOOP_NETWORK: names.network,
      PP_DAILY_LOOP_IMPORTER: names.importer,
      PP_DAILY_LOOP_IMPORT_VOLUME: names.importVolume,
      PP_DAILY_LOOP_RESTORE: names.restore,
      PP_DAILY_LOOP_RESTORE_VOLUME: names.restoreVolume,
    },
  });
}

let code = 1;
// The run's folder, new and 0700: the synthetic Node installation, which Step 2 imports, and
// the browser's files and Playwright's output, downloads included.
const runRoot = mkdtempSync(join(tmpdir(), `${name}-`));
const browserDir = join(runRoot, "browser");
try {
  mkdirSync(browserDir);
  // A run that stops before the tests must not leave an older run's score in place.
  rmSync(resolve(root, "web/test-results/daily-loop/daily-loop-score.json"), { force: true });
  // The printer's API key, new for each run; only the simulator and the tests read it.
  code = await withKeyFile(`${name}-`, (keyFile) => suite(keyFile, join(runRoot, "node"), browserDir));
  if (runner.stopped) code ||= 1;
} finally {
  // Every process the tests started has exited: runner.mjs waits for each command's whole
  // group. Chromium has a process group of its own; its profile path names the browser folder.
  const running = await stopProcessesNaming(browserDir).then(
    (pids) => pids.map((pid) => `process ${pid}`),
    (error) => [error.message],
  );
  // Only what this run created, by its unique name. After a stop, a `docker` command the
  // tests started can still create its container after an empty listing (runner.mjs).
  const left = await teardown(names, {
    folder: runRoot,
    removeFolder: (dir) => rmSync(dir, { recursive: true, force: true }),
    exists: existsSync,
    confirm: runner.stopped,
  });
  left.push(...runner.stuck, ...running);
  if (left.length > 0) {
    console.error(`Not removed, check by hand: ${left.join("; ")}`);
    code ||= 1;
  } else {
    const { importer, restore, importVolume, restoreVolume } = names;
    console.log(
      `Removed containers ${name}, ${importer}, ${restore}, ${simulator} and ${spoolman}, volumes ${name}, ${importVolume} and ${restoreVolume}, network ${name}, image tag ${image}, and the run's folder.`,
    );
  }
}
process.exit(code);
