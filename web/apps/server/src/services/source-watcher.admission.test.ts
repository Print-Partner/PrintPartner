import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import {
  beginSourceSyncAdmission,
  packagedSourceSyncManifestPath,
} from "../ownership/source-sync-admission.js";
import * as sync from "../routes/sources.js";
import * as discord from "./discord-notify.js";
import * as webhooks from "./webhook-store.js";
import * as updates from "./source-update-check.js";
vi.mock("../routes/sources.js", { spy: true });
vi.mock("./source-update-check.js", { spy: true });
vi.mock("./discord-notify.js", { spy: true });
vi.mock("./webhook-store.js", { spy: true });
import { startSourceWatcher } from "./source-watcher.js";

describe("Source watcher runtime admission", () => {
  it.each([
    "startup-success",
    "startup-failure",
    "periodic-success",
    "periodic-failure",
    "notify-only",
    "webhook-only",
  ])("refuses post-sync webhooks after admission changes during %s delivery", async (phase) => {
    const dir = mkdtempSync(join(tmpdir(), "pp-watcher-delivery-"));
    const sqlite = new SqliteDatabase(dir);
    const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
    const startup = phase.startsWith("startup");
    let watcher: ReturnType<typeof startSourceWatcher> | undefined;
    try {
      sqlite.connect();
      const repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
      const source = repo.createSource({
        name: "Delivery source",
        source_kind: startup || phase.endsWith("failure") ? "github" : "local",
        url: "invalid",
      });
      mkdirSync(join(sqlite.reposDir, String(source.id)), { recursive: true });
      if (!startup)
        repo.updateSource(source.id, {
          last_synced_at: new Date().toISOString(),
          last_commit_sha: "previous",
        });
      repo.setSetting("source_update_check_hours", "1");
      const manifest = join(dir, "ownership.json");
      const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
      writeFileSync(manifest, original);
      process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
      beginSourceSyncAdmission(dir);
      if (!startup) {
        const actual = await vi.importActual<typeof updates>("./source-update-check.js");
        vi.spyOn(updates, "checkAllSourceUpdates").mockImplementationOnce(async (...args) => {
          const result = await actual.checkAllSourceUpdates(...args);
          repo.updateSource(source.id, { metadata: { remote_update_status: "updates_available" } });
          if (phase === "webhook-only") writeFileSync(manifest, `${original}\n`);
          return { ...result, updates_available: 1 };
        });
      } else if (phase === "startup-success") {
        const actual = await vi.importActual<typeof sync>("../routes/sources.js");
        vi.spyOn(sync, "syncProjectById").mockImplementationOnce(async (...args) => {
          repo.updateSource(source.id, { source_kind: "local" });
          return actual.syncProjectById(...args);
        });
      }
      const notify = vi.spyOn(discord, "sendDiscordNotification").mockImplementation(async () => {
        await Promise.resolve();
        writeFileSync(manifest, `${original}\n`);
        return { ok: true, status: 204, attempts: 1 };
      });
      const dispatch = vi.spyOn(webhooks, "dispatchWebhooks").mockResolvedValue(undefined);
      vi.useFakeTimers();
      watcher = startSourceWatcher({
        repo,
        reposDir: sqlite.reposDir,
        listTenantIds: () => ["default"],
        getSettings: () => ({
          discordWebhookUrl: phase === "webhook-only" ? null : "https://example.test/discord",
          notifyOnUpdate: true,
          notifyOnSync: true,
          autoSyncUpdates: phase.startsWith("periodic"),
        }),
      });
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(startup ? 7_000 : 3_600_000);
      expect(notify).toHaveBeenCalledTimes(phase === "webhook-only" ? 0 : 1);
      expect(dispatch).not.toHaveBeenCalled();
    } finally {
      watcher?.stop();
      vi.useRealTimers();
      vi.restoreAllMocks();
      vi.clearAllMocks();
      if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
      sqlite.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it.each(["success", "failure"])(
    "refuses watcher events when admission changes after sync %s",
    async (outcome) => {
      const dir = mkdtempSync(join(tmpdir(), "pp-watcher-events-"));
      const sqlite = new SqliteDatabase(dir);
      const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      let watcher: ReturnType<typeof startSourceWatcher> | undefined;
      try {
        sqlite.connect();
        const repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
        const source = repo.createSource({
          name: "Updated Source",
          source_kind: outcome === "success" ? "local" : "github",
          url: "invalid",
        });
        mkdirSync(join(sqlite.reposDir, String(source.id)), { recursive: true });
        repo.updateSource(source.id, {
          last_synced_at: new Date().toISOString(),
          last_commit_sha: "previous",
        });
        repo.setSetting("source_update_check_hours", "1");
        const manifest = join(dir, "ownership.json");
        const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
        writeFileSync(manifest, original);
        process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
        beginSourceSyncAdmission(dir);
        const actualUpdates = await vi.importActual<typeof updates>("./source-update-check.js");
        vi.spyOn(updates, "checkAllSourceUpdates").mockImplementationOnce(async (...args) => {
          const result = await actualUpdates.checkAllSourceUpdates(...args);
          repo.updateSource(source.id, { metadata: { remote_update_status: "updates_available" } });
          expect(repo.getSource(source.id)?.update_status).toBe("updates_available");
          return { ...result, updates_available: 1 };
        });
        const actualSync = await vi.importActual<typeof sync>("../routes/sources.js");
        const syncCall = vi
          .spyOn(sync, "syncProjectById")
          .mockImplementationOnce(async (...args) => {
            try {
              return await actualSync.syncProjectById(...args);
            } finally {
              writeFileSync(manifest, `${original}\n`);
            }
          });
        const eventsBefore = repo.listAppEvents({
          kinds: ["source.updated", "source.sync_failed"],
        });
        vi.useFakeTimers();
        watcher = startSourceWatcher({
          repo,
          reposDir: sqlite.reposDir,
          listTenantIds: () => ["default"],
          getSettings: () => ({
            discordWebhookUrl: null,
            notifyOnUpdate: false,
            notifyOnSync: false,
            autoSyncUpdates: true,
          }),
        });
        await vi.advanceTimersByTimeAsync(0);
        await vi.advanceTimersByTimeAsync(3_600_000);
        expect(syncCall).toHaveBeenCalledOnce();
        expect(repo.listAppEvents({ kinds: ["source.updated", "source.sync_failed"] })).toEqual(
          eventsBefore,
        );
      } finally {
        watcher?.stop();
        vi.useRealTimers();
        vi.restoreAllMocks();
        vi.clearAllMocks();
        if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
        else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
        sqlite.close();
        rmSync(dir, { recursive: true, force: true });
      }
    },
  );

  it.each([
    { phase: "startup", advanceBefore: 0, advanceAfter: 7_000 },
    { phase: "periodic", advanceBefore: 7_000, advanceAfter: 3_600_000 },
    { phase: "startup batch", advanceBefore: 5_000, advanceAfter: 2_000 },
  ])("refuses a changed manifest during $phase work", async ({ advanceBefore, advanceAfter }) => {
    const dir = mkdtempSync(join(tmpdir(), "pp-watcher-admission-"));
    const sqlite = new SqliteDatabase(dir);
    const priorManifest = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
    let watcher: ReturnType<typeof startSourceWatcher> | undefined;
    try {
      sqlite.connect();
      const repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
      const first = repo.createSource({
        name: "A unsynced",
        source_kind: "github",
        url: "invalid",
      });
      const second = repo.createSource({
        name: "B unsynced",
        source_kind: "github",
        url: "invalid",
      });
      repo.setSetting("source_update_check_hours", "1");
      const manifest = join(dir, "manifest.json");
      const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
      writeFileSync(manifest, original);
      process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
      beginSourceSyncAdmission(dir);
      vi.useFakeTimers();
      watcher = startSourceWatcher({
        repo,
        reposDir: sqlite.reposDir,
        listTenantIds: () => ["default"],
        getSettings: () => ({
          discordWebhookUrl: null,
          notifyOnUpdate: false,
          notifyOnSync: false,
          autoSyncUpdates: true,
        }),
      });
      await vi.advanceTimersByTimeAsync(0);
      if (advanceBefore > 0) {
        await vi.advanceTimersByTimeAsync(advanceBefore);
        expect(repo.getProjectRow(first.id)?.metadataJson).toContain("sync_error");
      }
      const before = [repo.getProjectRow(first.id), repo.getProjectRow(second.id)];
      const beforeEvents = repo.listAppEvents({
        kinds: ["source.updated", "source.update_available"],
      });
      writeFileSync(manifest, `${original}\n`);
      await vi.advanceTimersByTimeAsync(advanceAfter);
      expect([repo.getProjectRow(first.id), repo.getProjectRow(second.id)]).toEqual(before);
      expect(repo.listAppEvents({ kinds: ["source.updated", "source.update_available"] })).toEqual(
        beforeEvents,
      );
      expect(repo.getSetting("source_update_check_last_run_at")).toBeNull();
    } finally {
      watcher?.stop();
      vi.useRealTimers();
      if (priorManifest === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = priorManifest;
      sqlite.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
