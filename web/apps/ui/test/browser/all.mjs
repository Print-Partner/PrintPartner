// Runs every browser check even when an earlier one skips or fails, then exits with the
// worst status: a failure's code beats 77 (an engine was missing, so SKIP), which beats 0.
import { spawnSync } from "node:child_process";
import process from "node:process";
import { URL, fileURLToPath } from "node:url";

const checks = ["font-requests.browser.mjs", "frame.browser.mjs", "builds.browser.mjs"];
const statuses = checks.map((check) => {
  const script = fileURLToPath(new URL(check, import.meta.url));
  const { status, signal } = spawnSync(process.execPath, [script], { stdio: "inherit" });
  return status ?? (signal ? 1 : 0);
});
process.exitCode = statuses.find((s) => s !== 0 && s !== 77) ?? (statuses.includes(77) ? 77 : 0);
