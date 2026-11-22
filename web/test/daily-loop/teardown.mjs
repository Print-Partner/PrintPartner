// How run.mjs makes sure nothing of a stopped run is still working when it removes what the run
// created (GRE-1888). A test blocked in a synchronous `docker run` keeps going after the
// Playwright CLI exits, and Chromium, which Playwright starts in a process group of its own,
// can still hold files in the run's folder. So run.mjs waits for the command's whole process
// group and for every process that names the run's folder, then removes things until a look
// taken a while after a removal finds nothing.
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

/**
 * Calls `remove`, then looks with `leftovers` every `quiet` ms. It is done when `looks` looks
 * in a row find nothing; anything a look finds starts the next pass with another `remove`, up
 * to `passes` passes. A docker request that a stopped command sent can still be carried out
 * after the command has gone, so an empty list taken straight after a removal proves nothing.
 * With the defaults it catches what appears up to 6 s after a removal; run.mjs calls it only
 * once no process of the run is left to send a request.
 * Resolves to what the last look found: empty when everything is gone.
 * @param {{ remove: () => void, leftovers: () => string[], quiet?: number, looks?: number, passes?: number }} options
 * @returns {Promise<string[]>}
 */
export async function removeUntilQuiet({ remove, leftovers, quiet = 3000, looks = 2, passes = 5 }) {
  /** @type {string[]} */
  let left = [];
  for (let pass = 0; pass < passes; pass++) {
    remove();
    let empty = 0;
    while (empty < looks) {
      await delay(quiet);
      left = leftovers();
      if (left.length > 0) break;
      empty++;
    }
    if (empty === looks) return [];
  }
  return left;
}
