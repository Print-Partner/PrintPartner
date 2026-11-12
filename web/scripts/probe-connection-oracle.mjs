// Record the legacy Node connection result with fetch stubbed; no socket is opened.
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { StatusExecution } from "../apps/server/src/integrations/status-execution.ts";
import { moonrakerAdapter } from "../apps/server/src/integrations/adapters/moonraker.ts";
import { prusalinkAdapter } from "../apps/server/src/integrations/adapters/prusalink.ts";

const directory = new URL("../../fixtures/drivers/probe/", import.meta.url);
const cases = JSON.parse(readFileSync(new URL("connection-cases.json", directory), "utf8"));
const results = {};
const originalFetch = globalThis.fetch;
try {
  for (const scenario of cases) {
    const requests = [];
    globalThis.fetch = async (url, init) => {
      const target = new URL(url).pathname;
      const method = init?.method ?? "GET";
      assert.equal(method, "GET");
      requests.push(`${method} ${target}`);
      const [status, body] = scenario.routes[target] ?? [404, {}];
      return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
    };
    const execution = new StatusExecution();
    const adapter = scenario.kind === "moonraker" ? moonrakerAdapter : prusalinkAdapter;
    const result = await execution.test(adapter, {
      base_url: "http://127.0.0.1:9999", username: "fixture-user", password: "synthetic-fixture-password",
    });
    await execution.stop();
    results[scenario.name] = { result, requests };
  }
} finally {
  globalThis.fetch = originalFetch;
}
const oracle = new URL("node-connections.json", directory);
if (process.argv.includes("--write")) {
  writeFileSync(oracle, `${JSON.stringify(results, null, 2)}\n`);
} else {
  assert.deepEqual(results, JSON.parse(readFileSync(oracle, "utf8")));
}
process.stdout.write(`Verified ${cases.length} Node connection cases with stubbed fetch.\n`);
