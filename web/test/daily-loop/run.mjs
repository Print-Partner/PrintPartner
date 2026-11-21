#!/usr/bin/env node
// Runs the daily-loop suite (docs/product/daily-loop.md) the same way locally and in
// daily-loop.yml:
//
//   npm --prefix web run test:daily-loop
//
// It checks the test titles against the steps table, builds the Rust-only image from
// Dockerfile.rust at HEAD, and creates a Docker network for the run. On that network it
// starts the Moonraker simulator as `pp-sim` (simulator.mjs) and the image on a new volume
// with the run command from docs/docker.md. It checks that the container reaches the
// simulator and that both publish ports on host loopback only, then runs the twelve
// Playwright tests. It removes the two containers, the volume, the image tag, the network
// and the simulator's API key file it created, whether the run passed, failed or was stopped.
//
// The tests read the simulator from these variables:
//   PP_DAILY_LOOP_SIMULATOR        its container; `docker port` names its control port
//   PP_DAILY_LOOP_PRINTER_URL      the printer's address inside the product container
//   PP_DAILY_LOOP_PRINTER_KEY_FILE a host file holding the printer's API key
import { execFileSync, spawn, spawnSync } from "node:child_process";
import console from "node:console";
import { randomBytes } from "node:crypto";
import { readFileSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
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

const here = import.meta.dirname;
const root = resolve(here, "../../..");
const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
// Unique per run: another run on this machine keeps its own containers, volume, network and tag.
const name = `pp-daily-loop-${randomBytes(4).toString("hex")}`;
const image = `${name}:test`;
const simulator = `${name}-sim`;

// A stop ends the running command, starts no other, and still runs the cleanup below.
let child;
let stopped = false;
const stop = (signal) => {
  stopped = true;
  child?.kill(signal);
};
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => stop(signal));
// npm runs this with `exec`, so a signal sent to npm reaches this process. If the parent
// dies without passing one on (killed with SIGKILL, say), this process is reparented:
// stop as if it sent SIGTERM.
const parent = process.ppid;
setInterval(() => process.ppid !== parent && !stopped && stop("SIGTERM"), 1000).unref();

/** Runs a command with this terminal's output; resolves to its exit code. */
const run = (command, args, options = {}) =>
  new Promise((done) => {
    child = spawn(command, args, { stdio: "inherit", cwd: root, ...options });
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

async function suite(keyFile) {
  const steps = [
    [process.execPath, ["--test", resolve(here, "steps.test.mjs"), resolve(here, "simulator.test.mjs")]],
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
    ["docker", ["volume", "create", name]],
    // The run command from docs/docker.md, on the run's network, except that Docker picks
    // the host port, so two runs on one machine do not collide; the tests read it back
    // with `docker port`.
    [
      "docker",
      [
        ...["run", "-d", "--name", name, "--network", name, "-p", "127.0.0.1::8080"],
        ...["--restart", "unless-stopped", "--read-only", "--tmpfs", "/tmp:size=1g,mode=1777"],
        ...["--cap-drop", "ALL", "--security-opt", "no-new-privileges"],
        ...["-v", `${name}:/data`, image],
      ],
    ],
  ];
  for (const [command, args] of steps) {
    const code = await run(command, args);
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
    },
  });
}

let code = 1;
try {
  // A run that stops before the tests must not leave an older run's score in place.
  rmSync(resolve(root, "web/test-results/daily-loop/daily-loop-score.json"), { force: true });
  // The printer's API key, new for each run; only the simulator and the tests read it.
  code = await withKeyFile(`${name}-`, suite);
  if (stopped) code ||= 1;
} finally {
  // Only what this run created, by its unique name. The network goes after its containers.
  for (const args of [
    ["rm", "-f", name, simulator],
    ["network", "rm", name],
    ["volume", "rm", "-f", name],
    ["rmi", image],
  ]) {
    spawnSync("docker", args, { stdio: "ignore" });
  }
  // A name filter matches part of a name, so the first list also finds the simulator.
  const left = ["ps -aq", "volume ls -q", "network ls -q", "images -q"]
    .map((list) => `${list} --filter ${list.startsWith("images") ? "reference" : "name"}=${name}`)
    .filter((list) => {
      const listed = spawnSync("docker", list.split(" "), { encoding: "utf8" });
      return listed.status !== 0 || listed.stdout.trim() !== "";
    });
  if (left.length > 0) {
    console.error(`Not removed, check by hand: ${left.map((list) => `docker ${list}`).join("; ")}`);
    code ||= 1;
  } else {
    console.log(`Removed containers ${name} and ${simulator}, volume and network ${name}, and image tag ${image}.`);
  }
}
process.exit(code);
