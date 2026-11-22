// How run.mjs runs the commands of one daily-loop run, how a stop ends them, and how it
// removes what the run created (GRE-1887 review, GRE-1888). A stop must never let cleanup
// report success while Docker is still creating something:
//
// - A `settle` command, one that asks Docker to create something, is left to finish. A
//   killed `docker run` client does not stop the daemon from creating the container, so
//   killing it would let cleanup list nothing and then the container appear. It runs in a
//   process group of its own, so a terminal's Ctrl-C does not reach it either. If it has
//   not finished 60 s after a stop, it is killed.
// - Any other command is signalled; with `group`, its whole process group, because
//   `npm run` does not pass a signal on to the script it started.
// - A command with a process group of its own counts as finished only once every process
//   in the group has exited (teardown.mjs): the Playwright CLI and `node --test` can exit
//   while a test file's process, or a `docker run` it started, still runs.
// - After a stop, cleanup also waits for three empty listings in a row, 3 s apart, because
//   a `docker` command the tests started dies with them and its container can still appear.
import { spawn, spawnSync } from "node:child_process";
import process from "node:process";
import { clearTimeout, setTimeout } from "node:timers";
import { setTimeout as delay } from "node:timers/promises";
import { cleanupCommands, leftoverLists } from "./installation.mjs";
import { drainGroup } from "./teardown.mjs";

/** How long a settling command may run after a stop before it is killed. */
export const SETTLE_MS = 60_000;

/**
 * @typedef {object} Running
 * @property {import("node:child_process").ChildProcess} child
 * @property {boolean} group whether it leads a process group of its own
 * @property {boolean} settle whether a stop lets it finish
 * @property {ReturnType<typeof setTimeout>} [watchdog] the kill a stop scheduled for it
 * @property {boolean} [done] whether it and its whole group have exited
 */

/**
 * Runs one command at a time with this terminal's output. `stop(signal)` ends the running
 * command as above; `stopped` says whether a stop came, and `stuck` lists the process groups
 * that were still there after SIGKILL.
 * @param {{ cwd: string, settleMs?: number, grace?: number }} options
 */
export function createRunner({ cwd, settleMs = SETTLE_MS, grace }) {
  /** @type {Running | null} */
  let current = null;
  let stopped = false;
  /** @type {string[]} */
  const stuck = [];
  /** @type {(target: Running, name: NodeJS.Signals) => void} */
  const signal = ({ child, group }, name) => {
    try {
      if (group && child.pid !== undefined) process.kill(-child.pid, name);
      else child.kill(name);
    } catch {
      // It has already exited.
    }
  };
  /** @param {NodeJS.Signals} name */
  const stop = (name) => {
    stopped = true;
    const running = current;
    // A group outlives its leader, so it is signalled until every process in it has exited.
    if (!running || running.done) return;
    if (!running.settle) return signal(running, name);
    running.watchdog ??= setTimeout(() => signal(running, "SIGKILL"), settleMs);
  };
  /**
   * Resolves to the command's exit code, once every process of its group has exited.
   * @param {string} command
   * @param {string[]} args
   * @param {import("node:child_process").SpawnOptions & { group?: boolean, settle?: boolean }} [options]
   * @returns {Promise<number>}
   */
  const run = (command, args, { group = false, settle = false, ...options } = {}) =>
    new Promise((done) => {
      const child = spawn(command, args, { stdio: "inherit", cwd, detached: group || settle, ...options });
      /** @type {Running} */
      const running = { child, group: group || settle, settle };
      current = running;
      let finishing = false;
      /** @param {number} code */
      const finish = async (code) => {
        // A spawn that fails can report both `error` and `close`.
        if (finishing) return;
        finishing = true;
        const { pid } = child;
        if (running.group && pid !== undefined && (await drainGroup(pid, { grace })).stuck) {
          stuck.push(`process group ${pid}`);
        }
        running.done = true;
        clearTimeout(running.watchdog);
        done(code);
      };
      child.on("error", () => void finish(1));
      child.on("close", (code) => void finish(code ?? 1));
    });
  return {
    run,
    stop,
    get stopped() {
      return stopped;
    },
    get stuck() {
      return [...stuck];
    },
  };
}

/** @type {(args: string[], options: import("node:child_process").SpawnSyncOptions) => import("node:child_process").SpawnSyncReturns<string | Buffer>} */
const dockerSync = (args, options) => spawnSync("docker", args, options);

/** How many times teardown removes and lists again, PASS_WAIT_MS apart. */
export const TEARDOWN_PASSES = 6;
/** The wait between two passes of teardown. */
export const PASS_WAIT_MS = 3000;
/** After a stop, how many empty listings in a row teardown needs: 0, 3 and 6 s after removal. */
export const CONFIRM_LISTINGS = 3;

/**
 * Removes what the run named `names.container` created, and `folder`, then lists what is
 * left, up to TEARDOWN_PASSES times, PASS_WAIT_MS apart. It stops at an empty listing; with
 * `confirm`, only at the CONFIRM_LISTINGS-th empty listing in a row, so it catches what a
 * create finishes up to 6 s after an empty listing. Resolves to what is left to check by
 * hand; empty means clean. With `confirm`, empty listings that run out of passes before
 * CONFIRM_LISTINGS are not clean: each list is returned, marked as not confirmed.
 * @param {ReturnType<typeof import("./installation.mjs").runNames>} names
 * @param {{
 *   folder: string,
 *   removeFolder: (folder: string) => void,
 *   exists: (folder: string) => boolean,
 *   confirm: boolean,
 *   docker?: (args: string[], options: object) => { status: number | null, stdout: string | Buffer },
 *   wait?: (ms: number) => Promise<unknown>,
 * }} options
 * @returns {Promise<string[]>}
 */
export async function teardown(names, { folder, removeFolder, exists, confirm, docker = dockerSync, wait = delay }) {
  const lists = leftoverLists(names.container);
  const needed = confirm ? CONFIRM_LISTINGS : 1;
  /** @type {string[]} */
  let left = [];
  let clean = 0;
  for (let pass = 0; pass < TEARDOWN_PASSES && clean < needed; pass++) {
    if (pass > 0) await wait(PASS_WAIT_MS);
    for (const args of cleanupCommands(names)) docker(args, { stdio: "ignore" });
    removeFolder(folder);
    left = lists
      .filter((list) => {
        const listed = docker(list, { encoding: "utf8" });
        return listed.status !== 0 || String(listed.stdout).trim() !== "";
      })
      .map((list) => `docker ${list.join(" ")}`);
    if (exists(folder)) left.push(`ls ${folder}`);
    clean = left.length === 0 ? clean + 1 : 0;
  }
  if (clean >= needed || left.length > 0) return left;
  // After a stop, one empty listing proves nothing: a create can still finish after it.
  return [...lists.map((list) => `docker ${list.join(" ")}`), `ls ${folder}`].map(
    (check) => `${check} (empty only at the last ${clean} of ${TEARDOWN_PASSES} passes, so not confirmed)`,
  );
}
