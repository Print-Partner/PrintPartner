// A Spoolman stand-in for Step 11 of the daily loop (GRE-1887). run.mjs starts it beside
// the Rust-only container, the way simulator.mjs starts the Moonraker simulator: a third
// container on the run's own Docker network, named `pp-spoolman` there, with only its
// control port published, on host loopback. No real Spoolman server is ever contacted.
//
// It answers the Spoolman paths the core calls (rust/crates/pp-core/src/spoolman.rs):
//   GET /api/v1/info, GET /api/v1/health
//   GET /api/v1/filament, GET /api/v1/spool, GET /api/v1/spool/{id}
//   PUT /api/v1/spool/{id}/use  {"use_weight": grams}  takes grams off that spool
// The control port serves the harness:
//   GET  /_stand-in/log    the spools, request counts and every use, in order
//   POST /_stand-in/stock  {"spool_id": n, "remaining_weight": g} sets a spool's grams
//
//   node spoolman.mjs [--host H] [--port P] [--control-host H] [--control-port P]
//
// The first stdout line is {"port":N,"control_port":N}. SIGTERM or SIGINT stops it. This
// file uses only Node's standard library: the container mounts it alone.
import { Buffer } from "node:buffer";
import { createServer } from "node:http";
import process from "node:process";
import { URL, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

export const SPOOLMAN_ALIAS = "pp-spoolman";
export const SPOOLMAN_PORT = 7912;
export const SPOOLMAN_CONTROL_PORT = 7913;
/** The stand-in's address as the product container sees it. */
export const SPOOLMAN_URL = `http://${SPOOLMAN_ALIAS}:${SPOOLMAN_PORT}`;

/** One filament on two spools, and an archived spool the core must leave out. */
export const STOCK = {
  filaments: [
    {
      id: 1,
      name: "Daily loop black",
      material: "PLA",
      vendor: { id: 1, name: "Stand-in" },
      color_hex: "1a1a1a",
      density: 1.24,
      diameter: 1.75,
    },
  ],
  spools: [
    { id: 1, filament_id: 1, remaining_weight: 750, used_weight: 250, archived: false },
    { id: 2, filament_id: 1, remaining_weight: 120, used_weight: 880, archived: false },
    { id: 3, filament_id: 1, remaining_weight: 1000, used_weight: 0, archived: true },
  ],
};
const MAX_BODY = 64 * 1024;
const MAX_LOG = 1000;

const json = (response, status, body) => {
  const text = JSON.stringify(body);
  response.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
  response.end(text);
};
const failure = (response, status, message) => json(response, status, { detail: message });

/** The JSON body, `{}` when empty, or null when it is too long or not JSON. */
async function readJson(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size <= MAX_BODY) chunks.push(chunk);
  }
  if (size > MAX_BODY) return null;
  try {
    return JSON.parse(Buffer.concat(chunks).toString() || "{}");
  } catch {
    return null;
  }
}

/** A spool as Spoolman answers it: its filament nested, as `GET /api/v1/spool` does. */
function spoolBody(state, { filament_id: filamentId, ...spool }) {
  return { ...spool, filament: state.filaments.find(({ id }) => id === filamentId) };
}

/**
 * Spoolman's own rule: the whole use counts as used, and the remaining weight it reports
 * stops at zero.
 */
function use(state, spool, grams) {
  const taken = Math.min(grams, spool.remaining_weight);
  spool.remaining_weight = Math.round((spool.remaining_weight - taken) * 1000) / 1000;
  spool.used_weight = Math.round((spool.used_weight + grams) * 1000) / 1000;
  state.uses.push({ spool_id: spool.id, use_weight: grams });
}

/** The lists and the service paths, by `GET` path. */
const READS = {
  "/api/v1/info": () => ({ version: "0.0.0-stand-in" }),
  "/api/v1/health": () => ({ status: "healthy" }),
  "/api/v1/filament": (state) => state.filaments,
  "/api/v1/spool": (state) => state.spools.map((row) => spoolBody(state, row)),
};

/** `GET /api/v1/spool/{id}` and `PUT /api/v1/spool/{id}/use`. */
async function spoolRequest(state, request, response, [, id, isUse]) {
  const spool = state.spools.find((row) => row.id === Number(id));
  if (!spool) return failure(response, 404, `No spool with ID ${id}`);
  if (request.method === "GET" && !isUse) return json(response, 200, spoolBody(state, spool));
  if (request.method !== "PUT" || !isUse) return failure(response, 405, "Method Not Allowed");
  const grams = (await readJson(request))?.use_weight;
  if (typeof grams !== "number" || !Number.isFinite(grams) || grams <= 0) {
    return failure(response, 422, "use_weight must be a positive number");
  }
  use(state, spool, grams);
  return json(response, 200, spoolBody(state, spool));
}

