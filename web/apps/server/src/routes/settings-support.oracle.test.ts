import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it } from "vitest";
import { makeApp } from "../test/make-app.js";

it("records static help and OpenAPI shapes from Node", async () => {
  const directory = mkdtempSync(join(tmpdir(), "pp-support-oracle-"));
  const previousMode = process.env.NODE_ENV;
  process.env.SINGLE_USER_AUTH = "1";
  process.env.NODE_ENV = "production";
  const production = await makeApp(directory);
  let document;
  try {
    document = await production.app.inject({ method: "GET", url: "/api/v1/openapi.json" });
    expect(document.statusCode).toBe(200);
  } finally {
    await production.app.close();
    await production.ports.db.close();
    delete process.env.SINGLE_USER_AUTH;
    process.env.NODE_ENV = previousMode;
  }
  const { app, ports } = await makeApp(directory);
  try {
    ports.repository!.setSetting("external_access_mode", "off");
    const off = await app.inject({ method: "GET", url: "/help/workflow" });
    ports.repository!.setSetting("external_access_mode", "api");
    const on = await app.inject({ method: "GET", url: "/help/workflow" });
    const { paths, ...metadata } = document.json();
    const groups = new Map<string, string[]>();
    for (const [path, shape] of Object.entries(paths)) {
      const key = JSON.stringify(shape);
      groups.set(key, [...(groups.get(key) ?? []), path]);
    }
    const routeGroups = [...groups].map(([shape, paths]) => ({ shape: JSON.parse(shape), paths }));
    const cases = JSON.parse(
      readFileSync(
        new URL("../../../../../fixtures/admin/bindings-cases.json", import.meta.url),
        "utf8",
      ),
    );
    const bindings = [];
    for (const step of cases) {
      const reply = await app.inject({
        method: step.method,
        url: step.path,
        payload: step.body,
        headers: step.body ? { "content-type": step.content_type ?? "application/json" } : {},
      });
      const body = reply.json();
      for (const entry of body.bindings ?? [])
        if (entry.updated_at) entry.updated_at = "<timestamp>";
      const stored = JSON.parse(ports.repository!.getSetting("printer.plan_bindings") ?? "null");
      for (const entry of stored ?? []) if (entry.updated_at) entry.updated_at = "<timestamp>";
      bindings.push({ status: reply.statusCode, body, stored });
    }
    const value = { metadata, route_groups, help_off: off.body, help_on: on.body, bindings };
    const file = new URL("../../../../../fixtures/admin/settings-support.json", import.meta.url);
    if (process.env.UPDATE_ADMIN_ORACLE) {
      // Group repeated shapes; each line names one shape and all its aliases.
      const lines = routeGroups.map((group) => `    ${JSON.stringify(group)}`).join(",\n");
      writeFileSync(
        file,
        `{\n  "metadata": ${JSON.stringify(metadata)},\n  "route_groups": [\n${lines}\n  ],\n  "help_off": ${JSON.stringify(off.body)},\n  "help_on": ${JSON.stringify(on.body)},\n  "bindings": ${JSON.stringify(bindings)}\n}\n`,
      );
    } else expect(value).toEqual(JSON.parse(readFileSync(file, "utf8")));
  } finally {
    await app.close();
    await ports.db.close();
    rmSync(directory, { recursive: true, force: true });
    delete process.env.PRINT_PARTNER_DATA_DIR;
  }
});

it("Node keeps OpenAPI public while protecting help when sign-in is required", async () => {
  const directory = mkdtempSync(join(tmpdir(), "pp-support-auth-"));
  process.env.SINGLE_USER_AUTH = "1";
  const { app, ports } = await makeApp(directory);
  try {
    for (const url of [
      "/openapi.json",
      "/api/v1/openapi.json",
      "/api/v2/openapi.json",
      "/help/workflow",
    ]) {
      const reply = await app.inject({ method: "GET", url });
      expect(reply.statusCode).toBe(
        url === "/openapi.json" ? 302 : url === "/help/workflow" ? 401 : 200,
      );
    }
  } finally {
    await app.close();
    await ports.db.close();
    rmSync(directory, { recursive: true, force: true });
    delete process.env.SINGLE_USER_AUTH;
    delete process.env.PRINT_PARTNER_DATA_DIR;
  }
});
