import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { buildApp } from "../app.js";
import { loadConfig } from "../config.js";

async function makeApp(directory: string, integrationApiKey: string | null = "external-tool-key") {
  const config = {
    ...loadConfig(),
    dataDir: directory,
    integrationApiKey,
  };
  const ports = createSelfHostPorts(directory);
  await ports.db.connect();
  const app = await buildApp(config, ports);
  return { app, ports };
}

const initializeRequest = {
  jsonrpc: "2.0",
  id: 1,
  method: "initialize",
  params: {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "test", version: "0.0.1" },
  },
};

describe("/settings/external-access", () => {
  afterEach(() => {
    delete process.env.PRINT_PARTNER_DATA_DIR;
    delete process.env.PRINT_PARTNER_API_KEY;
  });

  it("defaults existing installations to API and MCP access", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-external-access-default-"));
    const { app, ports } = await makeApp(directory);

    try {
      const response = await app.inject({ method: "GET", url: "/settings/external-access" });
      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({ mode: "api_and_mcp" });
    } finally {
      await app.close();
      ports.db.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("turns off external API keys and MCP without breaking browser API access", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-external-access-off-"));
    const { app, ports } = await makeApp(directory, null);

    try {
      const saved = await app.inject({
        method: "PUT",
        url: "/settings/external-access",
        payload: { mode: "off" },
      });
      expect(saved.statusCode).toBe(200);
      expect(saved.json()).toEqual({ mode: "off" });

      const externalApi = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: "203.0.113.10",
        headers: { authorization: "Bearer external-tool-key" },
      });
      expect(externalApi.statusCode).toBe(403);

      const externalApiWithoutKey = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: "203.0.113.10",
      });
      expect(externalApiWithoutKey.statusCode).toBe(403);

      const forgedBrowserHeaders = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: "203.0.113.10",
        headers: {
          host: "printpartner.example",
          referer: "https://printpartner.example/plans/12",
          "sec-fetch-dest": "empty",
          "sec-fetch-mode": "cors",
          "sec-fetch-site": "same-origin",
        },
      });
      expect(forgedBrowserHeaders.statusCode).toBe(403);

      const mcp = await app.inject({
        method: "POST",
        url: "/api/v1/mcp",
        remoteAddress: "203.0.113.10",
        headers: {
          authorization: "Bearer external-tool-key",
          "content-type": "application/json",
          accept: "application/json, text/event-stream",
        },
        payload: initializeRequest,
      });
      expect(mcp.statusCode).toBe(403);
      expect(mcp.json().detail).toMatch(/turned off/i);

      const browserApi = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: "192.168.200.50",
      });
      expect(browserApi.statusCode).toBe(200);

      const help = await app.inject({ method: "GET", url: "/help/workflow" });
      expect(help.body).not.toContain("/api/v1/openapi.json");
    } finally {
      await app.close();
      ports.db.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("allows API keys while keeping MCP off and persists the choice", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-external-access-api-"));
    const first = await makeApp(directory);

    try {
      const saved = await first.app.inject({
        method: "PUT",
        url: "/settings/external-access",
        payload: { mode: "api" },
      });
      expect(saved.statusCode).toBe(200);

      const externalApi = await first.app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: "203.0.113.10",
        headers: { authorization: "Bearer external-tool-key" },
      });
      expect(externalApi.statusCode).toBe(200);

      const mcp = await first.app.inject({
        method: "POST",
        url: "/api/v1/mcp",
        remoteAddress: "203.0.113.10",
        headers: {
          authorization: "Bearer external-tool-key",
          "content-type": "application/json",
          accept: "application/json, text/event-stream",
        },
        payload: initializeRequest,
      });
      expect(mcp.statusCode).toBe(403);

      const help = await first.app.inject({ method: "GET", url: "/help/workflow" });
      expect(help.body).toContain("/api/v1/openapi.json");
    } finally {
      await first.app.close();
      first.ports.db.close();
    }

    const second = await makeApp(directory);
    try {
      const restored = await second.app.inject({
        method: "GET",
        url: "/settings/external-access",
      });
      expect(restored.json()).toEqual({ mode: "api" });
    } finally {
      await second.app.close();
      second.ports.db.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("rejects unknown modes without changing the saved setting", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-external-access-invalid-"));
    const { app, ports } = await makeApp(directory);

    try {
      const invalid = await app.inject({
        method: "PUT",
        url: "/settings/external-access",
        payload: { mode: "sometimes" },
      });
      expect(invalid.statusCode).toBe(400);

      const current = await app.inject({ method: "GET", url: "/settings/external-access" });
      expect(current.json()).toEqual({ mode: "api_and_mcp" });
    } finally {
      await app.close();
      ports.db.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
});
