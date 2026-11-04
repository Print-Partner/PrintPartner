// Frame accessibility check in the system WebKitGTK (the Linux release webview): serve the
// built UI on loopback, run axe at WCAG 2.2 AA on every frame page in all three themes,
// then fail on sideways scrolling at 200% text and at 400% zoom. PP_UI_EVIDENCE_DIR=<dir>
// also saves a screenshot of each load there. A missing engine prints SKIP and exits 77;
// a SKIP is not a pass. Run `npm run build` first.
import { spawn } from "node:child_process";
import { log } from "node:console";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { URL, fileURLToPath } from "node:url";

const UI_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const DIST = path.join(UI_ROOT, "dist");
const AXE = createRequire(import.meta.url).resolve("axe-core/axe.min.js");
const EVIDENCE = process.env.PP_UI_EVIDENCE_DIR;
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22a", "wcag22aa"];
const THEMES = ["dark", "light", "shop-floor"];
const MIME = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css" };

const key = (init) =>
  `document.dispatchEvent(new KeyboardEvent("keydown", ${JSON.stringify({ ...init, bubbles: true })}));`;
const STATES = ["loading", "empty", "error", "module-off", "offline", "ready"];
// [name, path, setup]. "?core=down" makes /health fail for that page, so the frame is offline.
const PAGES = [
  ...["today", "builds", "farm", "library", "settings"].map((p) => [p, `/${p}`, ""]),
  ...STATES.map((s) => [`states-${s}`, `/states/${s}`, ""]),
  ["offline-frame", "/builds?core=down", ""],
  ["palette", "/today", key({ key: "k", ctrlKey: true })],
  ["shortcuts", "/today", key({ key: "?" })],
];

/** The async body run in the page: wait for the frame, set up, then measure. */
const body = ({ setup = "", axe = false, theme = "dark" }) => `
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 15000;
  while (!document.querySelector("h1") || !document.querySelector(".pp-connection:not([data-status=checking])")) {
    if (Date.now() > deadline) throw new Error("the frame did not render");
    await sleep(50);
  }
  await document.fonts.ready;
  ${setup}
  await sleep(250);
  const root = document.documentElement;
  const out = { overflow: root.scrollWidth - root.clientWidth, scrollHeight: root.scrollHeight,
    innerWidth: window.innerWidth, violations: [] };
  for (const theme of ${axe ? JSON.stringify(THEMES) : "[]"}) {
    root.dataset.theme = theme;
    const result = await runAxe({ runOnly: { type: "tag", values: ${JSON.stringify(TAGS)} } });
    for (const v of result.violations)
      out.violations.push(theme + " " + v.id + ": " + v.nodes.map((n) => n.target.join(" ")).join(", "));
  }
  root.dataset.theme = ${JSON.stringify(theme)};
  return JSON.stringify(out);
`;

function jobs(origin) {
  const png = (name) => (EVIDENCE ? path.join(EVIDENCE, `webkitgtk-${name}.png`) : null);
  const list = [];
  const add = (label, pagePath, options, zoom = {}) =>
    list.push({
      label,
      url: `${origin}${pagePath}`,
      body: body(options),
      png: png(label),
      ...zoom,
    });
  for (const [name, pagePath, setup] of PAGES) add(name, pagePath, { setup, axe: true });
  for (const theme of ["light", "shop-floor"]) add(`today-${theme}`, "/today", { theme });
  for (const [name, pagePath, setup] of PAGES) {
    add(`${name}-text-200`, pagePath, { setup }, { zoom: 2, text_only: true });
    add(`${name}-reflow-400`, pagePath, { setup }, { zoom: 4 });
  }
  return list;
}

function serveDist() {
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://x");
    if (url.pathname === "/health") {
      const down = (request.headers.referer ?? "").includes("core=down");
      response.writeHead(down ? 503 : 200, { "content-type": "application/json" }).end("{}");
      return;
    }
    let file = path.join(DIST, decodeURIComponent(url.pathname));
    if (!file.startsWith(DIST + path.sep) || !existsSync(file) || statSync(file).isDirectory()) {
      file = path.extname(url.pathname) ? null : path.join(DIST, "index.html");
    }
    if (!file) return void response.writeHead(404).end();
    const type = MIME[path.extname(file)] ?? "application/octet-stream";
    response.writeHead(200, { "content-type": type }).end(readFileSync(file));
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

// Async on purpose: the static server runs in this process, so a blocking spawn would
// leave every WebKitGTK request unanswered.
function runWebKitGtk(list) {
  const dir = mkdtempSync(path.join(tmpdir(), "pp-ui-frame-"));
  const spec = path.join(dir, "jobs.json");
  writeFileSync(spec, JSON.stringify({ axe: AXE, jobs: list }));
  const args = ["-I", path.join(UI_ROOT, "test/browser/webkitgtk-frame.py"), spec];
  const hasDisplay = process.env.DISPLAY || process.env.WAYLAND_DISPLAY;
  const [command, argv] = hasDisplay ? ["python3", args] : ["xvfb-run", ["-a", "python3", ...args]];
  return new Promise((resolve, reject) => {
    const env = { ...process.env, WEBKIT_DISABLE_DMABUF_RENDERER: "1" };
    const child = spawn(command, argv, { env });
    let [stdout, stderr] = ["", ""];
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", (error) =>
      error.code === "ENOENT" ? resolve({ skip: `${command} not found` }) : reject(error),
    );
    child.on("close", (status) => {
      rmSync(dir, { recursive: true, force: true });
      const detail = stderr
        .split("\n")
        .filter((l) => l && !l.startsWith("libEGL warning"))
        .join("; ");
      if (status === 3) resolve({ skip: detail });
      else if (status !== 0) reject(new Error(`webkitgtk-frame.py exited ${status}: ${detail}`));
      else resolve({ results: JSON.parse(stdout) });
    });
  });
}

if (!existsSync(path.join(DIST, "index.html"))) {
  throw new Error(`${DIST}/index.html is missing; run the package build first`);
}
const server = await serveDist();
const list = jobs(`http://127.0.0.1:${server.address().port}`);
let outcome;
try {
  outcome = await runWebKitGtk(list);
} finally {
  server.close();
}
if (outcome.skip) {
  log(`SKIP webkitgtk: ${outcome.skip}`);
  process.exit(77);
}
const problems = outcome.results.flatMap(({ result }, i) => [
  ...result.violations.map((v) => `${list[i].label}: ${v}`),
  ...(result.overflow > 0 ? [`${list[i].label}: scrolls sideways by ${result.overflow}px`] : []),
]);
if (problems.length) throw new Error(`webkitgtk:\n  ${problems.join("\n  ")}`);
log(
  `PASS webkitgtk: axe WCAG 2.2 AA clean on ${PAGES.length} pages x ${THEMES.length} themes; ` +
    `no sideways scroll in ${list.length} loads, ${PAGES.length} each at 200% text and 400% zoom`,
);
if (EVIDENCE) log(`screenshots in ${EVIDENCE}`);
