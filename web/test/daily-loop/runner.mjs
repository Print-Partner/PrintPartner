// How run.mjs runs the commands of one daily-loop run, how a stop ends them, and how it
// removes what the run created (GRE-1887 review). A stop must never let cleanup report
// success while Docker is still creating something:
//
// - A `settle` command, one that asks Docker to create something, is left to finish. A
//   killed `docker run` client does not stop the daemon from creating the container, so
//   killing it would let cleanup list nothing and then the container appear. It runs in a
//   process group of its own, so a terminal's Ctrl-C does not reach it either. If it has
//   not finished 60 s after a stop, it is killed.
// - Any other command is signalled; with `group`, its whole process group, because
//   `npm run` does not pass a signal on to the script it started.
// - After a stop, cleanup also waits for two empty listings in a row, because a `docker`
//   command the tests started dies with them and its container can still appear.
import { spawn, spawnSync } from "node:child_process";
import process from "node:process";
import { clearTimeout, setTimeout } from "node:timers";
import { setTimeout as delay } from "node:timers/promises";
import { cleanupCommands, leftoverLists } from "./installation.mjs";

/** How long a settling command may run after a stop before it is killed. */
export const SETTLE_MS = 60_000;

/**
 * Runs one command at a time with this terminal's output. `stop(signal)` ends the running
 * command as above; `stopped` says whether a stop came.
 */
export function createRunner({ cwd, settleMs = SETTLE_MS }) {
  let child = null;
  let stopped = false;
  const signal = (target, name) => {
    try {
      if (target.group) process.kill(-target.pid, name);
      else target.kill(name);
    } catch {
      // It has already exited.
    }
  };
  const stop = (name) => {
    stopped = true;
    if (!child || child.exitCode !== null || child.signalCode !== null) return;
    if (!child.settle) return signal(child, name);
    const settling = child;
    settling.watchdog ??= setTimeout(() => signal(settling, "SIGKILL"), settleMs);
  };
  /** Resolves to the command's exit code. */
  const run = (command, args, { group = false, settle = false, ...options } = {}) =>
    new Promise((done) => {
      child = spawn(command, args, { stdio: "inherit", cwd, detached: group || settle, ...options });
      Object.assign(child, { group: group || settle, settle });
      const finish = (code) => {
        clearTimeout(child.watchdog);
        done(code);
      };
      child.on("error", () => finish(1));
      child.on("close", (code) => finish(code ?? 1));
    });
  return {
    run,
    stop,
    get stopped() {
      return stopped;
    },
  };
}

const dockerSync = (args, options) => spawnSync("docker", args, options);

/** How many times teardown removes and lists again, 2 s apart. */
export const TEARDOWN_PASSES = 4;

/**
 * Removes what the run named `names.container` created, and `folder`, then lists what is
 * left, up to TEARDOWN_PASSES times, 2 s apart. It stops at an empty listing; with
 * `confirm`, only at the second empty listing in a row. Resolves to what is left to check
 * by hand; empty means clean. With `confirm`, a single empty listing at the last pass is
 * not clean: each list is returned, marked as not confirmed.
 */
export async function teardown(names, { folder, removeFolder, exists, confirm, docker = dockerSync, wait = delay }) {
  const lists = leftoverLists(names.container);
  const needed = confirm ? 2 : 1;
  let left = [];
  let clean = 0;
  for (let pass = 0; pass < TEARDOWN_PASSES && clean < needed; pass++) {
    if (pass > 0) await wait(2000);
    for (const args of cleanupCommands(names)) docker(args, { stdio: "ignore" });
    removeFolder(folder);
    left = lists
      .filter((list) => {
        const listed = docker(list, { encoding: "utf8" });
        return listed.status !== 0 || listed.stdout.trim() !== "";
      })
      .map((list) => `docker ${list.join(" ")}`);
    if (exists(folder)) left.push(`ls ${folder}`);
    clean = left.length === 0 ? clean + 1 : 0;
  }
  if (clean >= needed || left.length > 0) return left;
  // After a stop, one empty listing proves nothing: a create can still finish after it.
  return [...lists.map((list) => `docker ${list.join(" ")}`), `ls ${folder}`].map(
    (check) => `${check} (empty only at the last of ${TEARDOWN_PASSES} passes, so not confirmed)`,
  );
}
