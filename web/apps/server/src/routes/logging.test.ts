import { afterEach, describe, expect, it } from "vitest";
import compress from "@fastify/compress";
import Fastify from "fastify";
import { getLogger } from "../services/logger.js";
import { registerLoggingRoutes } from "./logging.js";

const apps: Array<ReturnType<typeof Fastify>> = [];

afterEach(async () => {
  getLogger().setMinSeverity("info");
  getLogger().setWorkflowTracking(true);
  getLogger().clear();
  for (const app of apps.splice(0)) await app.close();
});

async function fixture() {
  const app = Fastify();
  apps.push(app);
  await registerLoggingRoutes(app);
  getLogger().clear();
  getLogger().logWorkflow({
    method: "POST",
    url: "/plans/12/drafts/40/apply",
    duration: 42,
    statusCode: 422,
    severity: "warn",
    message: "Plan publication did not complete",
  });
  return app;
}

describe("logging routes", () => {
  it("turns workflow capture off and back on without losing existing logs", async () => {
    const app = await fixture();
    const before = getLogger().getLogs();
    try {
      const disabled = await app.inject({
        method: "POST",
        url: "/settings/logging/config",
        payload: { enableWorkflowTracking: false },
      });
      expect(disabled.statusCode).toBe(200);
      expect(disabled.json().enableWorkflowTracking).toBe(false);
      expect((await app.inject("/settings/logging/config")).json().enableWorkflowTracking).toBe(
        false,
      );
      getLogger().logWorkflow({
        method: "GET",
        url: "/disabled",
        duration: 1,
        statusCode: 200,
        severity: "info",
        message: "Not captured",
      });
      getLogger().logWorkflow({
        method: "GET",
        url: "/disabled-error",
        duration: 1,
        statusCode: 500,
        severity: "error",
        message: "Not captured",
      });
      expect(getLogger().getLogs()).toEqual(before);
      const enabled = await app.inject({
        method: "POST",
        url: "/settings/logging/config",
        payload: { enableWorkflowTracking: true },
      });
      expect(enabled.json().enableWorkflowTracking).toBe(true);
      getLogger().logWorkflow({
        method: "GET",
        url: "/enabled",
        duration: 1,
        statusCode: 200,
        severity: "info",
        message: "Captured",
      });
      expect(getLogger().getLogs()).toHaveLength(before.length + 1);
    } finally {
      await app.inject({
        method: "POST",
        url: "/settings/logging/config",
        payload: { enableWorkflowTracking: true },
      });
    }
  });

  it("rejects invalid configuration before applying any setting", async () => {
    const app = await fixture();
    const before = getLogger().getConfig();
    const response = await app.inject({
      method: "POST",
      url: "/settings/logging/config",
      payload: { minSeverity: "debug", enableWorkflowTracking: "invalid" },
    });
    expect(response.statusCode).toBe(400);
    expect(getLogger().getConfig()).toEqual(before);
  });

  it("returns recent workflow logs for the in-app viewer", async () => {
    const app = await fixture();
    const response = await app.inject({
      method: "GET",
      url: "/settings/logging/logs?limit=100",
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toEqual([
      expect.objectContaining({
        method: "POST",
        url: "/plans/12/drafts/40/apply",
        statusCode: 422,
        severity: "warn",
      }),
    ]);
  });

  it("exports logs through the read-only URL used by Settings", async () => {
    const app = await fixture();
    const response = await app.inject({
      method: "GET",
      url: "/settings/logging/export?format=json",
    });

    expect(response.statusCode).toBe(200);
    expect(response.headers["content-type"]).toContain("application/json");
    expect(response.json()).toMatchObject({
      logs: [expect.objectContaining({ url: "/plans/12/drafts/40/apply" })],
    });
  });

  it("keeps large diagnostic responses intact for browsers that request compression", async () => {
    const app = Fastify();
    apps.push(app);
    await app.register(compress, { global: true });
    await registerLoggingRoutes(app);
    getLogger().clear();
    const logCount = 12;
    for (let index = 0; index < logCount; index += 1) {
      getLogger().logWorkflow({
        method: "GET",
        url: `/diagnostics/${index}`,
        duration: index,
        statusCode: 200,
        severity: "info",
        message: "A sufficiently large diagnostic entry for compression coverage",
      });
    }

    const response = await app.inject({
      method: "GET",
      url: "/settings/logging/logs?limit=100",
      headers: { "accept-encoding": "gzip" },
    });

    expect(response.statusCode).toBe(200);
    expect(response.headers["content-encoding"]).toBeUndefined();
    expect(response.json()).toHaveLength(logCount);
  });
});
