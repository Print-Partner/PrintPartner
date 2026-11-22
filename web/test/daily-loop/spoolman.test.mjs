import assert from "node:assert/strict";
import { createServer } from "node:net";
import test from "node:test";
import {
  SPOOLMAN_CONTROL_PORT,
  SPOOLMAN_URL,
  STOCK,
  spoolmanRunArgs,
  startSpoolman,
} from "./spoolman.mjs";

const { fetch } = globalThis;

/** Runs `use(api, control)` against a new stand-in on loopback, and stops it after. */
async function withStandIn(use) {
  const stand = await startSpoolman();
  const at = (port) => async (path, init) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, init);
    return { status: response.status, body: await response.json() };
  };
  try {
    await use(at(stand.port), at(stand.controlPort));
  } finally {
    await stand.close();
  }
}

const put = (body) => ({
  method: "PUT",
  headers: { "content-type": "application/json" },
  body: JSON.stringify(body),
});
const post = (body) => ({ ...put(body), method: "POST" });

test("the stand-in is pp-spoolman on the run's network and publishes only its control port, on loopback", () => {
  const args = spoolmanRunArgs({
    container: "pp-daily-loop-0a1b2c3d-spoolman",
    network: "pp-daily-loop-0a1b2c3d",
    image: "node:24@sha256:0",
    user: "1000:1000",
  });
  const after = (flag) => args.filter((_, index) => args[index - 1] === flag);
  assert.deepEqual(after("--name"), ["pp-daily-loop-0a1b2c3d-spoolman"]);
  assert.deepEqual(after("--network"), ["pp-daily-loop-0a1b2c3d"]);
  assert.deepEqual(after("--network-alias"), ["pp-spoolman"]);
  assert.deepEqual(after("-p"), [`127.0.0.1::${SPOOLMAN_CONTROL_PORT}`]);
  assert.ok(!args.includes("--publish") && !args.includes("-P"));
  assert.ok(args.includes("--read-only"));
  assert.deepEqual(after("-v"), [`${import.meta.dirname}/spoolman.mjs:/stand-in/spoolman.mjs:ro`]);
  assert.equal(SPOOLMAN_URL, "http://pp-spoolman:7912");
});

test("it answers the filament and spool lists the shortfall check reads", async () => {
  await withStandIn(async (api) => {
    assert.deepEqual(await api("/api/v1/filament"), { status: 200, body: STOCK.filaments });
    const spools = await api("/api/v1/spool");
    assert.equal(spools.status, 200);
    assert.deepEqual(
      spools.body.map(({ id, remaining_weight, archived, filament }) => [id, remaining_weight, archived, filament.id]),
      [
        [1, 750, false, 1],
        [2, 120, false, 1],
        [3, 1000, true, 1],
      ],
    );
    assert.equal((await api("/api/v1/spool/2")).body.remaining_weight, 120);
    assert.equal((await api("/api/v1/spool/9")).status, 404);
    assert.equal((await api("/api/v1/info")).status, 200);
  });
});

test("a use takes its grams off one spool once, and the log records each use", async () => {
  await withStandIn(async (api, control) => {
    const used = await api("/api/v1/spool/1/use", put({ use_weight: 12.5 }));
    assert.equal(used.status, 200);
    assert.equal(used.body.remaining_weight, 737.5);
    assert.equal(used.body.used_weight, 262.5);
    // A refused use changes nothing.
    assert.equal((await api("/api/v1/spool/1/use", put({ use_weight: -1 }))).status, 422);
    assert.equal((await api("/api/v1/spool/1/use", put({}))).status, 422);
    assert.equal((await api("/api/v1/spool/9/use", put({ use_weight: 1 }))).status, 404);
    // The whole use counts as used; the remaining weight stops at zero.
    const over = await api("/api/v1/spool/2/use", put({ use_weight: 500 }));
    assert.deepEqual([over.body.remaining_weight, over.body.used_weight], [0, 1380]);
    const { body } = await control("/_stand-in/log");
    assert.deepEqual(body.uses, [
      { spool_id: 1, use_weight: 12.5 },
      { spool_id: 2, use_weight: 500 },
    ]);
    assert.deepEqual(
      body.spools.map(({ id, remaining_weight }) => [id, remaining_weight]),
      [
        [1, 737.5],
        [2, 0],
        [3, 1000],
      ],
    );
    assert.equal(body.counts.requests, 5);
    assert.deepEqual(body.requests[0], { method: "PUT", path: "/api/v1/spool/1/use" });
  });
});

test("the control port sets a spool's stock, and the API reads the new grams", async () => {
  await withStandIn(async (api, control) => {
    const set = await control("/_stand-in/stock", post({ spool_id: 2, remaining_weight: 40 }));
    assert.equal(set.status, 200);
    assert.equal((await api("/api/v1/spool/2")).body.remaining_weight, 40);
    assert.equal((await control("/_stand-in/stock", post({ spool_id: 9, remaining_weight: 1 }))).status, 400);
    assert.equal((await control("/_stand-in/stock", post({ spool_id: 2, remaining_weight: -1 }))).status, 400);
    // The control paths are not on the API port, and the API paths are not on the control port.
    assert.equal((await api("/_stand-in/log")).status, 404);
    assert.equal((await control("/api/v1/spool")).status, 404);
    // The harness's reads are not counted as the core's requests.
    assert.equal((await control("/_stand-in/log")).body.counts.requests, 2);
  });
});

test("each stand-in starts from the same stock", async () => {
  await withStandIn(async (api) => {
    await api("/api/v1/spool/1/use", put({ use_weight: 100 }));
  });
  await withStandIn(async (api) => {
    assert.equal((await api("/api/v1/spool/1")).body.remaining_weight, 750);
  });
});

/** A free loopback port: bound, then released. */
async function freePort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

test("a start whose control port is taken rejects and leaves its API port closed", async () => {
  const taken = createServer();
  await new Promise((resolve) => taken.listen(0, "127.0.0.1", resolve));
  const port = await freePort();
  try {
    await assert.rejects(startSpoolman({ port, controlPort: taken.address().port }), { code: "EADDRINUSE" });
    // Nothing answers on the API port the failed start had bound.
    await assert.rejects(fetch(`http://127.0.0.1:${port}/api/v1/info`), (error) => {
      assert.equal(error.cause?.code, "ECONNREFUSED");
      return true;
    });
  } finally {
    await new Promise((resolve) => taken.close(resolve));
  }
});
