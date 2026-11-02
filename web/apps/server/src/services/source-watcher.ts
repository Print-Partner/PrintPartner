import { join, dirname } from "node:path";
import type { AppRepository } from "../db/repository.js";
import { checkAllSourceUpdates } from "./source-update-check.js";
import { syncProjectById } from "../routes/sources.js";
import { sendDiscordNotification } from "./discord-notify.js";
import { dispatchWebhooks } from "./webhook-store.js";
import { getLogger } from "./logger.js";
import { tenantStorage, getRequestTenantId } from "../middleware/tenant-context.js";
import { runWithTenantDiskQuota } from "../lib/tenant-disk-quota.js";
import { readStoredSourceUpdateIntervalHours } from "./source-monitoring-settings.js";
import {
  OwnershipAdmissionError,
  requireSourceSyncParticipant,
} from "../ownership/source-sync-admission.js";

type SourceWatcherSettings = {
  discordWebhookUrl: string | null;
  notifyOnUpdate: boolean;
  notifyOnSync: boolean;
  autoSyncUpdates: boolean;
};

let sourceSyncWatcherInstallations = 0;

/** Watchers that passed admission and installed startup timers. */
export function installedSourceSyncWatchers(): number {
  return sourceSyncWatcherInstallations;
}

const STARTUP_SYNC_DELAY_MS = 2_000;
const STARTUP_SYNC_WAIT_MS = 5_000;
const SCHEDULE_POLL_MS = 60_000;
const HOUR_MS = 60 * 60 * 1_000;

type SourceWatcherCoordinatorDependencies = {
  listTenantIds: () => readonly string[];
  readIntervalHours: () => number;
  runStartupForCurrentTenant: () => Promise<void>;
  runPeriodicForCurrentTenant: () => Promise<void>;
  now: () => number;
};

type SourceWatcherCoordinator = {
  runStartup: () => Promise<void>;
  runScheduledChecks: () => Promise<void>;
};

type TenantSchedule = {
  intervalHours: number;
  nextCheckAt: number | null;
};

type CoordinatorPhase = "startup" | "scheduled";

function distinctTenantIds(tenantIds: readonly string[]): string[] {
  return [...new Set(tenantIds)];
}

