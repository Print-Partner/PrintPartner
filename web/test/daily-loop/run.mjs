#!/usr/bin/env node
// Runs the daily-loop suite (docs/product/daily-loop.md) the same way locally and in
// daily-loop.yml:
//
//   npm --prefix web run test:daily-loop
//
// It checks the test titles against the steps table, builds the Rust-only image from
// Dockerfile.rust at HEAD, starts it on a new volume with the run command from
// docs/docker.md, runs the twelve Playwright tests, and removes the container, the
// volume and the image tag it created, whether the run passed, failed or was stopped.
import { execFileSync, spawn, spawnSync } from "node:child_process";
import console from "node:console";
import { randomBytes } from "node:crypto";
import { rmSync } from "node:fs";
import { resolve } from "node:path";
import process from "node:process";

const here = import.meta.dirname;
const root = resolve(here, "../../..");
const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
// Unique per run: another run on this machine keeps its own container, volume and tag.
const name = `pp-daily-loop-${randomBytes(4).toString("hex")}`;
const image = `${name}:test`;

// A run that stops before the tests must not leave an older run's score in place.
rmSync(resolve(root, "web/test-results/daily-loop/daily-loop-score.json"), { force: true });

let child;
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => child?.kill(signal));

/** Runs a command with this terminal's output; resolves to its exit code. */
const run = (command, args, options = {}) =>
  new Promise((done) => {
    child = spawn(command, args, { stdio: "inherit", cwd: root, ...options });
    child.on("error", () => done(1));
    child.on("close", (code) => done(code ?? 1));
  });

async function suite() {
  const steps = [
    [process.execPath, ["--test", resolve(here, "steps.test.mjs")]],
    ["docker", ["build", "-f", "Dockerfile.rust", "--build-arg", `PP_COMMIT=${commit}`, "-t", image, "."]],
    ["docker", ["volume", "create", name]],
    // The run command from docs/docker.md, except that Docker picks the host port, so
    // two runs on one machine do not collide; the tests read it back with `docker port`.
    // `--restart unless-stopped` matters: until GRE-1453, sign-out stops pp-server, and
    // Docker starts it again.
    [
      "docker",
      [
        ...["run", "-d", "--name", name, "-p", "127.0.0.1::8080", "--restart", "unless-stopped"],
        ...["--read-only", "--tmpfs", "/tmp:size=1g,mode=1777"],
        ...["--cap-drop", "ALL", "--security-opt", "no-new-privileges"],
        ...["-v", `${name}:/data`, image],
      ],
    ],
  ];
  for (const [command, args] of steps) {
    const code = await run(command, args);
    if (code !== 0) return code;
  }
  return run("npx", ["playwright", "test", "--config", "test/daily-loop/playwright.config.ts"], {
    cwd: resolve(root, "web"),
    env: { ...process.env, PP_DAILY_LOOP_CONTAINER: name, PP_DAILY_LOOP_COMMIT: commit },
  });
}

let code = 1;
try {
  code = await suite();
} finally {
  // Only what this run created, by its unique name.
  for (const args of [["rm", "-f", name], ["volume", "rm", "-f", name], ["rmi", image]]) {
    spawnSync("docker", args, { stdio: "ignore" });
  }
  const left = ["ps -aq --filter", "volume ls -q --filter", "images -q --filter"]
    .map((list, index) => `${list} ${index === 2 ? "reference" : "name"}=${name}`)
    .filter((list) => {
      const listed = spawnSync("docker", list.split(" "), { encoding: "utf8" });
      return listed.status !== 0 || listed.stdout.trim() !== "";
    });
  if (left.length > 0) {
    console.error(`Not removed, check by hand: ${left.map((list) => `docker ${list}`).join("; ")}`);
    code ||= 1;
  } else {
    console.log(`Removed container ${name}, volume ${name} and image tag ${image}.`);
  }
}
process.exit(code);
