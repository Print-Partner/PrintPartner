import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import process from "node:process";
import { after, before, describe, it } from "node:test";
import { setTimeout } from "node:timers";
import { URL, fileURLToPath } from "node:url";
import { filePart, startSimulator } from "./moonraker-sim.mjs";

const KEY = "sim-key-canary-5b1e";
const QUERY = "/printer/objects/query?print_stats&virtual_sdcard&display_status&extruder&heater_bed";
const GCODE = Buffer.from("G28\nG1 X10 Y10\n; contents-canary\n");

/** One request; resolves {status, body} or rejects when the connection closes unanswered. */
function call(port, method, path, { key, body, type } = {}) {
  return new Promise((resolve, reject) => {
    const headers = { connection: "close" };
    if (key) headers["x-api-key"] = key;
    if (type) headers["content-type"] = type;
    const req = httpRequest({ host: "127.0.0.1", port, method, path, headers }, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => {
        const text = Buffer.concat(chunks).toString();
        resolve({ status: res.statusCode, body: text ? JSON.parse(text) : null });
      });
    });
    req.on("error", reject);
    req.end(body);
  });
}

function multipart(name, bytes) {
  const boundary = "----sim-boundary";
  const body = Buffer.concat([
    Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="root"\r\n\r\ngcodes\r\n`),
    Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="${name}"\r\nContent-Type: application/octet-stream\r\n\r\n`),
    bytes,
    Buffer.from(`\r\n--${boundary}--\r\n`),
  ]);
  return { body, type: `multipart/form-data; boundary=${boundary}` };
}

describe("moonraker simulator", () => {
  let sim;
  const printer = (method, path, options = {}) => call(sim.printerPort, method, path, { key: KEY, ...options });
  const control = (method, path, body) =>
    call(sim.controlPort, method, path, body ? { body: JSON.stringify(body), type: "application/json" } : {});
  const upload = (name, bytes = GCODE) => printer("POST", "/server/files/upload", multipart(name, bytes));

  before(async () => {
    sim = await startSimulator({ apiKey: KEY });
  });
  after(() => sim.close());

  it("answers server info and a standby status, and refuses a request without the key", async () => {
    const info = await printer("GET", "/server/info");
    assert.equal(info.body.result.klippy_state, "ready");
    const status = await printer("GET", QUERY);
    assert.equal(status.body.result.status.print_stats.state, "standby");
    assert.equal((await call(sim.printerPort, "GET", "/server/info", { key: "wrong" })).status, 401);
    assert.equal((await call(sim.printerPort, "GET", QUERY)).status, 401);
    const log = (await control("GET", "/_sim/log")).body;
    assert.equal(log.counts.unauthorized, 2);
    assert.doesNotMatch(JSON.stringify(log), new RegExp(KEY));
  });

  it("stores an upload by name, size and hash only, and a start only changes its own state", async () => {
    const stored = await upload("bracket.gcode");
    assert.equal(stored.status, 201);
    assert.deepEqual(stored.body.result.item, { path: "bracket.gcode", root: "gcodes" });
    assert.equal((await printer("POST", "/printer/print/start?filename=missing.gcode")).status, 400);
    assert.equal((await printer("POST", "/printer/print/start?filename=bracket.gcode")).body.result, "ok");
    assert.equal((await printer("POST", "/printer/print/start?filename=bracket.gcode")).status, 400);
    const status = (await printer("GET", QUERY)).body.result.status;
    assert.equal(status.print_stats.state, "printing");
    assert.equal(status.print_stats.filename, "bracket.gcode");
    const log = (await control("GET", "/_sim/log")).body;
    assert.deepEqual(log.files, [
      { name: "bracket.gcode", size: GCODE.length, sha256: createHash("sha256").update(GCODE).digest("hex"), uploads: 1 },
    ]);
    assert.equal(log.counts.starts, 1);
    assert.doesNotMatch(JSON.stringify(log), /contents-canary/);
    const listed = (await printer("GET", "/server/files/list?root=gcodes")).body.result;
    assert.deepEqual(listed.map((file) => file.path), ["bracket.gcode"]);
  });

  it("takes state and progress from the control port, and refuses values it cannot report", async () => {
    await control("POST", "/_sim/state", { state: "printing", progress: 0.4 });
    const status = (await printer("GET", QUERY)).body.result.status;
    assert.equal(status.virtual_sdcard.progress, 0.4);
    assert.equal((await control("POST", "/_sim/state", { state: "melting" })).status, 400);
    assert.equal((await control("POST", "/_sim/state", { state: "complete", progress: 2 })).status, 400);
    await control("POST", "/_sim/state", { state: "complete", progress: 1 });
    assert.equal((await printer("GET", QUERY)).body.result.status.print_stats.state, "complete");
  });

  it("closes every printer connection while offline, and keeps the control port", async () => {
    await control("POST", "/_sim/state", { state: "offline" });
    await assert.rejects(printer("GET", "/server/info"));
    assert.equal((await control("GET", "/_sim/log")).body.state, "offline");
    await control("POST", "/_sim/state", { state: "standby", progress: 0 });
    assert.equal((await printer("GET", "/server/info")).status, 200);
  });

  it("loses one upload answer after storing the file, then answers again", async () => {
    await control("POST", "/_sim/fault", { fault: "lose-upload" });
    await assert.rejects(upload("lost.gcode"));
    const log = (await control("GET", "/_sim/log")).body;
    assert.equal(log.files.find((file) => file.name === "lost.gcode").uploads, 1);
    assert.equal(log.fault, null);
    assert.equal((await upload("lost.gcode")).status, 201);
  });

  it("loses one start answer after starting, answers 500 once, and answers late once", async () => {
    await control("POST", "/_sim/fault", { fault: "lose-start" });
    await assert.rejects(printer("POST", "/printer/print/start?filename=lost.gcode"));
    assert.equal((await control("GET", "/_sim/log")).body.state, "printing");
    await control("POST", "/_sim/fault", { fault: "error-500", path: "/server/info" });
    assert.equal((await printer("GET", QUERY)).status, 200);
    assert.equal((await printer("GET", "/server/info")).status, 500);
    assert.equal((await printer("GET", "/server/info")).status, 200);
    await control("POST", "/_sim/fault", { fault: "late", ms: 150 });
    const started = Date.now();
    assert.equal((await printer("GET", "/server/info")).status, 200);
    assert.ok(Date.now() - started >= 140);
    assert.equal((await control("POST", "/_sim/fault", { fault: "explode" })).status, 400);
  });
});

