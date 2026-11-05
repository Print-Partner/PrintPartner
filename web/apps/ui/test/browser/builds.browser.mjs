// UI2-04: the Builds list with 10,000 Builds stays interactive. Serves the built UI with a
// fake core holding 10,000 Builds, then in WebKitGTK (the Linux release webview) and
// Chromium (Blink, as in WebView2) records the time from navigation to the first and the
// last row, and the delay from each Down or End key to the next painted frame. It fails
// when a row is missing, next actions never load, or focus lands on the wrong row. There
// is no approved latency
// budget, so the timings are recorded, not judged. A missing engine prints SKIP and the
// run exits 77; a SKIP is not a pass. Run `npm run build` first.
import { log } from "node:console";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { chromium } from "playwright-core";
import { fakeCore } from "./coreFixture.mjs";
import { chromiumExecutable, runWebKitGtk, serveDist } from "./harness.mjs";

const COUNT = 10_000;
const PRESSES = 40;

/** An async function body run in the page. Returns a JSON string, as webkitgtk-frame.py expects. */
const PROBE = `
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const frame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const row = (i) => document.querySelector('[data-row="' + i + '"]');
  const deadline = Date.now() + 60000;
  while (!row(0)) { if (Date.now() > deadline) throw new Error("no rows"); await sleep(5); }
  const firstRow = performance.now();
  while (!row(${COUNT - 1})) { if (Date.now() > deadline) throw new Error("rows missing"); await sleep(5); }
  const lastRow = performance.now();
  const rows = document.querySelectorAll(".pp-builds-table tbody tr").length;
  while (!row(0).closest("tr").textContent.includes("Verify prints")) {
    if (Date.now() > deadline) throw new Error("next actions missing");
    await sleep(5);
  }
  await frame();
  row(0).focus();
  const key = async (k) => {
    const start = performance.now();
    document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true }));
    await frame();
    return performance.now() - start;
  };
  const down = [];
  let wrong = null;
  for (let i = 1; i <= ${PRESSES}; i++) {
    down.push(await key("ArrowDown"));
    if (document.activeElement !== row(i)) wrong = wrong || "Down to row " + i;
  }
  const end = await key("End");
  if (document.activeElement !== row(${COUNT - 1})) wrong = wrong || "End";
  return JSON.stringify({ firstRow, lastRow, rows, down, end, wrong,
    scrollHeight: document.documentElement.scrollHeight, innerWidth: window.innerWidth });
`;

const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
const ms = (value) => `${Math.round(value)} ms`;

function summarize(engine, result) {
  log(
    `${engine}: ${COUNT} Builds; first row ${ms(result.firstRow)}, last row ${ms(result.lastRow)} ` +
      `after navigation; Down to next frame median ${ms(median(result.down))}, ` +
      `max ${ms(Math.max(...result.down))} over ${PRESSES} presses; End ${ms(result.end)}`,
  );
  return [
    ...(result.rows === COUNT ? [] : [`${engine}: rendered ${result.rows} of ${COUNT} rows`]),
    ...(result.wrong ? [`${engine}: focus was not on the expected row after ${result.wrong}`] : []),
  ];
}

async function inWebKitGtk(url) {
  const dir = mkdtempSync(path.join(tmpdir(), "pp-ui-builds-"));
  try {
    const spec = path.join(dir, "jobs.json");
    writeFileSync(spec, JSON.stringify({ axe: "", jobs: [{ url, body: PROBE, png: null }] }));
    const outcome = await runWebKitGtk("webkitgtk-frame.py", [spec], 180_000);
    return "skip" in outcome ? outcome : { result: JSON.parse(outcome.stdout)[0].result };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

async function inChromium(url) {
  const executablePath = chromiumExecutable();
  if (!executablePath) return { skip: "no Chromium executable found" };
  const browser = await chromium.launch({ headless: true, executablePath });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    await page.goto(url);
    return { result: JSON.parse(await page.evaluate(`(async () => {${PROBE}})()`)) };
  } finally {
    await browser.close();
  }
}

const server = await serveDist({ core: fakeCore(COUNT) });
const url = `http://127.0.0.1:${server.address().port}/builds`;
const problems = [];
let skipped = false;
try {
  for (const [engine, run] of [
    ["webkitgtk", inWebKitGtk],
    ["chromium", inChromium],
  ]) {
    const outcome = await run(url);
    if ("skip" in outcome) {
      log(`SKIP ${engine}: ${outcome.skip}`);
      skipped = true;
    } else {
      problems.push(...summarize(engine, outcome.result));
    }
  }
} finally {
  server.close();
}
if (problems.length) throw new Error(problems.join("\n"));
if (skipped) process.exit(77);
log(`PASS builds: ${COUNT} Builds render every row and keep keyboard focus in WebKitGTK and Chromium`);
