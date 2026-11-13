// A stateful Moonraker simulator for the daily-loop test (GRE-1528). It answers the
// Moonraker paths Print Partner calls and keeps its own printer state: a print start only
// changes that state. It never opens a connection of its own. The harness drives it
// through a separate control port and reads a request log that holds method, path,
// counts and upload hashes, never a file's contents, a header or the API key.
//
//   node moonraker-sim.mjs --api-key-file <file> [--host H] [--port P]
//                          [--control-host H] [--control-port P]
//
// The first stdout line is {"printer_port":N,"control_port":N}. SIGTERM or SIGINT stops it.

import { Buffer } from "node:buffer";
import { createHash, timingSafeEqual } from "node:crypto";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import process from "node:process";
import { setTimeout as delay } from "node:timers/promises";
import { URL, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

export const STATES = ["standby", "printing", "paused", "complete", "cancelled", "error", "offline"];
const FAULTS = ["lose-upload", "lose-start", "late", "error-500"];
// Klipper accepts a new print only from these states.
const IDLE = new Set(["standby", "complete", "cancelled"]);
const MAX_UPLOAD = 256 * 1024 * 1024;
const MAX_LOG = 1000;

const json = (response, status, body) => {
  const text = JSON.stringify(body);
  response.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
  response.end(text);
};
const failure = (response, status, message) => json(response, status, { error: { code: status, message } });

/**
 * The whole body, or null when it is longer than `limit`. A body over the limit is still
 * read to its end: leaving the loop early would destroy the socket, and the client would
 * see a reset instead of the refusal.
 */
async function readBody(request, limit) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size <= limit) chunks.push(chunk);
  }
  return size > limit ? null : Buffer.concat(chunks);
}

/** The `file` part of a multipart body: its file name and bytes, or null. */
export function filePart(body, contentType) {
  const boundary = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(contentType ?? "");
  if (!boundary) return null;
  const marker = Buffer.from(`--${boundary[1] ?? boundary[2]}`);
  let start = body.indexOf(marker);
  while (start !== -1) {
    const next = body.indexOf(marker, start + marker.length);
    if (next === -1) return null;
    const part = body.subarray(start + marker.length + 2, next - 2);
    const split = part.indexOf("\r\n\r\n");
    const head = split === -1 ? "" : part.subarray(0, split).toString("latin1");
    const name = /filename="([^"]*)"/i.exec(head);
    if (/name="file"/i.test(head) && name) return { name: name[1], bytes: part.subarray(split + 4) };
    start = next;
  }
  return null;
}

function statusBody(printer) {
  const printing = printer.state === "printing" || printer.state === "paused";
  return {
    result: {
      eventtime: (Date.now() - printer.startedAt) / 1000,
      status: {
        print_stats: { state: printer.state, filename: printer.filename ?? "" },
        virtual_sdcard: { progress: printer.progress, is_active: printer.state === "printing" },
        display_status: { progress: printer.progress, message: null },
        extruder: { temperature: printing ? 214.8 : 24.1, target: printing ? 215 : 0 },
        heater_bed: { temperature: printing ? 59.9 : 23.6, target: printing ? 60 : 0 },
      },
    },
  };
}

const fileItems = (printer) =>
  [...printer.files].map(([path, file]) => ({ path, modified: file.modified, size: file.size, permissions: "rw" }));

const READS = {
  "/server/info": () => ({ result: { klippy_connected: true, klippy_state: "ready", moonraker_version: "v0.9.3-sim", components: [] } }),
  "/printer/objects/query": statusBody,
  "/server/files/list": (printer) => ({ result: fileItems(printer) }),
  "/server/files/directory": (printer) => ({
    result: {
      dirs: [],
      files: fileItems(printer).map(({ path, ...rest }) => ({ filename: path, ...rest })),
      disk_usage: { total: 8e9, used: 1e9, free: 7e9 },
      root_info: { name: "gcodes", permissions: "rw" },
    },
  }),
  "/machine/proc_stats": () => ({ result: { moonraker_stats: [], throttled_state: { bits: 0, flags: [] }, system_uptime: 3600 } }),
  "/server/webcams/list": () => ({ result: { webcams: [] } }),
};

/** The armed fault, if it applies to `path`, consumed. */
function takeFault(printer, path, kinds) {
  const fault = printer.fault;
  if (!fault || !kinds.includes(fault.kind) || (fault.path && fault.path !== path)) return null;
  printer.fault = null;
  return fault;
}

async function upload(printer, request, response) {
  const body = await readBody(request, printer.maxUpload);
  if (!body) return failure(response, 413, "Upload too large");
  const part = filePart(body, request.headers["content-type"]);
  if (!part || !part.name) return failure(response, 400, "No file part in the upload");
  const sha256 = createHash("sha256").update(part.bytes).digest("hex");
  const known = printer.files.get(part.name);
  printer.files.set(part.name, { size: part.bytes.length, sha256, modified: Date.now() / 1000, uploads: (known?.uploads ?? 0) + 1 });
  printer.counts.uploads += 1;
  // The file is stored, as a printer whose answer was lost would have stored it.
  if (takeFault(printer, "/server/files/upload", ["lose-upload"])) return response.socket.destroy();
  json(response, 201, { result: { item: { path: part.name, root: "gcodes" }, print_started: false, print_queued: false, action: "create_file" } });
}

