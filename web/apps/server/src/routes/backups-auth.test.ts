import { describe, expect, it } from "vitest";
import { mkdtempSync, rmSync, truncateSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { buildApp } from "../app.js";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { loadConfig } from "../config.js";
import {
  KIT_BUNDLE_UPLOAD_TOO_LARGE_DETAIL,
  MAX_KIT_BUNDLE_UPLOAD_BYTES,
} from "../services/upload-limits.js";

describe("administrative route authentication", () => {
  it("allows unambiguous loopback administration", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-admin-loopback-"));
    const config = { ...loadConfig(), dataDir: dir };
    const ports = createSelfHostPorts(dir);
    await ports.db.connect();
    const app = await buildApp(config, ports);

    try {
      const response = await app.inject({
        method: "GET",
        url: "/backups",
        remoteAddress: "127.0.0.1",
      });

      expect(response.statusCode).toBe(200);
    } finally {
      await app.close();
      ports.db.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("disables loopback administration when peer identity is ambiguous", async () => {
    const cases = [
      { name: "proxy trust", config: { trustProxy: true, authRequired: false } },
      { name: "required authentication", config: { trustProxy: false, authRequired: true } },
    ];

    for (const testCase of cases) {
      const dir = mkdtempSync(join(tmpdir(), "pp-admin-ambiguous-"));
      const config = {
        ...loadConfig(),
        dataDir: dir,
        multiUser: false,
        ...testCase.config,
      };
      const ports = createSelfHostPorts(dir);
      await ports.db.connect();
      const app = await buildApp(config, ports);

      try {
        const response = await app.inject({
          method: "GET",
          url: "/backups",
          remoteAddress: "127.0.0.1",
          headers:
            testCase.name === "proxy trust" ? { "x-forwarded-for": "203.0.113.10" } : undefined,
        });

        expect.soft(response.statusCode, testCase.name).toBe(401);
      } finally {
        await app.close();
        ports.db.close();
        rmSync(dir, { recursive: true, force: true });
      }
    }
  });

  it("rejects unauthenticated non-loopback requests with one policy", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-admin-auth-"));
    const config = { ...loadConfig(), dataDir: dir };
    const ports = createSelfHostPorts(dir);
    await ports.db.connect();
    const app = await buildApp(config, ports);

    try {
      const requests = [
        { method: "GET" as const, url: "/backups" },
        { method: "GET" as const, url: "/settings/api-keys" },
        { method: "GET" as const, url: "/settings/logging/config" },
        { method: "DELETE" as const, url: "/api/v1/webhooks/missing" },
        {
          method: "POST" as const,
          url: "/admin/import-kit-bundle",
          payload: {},
        },
      ];

      for (const request of requests) {
        const response = await app.inject({
          ...request,
          remoteAddress: "203.0.113.10",
        });
        expect.soft(response.statusCode, `${request.method} ${request.url}`).toBe(401);
      }
    } finally {
      await app.close();
      ports.db.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("rejects an oversized local kit bundle before reading it", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-admin-import-limit-"));
    const kitPath = join(dir, "oversized.print-partner-kit.zip");
    writeFileSync(kitPath, "");
    truncateSync(kitPath, MAX_KIT_BUNDLE_UPLOAD_BYTES + 1);
    const config = { ...loadConfig(), dataDir: dir };
    const ports = createSelfHostPorts(dir);
    await ports.db.connect();
    const app = await buildApp(config, ports);

    try {
      const response = await app.inject({
        method: "POST",
        url: "/admin/import-kit-bundle",
        remoteAddress: "127.0.0.1",
        payload: { path: kitPath },
      });

      expect(response.statusCode).toBe(413);
      expect(response.json()).toEqual({ detail: KIT_BUNDLE_UPLOAD_TOO_LARGE_DETAIL });
    } finally {
      await app.close();
      ports.db.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
