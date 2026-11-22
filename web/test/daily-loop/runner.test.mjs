import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import process from "node:process";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { runNames } from "./installation.mjs";
import { createRunner, TEARDOWN_PASSES, teardown } from "./runner.mjs";

const RUN = "pp-daily-loop-0a1b2c3d";
const STAND_IN = `${RUN}-spoolman`;

// A `docker` on PATH that keeps its containers in a state file. `run --name N` writes
// `started` once it can take a signal, then creates N when the test writes `release`: the
// daemon finishing. A daemon goes on creating a container when its client is killed, so a
// killed `run` hands N to a detached process that still creates it on `release`. `run` for
// a name starting with `hang` never creates anything.
const FAKE_DOCKER = String.raw`#!/usr/bin/env node
const { appendFileSync, existsSync, readFileSync, writeFileSync } = require("node:fs");
const { spawn } = require("node:child_process");
const { join } = require("node:path");
const dir = process.env.FAKE_DOCKER_DIR;
const state = join(dir, "state");
const listed = () => readFileSync(state, "utf8").split("\n").filter(Boolean);
const args = process.argv.slice(2);
const after = (flag) => args[args.indexOf(flag) + 1];
const onRelease = (create) => {
  const poll = setInterval(() => {
    if (!existsSync(join(dir, "release"))) return;
    clearInterval(poll);
    create();
  }, 20);
  return poll;
};
if (args[0] === "register") {
  onRelease(() => appendFileSync(state, args[1] + "\n"));
} else if (args[0] === "run") {
  const name = after("--name");
  const poll = name.startsWith("hang")
    ? setInterval(() => {}, 1000)
    : onRelease(() => {
        appendFileSync(state, name + "\n");
        process.exit(0);
      });
  process.on("SIGTERM", () => {
    clearInterval(poll);
    if (!name.startsWith("hang")) {
      spawn(process.execPath, [__filename, "register", name], { detached: true, stdio: "ignore", env: process.env }).unref();
    }
    process.exit(143);
  });
  writeFileSync(join(dir, "started"), "");
} else if (args[0] === "rm") {
  writeFileSync(state, listed().filter((name) => !args.includes(name)).map((name) => name + "\n").join(""));
} else if (args[0] === "ps") {
  const filter = after("--filter").replace("name=", "");
  for (const name of listed()) if (name.includes(filter)) console.log(name);
}
`;

/** Waits up to 10 s for `check()` to be true. */
async function until(check, what) {
  for (const started = Date.now(); !check(); await delay(20)) {
    if (Date.now() - started > 10_000) throw new Error(`timed out waiting for ${what}`);
  }
}

/** A fake Docker for one test: its `env`, its containers, and `docker` for teardown. */
function fakeDocker(t) {
  const dir = mkdtempSync(join(tmpdir(), "pp-daily-loop-runner-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const bin = join(dir, "bin");
  writeFileSync(join(dir, "state"), "");
  mkdirSync(bin);
  writeFileSync(join(bin, "docker"), FAKE_DOCKER);
  chmodSync(join(bin, "docker"), 0o755);
  const env = { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, FAKE_DOCKER_DIR: dir };
  const containers = () => readFileSync(join(dir, "state"), "utf8").split("\n").filter(Boolean);
  return {
    env,
    folder: join(dir, "node"),
    containers,
    docker: (args, options) => spawnSync("docker", args, { ...options, env }),
    /** Resolves once `run` can take a signal. */
    started: () => until(() => existsSync(join(dir, "started")), "docker run to start"),
    /** The daemon finishes the create; resolves once the container exists. */
    release: async () => {
      writeFileSync(join(dir, "release"), "");
      await until(() => containers().length > 0, "the container");
    },
  };
}

const removeRun = (fake, confirm, wait = () => delay(50)) =>
  teardown(runNames(RUN), {
    folder: fake.folder,
    removeFolder: (dir) => rmSync(dir, { recursive: true, force: true }),
    exists: existsSync,
    confirm,
    docker: fake.docker,
    wait,
  });

/** Starts `docker run --name <name>` on `fake` and stops the runner while it creates. */
async function stopDuringCreate(fake, name, { settle = false, settleMs } = {}) {
  const runner = createRunner({ cwd: tmpdir(), settleMs });
  const creating = runner.run("docker", ["run", "-d", "--name", name], { settle, stdio: "ignore", env: fake.env });
  await fake.started();
  runner.stop("SIGTERM");
  assert.equal(runner.stopped, true);
  return creating;
}

test("a stop during a docker create lets it finish, so the cleanup removes the container", async (t) => {
  const fake = fakeDocker(t);
  const creating = stopDuringCreate(fake, STAND_IN, { settle: true });
  // The create was not signalled: it ends when the daemon has made the container.
  await fake.release();
  assert.equal(await creating, 0);
  assert.deepEqual(fake.containers(), [STAND_IN]);
  assert.deepEqual(await removeRun(fake, true), []);
  assert.deepEqual(fake.containers(), []);
});

test("a create killed by a stop still makes its container; one empty listing would miss it", async (t) => {
  // The bug this guards against: signal the `docker run` client and trust the first empty
  // listing. The daemon makes the container after the cleanup said it was done.
  const fake = fakeDocker(t);
  assert.notEqual(await stopDuringCreate(fake, STAND_IN), 0);
  assert.deepEqual(await removeRun(fake, false), []);
  await fake.release();
  assert.deepEqual(fake.containers(), [STAND_IN], "the orphan");
});

test("after a stop, the cleanup removes a container that appears after an empty listing", async (t) => {
  // A docker command the tests started dies with them, like a killed create. Here the
  // daemon makes its container while the cleanup waits between two listings.
  const fake = fakeDocker(t);
  assert.notEqual(await stopDuringCreate(fake, RUN), 0);
  assert.deepEqual(fake.containers(), [], "not made yet when the cleanup starts");
  assert.deepEqual(await removeRun(fake, true, fake.release), []);
  assert.deepEqual(fake.containers(), []);
});

test("a create that never ends is killed once the settle time after a stop runs out", async (t) => {
  const fake = fakeDocker(t);
  const started = Date.now();
  assert.notEqual(await stopDuringCreate(fake, "hang", { settle: true, settleMs: 200 }), 0);
  assert.ok(Date.now() - started < 9000, "it did not wait for the create");
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

test("after a stop, an empty listing only at the last pass is reported as not confirmed", async () => {
  // The run's container shows on every pass but the last: one empty listing, no second.
  const pass = { n: 0 };
  const docker = (args) => {
    if (args[0] === "rm") pass.n += 1;
    const present = args[0] === "ps" && pass.n < TEARDOWN_PASSES;
    return { status: 0, stdout: present ? `${RUN}\n` : "" };
  };
  const options = { folder: "/nonexistent", removeFolder: () => {}, exists: () => false, docker, wait: async () => {} };
  const left = await teardown(runNames(RUN), { ...options, confirm: true });
  assert.equal(pass.n, TEARDOWN_PASSES);
  assert.notDeepEqual(left, []);
  assert.ok(left.every((check) => check.endsWith("so not confirmed)")), left.join("\n"));
  assert.ok(left.some((check) => check.startsWith(`docker ps -aq --filter name=${RUN} `)));
  // Without a stop the same last empty listing is enough.
  pass.n = 0;
  assert.deepEqual(await teardown(runNames(RUN), { ...options, confirm: false }), []);
});