function start(printer, url, response) {
  const name = url.searchParams.get("filename") ?? "";
  if (!printer.files.has(name)) return failure(response, 400, `File not found: ${name}`);
  if (!IDLE.has(printer.state)) return failure(response, 400, `Printer is busy: ${printer.state}`);
  Object.assign(printer, { state: "printing", progress: 0, filename: name });
  printer.counts.starts += 1;
  if (takeFault(printer, "/printer/print/start", ["lose-start"])) return response.socket.destroy();
  json(response, 200, { result: "ok" });
}

function authorized(printer, request) {
  if (printer.apiKey === null) return true;
  const sent = Buffer.from(String(request.headers["x-api-key"] ?? ""));
  const key = Buffer.from(printer.apiKey);
  return sent.length === key.length && timingSafeEqual(sent, key);
}

async function answer(printer, request, response) {
  // A connection kept alive from before the printer went offline gets no answer either.
  if (printer.state === "offline") return response.socket.destroy();
  const url = new URL(request.url ?? "/", "http://sim");
  printer.counts.requests += 1;
  printer.log.push({ method: request.method, path: url.pathname + url.search });
  if (printer.log.length > MAX_LOG) printer.log.shift();
  if (!authorized(printer, request)) {
    printer.counts.unauthorized += 1;
    return failure(response, 401, "Unauthorized");
  }
  const late = takeFault(printer, url.pathname, ["late"]);
  if (late) await delay(late.ms);
  if (takeFault(printer, url.pathname, ["error-500"])) return failure(response, 500, "Internal Server Error");
  if (request.method === "POST" && url.pathname === "/server/files/upload") return upload(printer, request, response);
  if (request.method === "POST" && url.pathname === "/printer/print/start") return start(printer, url, response);
  const read = request.method === "GET" && READS[url.pathname];
  if (read) return json(response, 200, read(printer));
  failure(response, 404, "Not Found");
}

function logBody(printer) {
  const files = [...printer.files].map(([name, { size, sha256, uploads }]) => ({ name, size, sha256, uploads }));
  const { state, progress, filename, counts, log } = printer;
  return { state, progress, filename, counts, files, fault: printer.fault?.kind ?? null, requests: log };
}

async function control(printer, request, response) {
  const path = new URL(request.url ?? "/", "http://sim").pathname;
  if (request.method === "GET" && path === "/_sim/log") return json(response, 200, logBody(printer));
  if (request.method !== "POST") return failure(response, 404, "Not Found");
  let body;
  try {
    body = JSON.parse((await readBody(request, 64 * 1024))?.toString() || "{}");
  } catch {
    return failure(response, 400, "Body is not JSON");
  }
  if (path === "/_sim/state") {
    if (!STATES.includes(body.state)) return failure(response, 400, `state must be one of ${STATES.join(", ")}`);
    const progress = body.progress ?? printer.progress;
    if (typeof progress !== "number" || progress < 0 || progress > 1) return failure(response, 400, "progress must be 0 to 1");
    Object.assign(printer, { state: body.state, progress, filename: body.filename ?? printer.filename });
    return json(response, 200, logBody(printer));
  }
  if (path === "/_sim/fault") {
    if (!FAULTS.includes(body.fault)) return failure(response, 400, `fault must be one of ${FAULTS.join(", ")}`);
    printer.fault = { kind: body.fault, path: body.path ?? null, ms: Number(body.ms ?? 70_000) };
    return json(response, 200, logBody(printer));
  }
  failure(response, 404, "Not Found");
}

const listen = (server, port, host) =>
  new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => resolve(server.address().port));
  });

/**
 * Starts the printer and control servers. `apiKey` null accepts every request. Returns
 * the two origins and `close()`. Nothing here connects out.
 */
export async function startSimulator({ apiKey = null, host = "127.0.0.1", port = 0, controlHost = "127.0.0.1", controlPort = 0, maxUpload = MAX_UPLOAD } = {}) {
  const printer = {
    apiKey,
    maxUpload,
    state: "standby",
    progress: 0,
    filename: null,
    files: new Map(),
    fault: null,
    log: [],
    counts: { requests: 0, unauthorized: 0, uploads: 0, starts: 0 },
    startedAt: Date.now(),
  };
  const printerServer = createServer((request, response) => {
    answer(printer, request, response).catch(() => response.socket?.destroy());
  });
  // Offline: every connection is closed before a byte is read.
  printerServer.on("connection", (socket) => printer.state === "offline" && socket.destroy());
  const controlServer = createServer((request, response) => {
    control(printer, request, response).catch(() => response.socket?.destroy());
  });
  const printerPort = await listen(printerServer, port, host);
  const boundControl = await listen(controlServer, controlPort, controlHost);
  const close = async () => {
    for (const server of [printerServer, controlServer]) {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    }
  };
  return { printerPort, controlPort: boundControl, close };
}

async function main() {
  const { values } = parseArgs({
    options: {
      "api-key-file": { type: "string" },
      host: { type: "string", default: "127.0.0.1" },
      port: { type: "string", default: "0" },
      "control-host": { type: "string", default: "127.0.0.1" },
      "control-port": { type: "string", default: "0" },
    },
  });
  const file = values["api-key-file"];
  const apiKey = file ? readFileSync(file, "utf8").trim() : null;
  const sim = await startSimulator({
    apiKey,
    host: values.host,
    port: Number(values.port),
    controlHost: values["control-host"],
    controlPort: Number(values["control-port"]),
  });
  process.stdout.write(`${JSON.stringify({ printer_port: sim.printerPort, control_port: sim.controlPort })}\n`);
  const stop = () => void sim.close().then(() => process.exit(0));
  process.once("SIGTERM", stop);
  process.once("SIGINT", stop);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    process.stderr.write(`moonraker-sim: ${error.message}\n`);
    process.exit(1);
  });
}
