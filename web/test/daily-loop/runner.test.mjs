import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import process from "node:process";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { runNames } from "./installation.mjs";
import { CONFIRM_LISTINGS, createRunner, PASS_WAIT_MS, TEARDOWN_PASSES, teardown } from "./runner.mjs";
import { DEAF, tiedTo } from "./teardown.fixture.mjs";
import { groupAlive, processesNaming } from "./teardown.mjs";

const RUN = "pp-daily-loop-0a1b2c3d";
const STAND_IN = `${RUN}-spoolman`;

// A `docker` on PATH that keeps its containers in a state file. `run --name N` writes
// `started` once it can take a signal, then creates N when the test writes `release`: the
// daemon finishing. A daemon goes on creating a container when its client is killed, so a
// killed `run` hands N to a separate daemon that still creates it on `release`. The test
// owns and reaps that daemon. `run` for a name starting with `hang` never creates anything.
const FAKE_DOCKER = String.raw`#!/usr/bin/env node
const { appendFileSync, existsSync, readFileSync, writeFileSync } = require("node:fs");
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
  const owner = Number(args[1]);
  const request = join(dir, "register");
  const poll = setInterval(() => {
    if (process.ppid !== owner) process.exit(0);
    if (!existsSync(request) || !existsSync(join(dir, "release"))) return;
    appendFileSync(state, readFileSync(request));
    clearInterval(poll);
  }, 20);
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
      writeFileSync(join(dir, "register"), name + "\n");
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
  const bin = join(dir, "bin");
  const cli = join(bin, "docker");
  let daemon;
  let reaped;
  t.after(async () => {
    try {
      daemon?.kill();
      await reaped;
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
  writeFileSync(join(dir, "state"), "");
  mkdirSync(bin);
  writeFileSync(cli, FAKE_DOCKER);
  chmodSync(cli, 0o755);
  const env = { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, FAKE_DOCKER_DIR: dir };
  daemon = spawn(process.execPath, [cli, "register", String(process.pid)], { stdio: "ignore", env });
  reaped = once(daemon, "close");
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

const removeRun = (fake, wait = () => delay(50)) =>
  teardown(runNames(RUN), {
    folders: [fake.folder],
    removeFolder: (dir) => rmSync(dir, { recursive: true, force: true }),
    exists: existsSync,
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
  assert.deepEqual(await removeRun(fake), []);
  assert.deepEqual(fake.containers(), []);
});

test("a create killed by a stop still makes its container after its client has gone", async (t) => {
  // Why the cleanup never trusts one empty listing: the daemon makes the container after
  // its `docker run` client was killed.
  const fake = fakeDocker(t);
  assert.notEqual(await stopDuringCreate(fake, STAND_IN), 0);
  assert.deepEqual(fake.containers(), [], "nothing yet when the client has gone");
  await fake.release();
  assert.deepEqual(fake.containers(), [STAND_IN], "the daemon made it anyway");
  assert.deepEqual(await removeRun(fake), []);
  assert.deepEqual(fake.containers(), []);
});

test("after a stop, the cleanup removes a container that appears after an empty listing", async (t) => {
  // A docker command the tests started dies with them, like a killed create. Here the
  // daemon makes its container while the cleanup waits between two listings.
  const fake = fakeDocker(t);
  assert.notEqual(await stopDuringCreate(fake, RUN), 0);
  assert.deepEqual(fake.containers(), [], "not made yet when the cleanup starts");
  // The daemon finishes the create during the first wait.
  let released = false;
  const releaseOnce = async () => {
    if (released) return;
    released = true;
    await fake.release();
  };
  assert.deepEqual(await removeRun(fake, releaseOnce), []);
  assert.deepEqual(fake.containers(), []);
});

test("a create that never ends is killed once the settle time after a stop runs out", async (t) => {
  const fake = fakeDocker(t);
  const started = Date.now();
  assert.notEqual(await stopDuringCreate(fake, "hang", { settle: true, settleMs: 200 }), 0);
  assert.ok(Date.now() - started < 9000, "it did not wait for the create");
});

/**
 * A Docker that keeps containers and volumes in memory: `rm -f` and `volume rm -f` remove
 * what they name, and `ps` and `volume ls` list what is there. `listings` records how many
 * containers each `ps` found.
 */
function memoryDocker() {
  const containers = new Set();
  const volumes = new Set();
  const listings = [];
  const docker = (args) => {
    if (args[0] === "rm") for (const name of args.slice(2)) containers.delete(name);
    if (args[0] === "volume" && args[1] === "rm") for (const name of args.slice(3)) volumes.delete(name);
    if (args[0] === "ps") {
      listings.push(containers.size);
      return { status: 0, stdout: [...containers].join("\n") };
    }
    if (args[0] === "volume" && args[1] === "ls") return { status: 0, stdout: [...volumes].join("\n") };
    return { status: 0, stdout: "" };
  };
  return { containers, volumes, listings, docker };
}

test("a failed run, with no stop, removes a restore container and volume that appear after two empty listings", async () => {
  // Step 12's `docker run` times out in a failed test: its client is killed, the test
  // fails, and nothing stops the run. The daemon finishes the create, container and volume,
  // 3.2 s after the cleanup's first empty listing, during its second wait.
  const runner = createRunner({ cwd: tmpdir() });
  assert.equal(await runner.run(process.execPath, ["-e", "process.exit(1)"], { stdio: "ignore" }), 1);
  assert.equal(runner.stopped, false, "a failed run, not a stopped one");
  const fake = memoryDocker();
  const waits = [];
  const wait = async (ms) => {
    waits.push(ms);
    if (waits.length !== 2) return;
    fake.containers.add(`${RUN}-restore`);
    fake.volumes.add(`${RUN}-restored`);
  };
  const options = { folders: [], removeFolder: () => {}, exists: () => false, docker: fake.docker, wait };
  assert.deepEqual(await teardown(runNames(RUN), options), []);
  assert.deepEqual([...fake.containers], [], "the late restore container was removed");
  assert.deepEqual([...fake.volumes], [], "the late restore volume was removed");
  // It appeared after two empty listings, where a cleanup that trusts its first empty
  // listing, or two, would already have stopped.
  assert.deepEqual(fake.listings, [0, 0, 0]);
  assert.deepEqual(waits, [PASS_WAIT_MS, PASS_WAIT_MS]);
});

test("an empty listing only at the last pass is reported as not confirmed", async () => {
  // The run's container shows on every pass but the last: one empty listing, no second.
  const pass = { n: 0 };
  const docker = (args) => {
    if (args[0] === "rm") pass.n += 1;
    const present = args[0] === "ps" && pass.n < TEARDOWN_PASSES;
    return { status: 0, stdout: present ? `${RUN}\n` : "" };
  };
  const options = { folders: [], removeFolder: () => {}, exists: () => false, docker, wait: async () => {} };
  const left = await teardown(runNames(RUN), options);
  assert.equal(pass.n, TEARDOWN_PASSES);
  assert.notDeepEqual(left, []);
  assert.ok(left.every((check) => check.endsWith("so not confirmed)")), left.join("\n"));
  assert.ok(left.some((check) => check.startsWith(`docker ps -aq --filter name=${RUN} `)));
});

test("a folder removal failure leaves the other folder and Docker cleanup running", async () => {
  const fake = memoryDocker();
  fake.containers.add(RUN);
  const folders = ["/tmp/data", "/tmp/browser"];
  const present = new Set(folders);
  let first = true;
  const removeFolder = (folder) => {
    if (folder === folders[0] && first) {
      first = false;
      throw new Error("folder was busy");
    }
    present.delete(folder);
  };
  const wait = async () => {
    assert.equal(fake.containers.size, 0);
    assert.equal(present.has(folders[1]), false);
  };
  assert.deepEqual(await teardown(runNames(RUN), {
    folders,
    removeFolder,
    exists: (folder) => present.has(folder),
    docker: fake.docker,
    wait,
  }), []);
  assert.equal(present.size, 0);
  assert.deepEqual(fake.listings, [0, 0, 0, 0]);
});

test("the cleanup removes a container that appears after two empty listings", async () => {
  // A create the daemon finishes 3.2 s after the removal: the listings at 0 and 3 s are
  // empty. Two empty listings in a row would have called it clean there; the third pass, at
  // 6 s, removes it before its listing.
  const present = new Set();
  const listings = [];
  const docker = (args) => {
    if (args[0] === "rm") for (const name of args.slice(2)) present.delete(name);
    if (args[0] !== "ps") return { status: 0, stdout: "" };
    listings.push(present.size);
    return { status: 0, stdout: [...present].join("\n") };
  };
  const waits = [];
  const wait = async (ms) => {
    waits.push(ms);
    if (waits.length === 2) present.add(`${RUN}-restore`);
  };
  const options = { folders: [], removeFolder: () => {}, exists: () => false, docker, wait };
  assert.deepEqual(await teardown(runNames(RUN), options), []);
  assert.equal(present.size, 0, "the late restore container was removed");
  assert.deepEqual(listings, [0, 0, 0], "it appeared after the second empty listing");
  assert.equal(CONFIRM_LISTINGS, 3);
  assert.deepEqual(waits, [PASS_WAIT_MS, PASS_WAIT_MS]);
  assert.equal(PASS_WAIT_MS, 3000);
});

/**
 * Node source for a group leader that starts `member` (Node source, tied to this test's
 * process) in its own group with `mark` on its command line, then exits at once.
 */
const leaderOf = (member, mark) =>
  `require("node:child_process")
     .spawn(process.execPath, ["-e", ${JSON.stringify(`${tiedTo(process.pid)} ${member}`)}, "--", ${JSON.stringify(mark)}], { stdio: "ignore" })
     .unref();`;
const markOf = () => join(tmpdir(), `pp-daily-loop-runner-test-${process.pid}-${Date.now()}`);

test("a command with a group of its own counts as done only once its whole group has exited", { timeout: 20_000 }, async () => {
  const mark = markOf();
  const runner = createRunner({ cwd: tmpdir() });
  const started = Date.now();
  const code = await runner.run(process.execPath, ["-e", leaderOf("setTimeout(() => process.exit(0), 800);", mark)], {
    group: true,
    stdio: "ignore",
  });
  assert.equal(code, 0);
  assert.ok(Date.now() - started >= 700, "it waited for the member, not only the leader");
  assert.deepEqual(processesNaming(mark), []);
  assert.deepEqual(runner.stuck, []);
});

test("a stop reaches a group member after its leader has exited, and kills one that ignores SIGTERM", { timeout: 20_000 }, async () => {
  const mark = markOf();
  const runner = createRunner({ cwd: tmpdir(), grace: 300 });
  const running = runner.run(process.execPath, ["-e", leaderOf(DEAF, mark)], { group: true, stdio: "ignore" });
  await until(() => processesNaming(mark).length === 1, "the member to start");
  const [member] = processesNaming(mark);
  const pgid = Number(spawnSync("ps", ["-o", "pgid=", "-p", String(member)], { encoding: "utf8" }).stdout.trim());
  runner.stop("SIGTERM");
  await running;
  assert.equal(groupAlive(pgid), false, "the member was killed once the grace period ran out");
  assert.deepEqual(processesNaming(mark), []);
  assert.deepEqual(runner.stuck, []);
});

test("a stop kills a group leader that ignores SIGTERM once the grace period is over", { timeout: 20_000 }, async () => {
  const mark = markOf();
  const runner = createRunner({ cwd: tmpdir(), grace: 300 });
  // The leader itself ignores SIGTERM and never exits on its own; its member does the same.
  const leader = `${leaderOf(DEAF, mark)} ${tiedTo(process.pid)} ${DEAF}`;
  const running = runner.run(process.execPath, ["-e", leader, "--", mark], { group: true, stdio: "ignore" });
  await until(() => processesNaming(mark).length === 2, "the leader and its member to start");
  const [pgid] = processesNaming(mark).map(
    (pid) => Number(spawnSync("ps", ["-o", "pgid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim()),
  );
  const stoppedAt = Date.now();
  runner.stop("SIGTERM");
  assert.equal(await running, 1, "it ended by SIGKILL, so with no exit code");
  const took = Date.now() - stoppedAt;
  assert.ok(took < 3000, `SIGKILL came ${took} ms after the stop, not after the leader's close`);
  assert.equal(groupAlive(pgid), false);
  assert.deepEqual(processesNaming(mark), []);
  assert.deepEqual(runner.stuck, []);
});
