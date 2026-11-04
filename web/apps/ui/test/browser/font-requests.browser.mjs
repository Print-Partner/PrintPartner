// Cold-start check for the bundled fonts: serve the built UI on loopback, open /type in a
// fresh profile per engine, and fail on any request to another origin. Also fails when a
// bundled face did not load or the figure cells are not Plex Mono with tabular figures.
// Engines: Chromium (Blink, as in WebView2) through playwright-core, and the system
// WebKitGTK (WebKit, as in WKWebView) through webkitgtk-requests.py. A missing engine
// prints SKIP and the run exits 77; a SKIP is not a pass. Run `npm run build` first.
import { spawn } from "node:child_process";
import { log } from "node:console";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { createServer } from "node:http";
import path from "node:path";
import process from "node:process";
import { URL, fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const UI_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const DIST = path.join(UI_ROOT, "dist");
const EXPECTED_FACES = [
  "IBM Plex Mono 400",
  "IBM Plex Mono 500",
  "IBM Plex Sans 400",
  "IBM Plex Sans 500",
  "IBM Plex Sans 600",
];
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".woff2": "font/woff2",
  ".svg": "image/svg+xml",
};

const PROBE_BODY = `
  const deadline = Date.now() + 15000;
  while (!document.querySelector(".specimen-figures .num")) {
    if (Date.now() > deadline) throw new Error("the type specimen did not render");
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  // Layout starts each face's load, so fonts.ready can settle before every face has
  // been requested. Poll until the expected faces load or the deadline passes.
  const loaded = new Set();
  while (loaded.size < ${EXPECTED_FACES.length} && Date.now() < deadline) {
    await document.fonts.ready;
    for (const face of document.fonts) {
      if (face.status === "loaded") loaded.add(face.family.replaceAll('"', "") + " " + face.weight);
    }
    if (loaded.size < ${EXPECTED_FACES.length}) await new Promise((resolve) => setTimeout(resolve, 100));
  }
  const cell = getComputedStyle(document.querySelector(".specimen-figures .num"));
  return JSON.stringify({
    loaded: [...loaded].sort(),
    numFamily: cell.fontFamily,
    numVariant: cell.fontVariantNumeric,
  });
`;
const controlBody = (url) => `
  await new FontFace("pp-control", "url(${url})").load().catch(() => {});
  return "done";
`;