export function createSourceWatcherCoordinator(
  dependencies: SourceWatcherCoordinatorDependencies,
): SourceWatcherCoordinator {
  const logger = getLogger();
  const tenantSchedules = new Map<string, TenantSchedule>();
  const queuedPhases = new Set<CoordinatorPhase>();
  let activeRun: Promise<void> | null = null;
  let activePhase: CoordinatorPhase | null = null;

  async function runForTenant(
    tenantId: string,
    phase: "startup sync" | "periodic update check",
    work: () => Promise<void>,
  ): Promise<void> {
    try {
      await tenantStorage.run(tenantId, work);
    } catch (error) {
      logger.log(
        "warn",
        `[source-watcher] ${phase} failed for tenant ${tenantId}: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  async function runForEachTenant(
    phase: "startup sync" | "periodic update check",
    work: () => Promise<void>,
  ): Promise<void> {
    for (const tenantId of distinctTenantIds(dependencies.listTenantIds())) {
      await runForTenant(tenantId, phase, work);
    }
  }

  async function runScheduledPhase(): Promise<void> {
    const observedAt = dependencies.now();
    const currentTenantIds = distinctTenantIds(dependencies.listTenantIds());
    const currentTenants = new Set(currentTenantIds);

    for (const tenantId of currentTenantIds) {
      await runForTenant(tenantId, "periodic update check", async () => {
        const intervalHours = dependencies.readIntervalHours();
        const schedule = tenantSchedules.get(tenantId);
        if (!schedule || schedule.intervalHours !== intervalHours) {
          tenantSchedules.set(tenantId, {
            intervalHours,
            nextCheckAt: intervalHours === 0 ? null : observedAt + intervalHours * HOUR_MS,
          });
          return;
        }
        if (schedule.nextCheckAt === null || observedAt < schedule.nextCheckAt) return;

        try {
          await dependencies.runPeriodicForCurrentTenant();
        } finally {
          const nextIntervalHours = dependencies.readIntervalHours();
          tenantSchedules.set(tenantId, {
            intervalHours: nextIntervalHours,
            nextCheckAt:
              nextIntervalHours === 0 ? null : dependencies.now() + nextIntervalHours * HOUR_MS,
          });
        }
      });
    }

    for (const tenantId of tenantSchedules.keys()) {
      if (!currentTenants.has(tenantId)) tenantSchedules.delete(tenantId);
    }
  }

  async function drainQueuedPhases(): Promise<void> {
    while (queuedPhases.size > 0) {
      const phase = queuedPhases.values().next().value;
      if (phase === undefined) break;
      queuedPhases.delete(phase);
      activePhase = phase;
      try {
        if (phase === "startup") {
          await runForEachTenant("startup sync", dependencies.runStartupForCurrentTenant);
        } else {
          await runScheduledPhase();
        }
      } catch (error: unknown) {
        logger.log(
          "warn",
          `[source-watcher] Scheduled work failed: ${error instanceof Error ? error.message : String(error)}`,
        );
      }
    }
    activePhase = null;
  }

  function enqueuePhase(phase: CoordinatorPhase): Promise<void> {
    if (activePhase !== phase) queuedPhases.add(phase);
    if (activeRun) return activeRun;

    const run = Promise.resolve().then(drainQueuedPhases);
    activeRun = run;
    void run.then(() => {
      if (activeRun === run) activeRun = null;
    });
    return run;
  }

  return {
    runStartup(): Promise<void> {
      return enqueuePhase("startup");
    },

    runScheduledChecks(): Promise<void> {
      return enqueuePhase("scheduled");
    },
  };
}

async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Auto-sync any sources that have never been synced (last_synced_at = null).
 * Processes them one at a time with a delay to avoid flooding GitHub.
 */
async function syncUnsyncedSources(
  repo: AppRepository,
  reposDir: string,
  getSettings: () => SourceWatcherSettings,
  quotaBytes: number | null,
): Promise<void> {
  requireSourceSyncParticipant("startup.startSourceWatcher");
  const logger = getLogger();
  const sources = repo
    .listSources()
    .filter(
      (source) =>
        source.last_synced_at === null &&
        (source.source_kind === "github" || source.source_kind === "git"),
    );
  if (sources.length === 0) return;

  logger.log(
    "info",
    `[source-watcher] Auto-syncing ${sources.length} unsynced source(s) on startup`,
  );

  const coversDir = join(dirname(reposDir), "covers");

  for (const source of sources) {
    requireSourceSyncParticipant("startup.startSourceWatcher");
    try {
      logger.log(
        "info",
        `[source-watcher] Auto-syncing new source: ${source.name} (id=${source.id})`,
      );
      const result = await runWithTenantDiskQuota(
        {
          dataDir: dirname(reposDir),
          reposDir,
          tenantId: getRequestTenantId(),
          quotaBytes,
          sourceIds: () => repo.listSources().map((item) => item.id),
        },
        () =>
          syncProjectById(repo, reposDir, source.id, coversDir, {
            beforePublish: () => requireSourceSyncParticipant("startup.startSourceWatcher"),
          }),
      );

      const settings = getSettings();
      if (settings.discordWebhookUrl && settings.notifyOnSync) {
        const row = repo.getProjectRow(source.id);
        requireSourceSyncParticipant("startup.startSourceWatcher");
        await sendDiscordNotification(settings.discordWebhookUrl, "source.synced", {
          sourceName: source.name,
          sourceUrl: source.url,
          branch: row?.branch ?? source.branch ?? "main",
          commitSha: row?.lastCommitSha ?? null,
          stlCount: result.stl_count,
        });
      }
      requireSourceSyncParticipant("startup.startSourceWatcher");
      await dispatchWebhooks(repo, "source.synced", {
        source_id: source.id,
        source_name: source.name,
        source_url: source.url,
        branch: repo.getProjectRow(source.id)?.branch ?? source.branch ?? "main",
        commit_sha: repo.getProjectRow(source.id)?.lastCommitSha ?? null,
        stl_count: result.stl_count,
      });
    } catch (err) {
      if (err instanceof OwnershipAdmissionError) throw err;
      logger.log(
        "warn",
        `[source-watcher] Startup sync failed for ${source.name}: ${err instanceof Error ? err.message : String(err)}`,
      );
      const settings = getSettings();
      if (settings.discordWebhookUrl && settings.notifyOnSync) {
        const row = repo.getProjectRow(source.id);
        requireSourceSyncParticipant("startup.startSourceWatcher");
        await sendDiscordNotification(settings.discordWebhookUrl, "source.sync_failed", {
          sourceName: source.name,
          sourceUrl: source.url,
          branch: row?.branch ?? source.branch ?? "main",
          error: err instanceof Error ? err.message : String(err),
        }).catch(() => {});
      }
      requireSourceSyncParticipant("startup.startSourceWatcher");
      await dispatchWebhooks(repo, "source.sync_failed", {
        source_id: source.id,
        source_name: source.name,
        source_url: source.url,
        branch: repo.getProjectRow(source.id)?.branch ?? source.branch ?? "main",
        error: err instanceof Error ? err.message : String(err),
      });
    }
    await sleep(STARTUP_SYNC_DELAY_MS);
  }
}

/**
 * Periodic update check + auto-sync for sources with available updates.
 */
async function runPeriodicUpdateCheck(
  repo: AppRepository,
  reposDir: string,
  getSettings: () => SourceWatcherSettings,
  quotaBytes: number | null,
): Promise<void> {
  requireSourceSyncParticipant("worker.startSourceWatcher");
  const logger = getLogger();
  const settings = getSettings();

  logger.log("info", "[source-watcher] Running periodic source update check");

  let checkResult: Awaited<ReturnType<typeof checkAllSourceUpdates>>;
  try {
    checkResult = await checkAllSourceUpdates(repo, "worker.startSourceWatcher");
  } catch (err) {
    logger.log(
      "warn",
      `[source-watcher] Update check failed: ${err instanceof Error ? err.message : String(err)}`,
    );
    return;
  }

  logger.log(
    "info",
    `[source-watcher] Update check: ${checkResult.checked_count} checked, ${checkResult.updates_available} with updates`,
  );

  if (checkResult.updates_available === 0) return;

  const coversDir = join(dirname(reposDir), "covers");

  // Find sources that have updates_available
  const allSources = repo.listSources();
  const sourcesWithUpdates = allSources.filter((s) => s.update_status === "updates_available");

  for (const source of sourcesWithUpdates) {
    const row = repo.getProjectRow(source.id);
    const previousSha = row?.lastCommitSha ?? null;

    if (settings.autoSyncUpdates) {
      requireSourceSyncParticipant("worker.startSourceWatcher");
      // Auto-sync
      try {
        logger.log("info", `[source-watcher] Auto-syncing updated source: ${source.name}`);
        const result = await runWithTenantDiskQuota(
          {
            dataDir: dirname(reposDir),
            reposDir,
            tenantId: getRequestTenantId(),
            quotaBytes,
            sourceIds: () => repo.listSources().map((item) => item.id),
          },
          () =>
            syncProjectById(repo, reposDir, source.id, coversDir, {
              beforePublish: () => requireSourceSyncParticipant("worker.startSourceWatcher"),
            }),
        );
        const updatedRow = repo.getProjectRow(source.id);
        requireSourceSyncParticipant("worker.startSourceWatcher");
        repo.recordAppEvent({
          kind: "source.updated",
          actorType: "source",
          actorId: String(source.id),
          payload: {
            source_id: source.id,
            source_name: source.name,
            source_url: source.url,
            branch: row?.branch ?? source.branch ?? "main",
            previous_sha: previousSha,
            commit_sha: updatedRow?.lastCommitSha ?? null,
            stl_count: result.stl_count,
          },
        });

        const currentSettings = getSettings();
        if (currentSettings.discordWebhookUrl && currentSettings.notifyOnUpdate) {
          requireSourceSyncParticipant("worker.startSourceWatcher");
          await sendDiscordNotification(currentSettings.discordWebhookUrl, "source.updated", {
            sourceName: source.name,
            sourceUrl: source.url,
            branch: row?.branch ?? source.branch ?? "main",
            commitSha: updatedRow?.lastCommitSha ?? null,
            previousSha,
            stlCount: result.stl_count,
          });
        }
        requireSourceSyncParticipant("worker.startSourceWatcher");
        await dispatchWebhooks(repo, "source.updated", {
          source_id: source.id,
          source_name: source.name,
          source_url: source.url,
          branch: row?.branch ?? source.branch ?? "main",
          commit_sha: repo.getProjectRow(source.id)?.lastCommitSha ?? null,
          previous_sha: previousSha,
          stl_count: result.stl_count,
        });
      } catch (err) {
        if (err instanceof OwnershipAdmissionError) throw err;
        logger.log(
          "warn",
          `[source-watcher] Auto-sync failed for ${source.name}: ${err instanceof Error ? err.message : String(err)}`,
        );
        const currentSettings = getSettings();
        requireSourceSyncParticipant("worker.startSourceWatcher");
        repo.recordAppEvent({
          kind: "source.sync_failed",
          actorType: "source",
          actorId: String(source.id),
          payload: {
            source_id: source.id,
            source_name: source.name,
            source_url: source.url,
            branch: row?.branch ?? source.branch ?? "main",
            error: err instanceof Error ? err.message : String(err),
          },
        });
        if (currentSettings.discordWebhookUrl) {
          requireSourceSyncParticipant("worker.startSourceWatcher");
          await sendDiscordNotification(currentSettings.discordWebhookUrl, "source.sync_failed", {
            sourceName: source.name,
            sourceUrl: source.url,
            branch: row?.branch ?? source.branch ?? "main",
            error: err instanceof Error ? err.message : String(err),
          }).catch(() => {});
        }
        requireSourceSyncParticipant("worker.startSourceWatcher");
        await dispatchWebhooks(repo, "source.sync_failed", {
          source_id: source.id,
          source_name: source.name,
          source_url: source.url,
          branch: row?.branch ?? source.branch ?? "main",
          error: err instanceof Error ? err.message : String(err),
        });
      }
    } else if (settings.discordWebhookUrl && settings.notifyOnUpdate) {
      // Notify but don't sync
      requireSourceSyncParticipant("worker.startSourceWatcher");
      await sendDiscordNotification(settings.discordWebhookUrl, "source.update_available", {
        sourceName: source.name,
        sourceUrl: source.url,
        branch: row?.branch ?? source.branch ?? "main",
        commitSha: previousSha,
      });
      requireSourceSyncParticipant("worker.startSourceWatcher");
      await dispatchWebhooks(repo, "source.update_available", {
        source_id: source.id,
        source_name: source.name,
        source_url: source.url,
        branch: row?.branch ?? source.branch ?? "main",
        commit_sha: previousSha,
      });
    } else {
      // No Discord configured — still fire webhook if registered
      requireSourceSyncParticipant("worker.startSourceWatcher");
      await dispatchWebhooks(repo, "source.update_available", {
        source_id: source.id,
        source_name: source.name,
        source_url: source.url,
        branch: row?.branch ?? source.branch ?? "main",
        commit_sha: previousSha,
      });
    }
  }
}

export function startSourceWatcher(input: {
  tenantDiskQuotaBytes?: number | null;
  repo: AppRepository;
  reposDir: string;
  getSettings: () => SourceWatcherSettings;
  listTenantIds: () => readonly string[];
}): { stop: () => void } {
  requireSourceSyncParticipant("startup.startSourceWatcher");
  requireSourceSyncParticipant("worker.startSourceWatcher");
  const logger = getLogger();
  const coordinator = createSourceWatcherCoordinator({
    listTenantIds: input.listTenantIds,
    readIntervalHours: () =>
      readStoredSourceUpdateIntervalHours(input.repo.getSetting("source_update_check_hours")),
    runStartupForCurrentTenant: () =>
      syncUnsyncedSources(
        input.repo,
        input.reposDir,
        input.getSettings,
        input.tenantDiskQuotaBytes ?? null,
      ),
    runPeriodicForCurrentTenant: () =>
      runPeriodicUpdateCheck(
        input.repo,
        input.reposDir,
        input.getSettings,
        input.tenantDiskQuotaBytes ?? null,
      ),
    now: Date.now,
  });

  void coordinator.runScheduledChecks();
  sourceSyncWatcherInstallations += 1;
  const startupTimer = setTimeout(() => {
    void coordinator.runStartup();
  }, STARTUP_SYNC_WAIT_MS);
  const schedulePoller = setInterval(() => {
    void coordinator.runScheduledChecks();
  }, SCHEDULE_POLL_MS);

  return {
    stop(): void {
      clearTimeout(startupTimer);
      clearInterval(schedulePoller);
      sourceSyncWatcherInstallations = Math.max(0, sourceSyncWatcherInstallations - 1);
      logger.log("info", "[source-watcher] Stopped");
    },
  };
}
