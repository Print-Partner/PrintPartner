// GTK-02: the Checkoff tab's Plate preview renders with WebGL forced off. Serves the built
// UI with the fake core, turns WebGL off in WebKitGTK (the Linux release webview) and in
// Chromium (Blink, as in WebView2), and fails unless WebGL is really off, each Plate has its
// sentence and a hidden, visible SVG bar, the page holds no canvas, and axe at WCAG 2.2 AA
// finds nothing in the preview. A missing engine prints SKIP and the run exits 77; a SKIP is
// not a pass. Run `npm run build` first.
import axe from "axe-core";
import { log } from "node:console";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { chromium } from "playwright-core";
import { fakeCore } from "./coreFixture.mjs";
import { chromiumExecutable, runWebKitGtk, serveDist } from "./harness.mjs";

const EXPECTED = [
  "Plate 1 on Voron 2.4: 3 units, 1 printed",
  "Plate 2 on Trident: 1 unit, 0 printed",
  "On no Plate: 1 unit, 0 printed",
];
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22a", "wcag22aa"];

/** An async function body run in the page, after axe is loaded. Returns a JSON string. */
const PROBE = `
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 15000;
  const section = () => document.querySelector(".pp-plate-preview");
  while (!section()?.querySelector("li")) {
    if (Date.now() > deadline) throw new Error("the Plate preview did not render");
    await sleep(50);
  }
  const gl = ["webgl", "webgl2"].filter((kind) => document.createElement("canvas").getContext(kind) !== null);
  const items = [...section().querySelectorAll("li")].map((li) => {
    const svg = li.querySelector("svg");
    return { text: li.textContent, hidden: svg?.getAttribute("aria-hidden") ?? null,
      rects: svg?.querySelectorAll("rect").length ?? 0, width: svg?.getBoundingClientRect().width ?? 0 };
  });
  const audit = await window.axe.run(section(), { runOnly: { type: "tag", values: ${JSON.stringify(TAGS)} } });
  return JSON.stringify({ gl, items, canvases: document.querySelectorAll("canvas").length,
    violations: audit.violations.map((v) => v.id + ": " + v.nodes.map((n) => n.target.join(" ")).join(", ")),
    scrollHeight: document.documentElement.scrollHeight, innerWidth: window.innerWidth });
`;

const ENGINES = [
  [
    "webkitgtk",
    async (url) => {
      const dir = mkdtempSync(path.join(tmpdir(), "pp-ui-plate-"));
      try {
        const spec = path.join(dir, "jobs.json");
        const job = { url, body: PROBE, png: null, webgl: false };
        writeFileSync(spec, JSON.stringify({ axe: axe.source, jobs: [job] }));
        const outcome = await runWebKitGtk("webkitgtk-frame.py", [spec], 120_000);
        return "skip" in outcome ? outcome : { result: JSON.parse(outcome.stdout)[0].result };
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    },
  ],
  [
    "chromium",
    async (url) => {
      const executablePath = chromiumExecutable();
      if (!executablePath) return { skip: "no Chromium executable found" };
      const args = ["--disable-webgl", "--disable-3d-apis"];
      const browser = await chromium.launch({ headless: true, executablePath, args });
      try {
        const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
        await page.goto(url);
        await page.addScriptTag({ content: axe.source });
        return { result: JSON.parse(await page.evaluate(`(async () => {${PROBE}})()`)) };
      } finally {
        await browser.close();
      }
    },
  ],
];

function problems(engine, { gl, items, canvases, violations }) {
  const texts = items.map((i) => i.text);
  return [
    ...(gl.length
      ? [`WebGL was not forced off in ${engine}: ${gl.join(", ")} gave a context`]
      : []),
    ...(JSON.stringify(texts) === JSON.stringify(EXPECTED)
      ? []
      : [
          `${engine}: preview items ${JSON.stringify(texts)}, expected ${JSON.stringify(EXPECTED)}`,
        ]),
    ...items
      .filter((i) => i.hidden !== "true" || i.rects !== 2 || !(i.width > 0))
      .map(
        (i) =>
          `${engine}: bar for "${i.text}" is aria-hidden=${i.hidden}, ${i.rects} rects, ${i.width}px wide`,
      ),
    ...(canvases ? [`${engine}: the page holds ${canvases} canvas elements`] : []),
    ...violations.map((v) => `${engine}: axe ${v}`),
  ];
}

const server = await serveDist({ core: fakeCore(1) });
const url = `http://127.0.0.1:${server.address().port}/builds/1/checkoff`;
const found = [];
const passed = [];
let skipped = false;
try {
  for (const [engine, run] of ENGINES) {
    const outcome = await run(url);
    if ("skip" in outcome) {
      log(`SKIP ${engine}: ${outcome.skip}`);
      skipped = true;
      continue;
    }
    const own = problems(engine, outcome.result);
    found.push(...own);
    if (!own.length) passed.push(engine);
  }
} finally {
  server.close();
}
if (found.length) throw new Error(found.join("\n"));
if (passed.length) {
  log(
    `PASS plate-preview: ${passed.join(" and ")} draw ${EXPECTED.length} Plate bars with WebGL off, no canvas, axe clean`,
  );
}
if (skipped) process.exit(77);
