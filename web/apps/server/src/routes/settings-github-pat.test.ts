import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { makeApp } from "../test/make-app.js";

describe("GitHub token settings", () => {
  afterEach(() => {
    delete process.env.PRINT_PARTNER_DATA_DIR;
  });

  it("saves, replaces and clears the token without exposing it, including after restart", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-github-token-"));
    const first = await makeApp(directory);
    try {
      for (const token of ["synthetic-first-token", "synthetic-replacement-token", ""]) {
        const saved = await first.app.inject({
          method: "PUT",
          url: "/settings/github-pat",
          payload: { token },
        });
        expect(saved.statusCode).toBe(200);
        expect(saved.json().configured).toBe(Boolean(token));
        expect(first.ports.repository!.getSetting("github_pat")).toBe(token || null);
        if (token) expect(saved.body).not.toContain(token);
        const status = await first.app.inject({ method: "GET", url: "/settings/github-pat" });
        expect(status.json().configured).toBe(Boolean(token));
        if (token) expect(status.body).not.toContain(token);
      }
    } finally {
      await first.app.close();
      first.ports.db.close();
    }
    const restarted = await makeApp(directory);
    try {
      const status = await restarted.app.inject({ method: "GET", url: "/settings/github-pat" });
      expect(status.json().configured).toBe(false);
    } finally {
      await restarted.app.close();
      restarted.ports.db.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("rejects malformed token writes without changing the saved token", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-github-token-invalid-"));
    const { app, ports } = await makeApp(directory);
    try {
      ports.repository!.setSetting("github_pat", "synthetic-retained-token");
      for (const payload of [{}, { token: null }, { token: 123 }, { token: {} }]) {
        const response = await app.inject({ method: "PUT", url: "/settings/github-pat", payload });
        expect(response.statusCode).toBe(400);
        expect(ports.repository!.getSetting("github_pat")).toBe("synthetic-retained-token");
      }
    } finally {
      await app.close();
      ports.db.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
});
