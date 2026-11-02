import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { describe, expect, it } from "vitest";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { buildApp } from "../app.js";
import { loadConfig, type ServerConfig } from "../config.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import { createApiKey } from "../services/api-key-manager.js";

const REMOTE_ADDRESS = "203.0.113.10";
const PLAN_A = "tenant-a-marker";
const PLAN_B = "tenant-b-marker";
const DEPLOYMENT_KEY = "deployment-key-marker";

const MCP_HEADERS = {
  "content-type": "application/json",
  accept: "application/json, text/event-stream",
};

const INITIALIZE = {
  jsonrpc: "2.0",
  id: 1,
  method: "initialize",
  params: {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "bearer-only-tenant", version: "0" },
  },
};

async function makeApp(dir: string) {
  const config: ServerConfig = {
    ...loadConfig(),
    dataDir: dir,
    deployMode: "self-host",
    host: "0.0.0.0",
    multiUser: true,
    singleUserAuth: false,
    authRequired: true,
    trustProxy: true,
    integrationApiKey: DEPLOYMENT_KEY,
    sessionSecret: "test-session-secret",
    sessionCookieSecure: false,
    basicAuthUser: null,
    basicAuthPass: null,
  };
  const ports = createSelfHostPorts(dir);
  await ports.db.connect();
  const app = await buildApp(config, ports);
  return { app, ports };
}

function cookieOf(response: { cookies: Array<{ name: string; value: string }> }): string {
  const cookie = response.cookies.find((item) => item.name === "pp_session");
  if (!cookie?.value) throw new Error("missing pp_session");
  return cookie.value;
}

function readUser(response: { json: () => unknown }): { user_id: string; is_admin: boolean } {
  const body = response.json();
  if (!body || typeof body !== "object" || !("user" in body)) throw new Error("missing user");
  const user = body.user;
  if (!user || typeof user !== "object" || !("user_id" in user) || !("is_admin" in user)) {
    throw new Error("missing user fields");
  }
  if (typeof user.user_id !== "string" || typeof user.is_admin !== "boolean") {
    throw new Error("bad user fields");
  }
  return { user_id: user.user_id, is_admin: user.is_admin };
}

function readId(response: { json: () => unknown }): number {
  const body = response.json();
  if (!body || typeof body !== "object" || !("id" in body) || typeof body.id !== "number") {
    throw new Error("missing id");
  }
  return body.id;
}

function readKey(response: { json: () => unknown }): string {
  const body = response.json();
  if (!body || typeof body !== "object" || !("key" in body) || typeof body.key !== "string") {
    throw new Error("missing key");
  }
  return body.key;
}

function profileNames(response: { json: () => unknown }): string[] {
  const body = response.json();
  if (!body || typeof body !== "object" || !("profiles" in body) || !Array.isArray(body.profiles)) {
    throw new Error("missing profiles");
  }
  return body.profiles.map((profile) => {
    if (
      !profile ||
      typeof profile !== "object" ||
      !("name" in profile) ||
      typeof profile.name !== "string"
    ) {
      throw new Error("missing profile name");
    }
    return profile.name;
  });
}

