// Content-Security-Policy check for the replacement UI. Serves the built UI on loopback
// with the exact policy the Rust-only gateway sends (rust/crates/pp-gateway/src/
// replacement-csp.txt) and the same per-response style nonce element, then in each engine:
//   - the app frame loads with no violation, and the printer event stream at /ws/printers
//     opens over the same origin;
//   - the command palette opens, and Radix's nonced scroll-lock <style> applies;
//   - a planted external script, a planted inline script and an external fetch are all
//     blocked, report a violation, and never reach the server.
// A control load without the policy proves the planted script and fetch do run and reach
// the server there, so a pass is not vacuous.
// Engines: Chromium (Blink, as in WebView2) through playwright-core, and the system
// WebKitGTK (WebKit, as in WKWebView) through webkitgtk-csp.py. A missing engine prints
// SKIP and the run exits 77; a SKIP is not a pass. Run `npm run build` first.
import { log } from "node:console";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { URL } from "node:url";
import { chromium } from "playwright-core";
import {
  chromiumExecutable,
  runWebKitGtk as launchWebKitGtk,
  serveDist,
  UI_ROOT,
} from "./harness.mjs";

const TEMPLATE = readFileSync(
  path.resolve(UI_ROOT, "../../../rust/crates/pp-gateway/src/replacement-csp.txt"),
  "utf8",
).trimEnd();

/** The policy and nonce element exactly as `pp-gateway/src/assets.rs` builds them. */
function gatewayDocument(origin) {
  const nonce = randomBytes(16).toString("hex");
  const policy = TEMPLATE.replace("{nonce}", nonce).replace(
    "{ws_origin}",
    origin.replace(/^http/, "ws"),
  );
  const element = `<meta name="pp-style-nonce" nonce="${nonce}">`;
  return {
    headers: { "content-security-policy": policy },
    html: (text) =>
      text.includes("<head>") ? text.replace("<head>", `<head>${element}`) : element + text,
  };
}

const INIT = `
  window.__cspViolations = [];
  document.addEventListener("securitypolicyviolation", (event) => {
    window.__cspViolations.push({
      directive: event.effectiveDirective || event.violatedDirective,
      blocked: event.blockedURI,
      source: event.sourceFile + ":" + event.lineNumber + ":" + event.columnNumber,
    });
  });
`;

const probe = (external) => `
  const deadline = Date.now() + 20000;
  const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const until = async (test, what) => {
    while (!test()) {
      if (Date.now() > deadline) throw new Error(what + " did not appear");
      await pause(50);
    }
  };
  await until(() => document.querySelector('nav[aria-label="Main"]'), "the app frame");
  await document.fonts.ready;
  await pause(300);
  const atLoad = window.__cspViolations.slice();

  const stream = await new Promise((resolve) => {
    const url = new URL("/ws/printers", location.href);
    url.protocol = "ws:";
    let socket;
    try {
      socket = new WebSocket(url.toString());
    } catch (error) {
      resolve("threw " + error.name);
      return;
    }
    socket.onopen = () => { socket.close(); resolve("open"); };
    socket.onerror = () => resolve("error");
    setTimeout(() => resolve("timeout"), 5000);
  });

  document.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true }));
  await until(() => document.querySelector('[role="dialog"]'), "the command palette");
  await pause(300);
  const nonced = [...document.querySelectorAll("style")].filter((style) => style.nonce).length;
  const scrollLock = getComputedStyle(document.body).overflow;
  const clean = window.__cspViolations.length;

  const external = await new Promise((resolve) => {
    const script = document.createElement("script");
    script.src = "${external}/planted.js";
    script.onload = () => resolve("loaded");
    script.onerror = () => resolve("blocked");
    document.head.append(script);
  });
  const inline = document.createElement("script");
  inline.textContent = "window.__inlineRan = true";
  document.head.append(inline);
  let fetched;
  try {
    await fetch("${external}/external", { mode: "no-cors" });
    fetched = "sent";
  } catch {
    fetched = "blocked";
  }
  await pause(500);
  return JSON.stringify({
    atLoad,
    beforePlanting: window.__cspViolations.slice(atLoad.length, clean),
    stream,
    nonced,
    scrollLock,
    external,
    externalRan: Boolean(window.__plantedRan),
    inlineRan: Boolean(window.__inlineRan),
    fetched,
    planted: window.__cspViolations.slice(clean),
  });
`;

async function runChromium(url, body) {
  const executablePath = chromiumExecutable();
  if (!executablePath)
    return { skip: "Chrome or Chromium not found; set PLAYWRIGHT_CHROMIUM_EXECUTABLE" };
  const browser = await chromium.launch({ headless: true, executablePath });
  try {
    const page = await (await browser.newContext()).newPage();
    const console = [];
    page.on("console", (message) => console.push(message.text()));
    await page.addInitScript(INIT);
    await page.goto(url, { waitUntil: "load" });
    const report = JSON.parse(
      await page.evaluate(
        (source) => new (Object.getPrototypeOf(async () => {}).constructor)(source)(),
        body,
      ),
    );
    return { report, console };
  } finally {
    await browser.close();
  }
}

