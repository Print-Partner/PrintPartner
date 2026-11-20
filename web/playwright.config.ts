import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import process from "node:process";
import { defineConfig } from "@playwright/test";
import { browserExecutable } from "./apps/web/test/browser/browserExecutable.mjs";

// Boots the real Rust pp-server through `verify.py launch --binary`, so the specs drive the
// same gateway and Node child the desktop app ships. Build it first:
//   VITE_PRINT_PARTNER_DESKTOP=1 npm run build
//   python3 ../rust/scripts/build-desktop-manifest.py --web . --node "$(command -v node)" \
//     --output ../rust/bundle-manifest.json --commit "$(git rev-parse HEAD)"
//   cargo build --manifest-path ../rust/Cargo.toml --locked -p pp-server
const repoRoot = resolve(import.meta.dirname, "..");
const binary = process.env.PP_SERVER_BIN ?? join(repoRoot, "rust/target/debug/pp-server");
// The config also loads in each worker; workers inherit this from the runner.
process.env.PP_E2E_STATE ??= mkdtempSync(join(tmpdir(), "pp-e2e-"));
const state = process.env.PP_E2E_STATE;
const evidence = process.env.PP_EVIDENCE
  ? join(process.env.PP_EVIDENCE, "playwright")
  : join(import.meta.dirname, "test-results");

export default defineConfig({
  testDir: "test/e2e",
  // One server and one data dir: specs share state, so they run one at a time.
  workers: 1,
  fullyParallel: false,
  retries: 1,
  timeout: 90_000,
  outputDir: join(evidence, "results"),
  globalSetup: "./test/e2e/global-setup.ts",
  reporter: [
    ["list"],
    ["html", { outputFolder: join(evidence, "report"), open: "never" }],
    ["json", { outputFile: join(evidence, "report.json") }],
  ],
  expect: {
    toHaveScreenshot: { maxDiffPixelRatio: 0.01, animations: "disabled", caret: "hide" },
  },
  use: {
    baseURL: process.env.PP_SERVER_URL,
    storageState: join(state, "storage.json"),
    viewport: { width: 1280, height: 900 },
    trace: "on-first-retry",
    // CI sets PP_E2E_SCREENSHOTS=on so the e2e receipt carries a screenshot of every test.
    screenshot: process.env.PP_E2E_SCREENSHOTS === "on" ? "on" : "only-on-failure",
    launchOptions: { executablePath: browserExecutable() },
  },
  webServer: {
    command: `python3 scripts/verify-rust-gateway/verify.py launch --binary "${binary}" --credential-file "${join(state, "bootstrap.json")}"`,
    cwd: repoRoot,
    wait: { stdout: /PP_SERVER_URL=(?<PP_SERVER_URL>http:\/\/127\.0\.0\.1:\d+)/ },
    stdout: "pipe",
    timeout: 120_000,
    // verify.py stops pp-server on SIGTERM and checks that its shutdown was clean.
    gracefulShutdown: { signal: "SIGTERM", timeout: 60_000 },
  },
});