describe("multi-user bearer-only tenant keys", () => {
  it("rejects a bearer key that has no login session and keeps each tenant on its own plans", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-bearer-tenant-"));
    const { app, ports } = await makeApp(dir);
    try {
      const admin = await app.inject({
        method: "POST",
        url: "/auth/register",
        remoteAddress: REMOTE_ADDRESS,
        payload: {
          email: "admin@example.com",
          password: "correct-horse-battery",
          display_name: "Admin",
        },
      });
      const member = await app.inject({
        method: "POST",
        url: "/auth/register",
        remoteAddress: REMOTE_ADDRESS,
        payload: {
          email: "member@example.com",
          password: "correct-horse-battery",
          display_name: "Member",
        },
      });
      expect(admin.statusCode).toBe(200);
      expect(member.statusCode).toBe(200);
      expect(readUser(admin)).toEqual({ user_id: "default", is_admin: true });
      const memberUser = readUser(member);
      expect(memberUser.is_admin).toBe(false);
      expect(memberUser.user_id).not.toBe("default");

      const adminCookie = cookieOf(admin);
      const memberCookie = cookieOf(member);
      const planA = await app.inject({
        method: "POST",
        url: "/plans",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: adminCookie },
        payload: { name: PLAN_A },
      });
      const planB = await app.inject({
        method: "POST",
        url: "/plans",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: memberCookie },
        payload: { name: PLAN_B },
      });
      expect(planA.statusCode).toBe(200);
      expect(planB.statusCode).toBe(200);
      const planAId = readId(planA);
      const planBId = readId(planB);

      const adminKey = await app.inject({
        method: "POST",
        url: "/settings/api-keys",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: adminCookie },
      });
      const memberKey = await app.inject({
        method: "POST",
        url: "/settings/api-keys",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: memberCookie },
      });
      expect(adminKey.statusCode).toBe(201);
      expect(memberKey.statusCode).toBe(403);
      expect(memberKey.json()).toEqual({
        detail: "Administrator access required",
        title: "Forbidden",
        status: 403,
      });
      const settingsKey = readKey(adminKey);
      const repository = ports.repository;
      if (!repository) throw new Error("missing repository");
      const planted = tenantStorage.run(memberUser.user_id, () => createApiKey(repository));

      const adminList = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: adminCookie },
      });
      const memberList = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: memberCookie },
      });
      expect(adminList.statusCode).toBe(200);
      expect(memberList.statusCode).toBe(200);
      expect(profileNames(adminList)).toEqual([PLAN_A]);
      expect(profileNames(memberList)).toEqual([PLAN_B]);

      const withOwnKey = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: adminCookie },
        headers: { authorization: `Bearer ${settingsKey}` },
      });
      expect(withOwnKey.statusCode).toBe(200);
      expect(profileNames(withOwnKey)).toEqual([PLAN_A]);

      const crossKey = await app.inject({
        method: "GET",
        url: "/api/v1/plans",
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: adminCookie },
        headers: { authorization: `Bearer ${planted.key}` },
      });
      expect(crossKey.statusCode).toBe(401);
      expect(crossKey.json()).toEqual({
        detail: "Valid API key required",
        title: "Unauthorized",
        status: 401,
      });

      const hiddenFromAdmin = await app.inject({
        method: "GET",
        url: `/api/v1/plans/${planBId}`,
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: adminCookie },
      });
      const hiddenFromMember = await app.inject({
        method: "GET",
        url: `/api/v1/plans/${planAId}`,
        remoteAddress: REMOTE_ADDRESS,
        cookies: { pp_session: memberCookie },
      });
      expect(hiddenFromAdmin.statusCode).toBe(404);
      expect(hiddenFromAdmin.json()).toEqual({ detail: "Profile not found" });
      expect(hiddenFromMember.statusCode).toBe(404);
      expect(hiddenFromMember.json()).toEqual({ detail: "Profile not found" });

      const sessionRejection = { detail: "Authentication required" };
      const bearerCases = [
        {
          method: "GET" as const,
          url: "/api/v1/plans",
          headers: { authorization: `Bearer ${settingsKey}` },
        },
        {
          method: "GET" as const,
          url: "/api/v1/plans",
          headers: { authorization: `Bearer ${planted.key}` },
        },
        {
          method: "GET" as const,
          url: "/api/v1/plans",
          headers: { "x-print-partner-api-key": planted.key },
        },
        {
          method: "GET" as const,
          url: "/api/v1/plans",
          headers: { authorization: `Bearer ${DEPLOYMENT_KEY}` },
        },
        {
          method: "GET" as const,
          url: "/plans",
          headers: { authorization: `Bearer ${settingsKey}` },
        },
        {
          method: "GET" as const,
          url: `/api/v1/plans/${planAId}`,
          headers: { authorization: `Bearer ${planted.key}` },
        },
        {
          method: "GET" as const,
          url: `/api/v1/plans/${planBId}`,
          headers: { authorization: `Bearer ${settingsKey}` },
        },
      ];
      for (const item of bearerCases) {
        const response = await app.inject({
          method: item.method,
          url: item.url,
          remoteAddress: REMOTE_ADDRESS,
          headers: item.headers,
        });
        expect(response.statusCode).toBe(401);
        expect(response.json()).toEqual(sessionRejection);
        expect(response.body).not.toContain(PLAN_A);
        expect(response.body).not.toContain(PLAN_B);
      }

      const mcpCases = [settingsKey, planted.key, DEPLOYMENT_KEY, undefined];
      for (const key of mcpCases) {
        const response = await app.inject({
          method: "POST",
          url: "/api/v1/mcp",
          remoteAddress: REMOTE_ADDRESS,
          headers: key ? { ...MCP_HEADERS, authorization: `Bearer ${key}` } : MCP_HEADERS,
          payload: INITIALIZE,
        });
        expect(response.statusCode).toBe(401);
        expect(response.json()).toEqual(sessionRejection);
        expect(response.body).not.toContain(PLAN_A);
        expect(response.body).not.toContain(PLAN_B);
        expect(response.body).not.toContain("protocolVersion");
      }
    } finally {
      await app.close();
      ports.db.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
