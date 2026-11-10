// Replays fixtures/admin through the Node app and checks the replies against
// node-oracle.json, which the Rust routes test
// (rust/crates/pp-api/tests/admin_http.rs) also replays.
// UPDATE_ADMIN_ORACLE=1 rewrites the oracle file instead of checking it.
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
    const text = `${JSON.stringify(out, null, 2)}\n`;
    const oracle = new URL("node-oracle.json", here);
    if (process.env.UPDATE_ADMIN_ORACLE) {
      writeFileSync(oracle, text);
      return;
    }
    expect(JSON.parse(text)).toEqual(JSON.parse(readFileSync(oracle, "utf8")));
  });
});
