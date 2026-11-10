// Replays fixtures/admin/settings-cases.json through the Node app and checks
// the replies against settings-oracle.json, which the Rust routes test
// (rust/crates/pp-api/tests/settings_routes_http.rs) also replays.
// UPDATE_ADMIN_ORACLE=1 rewrites the oracle file instead of checking it.
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { makeApp } from "../test/make-app.js";

const here = new URL("../../../../../fixtures/admin/", import.meta.url);

type Step = { method: "GET" | "PUT"; path: string; body?: string; content_type?: string };
type Cases = { integrations: string; keys: string[]; steps: Step[] };

describe("settings routes oracle", () => {
  let dir: string | null = null;
  afterEach(() => {
    delete process.env.PRINT_PARTNER_DATA_DIR;
    if (dir) rmSync(dir, { recursive: true, force: true });
    dir = null;
  });

  it("date-format, build-tracking and spoolman-default replies match settings-oracle.json", async () => {
    dir = mkdtempSync(join(tmpdir(), "pp-settings-oracle-"));
    const { app, ports } = await makeApp(dir);
    const cases = JSON.parse(readFileSync(new URL("settings-cases.json", here), "utf8")) as Cases;
    const repo = ports.repository;
    repo.setSetting("integrations", cases.integrations);
    const replies: unknown[] = [];
    try {
      for (const step of cases.steps) {
        const response = await app.inject({
          method: step.method,
          url: step.path,
          ...(step.body === undefined
            ? {}
            : { payload: step.body, headers: { "content-type": step.content_type ?? "application/json" } }),
        });
        const stored: Record<string, string | null> = {};
        for (const key of cases.keys) {
          const snapshot = repo.getSettingSnapshot(key);
          stored[key] = snapshot.kind === "stored" ? snapshot.value : null;
        }
        replies.push({
          status: response.statusCode,
          content_type: response.headers["content-type"] ?? null,
          cache_control: response.headers["cache-control"] ?? null,
          body: response.statusCode < 500 ? JSON.parse(response.payload) : null,
          stored,
        });
      }
    } finally {
      await app.close();
    }
    // One reply per line keeps the recorded file reviewable.
    const text = `{"replies": [\n${replies.map((reply) => `  ${JSON.stringify(reply)}`).join(",\n")}\n]}\n`;
    const oracle = new URL("settings-oracle.json", here);
    if (process.env.UPDATE_ADMIN_ORACLE) {
      writeFileSync(oracle, text);
      return;
    }
    expect(JSON.parse(text)).toEqual(JSON.parse(readFileSync(oracle, "utf8")));
  });
});
