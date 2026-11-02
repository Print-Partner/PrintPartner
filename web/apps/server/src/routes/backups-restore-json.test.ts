import Fastify from "fastify";
import multipart from "@fastify/multipart";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InsufficientRestoreSpaceError, type BackupMetadata } from "../services/backup-restore.js";
import { registerBackupRoutes } from "./backups.js";

const restoreBackup = vi.hoisted(() =>
  vi.fn().mockResolvedValue({
    version: "1",
    createdAt: "2026-08-18T09:00:00.000Z",
    appVersion: "3.1.0",
    formatVersion: 1,
  }),
);
const inspectRestore = vi.hoisted(() =>
  vi.fn().mockResolvedValue({
    archiveBytes: 1_024,
    requiredBytes: 67_109_888,
    freeBytes: 2_000_000_000,
    sufficient: true,
  }),
);
const validateBackup = vi.hoisted(() =>
  vi.fn().mockResolvedValue({
    version: "1",
    createdAt: "2026-08-18T09:00:00.000Z",
    appVersion: "3.1.0",
    formatVersion: 1,
  }),
);

vi.mock("../services/backup-restore.js", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../services/backup-restore.js")>()),
  inspectRestore,
  restoreBackup,
  validateBackup,
}));

describe("POST /backups/restore JSON contract", () => {
  const dirs: string[] = [];

  afterEach(() => {
    restoreBackup.mockClear();
    inspectRestore.mockClear();
    validateBackup.mockClear();
    for (const dir of dirs) rmSync(dir, { recursive: true, force: true });
    dirs.length = 0;
  });

  it("preflights a canonical stored backup before confirmation", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-backup-route-"));
    dirs.push(dataDir);
    const backupsDir = join(dataDir, "backups");
    const name = "print-partner-backup-2026-08-18.tar.gz";
    mkdirSync(backupsDir);
    writeFileSync(join(backupsDir, name), "archive");
    const app = Fastify();
    await app.register(multipart);
    await registerBackupRoutes(app, {
      dataDir,
      sqlite: null,
      appVersion: "3.1.0",
    });

    try {
      const response = await app.inject({
        method: "GET",
        url: `/backups/${name}/preflight`,
      });

      expect(response.statusCode).toBe(200);
      expect(response.json()).toEqual({
        metadata: {
          version: "1",
          createdAt: "2026-08-18T09:00:00.000Z",
          appVersion: "3.1.0",
          formatVersion: 1,
        },
        restorePreflight: {
          archiveBytes: 1_024,
          requiredBytes: 67_109_888,
          freeBytes: 2_000_000_000,
          sufficient: true,
        },
      });
      expect(inspectRestore).toHaveBeenCalledWith(realpathSync(join(backupsDir, name)), dataDir);
    } finally {
      await app.close();
    }
  });

  it("passes the canonical stored backup path to the restore service", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-backup-route-"));
    dirs.push(dataDir);
    const backupsDir = join(dataDir, "backups");
    const name = "print-partner-backup-2026-08-18.tar.gz";
    const storedName = "stored-print-partner-backup-2026-08-18.tar.gz";
    mkdirSync(backupsDir);
    writeFileSync(join(backupsDir, storedName), "archive");
    symlinkSync(storedName, join(backupsDir, name));
    const app = Fastify();
    await app.register(multipart);
    await registerBackupRoutes(app, {
      dataDir,
      sqlite: null,
      appVersion: "3.1.0",
    });

    try {
      const response = await app.inject({
        method: "POST",
        url: "/backups/restore",
        payload: { backupName: name },
      });

      expect(response.statusCode).toBe(200);
      expect(restoreBackup).toHaveBeenCalledWith(
        realpathSync(join(backupsDir, storedName)),
        dataDir,
        null,
      );
    } finally {
      await app.close();
    }
  });

  it("rejects unsafe named backup paths before restore", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-backup-route-"));
    dirs.push(dataDir);
    const app = Fastify();
    await app.register(multipart);
    await registerBackupRoutes(app, {
      dataDir,
      sqlite: null,
      appVersion: "3.1.0",
    });

    try {
      const response = await app.inject({
        method: "POST",
        url: "/backups/restore",
        payload: { backupName: "../outside.tar.gz" },
      });

      expect(response.statusCode).toBe(400);
      expect(restoreBackup).not.toHaveBeenCalled();
    } finally {
      await app.close();
    }
  });

  it("rejects a second restore while the first restore is still running", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-backup-route-"));
    dirs.push(dataDir);
    const backupsDir = join(dataDir, "backups");
    const name = "print-partner-backup-2026-08-18.tar.gz";
    mkdirSync(backupsDir);
    writeFileSync(join(backupsDir, name), "archive");
    const pending = createDeferred<BackupMetadata>();
    restoreBackup.mockImplementationOnce(() => pending.promise);
    const app = Fastify();
    await app.register(multipart);
    await registerBackupRoutes(app, {
      dataDir,
      sqlite: null,
      appVersion: "3.1.0",
    });

    try {
      const firstResponsePromise = app.inject({
        method: "POST",
        url: "/backups/restore",
        payload: { backupName: name },
      });
      await vi.waitFor(() => expect(restoreBackup).toHaveBeenCalledTimes(1));

      const secondResponse = await app.inject({
        method: "POST",
        url: "/backups/restore",
        payload: { backupName: name },
      });

      expect(secondResponse.statusCode).toBe(409);
      expect(secondResponse.json()).toEqual({
        detail: "A backup restore is already in progress",
      });
      expect(restoreBackup).toHaveBeenCalledTimes(1);

      pending.resolve({
        version: "1",
        createdAt: "2026-08-18T09:00:00.000Z",
        appVersion: "3.1.0",
        formatVersion: 1,
      });
      expect((await firstResponsePromise).statusCode).toBe(200);

      const nextResponse = await app.inject({
        method: "POST",
        url: "/backups/restore",
        payload: { backupName: name },
      });
      expect(nextResponse.statusCode).toBe(200);
      expect(restoreBackup).toHaveBeenCalledTimes(2);
    } finally {
      await app.close();
    }
  });

  it("returns insufficient storage when the restore preflight fails", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-backup-route-"));
    dirs.push(dataDir);
    const backupsDir = join(dataDir, "backups");
    const name = "print-partner-backup-2026-08-18.tar.gz";
    mkdirSync(backupsDir);
    writeFileSync(join(backupsDir, name), "archive");
    restoreBackup.mockRejectedValueOnce(
      new InsufficientRestoreSpaceError({
        archiveBytes: 1_024,
        requiredBytes: 67_109_888,
        freeBytes: 10,
        sufficient: false,
      }),
    );
    const app = Fastify();
    await app.register(multipart);
    await registerBackupRoutes(app, {
      dataDir,
      sqlite: null,
      appVersion: "3.1.0",
    });

    try {
      const response = await app.inject({
        method: "POST",
        url: "/backups/restore",
        payload: { backupName: name },
      });

      expect(response.statusCode).toBe(507);
      expect(response.json()).toMatchObject({
        detail: expect.stringMatching(/Insufficient disk space/i),
        preflight: {
          requiredBytes: 67_109_888,
          freeBytes: 10,
          sufficient: false,
        },
      });
    } finally {
      await app.close();
    }
  });

  it("reports post-commit refresh failures without claiming the restore rolled back", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "pp-backup-route-"));
    dirs.push(dataDir);
    const backupsDir = join(dataDir, "backups");
    const name = "print-partner-backup-2026-08-18.tar.gz";
    mkdirSync(backupsDir);
    writeFileSync(join(backupsDir, name), "archive");
    const refreshDatabaseConsumers = vi.fn();
    const app = Fastify();
    await app.register(multipart);
    await registerBackupRoutes(app, {
      dataDir,
      sqlite: null,
      appVersion: "3.1.0",
      refreshDatabaseConsumers,
      afterDatabaseRefresh: async () => {
        throw new Error("legacy migration unavailable");
      },
    });

    try {
      const response = await app.inject({
        method: "POST",
        url: "/backups/restore",
        payload: { backupName: name },
      });

      expect(response.statusCode).toBe(500);
      expect(response.json()).toEqual({
        detail: "Backup restored, but live refresh failed: legacy migration unavailable",
      });
      expect(refreshDatabaseConsumers).toHaveBeenCalledTimes(2);
    } finally {
      await app.close();
    }
  });
});

function createDeferred<T>() {
  let resolve: ((value: T) => void) | undefined;
  const promise = new Promise<T>((onResolve) => {
    resolve = onResolve;
  });
  if (!resolve) throw new Error("Deferred promise was not initialized");
  return { promise, resolve };
}
