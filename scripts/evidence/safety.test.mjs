// node --test scripts/evidence/safety.test.mjs (run.sh --self-test runs it too).
// Proves a failed request prints no cookie: Playwright's own error does carry it.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createServer } from "node:net";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../..");
const COOKIE = "pp_session=evidence-test-cookie-5f0c2a";

/** A loopback port with nothing listening, so a request fails in transport. */
async function closedPort() {
  const server = createServer();
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const { port } = server.address();
  await new Promise((done) => server.close(done));
  return port;
}

/** Runs body in a child Node process that has playwright-core and safety.mjs in scope. */
function child(body) {
  const program = `
    import { createRequire } from "node:module";
    const { request } = createRequire(${JSON.stringify(resolve(root, "web/package.json"))})("playwright-core");
    const safety = await import(${JSON.stringify(pathToFileURL(resolve(here, "safety.mjs")).href)});
    ${body}
  `;
  return spawnSync(process.execPath, ["--input-type=module", "-e", program], { encoding: "utf8", timeout: 60_000 });
}

test("Playwright's own transport error carries the cookie", async () => {
  const port = await closedPort();
  const result = child(`
    const context = await request.newContext({ extraHTTPHeaders: { Cookie: ${JSON.stringify(COOKIE)} } });
    try { await context.fetch("http://127.0.0.1:${port}/api/v2/plans"); }
    catch (error) { console.log(error.message); }
    await context.dispose();
  `);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, new RegExp(COOKIE));
});

test("a failed request through makeApi prints no cookie", async () => {
  const port = await closedPort();
  const origin = `http://127.0.0.1:${port}`;
  const result = child(`
    safety.installReport();
    const context = await request.newContext({ extraHTTPHeaders: { Cookie: ${JSON.stringify(COOKIE)} } });
    const api = safety.makeApi(context, ${JSON.stringify(origin)});
    await api("POST", "/api/v2/plans/2/drafts/header", { x: 1 });
  `);
  const output = result.stdout + result.stderr;
  assert.equal(result.status, 1);
  assert.equal(result.stderr, "capture: POST /api/v2/plans/2/drafts/header: transport error\n");
  assert.doesNotMatch(output, /evidence-test-cookie/);
});

test("an unexpected error prints only its class name", () => {
  const result = child(`
    safety.installReport();
    throw new Error("Cookie: ${COOKIE}");
  `);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /^capture: failed with Error; details withheld/);
  assert.doesNotMatch(result.stdout + result.stderr, /evidence-test-cookie/);
});

test("guarded drops the original message and cause", async () => {
  const { CaptureError, guarded } = await import("./safety.mjs");
  const error = await guarded(Promise.reject(new Error(COOKIE, { cause: COOKIE })), "GET /x: transport error").catch(
    (caught) => caught,
  );
  assert.ok(error instanceof CaptureError);
  assert.equal(error.message, "GET /x: transport error");
  assert.equal(error.cause, undefined);
});
