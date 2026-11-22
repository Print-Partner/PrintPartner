import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import process from "node:process";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { runNames } from "./installation.mjs";
import { createRunner, teardown } from "./runner.mjs";

const RUN = "pp-daily-loop-0a1b2c3d";
const STAND_IN = `${RUN}-spoolman`;

// A `docker` on PATH that keeps its containers in a state file. `run --name N` takes
// 300 ms to create N. A daemon goes on creating a container when its client is killed, so a
// killed `run` hands N to a detached process that creates it FAKE_DOCKER_LATE_MS later.
// `run` for a name starting with `hang` never answers.
const FAKE_DOCKER = String.raw`#!/usr/bin/env node
const { appendFileSync, readFileSync, writeFileSync } = require("node:fs");
const { spawn } = require("node:child_process");
const state = process.env.FAKE_DOCKER_STATE;
const listed = () => readFileSync(state, "utf8").split("\n").filter(Boolean);
const args = process.argv.slice(2);
const after = (flag) => args[args.indexOf(flag) + 1];
if (args[0] === "register") {
  setTimeout(() => appendFileSync(state, args[1] + "\n"), Number(process.env.FAKE_DOCKER_LATE_MS));
} else if (args[0] === "run") {
  const name = after("--name");
  if (name.startsWith("hang")) setInterval(() => {}, 1000);
  const created = setTimeout(() => {
    appendFileSync(state, name + "\n");
    process.exit(0);
  }, 300);
  process.on("SIGTERM", () => {
    clearTimeout(created);
    spawn(process.execPath, [__filename, "register", name], { detached: true, stdio: "ignore", env: process.env }).unref();
    process.exit(143);
  });
} else if (args[0] === "rm") {
  writeFileSync(state, listed().filter((name) => !args.includes(name)).map((name) => name + "\n").join(""));
} else if (args[0] === "ps") {
  const filter = after("--filter").replace("name=", "");
  for (const name of listed()) if (name.includes(filter)) console.log(name);
}
`;

/** A fake Docker for one test: its `env`, its containers, and `docker` for teardown. */
function fakeDocker(t, lateMs = 1000) {
  const dir = mkdtempSync(join(tmpdir(), "pp-daily-loop-runner-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const bin = join(dir, "bin");
  writeFileSync(join(dir, "state"), "");
  spawnSync("mkdir", [bin]);
  writeFileSync(join(bin, "docker"), FAKE_DOCKER);
  chmodSync(join(bin, "docker"), 0o755);
  const env = { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, FAKE_DOCKER_STATE: join(dir, "state"), FAKE_DOCKER_LATE_MS: String(lateMs) };
  return {
    env,
    folder: join(dir, "node"),
    containers: () => readFileSync(join(dir, "state"), "utf8").split("\n").filter(Boolean),
    docker: (args, options) => spawnSync("docker", args, { ...options, env }),
  };
}

const removeRun = (fake, confirm, waitMs = 1500) =>
  teardown(runNames(RUN), {
    folder: fake.folder,
    removeFolder: (dir) => rmSync(dir, { recursive: true, force: true }),
    exists: existsSync,
    confirm,
    docker: fake.docker,
    wait: () => delay(waitMs),
  });

test("a stop during a docker create lets it finish, so the cleanup removes the container", async (t) => {
  const fake = fakeDocker(t);
  const runner = createRunner({ cwd: tmpdir() });
  const creating = runner.run("docker", ["run", "-d", "--name", STAND_IN], { settle: true, stdio: "ignore", env: fake.env });
  await delay(100);
  runner.stop("SIGTERM");
  assert.equal(runner.stopped, true);
  // The create was not signalled: it ends on its own, with the container made.
  assert.equal(await creating, 0);
  assert.deepEqual(fake.containers(), [STAND_IN]);
  assert.deepEqual(await removeRun(fake, true), []);
  await delay(700);
  assert.deepEqual(fake.containers(), [], "nothing appears after the cleanup");
});

test("a create killed by a stop still makes its container; one empty listing would miss it", async (t) => {
  // The bug this guards against: signal the `docker run` client and trust the first
  // empty listing. The container appears after the cleanup said it was done: here 3 s
  // after the kill, well after the first listing.
  const fake = fakeDocker(t, 3000);
  const runner = createRunner({ cwd: tmpdir() });
  const creating = runner.run("docker", ["run", "-d", "--name", STAND_IN], { stdio: "ignore", env: fake.env });
  await delay(100);
  runner.stop("SIGTERM");
  assert.notEqual(await creating, 0);
  assert.deepEqual(await removeRun(fake, false), []);
  assert.deepEqual(fake.containers(), [], "the cleanup found nothing");
  await delay(3500);
  assert.deepEqual(fake.containers(), [STAND_IN], "the orphan");
  // After a stop, the cleanup waits for two empty listings in a row and so removes it.
  assert.deepEqual(await removeRun(fake, true), []);
  assert.deepEqual(fake.containers(), []);
});

test("after a stop, the cleanup removes a container that appears after an empty listing", async (t) => {
  // A docker command the tests started dies with them, like a killed create.
  const fake = fakeDocker(t);
  const runner = createRunner({ cwd: tmpdir() });
  const creating = runner.run("docker", ["run", "-d", "--name", RUN], { stdio: "ignore", env: fake.env });
  await delay(100);
  runner.stop("SIGTERM");
  await creating;
  assert.deepEqual(fake.containers(), [], "not made yet when the cleanup starts");
  assert.deepEqual(await removeRun(fake, true), []);
  await delay(1500);
  assert.deepEqual(fake.containers(), []);
});

test("a create that never ends is killed once the settle time after a stop runs out", async (t) => {
  const fake = fakeDocker(t);
  const runner = createRunner({ cwd: tmpdir(), settleMs: 200 });
  const started = Date.now();
  const creating = runner.run("docker", ["run", "-d", "--name", "hang"], { settle: true, stdio: "ignore", env: fake.env });
  await delay(100);
  runner.stop("SIGTERM");
  assert.notEqual(await creating, 0);
  assert.ok(Date.now() - started < 5000, "it did not wait for the create");
});

test("without a stop, one empty listing is enough", async (t) => {
  const fake = fakeDocker(t);
  let waits = 0;
  const left = await teardown(runNames(RUN), {
    folder: fake.folder,
    removeFolder: () => {},
    exists: () => false,
    confirm: false,
    docker: fake.docker,
    wait: async () => void waits++,
  });
  assert.deepEqual(left, []);
  assert.equal(waits, 0);
});
