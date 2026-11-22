import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import process from "node:process";
import { test } from "node:test";
import { setTimeout } from "node:timers";
import { setTimeout as delay } from "node:timers/promises";
import { URL } from "node:url";
import { DEAF, detached, groupWithMember } from "./teardown.fixture.mjs";
import { drainGroup, groupAlive, processesNaming, removeUntilQuiet, stopProcessesNaming } from "./teardown.mjs";

// A test that fails must not hang the run, so each has a timeout and ends its fixtures
// (teardown.fixture.mjs) when it ends.
const PROCESS_TEST = { timeout: 20_000 };
const markOf = () => `pp-daily-loop-teardown-${randomBytes(4).toString("hex")}`;
/** @type {(t: import("node:test").TestContext, fixture: { end: () => void }) => void} */
const endAfter = (t, fixture) => t.after(fixture.end);

test("draining a group waits for a member that outlives the leader", PROCESS_TEST, async (t) => {
  const fixture = await groupWithMember(`setTimeout(() => process.exit(0), 800);`, markOf());
  endAfter(t, fixture);
  assert.equal(groupAlive(fixture.pgid), true, "the member is still in the group after the leader exited");
  const started = Date.now();
  const result = await drainGroup(fixture.pgid, { grace: 10_000 });
  assert.deepEqual(result, { killed: false, stuck: false });
  assert.ok(Date.now() - started >= 500, "it waited for the member");
  assert.equal(groupAlive(fixture.pgid), false);
});

test("draining a group kills a member that ignores SIGTERM once the grace period is over", PROCESS_TEST, async (t) => {
  const fixture = await groupWithMember(DEAF, markOf());
  endAfter(t, fixture);
  process.kill(-fixture.pgid, "SIGTERM");
  await delay(100);
  assert.equal(groupAlive(fixture.pgid), true, "SIGTERM alone does not end it");
  assert.deepEqual(await drainGroup(fixture.pgid, { grace: 300 }), { killed: true, stuck: false });
  assert.equal(groupAlive(fixture.pgid), false);
});

test("processes that name a folder are found, asked to stop, then killed", PROCESS_TEST, async (t) => {
  const folder = `/tmp/${markOf()}`;
  // Like Chromium under Playwright: its own group, the run's folder on its command line.
  const fixture = await detached(DEAF, [`--user-data-dir=${folder}/profile`]);
  endAfter(t, fixture);
  assert.deepEqual(processesNaming(folder), [fixture.pid]);
  assert.deepEqual(processesNaming(`${folder}-other`), []);
  const closed = once(fixture.child, "exit");
  assert.deepEqual(await stopProcessesNaming(folder, { grace: 300 }), []);
  const [, signal] = await closed;
  assert.equal(signal, "SIGKILL");
  assert.deepEqual(processesNaming(folder), []);
});

// The fixtures above ignore SIGTERM and leave their starter's process group, so a stop that
// ends the test runner before a test's cleanup runs must still not leave them behind.
for (const signal of ["SIGTERM", "SIGKILL"]) {
  test(`fixtures exit when the process that started them ends by ${signal}`, PROCESS_TEST, async (t) => {
    const mark = markOf();
    const module = new URL("./teardown.fixture.mjs", import.meta.url).href;
    // A stand-in for a test file's process: it starts both kinds of fixture, then waits.
    const runner = spawn(
      process.execPath,
      [
        "--input-type=module",
        "-e",
        `const { DEAF, detached, groupWithMember } = await import(${JSON.stringify(module)});
         await groupWithMember(DEAF, ${JSON.stringify(mark)});
         await detached(DEAF, [${JSON.stringify(mark)}]);
         console.log("ready");
         setInterval(() => {}, 1000);`,
      ],
      { detached: true, stdio: ["ignore", "pipe", "inherit"] },
    );
    const { pid } = runner;
    assert.ok(pid, "the runner started");
    t.after(() => {
      try {
        process.kill(-pid, "SIGKILL");
      } catch {
        // It has already gone.
      }
    });
    const [line] = await once(runner.stdout, "data");
    assert.match(String(line), /ready/);
    // The runner's own source names the mark too.
    const fixtures = () => processesNaming(mark).filter((other) => other !== pid);
    assert.equal(fixtures().length, 2, "the group member and the detached process run");
    const exited = once(runner, "exit");
    process.kill(-pid, signal);
    await exited;
    for (let tries = 0; tries < 50 && fixtures().length > 0; tries++) await delay(100);
    assert.deepEqual(processesNaming(mark), [], `no fixture outlives a runner ended by ${signal}`);
  });
}

/**
 * An inventory where a container appears `after` ms after the first removal: a slow create.
 * @param {number} after
 */
function inventory(after) {
  const present = new Set(["container"]);
  let removals = 0;
  return {
    remove: () => {
      present.clear();
      if (++removals === 1) setTimeout(() => present.add("late container"), after);
    },
    leftovers: () => [...present],
    removals: () => removals,
  };
}

test("removal catches what appears after the first quiet wait, before the second look", async () => {
  // As a create the daemon carries out 3.2 s after the removal, with the 3 s default: the
  // first look finds nothing, the second finds it.
  const late = inventory(320);
  const left = await removeUntilQuiet({ remove: late.remove, leftovers: late.leftovers, quiet: 300 });
  assert.deepEqual(left, []);
  assert.equal(late.removals(), 2, "the late container was removed by a second pass");
});

test("removal catches what appears within the first quiet wait", async () => {
  const late = inventory(20);
  assert.deepEqual(await removeUntilQuiet({ remove: late.remove, leftovers: late.leftovers, quiet: 100 }), []);
  assert.equal(late.removals(), 2);
});

test("removal reports what is still there after its last pass", async () => {
  let removals = 0;
  const left = await removeUntilQuiet({
    remove: () => removals++,
    leftovers: () => ["docker ps -aq --filter name=pp-daily-loop-0a1b2c3d"],
    quiet: 10,
    passes: 3,
  });
  assert.deepEqual(left, ["docker ps -aq --filter name=pp-daily-loop-0a1b2c3d"]);
  assert.equal(removals, 3);
});