async function runWebKitGtk(url, body) {
  const outcome = await launchWebKitGtk("webkitgtk-csp.py", [url, INIT, body], 90_000);
  return outcome.skip ? outcome : { report: JSON.parse(outcome.stdout) };
}

/** Accepts the WebSocket handshake on /ws/printers and holds the socket open. */
function acceptStream(server, opened) {
  server.on("upgrade", (request, socket) => {
    if (new URL(request.url, "http://x").pathname !== "/ws/printers") return void socket.destroy();
    const accept = createHash("sha1")
      .update(request.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
      .digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" +
        `Sec-WebSocket-Accept: ${accept}\r\n\r\n`,
    );
    socket.on("error", () => {});
    opened.count += 1;
  });
}

function checkEnforced(engine, { report, console }, hits, streams) {
  const problems = [];
  if (report.atLoad.length)
    problems.push(`violations while loading: ${JSON.stringify(report.atLoad)}`);
  if (report.beforePlanting.length) {
    problems.push(
      `violations from the stream or the palette: ${JSON.stringify(report.beforePlanting)}`,
    );
  }
  if (report.stream !== "open") problems.push(`/ws/printers ${report.stream}`);
  if (streams.count === 0) problems.push("no WebSocket reached /ws/printers");
  if (report.nonced < 1) problems.push("no nonced <style> while the palette is open");
  if (report.scrollLock !== "hidden")
    problems.push(`body overflow ${report.scrollLock} while the palette is open`);
  if (report.external !== "blocked" || report.externalRan)
    problems.push(`external script ${report.external}`);
  if (report.inlineRan) problems.push("planted inline script ran");
  if (report.fetched !== "blocked") problems.push(`external fetch ${report.fetched}`);
  if (hits.length) problems.push(`blocked requests reached the server: ${hits.join(", ")}`);
  const directives = report.planted
    .map((violation) => violation.directive.replace(/-elem$/, ""))
    .sort();
  if (JSON.stringify(directives) !== JSON.stringify(["connect-src", "script-src", "script-src"])) {
    problems.push(`planted violations ${JSON.stringify(report.planted)}`);
  }
  // Chromium also logs each refusal; every one must belong to a planted probe.
  const stray = (console ?? []).filter(
    (line) =>
      /Content Security Policy/i.test(line) && !/planted\.js|\/external|inline script/.test(line),
  );
  if (stray.length) problems.push(`console CSP messages: ${stray.join(" | ")}`);
  if (problems.length) throw new Error(`${engine} with policy: ${problems.join("; ")}`);
  log(
    `PASS ${engine}: frame and palette loaded with 0 violations; /ws/printers open; ` +
      `${report.nonced} nonced <style>, body overflow ${report.scrollLock}; ` +
      `external script, inline script and external fetch blocked (${directives.join(", ")}); 0 server hits`,
  );
}

function checkControl(engine, { report }, hits) {
  const problems = [];
  if (!report.externalRan) problems.push(`external script ${report.external} but did not run`);
  if (!report.inlineRan) problems.push("inline script did not run");
  if (!hits.includes("/planted.js") || !hits.includes("/external"))
    problems.push(`server saw ${hits.join(", ")}`);
  if (problems.length) throw new Error(`${engine} control without policy: ${problems.join("; ")}`);
  log(
    `PASS ${engine} control: without the policy the planted script ran and both requests reached the server`,
  );
}

let policy = true;
const hits = [];
const streams = { count: 0 };
const server = await serveDist({
  document: () => (policy ? gatewayDocument(origin) : {}),
  raw(pathname, _request, response) {
    if (pathname === "/planted.js") {
      hits.push(pathname);
      response
        .writeHead(200, { "content-type": "text/javascript" })
        .end("window.__plantedRan = true;");
      return true;
    }
    if (pathname === "/external") {
      hits.push(pathname);
      response.writeHead(200, { "content-type": "text/plain" }).end("reached");
      return true;
    }
    return false;
  },
});
acceptStream(server, streams);
const { port } = server.address();
const origin = `http://127.0.0.1:${port}`;
// Another origin on the same server, so a request that escapes the policy is counted.
const external = `http://localhost:${port}`;
try {
  for (const [engine, run] of [
    ["chromium", runChromium],
    ["webkitgtk", runWebKitGtk],
  ]) {
    policy = true;
    hits.length = 0;
    streams.count = 0;
    const result = await run(`${origin}/today`, probe(external));
    if (result.skip) {
      log(`SKIP ${engine}: ${result.skip}`);
      // 77 tells scripts/local-ci.py to report the step as SKIP, not PASS.
      process.exitCode = 77;
      continue;
    }
    checkEnforced(engine, result, [...hits], streams);
    policy = false;
    hits.length = 0;
    checkControl(engine, await run(`${origin}/today`, probe(external)), [...hits]);
  }
} finally {
  server.closeAllConnections();
  server.close();
}
