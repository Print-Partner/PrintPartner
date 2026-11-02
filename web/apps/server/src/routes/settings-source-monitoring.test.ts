import { afterEach, describe, expect, it } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { makeApp } from "../test/make-app.js";

afterEach(() => {
  delete process.env.PRINT_PARTNER_DATA_DIR;
});

describe("/settings/source-update-check", () => {
  it("keeps the schedule and automatic refresh setting independent", async () => {
    const dir = mkdtempSync(join(tmpdir(), "pp-source-monitoring-"));
    const { app, ports } = await makeApp(dir);
    try {
      expect(
        (await app.inject({ method: "GET", url: "/settings/source-update-check" })).json(),
      ).toEqual({ interval_hours: 24, auto_sync_updates: true, last_checked_at: null });

      const disabled = await app.inject({
        method: "PUT",
        url: "/settings/source-update-check",
        payload: { auto_sync_updates: false },
      });
      expect(disabled.json()).toMatchObject({ interval_hours: 24, auto_sync_updates: false });

      const rescheduled = await app.inject({
        method: "PUT",
        url: "/settings/source-update-check",
        payload: { interval_hours: 12 },
      });
      expect(rescheduled.json()).toMatchObject({ interval_hours: 12, auto_sync_updates: false });
    } finally {
      await app.close();
      ports.db.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it.each([-1, 0.000001, 1.5, 169, "1", null])(
    "rejects an unsafe interval value of %s without changing the schedule",
    async (intervalHours) => {
      const dir = mkdtempSync(join(tmpdir(), "pp-source-monitoring-"));
      const { app, ports } = await makeApp(dir);
      try {
        const response = await app.inject({
          method: "PUT",
          url: "/settings/source-update-check",
          payload: { interval_hours: intervalHours },
        });

        expect(response.statusCode).toBe(400);
        expect(response.json()).toEqual({
          detail: "interval_hours must be 0 or a whole number from 1 through 168",
        });
        expect(
          (await app.inject({ method: "GET", url: "/settings/source-update-check" })).json(),
        ).toMatchObject({ interval_hours: 24 });
      } finally {
        await app.close();
        ports.db.close();
        rmSync(dir, { recursive: true, force: true });
      }
    },
  );
});
