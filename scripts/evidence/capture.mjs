// Signs in to a Rust-only core, runs the named setup steps through the Rust API,
// then screenshots each path from the replacement UI. run.py starts the core and the
// UI server and passes everything in one JSON argument; see scripts/evidence/README.md.
//
// The bootstrap credential is read from CREDENTIAL_FILE and the file is removed at
// once. Nothing here prints the bootstrap URL, cookies or the account password: every
// failure goes through safety.mjs, which prints only fixed messages.
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { readFileSync, renameSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { CaptureError, guarded, installReport, makeApi } from "./safety.mjs";
import { STEPS } from "./steps.mjs";

installReport();

const config = JSON.parse(process.argv[2]);
const { root, coreOrigin, uiOrigin, shots, steps, outDir, fullPage, display } = config;
const { chromium } = createRequire(resolve(root, "web/package.json"))("playwright-core");
const resolver = pathToFileURL(resolve(root, "web/apps/web/test/browser/browserExecutable.mjs"));
const { browserExecutable } = await import(resolver.href);

const credentialFile = process.env.CREDENTIAL_FILE;
const credential = readFileSync(credentialFile, "utf8");
rmSync(credentialFile);
let bootstrapUrl;
try {
  bootstrapUrl = JSON.parse(credential).bootstrap_url;
} catch {
  throw new CaptureError("the credential file is not JSON");
}
// An invalid URL's error carries the input, so compare without printing it.
if (typeof bootstrapUrl !== "string" || !URL.canParse(bootstrapUrl) || new URL(bootstrapUrl).origin !== coreOrigin) {
  throw new CaptureError("the bootstrap link is not on the core origin");
}

const VIEWPORT = { width: 1440, height: 900 };
const READY_MS = 20_000;

function log(message) {
  console.log(`capture: ${message}`);
}

/** Waits until the screen has data: no busy region and no "Loading" text left. */
async function settle(page, path) {
  await page.waitForLoadState("networkidle", { timeout: READY_MS }).catch(() => {});
  try {
    await page.waitForFunction(
      () => {
        const main = document.querySelector("main") ?? document.body;
        // The frame polls /health; a shot under the Offline banner is not evidence.
        if (!document.querySelector('.pp-connection[data-status="online"]')) return false;
        if (!main || !main.querySelector("h1")) return false;
        if (main.querySelector('[aria-busy="true"]')) return false;
        return !/(^|\n)\s*Loading\b/.test(main.innerText);
      },
      null,
      { timeout: READY_MS },
    );
  } catch {
    const status = await page.locator(".pp-connection").getAttribute("data-status").catch(() => null);
    const health = await page.request.get(new URL("/health", page.url()).href).catch(() => null);
    const probe = health ? `${health.status()} ${health.headers()["content-type"]}` : "transport error";
    throw new CaptureError(
      `${path} was not ready after ${READY_MS / 1000}s (connection ${status}, UI /health ${probe})`,
    );
  }
  if (await page.getByRole("heading", { name: "Not found", exact: true }).count()) {
    throw new CaptureError(`${path} has no screen in the replacement UI`);
  }
  await page.waitForTimeout(300);
}

async function main() {
  const launch = display
    ? {
        headless: false,
        executablePath: browserExecutable(),
        env: { ...process.env, DISPLAY: display.name },
        args: ["--window-position=0,0", `--window-size=${display.width},${display.height}`],
      }
    : { headless: true, executablePath: browserExecutable() };
  const browser = await chromium.launch(launch);
  try {
    const context = await browser.newContext({ viewport: display ? null : VIEWPORT });
    const page = await context.newPage();

    await guarded(page.goto(bootstrapUrl), "opening the bootstrap link failed");
    if (new URL(page.url()).pathname === "/__desktop/bootstrap") throw new CaptureError("bootstrap did not redirect");
    await guarded(page.goto("about:blank"), "leaving the bootstrap page failed");

    // The Rust routes need a Rust session as well as the desktop cookie. The data is
    // disposable, so a fresh account with a random password is registered every run.
    const api = makeApi(page.request, coreOrigin);
    await api("POST", "/auth/register", {
      email: "evidence@example.com",
      display_name: "Evidence",
      password: randomBytes(24).toString("base64url"),
    });
    log("signed in a throwaway account");

    for (const step of steps) {
      log(`step ${step.name}:${step.arg} ${await STEPS[step.name](api, step.arg)}`);
    }

    for (const { path, file: name } of shots) {
      await guarded(page.goto(new URL(path, uiOrigin).href), `opening ${path} failed`);
      await settle(page, path);
      if (config.reviewPlan && /^\/builds\/\d+\/plan\/?$/.test(path)) {
        await guarded(page.getByRole("button", { name: "Review and accept", exact: true }).click(), "opening Accept preview failed");
        await guarded(page.getByRole("dialog").waitFor(), "Accept gate did not open");
        await guarded(page.getByRole("dialog").getByText("Checking what Accept would change.").waitFor({ state: "hidden" }), "Accept preview did not settle");
      }
      const file = join(outDir, name);
      if (display) {
        // The installed evidence tool captures the display; move its file into outDir.
        let written;
        try {
          written = execFileSync(display.evidenceBin, ["shot", name.replace(/\.png$/, "")], {
            cwd: outDir,
            env: { ...process.env, EVIDENCE_DISPLAY: display.name },
            encoding: "utf8",
          })
            .trim()
            .split("\n")
            .pop();
        } catch {
          throw new CaptureError(`evidence shot failed for ${path}`);
        }
        if (resolve(outDir, written) !== resolve(file)) renameSync(resolve(outDir, written), file);
      } else {
        await guarded(page.screenshot({ path: file, fullPage }), `screenshot of ${path} failed`);
      }
      log(`shot ${path} -> ${file}`);
    }
  } finally {
    await browser.close();
  }
}

await main();
