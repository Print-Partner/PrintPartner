import { resolve } from "node:path";
import process from "node:process";
import { defineConfig } from "@playwright/test";
import { browserExecutable } from "../../apps/web/test/browser/browserExecutable.mjs";

// The daily loop (docs/product/daily-loop.md): twelve tests, one per step, in order, on
// one installation. `npm run test:daily-loop` (run.mjs) builds and starts the Rust-only
// image and sets the two variables below; this config does not start anything.
for (const name of ["PP_DAILY_LOOP_CONTAINER", "PP_DAILY_LOOP_COMMIT"]) {
  if (!process.env[name]) throw new Error(`${name} is not set. Run npm run test:daily-loop.`);
}

export default defineConfig({
  testDir: ".",
  testMatch: "daily-loop.spec.ts",
  // One installation that every step changes: one worker, file order, and no second try.
  workers: 1,
  fullyParallel: false,
  retries: 0,
  // The first test waits for the first start; an assertion waits 5 s, so a marked step
  // fails at its assertion well inside this.
  timeout: 240_000,
  expect: { timeout: 5_000 },
  outputDir: resolve(import.meta.dirname, "../../test-results/daily-loop/results"),
  reporter: [["list"], ["./score.ts"]],
  use: {
    viewport: { width: 1280, height: 900 },
    actionTimeout: 5_000,
    navigationTimeout: 15_000,
    // Traces and screenshots could hold the sign-in link.
    trace: "off",
    screenshot: "off",
    launchOptions: { executablePath: browserExecutable() },
  },
});
