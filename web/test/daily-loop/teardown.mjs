// The processes a daily-loop run must see end before it removes what the run created
// (GRE-1888). A test blocked in a synchronous `docker run` keeps going after the Playwright
// CLI or `node --test` exits, and Chromium, which Playwright starts in a process group of its
// own, can still hold files in the run's folder. runner.mjs waits for each command's whole
// process group with `drainGroup`; run.mjs then stops every process that names the browser
// folder with `stopProcessesNaming`, before runner.mjs's `teardown` removes Docker resources.
import { spawnSync } from "node:child_process";
import process from "node:process";
import { setTimeout as delay } from "node:timers/promises";

/**
 * True while any process, a zombie included, is in process group `pgid`.
 * @param {number} pgid
 */
export function groupAlive(pgid) {
  try {
    process.kill(-pgid, 0);
    return true;
  } catch (error) {
    return error instanceof Error && "code" in error && error.code === "EPERM";
  }
}

/**
 * Waits until process group `pgid` is empty. After `grace` ms it sends SIGKILL to what is
 * left; if the group is still there `grace` ms after that, it stops waiting. Resolves to
 * `{ killed, stuck }`: whether it had to kill, and whether the group outlived the kill.
 * @param {number} pgid
 * @param {{ grace?: number, poll?: number }} [options]
 */
export async function drainGroup(pgid, { grace = 30_000, poll = 100 } = {}) {
  const start = Date.now();
  let killed = false;
  while (groupAlive(pgid)) {
    const waited = Date.now() - start;
    if (waited >= 2 * grace) return { killed, stuck: true };
    if (!killed && waited >= grace) {
      killed = true;
      try {
        process.kill(-pgid, "SIGKILL");
      } catch {
        // The group emptied in between.
      }
    }
    await delay(poll);
  }
  return { killed, stuck: false };
}

/**
 * The IDs of the processes, other than this one, whose command line contains `text`. A
 * listing that failed or was cut short throws: it must not read as "none left".
 * @param {string} text
 * @returns {number[]}
 */
export function processesNaming(text) {
  const listed = spawnSync("ps", ["-eww", "-o", "pid=,args="], { encoding: "utf8", maxBuffer: 64 * 1024 ** 2 });
  if (listed.error || listed.status !== 0) {
    throw new Error(`ps could not list processes: ${listed.error?.message ?? listed.stderr.trim()}`);
  }
  return listed.stdout.split("\n").flatMap((line) => {
    const [, pid, args] = /^\s*(\d+)\s(.*)$/.exec(line) ?? [];
    return args?.includes(text) && Number(pid) !== process.pid ? [Number(pid)] : [];
  });
}

/**
 * Sends SIGTERM to every process whose command line contains `text`, then SIGKILL to those
 * still there after `grace` ms. Resolves to the IDs still there after both; rejects when the
 * processes cannot be listed.
 * @param {string} text
 * @param {{ grace?: number, poll?: number }} [options]
 */
export async function stopProcessesNaming(text, { grace = 10_000, poll = 100 } = {}) {
  /** @type {(pids: number[], name: NodeJS.Signals) => void} */
  const signal = (pids, name) => {
    for (const pid of pids) {
      try {
        process.kill(pid, name);
      } catch {
        // It has already exited.
      }
    }
  };
  signal(processesNaming(text), "SIGTERM");
  for (const deadline = Date.now() + grace; Date.now() < deadline; await delay(poll)) {
    if (processesNaming(text).length === 0) return [];
  }
  signal(processesNaming(text), "SIGKILL");
  await delay(poll);
  return processesNaming(text);
}
