import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import process from "node:process";
import { test } from "node:test";
import { setTimeout } from "node:timers";
import { setTimeout as delay } from "node:timers/promises";
import { drainGroup, groupAlive, processesNaming, removeUntilQuiet, stopProcessesNaming } from "./teardown.mjs";

// A test that fails must not leave its processes running: they would keep this file's test
// process alive, so the run would hang instead of reporting the failure.
const PROCESS_TEST = { timeout: 20_000 };
const killGroupAfter = (t, pgid) =>
  t.after(() => {
    try {
      process.kill(-pgid, "SIGKILL");
    } catch {
      // The group has already gone.
    }
  });

/**
 * Starts a process group whose leader starts `member` (Node source) in the same group and
 * exits at once, as the Playwright CLI exits while its worker still runs. Resolves to the
 * group's ID once the leader has exited and the member has started.
 */
async function groupWithMember(member) {
  const leader = spawn(
    process.execPath,
    [
      "-e",
      `require("node:child_process")
         .spawn(process.execPath, ["-e", ${JSON.stringify(member)}], { stdio: ["ignore", "inherit", "ignore"] })
         .unref();`,
    ],
    { detached: true, stdio: ["ignore", "pipe", "ignore"] },
  );
  // The leader exits without waiting for the member. The member shares the leader's stdout
  // and writes one line once it runs.
  const exited = once(leader, "exit");
  const [line] = await once(leader.stdout, "data");
  assert.match(String(line), /running/);
  await exited;
  leader.stdout.destroy();
  return leader.pid;
}

test("draining a group waits for a member that outlives the leader", PROCESS_TEST, async (t) => {
  const pgid = await groupWithMember(`console.log("running"); setTimeout(() => {}, 800);`);
  killGroupAfter(t, pgid);
  assert.equal(groupAlive(pgid), true, "the member is still in the group after the leader exited");
  const started = Date.now();
  const result = await drainGroup(pgid, { grace: 10_000 });
  assert.deepEqual(result, { killed: false, stuck: false });
  assert.ok(Date.now() - started >= 500, "it waited for the member");
  assert.equal(groupAlive(pgid), false);
});

test("draining a group kills a member that ignores SIGTERM once the grace period is over", PROCESS_TEST, async (t) => {
  const pgid = await groupWithMember(
    `process.on("SIGTERM", () => {}); console.log("running"); setInterval(() => {}, 1000);`,
  );
  killGroupAfter(t, pgid);
  process.kill(-pgid, "SIGTERM");
  await delay(100);
  assert.equal(groupAlive(pgid), true, "SIGTERM alone does not end it");
  assert.deepEqual(await drainGroup(pgid, { grace: 300 }), { killed: true, stuck: false });
  assert.equal(groupAlive(pgid), false);
});

test("processes that name a folder are found, asked to stop, then killed", PROCESS_TEST, async (t) => {
  const folder = `/tmp/pp-daily-loop-teardown-${randomBytes(4).toString("hex")}`;
  // Like Chromium under Playwright: its own group, the run's folder on its command line.
  const deaf = spawn(
    process.execPath,
    [
      "-e",
      `process.on("SIGTERM", () => {}); console.log("running"); setInterval(() => {}, 1000);`,
      "--",
      `--user-data-dir=${folder}/profile`,
    ],
    { detached: true, stdio: ["ignore", "pipe", "ignore"] },
  );
  killGroupAfter(t, deaf.pid);
  // Once it runs, so its SIGTERM handler is in place.
  await once(deaf.stdout, "data");
  assert.deepEqual(processesNaming(folder), [deaf.pid]);
  assert.deepEqual(processesNaming(`${folder}-other`), []);
  const closed = once(deaf, "close");
  assert.deepEqual(await stopProcessesNaming(folder, { grace: 300 }), []);
  const [, signal] = await closed;
  assert.equal(signal, "SIGKILL");
  assert.deepEqual(processesNaming(folder), []);
});

test("removal goes on until a look taken after a quiet wait finds nothing", async () => {
  // A launch that was in flight when the run stopped creates its container after the first
  // removal. A look straight after that removal finds nothing; the next one does.
  const present = new Set(["container"]);
  let removals = 0;
  const left = await removeUntilQuiet({
    remove: () => {
      present.clear();
      if (++removals === 1) setTimeout(() => present.add("late container"), 20);
    },
    leftovers: () => [...present],
    quiet: 100,
  });
  assert.deepEqual(left, []);
  assert.equal(removals, 2, "the late container was removed by a second pass");
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