async function answer(state, request, response) {
  const url = new URL(request.url ?? "/", "http://stand-in");
  state.counts.requests += 1;
  state.requests.push({ method: request.method, path: url.pathname });
  if (state.requests.length > MAX_LOG) state.requests.shift();
  const path = url.pathname.replace(/\/+$/, "");
  const read = request.method === "GET" && READS[path];
  if (read) return json(response, 200, read(state));
  const spoolPath = /^\/api\/v1\/spool\/(\d+)(\/use)?$/.exec(path);
  if (spoolPath) return spoolRequest(state, request, response, spoolPath);
  failure(response, 404, "Not Found");
}

function logBody(state) {
  const { counts, uses, requests } = state;
  return { spools: state.spools.map((row) => spoolBody(state, row)), counts, uses, requests };
}

async function control(state, request, response) {
  const path = new URL(request.url ?? "/", "http://stand-in").pathname;
  if (request.method === "GET" && path === "/_stand-in/log") return json(response, 200, logBody(state));
  if (request.method !== "POST" || path !== "/_stand-in/stock") return failure(response, 404, "Not Found");
  const body = await readJson(request);
  const spool = state.spools.find(({ id }) => id === body?.spool_id);
  if (!spool) return failure(response, 400, "spool_id names no spool");
  const grams = body.remaining_weight;
  if (typeof grams !== "number" || !Number.isFinite(grams) || grams < 0) {
    return failure(response, 400, "remaining_weight must be 0 or more");
  }
  spool.remaining_weight = grams;
  return json(response, 200, logBody(state));
}

const listen = (server, port, host) =>
  new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => resolve(server.address().port));
  });

/**
 * Starts the Spoolman and control servers on a fresh copy of STOCK. Returns both ports
 * and `close()`. Nothing here connects out.
 */
export async function startSpoolman({ host = "127.0.0.1", port = 0, controlHost = "127.0.0.1", controlPort = 0 } = {}) {
  const state = {
    ...JSON.parse(JSON.stringify(STOCK)),
    counts: { requests: 0 },
    uses: [],
    requests: [],
  };
  const handle = (serve) => (request, response) => {
    serve(state, request, response).catch(() => response.socket?.destroy());
  };
  const api = createServer(handle(answer));
  const controller = createServer(handle(control));
  const bound = await listen(api, port, host);
  const boundControl = await listen(controller, controlPort, controlHost);
  const close = async () => {
    for (const server of [api, controller]) {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    }
  };
  return { port: bound, controlPort: boundControl, close };
}

/**
 * `docker run` arguments for the stand-in: this file alone, read-only, on the Node image
 * `image`, as the host's `user`, publishing only the control port, on host loopback.
 */
export function spoolmanRunArgs({ container, network, image, user }) {
  return [
    ...["run", "-d", "--name", container, "--user", user],
    ...["--network", network, "--network-alias", SPOOLMAN_ALIAS],
    ...["-p", `127.0.0.1::${SPOOLMAN_CONTROL_PORT}`],
    ...["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"],
    ...["-v", `${import.meta.filename}:/stand-in/spoolman.mjs:ro`],
    ...[image, "node", "/stand-in/spoolman.mjs"],
    // Inside the container's own network namespace: its private interface.
    ...["--host", "0.0.0.0", "--port", String(SPOOLMAN_PORT)],
    ...["--control-host", "0.0.0.0", "--control-port", String(SPOOLMAN_CONTROL_PORT)],
  ];
}

async function main() {
  const { values } = parseArgs({
    options: {
      host: { type: "string", default: "127.0.0.1" },
      port: { type: "string", default: "0" },
      "control-host": { type: "string", default: "127.0.0.1" },
      "control-port": { type: "string", default: "0" },
    },
  });
  const stand = await startSpoolman({
    host: values.host,
    port: Number(values.port),
    controlHost: values["control-host"],
    controlPort: Number(values["control-port"]),
  });
  process.stdout.write(`${JSON.stringify({ port: stand.port, control_port: stand.controlPort })}\n`);
  // As PID 1 in its container, Node has no default handler for these.
  const stop = () => void stand.close().then(() => process.exit(0));
  process.once("SIGTERM", stop);
  process.once("SIGINT", stop);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    process.stderr.write(`spoolman stand-in: ${error.message}\n`);
    process.exit(1);
  });
}