describe("filePart", () => {
  it("finds the file part and ignores the other fields", () => {
    const { body, type } = multipart("a b.gcode", GCODE);
    const part = filePart(body, type);
    assert.equal(part.name, "a b.gcode");
    assert.deepEqual(part.bytes, GCODE);
    assert.equal(filePart(body, "multipart/form-data"), null);
  });
});

describe("moonraker simulator command", () => {
  it("reads the key from a file, prints its ports, never the key, and stops on SIGTERM", async () => {
    const dir = mkdtempSync(join(tmpdir(), "moonraker-sim-"));
    const keyFile = join(dir, "key");
    writeFileSync(keyFile, `${KEY}\n`);
    const script = fileURLToPath(new URL("./moonraker-sim.mjs", import.meta.url));
    const child = spawn(process.execPath, [script, "--api-key-file", keyFile], { stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (chunk) => (output += chunk));
    child.stderr.on("data", (chunk) => (output += chunk));
    try {
      while (!output.includes("\n")) await new Promise((resolve) => setTimeout(resolve, 20));
      const ports = JSON.parse(output.split("\n")[0]);
      assert.equal((await call(ports.printer_port, "GET", "/server/info", { key: KEY })).status, 200);
      assert.equal((await call(ports.printer_port, "GET", "/server/info")).status, 401);
      const exit = new Promise((resolve) => child.once("exit", resolve));
      child.kill("SIGTERM");
      assert.equal(await exit, 0);
      assert.doesNotMatch(output, new RegExp(KEY));
    } finally {
      child.kill("SIGKILL");
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
