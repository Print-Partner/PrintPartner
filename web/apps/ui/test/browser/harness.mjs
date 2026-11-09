// Shared plumbing for the browser checks: a loopback server for the built UI and the
// WebKitGTK (PyGObject) launcher. Each check keeps its own probes and verdicts.
import { spawn } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createServer } from "node:http";
import path from "node:path";
import process from "node:process";
import { URL, fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

export const UI_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
export const DIST = path.join(UI_ROOT, "dist");
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".woff2": "font/woff2",
  ".svg": "image/svg+xml",
};

/**
 * Serves dist on a free 127.0.0.1 port. Client routes get index.html; anything with an
 * extension that is not in dist gets 404. `core(pathname, request)` may answer any path
 * first with `{ status, body }`, sent as JSON. Otherwise `/api/*` gets 404, and
 * `/health` gets 404 unless `health(request)` returns a status code, which is answered
 * with an empty JSON object. `raw(pathname, request, response)` runs before all of that
 * and returns true when it answered. `document()` may return `{ headers, html }` for each
 * index.html answer: extra response headers, and a function that rewrites its text.
 */
export function serveDist({ health, core, raw, document } = {}) {
  if (!existsSync(path.join(DIST, "index.html"))) {
    throw new Error(`${DIST}/index.html is missing; run the package build first`);
  }
  const server = createServer((request, response) => {
    const pathname = decodeURIComponent(new URL(request.url, "http://x").pathname);
    if (raw?.(pathname, request, response)) return;
    const answer = core?.(pathname, request);
    if (answer) {
      response.writeHead(answer.status, { "content-type": "application/json" });
      response.end(JSON.stringify(answer.body));
      return;
    }
    if (pathname.startsWith("/api/")) return void response.writeHead(404).end();
    if (pathname === "/health") {
      const status = health?.(request);
      if (status) response.writeHead(status, { "content-type": "application/json" }).end("{}");
      else response.writeHead(404).end();
      return;
    }
    let file = path.join(DIST, pathname);
    if (!file.startsWith(DIST + path.sep) || !existsSync(file) || statSync(file).isDirectory()) {
      file = path.extname(pathname) ? null : path.join(DIST, "index.html");
    }
    if (!file) return void response.writeHead(404).end();
    const type = MIME[path.extname(file)] ?? "application/octet-stream";
    if (file === path.join(DIST, "index.html") && document) {
      const { headers = {}, html = (text) => text } = document();
      const body = html(readFileSync(file, "utf8"));
      response.writeHead(200, { ...headers, "content-type": type }).end(body);
      return;
    }
    response.writeHead(200, { "content-type": type }).end(readFileSync(file));
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

/** A Chromium executable for playwright-core, or undefined when none is installed. */
export function chromiumExecutable() {
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

/**
 * Runs a WebKitGTK script from this folder with `python3 -I`, under `xvfb-run` when there
 * is no display. Resolves `{ stdout }`, or `{ skip }` when python3, xvfb-run or WebKitGTK
 * is missing (the script exits 3). Async on purpose: the server above runs in this
 * process, so a blocking spawn would leave every request unanswered.
 *
 * xvfb-run sends its command's stderr to stdout, which would put GTK and EGL warnings in
 * front of the JSON result. Under xvfb-run the script's stderr goes to file descriptor 3
 * instead, read here as stderr.
 */
export function runWebKitGtk(script, args, timeout = 0) {
  const argv = ["-I", path.join(UI_ROOT, "test/browser", script), ...args];
  const hasDisplay = process.env.DISPLAY || process.env.WAYLAND_DISPLAY;
  const [command, commandArgs] = hasDisplay
    ? ["python3", argv]
    : ["xvfb-run", ["-a", "sh", "-c", 'exec python3 "$@" 2>&3', "sh", ...argv]];
  return new Promise((resolve, reject) => {
    const env = { ...process.env, WEBKIT_DISABLE_DMABUF_RENDERER: "1" };
    const child = spawn(command, commandArgs, {
      env,
      timeout,
      stdio: ["ignore", "pipe", "pipe", "pipe"],
    });
    let [stdout, stderr] = ["", ""];
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.stdio[3].on("data", (chunk) => (stderr += chunk));
    child.on("error", (error) =>
      error.code === "ENOENT" ? resolve({ skip: `${command} not found` }) : reject(error),
    );
    child.on("close", (status) => {
      const detail = stderr
        .split("\n")
        .filter((line) => line && !line.startsWith("libEGL warning"))
        .join("; ");
      if (status === 3) resolve({ skip: detail });
      else if (status !== 0) reject(new Error(`${script} exited ${status}: ${detail}`));
      else resolve({ stdout });
    });
  });
}