function serveDist() {
  const server = createServer((request, response) => {
    const pathname = decodeURIComponent(new URL(request.url, "http://x").pathname);
    let file = path.join(DIST, pathname);
    if (!file.startsWith(DIST + path.sep) || !existsSync(file) || statSync(file).isDirectory()) {
      // Client routes get index.html; anything with an extension, and /health, gets 404.
      file = path.extname(pathname) || pathname === "/health" ? null : path.join(DIST, "index.html");
    }
    if (!file) {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, { "content-type": MIME[path.extname(file)] ?? "application/octet-stream" });
    response.end(readFileSync(file));
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

function chromiumExecutable() {
  const candidates = [process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE];
  try {
    candidates.push(chromium.executablePath());
  } catch {
    // No Playwright-managed browser; fall through to system installs.
  }
  candidates.push("/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
  return candidates.find((candidate) => candidate && existsSync(candidate));
}

async function runChromium(url, controlUrl) {
  const executablePath = chromiumExecutable();
  if (!executablePath) return { skip: "Chrome or Chromium not found; set PLAYWRIGHT_CHROMIUM_EXECUTABLE" };
  const browser = await chromium.launch({ headless: true, executablePath });
  try {
    const page = await (await browser.newContext()).newPage();
    const requests = [];
    page.on("request", (request) => requests.push(request.url()));
    await page.goto(url, { waitUntil: "networkidle" });
    const evaluate = (body) =>
      page.evaluate((source) => new (Object.getPrototypeOf(async () => {}).constructor)(source)(), body);
    const report = JSON.parse(await evaluate(PROBE_BODY));
    const before = requests.length;
    await evaluate(controlBody(controlUrl));
    return { report, requests: requests.slice(0, before), control: requests.slice(before) };
  } finally {
    await browser.close();
  }
}

// Async on purpose: the static server runs in this process, so a blocking spawn would
// leave every WebKitGTK request unanswered.
function runWebKitGtk(url, controlUrl) {
  const script = path.join(UI_ROOT, "test/browser/webkitgtk-requests.py");
  const args = ["-I", script, url, PROBE_BODY, controlBody(controlUrl)];
  const hasDisplay = process.env.DISPLAY || process.env.WAYLAND_DISPLAY;
  const [command, argv] = hasDisplay ? ["python3", args] : ["xvfb-run", ["-a", "python3", ...args]];
  return new Promise((resolve, reject) => {
    const child = spawn(command, argv, {
      env: { ...process.env, WEBKIT_DISABLE_DMABUF_RENDERER: "1" },
      timeout: 60_000,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", (error) =>
      error.code === "ENOENT" ? resolve({ skip: `${command} not found` }) : reject(error),
    );
    child.on("close", (status) => {
      const detail = stderr.split("\n").filter((line) => line && !line.startsWith("libEGL warning")).join("; ");
      if (status === 3) resolve({ skip: detail });
      else if (status !== 0) reject(new Error(`webkitgtk-requests.py exited ${status}: ${detail}`));
      else resolve(JSON.parse(stdout));
    });
  });
}

function check(engine, origin, controlUrl, { report, requests, control }) {
  const problems = [];
  const remote = requests.filter((request) => !request.startsWith(`${origin}/`) && !/^(data|blob):/.test(request));
  if (remote.length) problems.push(`requests to another origin: ${remote.join(", ")}`);
  const fonts = requests.filter((request) => request.endsWith(".woff2"));
  // One latin file per face: a latin-ext request on this latin-only page is a wasted download.
  if (fonts.length !== EXPECTED_FACES.length) problems.push(`${fonts.length} font requests: ${fonts.join(", ")}`);
  if (JSON.stringify(report.loaded) !== JSON.stringify(EXPECTED_FACES)) {
    problems.push(`loaded faces ${JSON.stringify(report.loaded)}`);
  }
  if (!report.numFamily.replaceAll('"', "").startsWith("IBM Plex Mono")) {
    problems.push(`figure cells use ${report.numFamily}`);
  }
  if (report.numVariant !== "tabular-nums") problems.push(`figure cells use ${report.numVariant}`);
  // Negative control: the recorder must see a cross-origin font request.
  if (!control.includes(controlUrl)) problems.push(`recorder missed the control request ${controlUrl}`);
  if (problems.length) throw new Error(`${engine}: ${problems.join("; ")}`);
  log(
    `PASS ${engine}: ${requests.length} requests, all ${origin}; ${fonts.length} WOFF2; ` +
      `loaded ${report.loaded.join(", ")}; figures ${report.numFamily} ${report.numVariant}; control seen`,
  );
}

if (!existsSync(path.join(DIST, "index.html"))) {
  throw new Error(`${DIST}/index.html is missing; run the package build first`);
}
const assets = readdirSync(path.join(DIST, "assets")).filter((name) => name.endsWith(".woff2")).sort();
let total = 0;
for (const name of assets) {
  const bytes = statSync(path.join(DIST, "assets", name)).size;
  total += bytes;
  log(`font ${name} ${bytes} bytes`);
}
log(`font total ${assets.length} files ${total} bytes`);

const server = await serveDist();
const { port } = server.address();
const origin = `http://127.0.0.1:${port}`;
const controlUrl = `http://localhost:${port}/control.woff2`;
try {
  for (const [engine, run] of [["chromium", runChromium], ["webkitgtk", runWebKitGtk]]) {
    const result = await run(`${origin}/type`, controlUrl);
    if (result.skip) {
      log(`SKIP ${engine}: ${result.skip}`);
      // 77 tells scripts/local-ci.py to report the step as SKIP, not PASS.
      process.exitCode = 77;
    } else {
      check(engine, origin, controlUrl, result);
    }
  }
} finally {
  server.close();
}
