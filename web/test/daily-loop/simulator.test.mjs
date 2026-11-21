import assert from "node:assert/strict";
import test from "node:test";
import {
  CONTROL_PORT,
  PRINTER_URL,
  isPrivateIPv4,
  nodeImage,
  parseAnswer,
  publishedOffLoopback,
  simulatorRunArgs,
} from "./simulator.mjs";

test("the simulator runs on the Node image Dockerfile.rust's web stage pins", () => {
  assert.match(nodeImage(), /^node:[\w.-]+@sha256:[0-9a-f]{64}$/);
  const digest = "a".repeat(64);
  assert.equal(
    nodeImage(`FROM node:24-slim@sha256:${digest} AS web\nFROM rust:1 AS rust\n`),
    `node:24-slim@sha256:${digest}`,
  );
  assert.throws(() => nodeImage("FROM node:24-slim AS web\n"), /pinned/);
});

test("the simulator is pp-sim on the run's network and publishes only its control port, on loopback", () => {
  const args = simulatorRunArgs({
    container: "pp-daily-loop-0a1b2c3d-sim",
    network: "pp-daily-loop-0a1b2c3d",
    image: "node:24@sha256:0",
    keyDir: "/tmp/key-dir",
    user: "1000:1000",
  });
  const after = (flag) => args.filter((_, index) => args[index - 1] === flag);
  assert.deepEqual(after("--network"), ["pp-daily-loop-0a1b2c3d"]);
  assert.deepEqual(after("--network-alias"), ["pp-sim"]);
  assert.deepEqual(after("-p"), [`127.0.0.1::${CONTROL_PORT}`]);
  assert.ok(!args.includes("--publish") && !args.includes("-P"));
  assert.ok(after("-v").includes("/tmp/key-dir:/key:ro"));
  assert.deepEqual(after("--api-key-file"), ["/key/api-key"]);
  assert.equal(PRINTER_URL, "http://pp-sim:7125");
});

test("a port published anywhere but host loopback is named", () => {
  const loopback = { "7200/tcp": [{ HostIp: "127.0.0.1", HostPort: "32768" }], "7125/tcp": null };
  assert.deepEqual(publishedOffLoopback(loopback), []);
  assert.deepEqual(publishedOffLoopback({}), []);
  assert.deepEqual(
    publishedOffLoopback({
      "8080/tcp": [
        { HostIp: "127.0.0.1", HostPort: "32769" },
        { HostIp: "172.18.0.1", HostPort: "32770" },
      ],
      "7200/tcp": [{ HostIp: "", HostPort: "32771" }],
      "7125/tcp": [{ HostIp: "0.0.0.0", HostPort: "7125" }],
    }),
    ["8080/tcp on 172.18.0.1:32770", "7200/tcp on all interfaces:32771", "7125/tcp on 0.0.0.0:7125"],
  );
});

test("only RFC 1918 addresses count as private", () => {
  for (const address of ["10.0.0.2", "172.16.0.2", "172.31.255.254", "192.168.1.5"]) {
    assert.ok(isPrivateIPv4(address), address);
  }
  for (const address of ["172.15.0.1", "172.32.0.1", "127.0.0.1", "169.254.1.1", "8.8.8.8", "", "10.0.0", "10.0.0.256"]) {
    assert.ok(!isPrivateIPv4(address), address);
  }
});

test("a raw HTTP answer gives its status and body", () => {
  const text = 'HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{"result":{"klippy_state":"ready"}}';
  assert.deepEqual(parseAnswer(text), { status: 200, body: '{"result":{"klippy_state":"ready"}}' });
  assert.deepEqual(parseAnswer(""), { status: null, body: "" });
});
