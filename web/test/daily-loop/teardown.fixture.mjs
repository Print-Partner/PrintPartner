// The processes teardown.test.mjs stops. Some are detached and ignore SIGTERM, as a test
// blocked in `docker run` or Chromium can, so nothing the test runner does on its way out
// reaches them. Each one checks every 200 ms that the process that started it is still
// there and exits when it is not, so however that process ends, even by SIGKILL before a
// test's cleanup runs, the fixture follows it. None lives longer than 60 s.
import { spawn } from "node:child_process";
import { once } from "node:events";
import process from "node:process";

/**
 * Node source each fixture runs first: exit once process `starter` has gone, or after 60 s.
 * @param {number} starter
 */
const tiedTo = (starter) =>
  `setInterval(() => { try { process.kill(${starter}, 0); } catch { process.exit(0); } }, 200);
   setTimeout(() => process.exit(0), 60_000);`;

/** Node source for a fixture that ignores SIGTERM: it runs until SIGKILL or its starter goes. */
export const DEAF = `process.on("SIGTERM", () => {});`;

/**
 * Ends a fixture's process group now, as a test's cleanup does.
 * @param {number} pgid
 */
const killGroup = (pgid) => () => {
  try {
    process.kill(-pgid, "SIGKILL");
  } catch {
    // It has already gone.
  }
};

/**
 * Starts a process group whose leader starts a member running `member` (Node source) in the
 * same group and exits at once, as the Playwright CLI exits while its worker still runs. The
 * member's command line ends with `mark`. Resolves once the leader has exited and the member
 * runs, to the group's ID and `end`, which kills the group.
 * @param {string} member
 * @param {string} mark
 */
export async function groupWithMember(member, mark) {
  const source = `${tiedTo(process.pid)} ${member} console.log("running");`;
  const leader = spawn(
    process.execPath,
    [
      "-e",
      `require("node:child_process")
         .spawn(process.execPath, ["-e", ${JSON.stringify(source)}, "--", ${JSON.stringify(mark)}], {
           stdio: ["ignore", "inherit", "ignore"],
         })
         .unref();`,
    ],
    { detached: true, stdio: ["ignore", "pipe", "ignore"] },
  );
  // The member shares the leader's stdout; it writes one line once it runs.
  const exited = once(leader, "exit");
  const [line] = await once(leader.stdout, "data");
  if (!String(line).includes("running")) throw new Error(`the member wrote ${line}`);
  await exited;
  leader.stdout.destroy();
  const pgid = leader.pid;
  if (pgid === undefined) throw new Error("the leader did not start");
  return { pgid, end: killGroup(pgid) };
}

/**
 * Starts `body` (Node source) in a process group of its own, as Playwright starts Chromium,
 * with `args` on its command line. Resolves once it runs, to the process and `end`.
 * @param {string} body
 * @param {string[]} args
 */
export async function detached(body, args) {
  const source = `${tiedTo(process.pid)} ${body} console.log("running");`;
  const child = spawn(process.execPath, ["-e", source, "--", ...args], {
    detached: true,
    stdio: ["ignore", "pipe", "ignore"],
  });
  await once(child.stdout, "data");
  child.stdout.destroy();
  const { pid } = child;
  if (pid === undefined) throw new Error("the fixture did not start");
  return { child, pid, end: killGroup(pid) };
}
