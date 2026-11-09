// Signs in to a Rust-only core, runs the named setup steps through the Rust API,
// then screenshots each path from the replacement UI. run.py starts the core and the
// UI server and passes everything in one JSON argument; see scripts/evidence/README.md.
//
// The bootstrap credential is read from CREDENTIAL_FILE and the file is removed at
// once. Nothing here prints the bootstrap URL, cookies or the account password.
import { execFileSync } from "node:child_process";
import { randomBytes, randomUUID } from "node:crypto";
import { readFileSync, renameSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const config = JSON.parse(process.argv[2]);
const { root, coreOrigin, uiOrigin, shots, steps, outDir, fullPage, display } = config;
const { chromium } = createRequire(resolve(root, "web/package.json"))("playwright-core");
const resolver = pathToFileURL(resolve(root, "web/apps/web/test/browser/browserExecutable.mjs"));
const { browserExecutable } = await import(resolver.href);

const credentialFile = process.env.CREDENTIAL_FILE;
const { bootstrap_url: bootstrapUrl } = JSON.parse(readFileSync(credentialFile, "utf8"));
rmSync(credentialFile);
if (new URL(bootstrapUrl).origin !== coreOrigin) throw new Error("bootstrap origin is not the core origin");

const VIEWPORT = { width: 1440, height: 900 };
const READY_MS = 20_000;

/** File name for a shot path: /builds/2/history -> builds-2-history.png. */
function shotName(path) {
  const slug = path.replace(/^\/+/, "").replace(/[^A-Za-z0-9]+/g, "-").replace(/^-|-$/g, "");
  return `${slug || "root"}.png`;
}

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
    const health = await page.request.get(new URL("/health", page.url()).href).catch((error) => error);
    const probe = health instanceof Error ? health.message : `${health.status()} ${health.headers()["content-type"]}`;
    throw new Error(
      `${path} was not ready after ${READY_MS / 1000}s (connection ${status}, UI /health ${probe})`,
    );
  }
  if (await page.getByRole("heading", { name: "Not found", exact: true }).count()) {
    throw new Error(`${path} has no screen in the replacement UI`);
  }
  await page.waitForTimeout(300);
}

function makeApi(request, origin) {
  return async (method, path, body, headers = {}) => {
    const response = await request.fetch(new URL(path, origin).href, {
      method,
      headers: { Origin: origin, ...(body === undefined ? {} : { "Content-Type": "application/json" }), ...headers },
      data: body === undefined ? undefined : JSON.stringify(body),
      maxRedirects: 0,
    });
    if (!response.ok()) {
      const detail = (await response.text()).slice(0, 300);
      throw new Error(`${method} ${path} answered ${response.status()}: ${detail}`);
    }
    const text = await response.text();
    return text ? JSON.parse(text) : null;
  };
}

const plan = (id) => `/api/v2/plans/${id}`;
const idempotent = () => ({ "Idempotency-Key": randomUUID() });

/** The Build's open draft, or null. */
async function openDraft(api, id) {
  const { drafts } = await api("GET", `${plan(id)}/drafts`);
  return drafts.find((draft) => draft.state === "open") ?? null;
}

/** Resolves every reconciliation conflict on the open draft by replacing the part. */
async function settleReconciliation(api, id) {
  const draft = await openDraft(api, id);
  if (!draft) return "no open draft";
  const workspace = await api("GET", `${plan(id)}/drafts/${draft.draft_id}`);
  if (workspace.reconciliation.kind === "ready") return "already settled";
  const decisions = workspace.reconciliation.conflicts.map((conflict) => ({
    kind: "replace",
    target_draft_part_id: conflict.target_draft_part_id,
  }));
  const settled = await api(
    "PUT",
    `${plan(id)}/drafts/${draft.draft_id}/reconciliation`,
    { decisions, expected_snapshot_digest: workspace.draft.snapshot_digest },
    idempotent(),
  );
  if (settled.reconciliation.kind !== "ready") throw new Error(`Build ${id} still has conflicts`);
  return `settled ${decisions.length} conflict(s)`;
}

/**
 * Accepts the Build's open draft, which stores a pre-apply snapshot first. Without an
 * open draft it opens one by saving the current header unchanged.
 */
async function snapshot(api, id) {
  let draft = await openDraft(api, id);
  if (!draft) {
    const { build, base } = await api("GET", `${plan(id)}/drafts/header`);
    await api(
      "PUT",
      `${plan(id)}/drafts/header`,
      { expected_base: base, expected_draft: null, expected_header_version: null, header: build },
      idempotent(),
    );
    draft = await openDraft(api, id);
    if (!draft) throw new Error(`Build ${id}: saving the header opened no draft`);
  }
  await settleReconciliation(api, id);
  const preview = await api("GET", `${plan(id)}/drafts/${draft.draft_id}/accept-preview`);
  const revision = preview.revision;
  await api(
    "POST",
    `${plan(id)}/drafts/${draft.draft_id}/apply`,
    {
      expected_snapshot_digest: revision.expected_snapshot_digest,
      expected_lifecycle_version: revision.expected_lifecycle_version,
      expected_base: revision.expected_base,
      expected_header_version: revision.expected_header_version,
      remap_checkoff_links: false,
    },
    idempotent(),
  );
  return `accepted draft ${draft.draft_id} (${preview.kind}); snapshot stored`;
}

// Named setup steps. Each takes a Build ID; run.py checks names and arguments.
const STEPS = {
  "settle-reconciliation": settleReconciliation,
  snapshot,
};

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

    await page.goto(bootstrapUrl);
    if (new URL(page.url()).pathname === "/__desktop/bootstrap") throw new Error("bootstrap did not redirect");
    await page.goto("about:blank");

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

    for (const path of shots) {
      await page.goto(new URL(path, uiOrigin).href);
      await settle(page, path);
      const file = join(outDir, shotName(path));
      if (display) {
        // The installed evidence tool captures the display; move its file into outDir.
        const written = execFileSync(display.evidenceBin, ["shot", shotName(path).replace(/\.png$/, "")], {
          cwd: outDir,
          env: { ...process.env, EVIDENCE_DISPLAY: display.name },
          encoding: "utf8",
        })
          .trim()
          .split("\n")
          .pop();
        if (resolve(written) !== resolve(file)) renameSync(written, file);
      } else {
        await page.screenshot({ path: file, fullPage });
      }
      log(`shot ${path} -> ${file}`);
    }
  } finally {
    await browser.close();
  }
}

await main();
