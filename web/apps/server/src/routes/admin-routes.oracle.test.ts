// Replays fixtures/admin through the Node app and checks the replies against
// node-oracle.json and settings-oracle.json, which the Rust routes tests
// (rust/crates/pp-api/tests/admin_http.rs and
// settings_routes_http.rs) also replay.
// UPDATE_ADMIN_ORACLE=1 rewrites the oracle files instead of checking them.
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { makeApp } from "../test/make-app.js";

const here = new URL("../../../../../fixtures/admin/", import.meta.url);
const LEGAL = ["summary", "license", "attribution", "third-party"];
const PREFIXES = ["", "/api/v1"];

type Case = { absent?: true; directory?: true; text?: string; base64?: string };
type Step = { method: "GET" | "PUT"; path: string; body?: string; content_type?: string };
type SettingsCases = { integrations: string; keys: string[]; steps: Step[] };

function matchOracle(file: string, text: string) {
  const oracle = new URL(file, here);
  if (process.env.UPDATE_ADMIN_ORACLE) {
    writeFileSync(oracle, text);
    return;
  }
  expect(JSON.parse(text)).toEqual(JSON.parse(readFileSync(oracle, "utf8")));
}

function reply(response: Awaited<ReturnType<Awaited<ReturnType<typeof makeApp>>["app"]["inject"]>>) {
  return {
    status: response.statusCode,
    content_type: response.headers["content-type"] ?? null,
    cache_control: response.headers["cache-control"] ?? null,
    content_length: response.headers["content-length"] ?? null,
  };
}

describe("admin routes oracle", () => {
  let dir: string | null = null;
  afterEach(() => {
    delete process.env.PRINT_PARTNER_DATA_DIR;
    if (dir) rmSync(dir, { recursive: true, force: true });
    dir = null;
  });

  it("legal and custom filament replies match node-oracle.json", async () => {
    dir = mkdtempSync(join(tmpdir(), "pp-admin-oracle-"));
    const { app } = await makeApp(dir);
    const store = join(dir, "custom_filaments.json");
    const { cases } = JSON.parse(readFileSync(new URL("custom-filaments-cases.json", here), "utf8")) as {
      cases: Record<string, Case>;
    };
    const out: Record<string, Record<string, unknown>> = { legal: {}, custom_filaments: {} };
    try {
      for (const name of LEGAL) {
        for (const prefix of PREFIXES) {
          const url = `${prefix}/legal/${name}`;
          const get = await app.inject({ method: "GET", url });
          const head = await app.inject({ method: "HEAD", url });
          out.legal[url] = {
            ...reply(get),
            body_sha256: createHash("sha256").update(get.rawPayload).digest("hex"),
            head: { ...reply(head), body_length: head.rawPayload.length },
          };
        }
      }
      for (const [name, fixture] of Object.entries(cases)) {
        rmSync(store, { recursive: true, force: true });
        if (fixture.directory) mkdirSync(store);
        else if (fixture.text !== undefined) writeFileSync(store, fixture.text, "utf8");
        else if (fixture.base64 !== undefined) writeFileSync(store, Buffer.from(fixture.base64, "base64"));
        const replies: Record<string, unknown> = {};
        for (const prefix of PREFIXES) {
          const get = await app.inject({ method: "GET", url: `${prefix}/filaments/custom` });
          replies[prefix || "/"] = { ...reply(get), body: get.payload };
        }
        const head = await app.inject({ method: "HEAD", url: "/filaments/custom" });
        replies.head = { ...reply(head), body_length: head.rawPayload.length };
        out.custom_filaments[name] = replies;
      }
    } finally {
      await app.close();
    }
    matchOracle("node-oracle.json", `${JSON.stringify(out, null, 2)}\n`);
  });

  it.each(["settings", "secret-settings"])("%s replies match the Node oracle", async (suite) => {
    dir = mkdtempSync(join(tmpdir(), "pp-settings-oracle-"));
    const { app, ports } = await makeApp(dir);
    const cases = JSON.parse(readFileSync(new URL(`${suite}-cases.json`, here), "utf8")) as SettingsCases;
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
    const lines = replies.map((entry) => `  ${JSON.stringify(entry)}`).join(",\n");
    matchOracle(`${suite}-oracle.json`, `{"replies": [\n${lines}\n]}\n`);
  });
});
