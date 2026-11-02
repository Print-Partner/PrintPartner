import type { FastifyInstance } from "fastify";
import { createReadStream, createWriteStream, mkdirSync, mkdtempSync } from "node:fs";
import { basename, isAbsolute, join, relative, resolve, sep } from "node:path";
import { promises as fs } from "node:fs";
import { pipeline } from "node:stream/promises";
import type { SqliteDatabase } from "../db/client.js";
import { resolvedFileUnderRoot } from "../lib/secure-path.js";
import {
  createBackup,
  inspectRestore,
  InsufficientRestoreSpaceError,
  restoreBackup,
  validateBackup,
  type BackupMetadata,
  type RestorePreflight,
} from "../services/backup-restore.js";
import { inspectStorage, type StorageInventory } from "../services/storage-inventory.js";
import {
  BACKUP_UPLOAD_TOO_LARGE_DETAIL,
  MAX_BACKUP_UPLOAD_BYTES,
} from "../services/upload-limits.js";

type RouteDeps = {
  dataDir: string;
  sqlite: SqliteDatabase | null;
  appVersion: string;
  refreshDatabaseConsumers?: () => void;
  afterDatabaseRefresh?: () => Promise<void>;
};

export function safeBackupUploadPath(root: string, multipartFilename: string): string {
  const leaf = basename(multipartFilename.replace(/\\/g, "/"));
  const cleaned = leaf
    .replace(/[^A-Za-z0-9._-]/g, "_")
    .replace(/^\.+/, "")
    .slice(0, 120);
  const filename = (cleaned || "backup-upload").toLowerCase().endsWith(".tar.gz")
    ? cleaned
    : `${cleaned || "backup-upload"}.tar.gz`;
  const base = resolve(root);
  const candidate = resolve(base, filename);
  const relativePath = relative(base, candidate);
  if (
    !relativePath ||
    relativePath === ".." ||
    relativePath.startsWith(`..${sep}`) ||
    isAbsolute(relativePath)
  ) {
    throw new Error("Invalid backup upload filename");
  }
  return candidate;
}

function safeStoredBackupPath(backupsDir: string, name: string): string | null {
  if (
    !/^[A-Za-z0-9][A-Za-z0-9._-]*\.tar\.gz$/.test(name) ||
    name.includes("..") ||
    name.includes("\\")
  ) {
    return null;
  }
  return resolvedFileUnderRoot(backupsDir, join(backupsDir, name));
}

async function removeTemporaryUploadDirectory(path: string): Promise<void> {
  try {
    await fs.rm(path, { recursive: true, force: true });
  } catch {
    // A failed best-effort cleanup must not replace the upload response.
  }
}

export async function registerBackupRoutes(app: FastifyInstance, deps: RouteDeps): Promise<void> {
  let restoreInProgress = false;

  app.get<{ Reply: StorageInventory | { detail: string } }>(
    "/backups/storage",
    async (_request, reply) => {
      try {
        return reply.send(await inspectStorage(deps.dataDir));
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return reply.status(500).send({
          detail: `Failed to inspect storage: ${message}`,
        });
      }
    },
  );

  /**
   * POST /backups
   * Creates a new backup archive.
   * Returns the backup file path and metadata.
   */
  app.post<{
    Reply: { path: string; name: string; size: number; metadata: object } | { detail: string };
  }>(
    "/backups",
    { config: { rateLimit: { max: 5, timeWindow: "1 minute" } } },
    async (_request, reply) => {
      try {
        const backupsDir = join(deps.dataDir, "backups");
        mkdirSync(backupsDir, { recursive: true });

        const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
        const backupFileName = `print-partner-backup-${timestamp}.tar.gz`;
        const backupPath = join(backupsDir, backupFileName);

        await createBackup(deps.sqlite, deps.dataDir, deps.appVersion, backupPath);

        const stats = await fs.stat(backupPath);

        return reply.status(201).send({
          path: backupPath,
          name: backupFileName,
          size: stats.size,
          metadata: {
            createdAt: new Date().toISOString(),
            appVersion: deps.appVersion,
          },
        });
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return reply.status(500).send({
          detail: `Backup failed: ${message}`,
        });
      }
    },
  );

  /**
   * GET /backups
   * Lists available backup files.
   */
  app.get<{ Reply: Array<{ name: string; size: number; createdAt: string }> | { detail: string } }>(
    "/backups",
    async (_request, reply) => {
      try {
        const backupsDir = join(deps.dataDir, "backups");

        try {
          const files = await fs.readdir(backupsDir);
          const backups = await Promise.all(
            files
              .filter((f) => /^[A-Za-z0-9][A-Za-z0-9._-]*\.tar\.gz$/.test(f))
              .map(async (name) => {
                const fullPath = join(backupsDir, name);
                const stats = await fs.stat(fullPath);
                return {
                  name,
                  size: stats.size,
                  createdAt: stats.mtime.toISOString(),
                };
              }),
          );

          // Sort by creation time, newest first
          backups.sort((a, b) => new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime());

          return reply.send(backups);
        } catch {
          // Backups directory doesn't exist yet
          return reply.send([]);
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return reply.status(500).send({
          detail: `Failed to list backups: ${message}`,
        });
      }
    },
  );

  app.get<{
    Params: { name: string };
    Reply: { metadata: BackupMetadata; restorePreflight: RestorePreflight } | { detail: string };
  }>("/backups/:name/preflight", async (request, reply) => {
    try {
      const backupsDir = join(deps.dataDir, "backups");
      const backupPath = safeStoredBackupPath(backupsDir, request.params.name);
      if (!backupPath) {
        return reply.status(404).send({ detail: "Backup file not found" });
      }
      const stats = await fs.stat(backupPath);
      if (!stats.isFile()) {
        return reply.status(404).send({ detail: "Backup file not found" });
      }
      const metadata = await validateBackup(backupPath);
      const restorePreflight = await inspectRestore(backupPath, deps.dataDir);
      return reply.send({ metadata, restorePreflight });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return reply.status(400).send({
        detail: `Backup preflight failed: ${message}`,
      });
    }
  });

  /**
   * GET /backups/:name
   * Downloads a specific backup file.
   */
  app.get("/backups/:name", async (request, reply) => {
    try {
      const { name } = request.params as { name: string };

      // Prevent directory traversal
      if (name.includes("..") || name.includes("/") || name.includes("\\")) {
        return reply.status(400).send({
          detail: "Invalid backup name",
        });
      }

      const backupsDir = join(deps.dataDir, "backups");
      const backupPath = safeStoredBackupPath(backupsDir, name);
      if (!backupPath) return reply.status(404).send({ detail: "Backup file not found" });

      try {
        const stats = await fs.stat(backupPath);
        if (!stats.isFile()) {
          return reply.status(404).send({
            detail: "Backup file not found",
          });
        }

        return reply
          .header("Content-Type", "application/gzip")
          .header("Content-Disposition", `attachment; filename="${name}"`)
          .header("Content-Length", stats.size)
          .send(createReadStream(backupPath));
      } catch {
        return reply.status(404).send({
          detail: "Backup file not found",
        });
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return reply.status(500).send({
        detail: `Failed to download backup: ${message}`,
      });
    }
  });

  /**
   * POST /backups/validate
   * Validates a backup file without extracting it.
   * Expects the backup file as form data.
   */
  app.post<{
    Reply:
      { valid: true; metadata: object; restorePreflight: RestorePreflight } | { detail: string };
  }>(
    "/backups/validate",
    {
      schema: {
        consumes: ["multipart/form-data"],
      },
    },
    async (request, reply) => {
      try {
        const data = await request.file({
          limits: {
            fileSize: MAX_BACKUP_UPLOAD_BYTES,
            files: 1,
            fields: 0,
            parts: 1,
          },
        });
        if (!data) {
          return reply.status(400).send({
            detail: "No backup file provided",
          });
        }

        mkdirSync(deps.dataDir, { recursive: true });
        const tempDir = mkdtempSync(join(deps.dataDir, ".validate-upload-"));

        try {
          const tempFilePath = safeBackupUploadPath(tempDir, data.filename);
          await pipeline(data.file, createWriteStream(tempFilePath, { flags: "wx" }));
          if (data.file.truncated) {
            await removeTemporaryUploadDirectory(tempDir);
            return reply.status(413).send({
              detail: BACKUP_UPLOAD_TOO_LARGE_DETAIL,
            });
          }

          const metadata = await validateBackup(tempFilePath);
          const restorePreflight = await inspectRestore(tempFilePath, deps.dataDir);

          return reply.send({
            valid: true,
            metadata,
            restorePreflight,
          });
        } finally {
          await removeTemporaryUploadDirectory(tempDir);
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return reply.status(400).send({
          detail: `Backup validation failed: ${message}`,
        });
      }
    },
  );

  /**
   * POST /backups/restore
   * Restores a backup file.
   * Expects the backup file as form data.
   * WARNING: This replaces all current application data.
   */
  app.post<{
    Body: { backupName?: string };
    Reply: { success: boolean; message: string } | { detail: string; preflight?: RestorePreflight };
  }>(
    "/backups/restore",
    {
      config: { rateLimit: { max: 3, timeWindow: "5 minutes" } },
      schema: { consumes: ["multipart/form-data"] },
    },
    async (request, reply) => {
      let restoreCommitted = false;
      if (restoreInProgress) {
        return reply.status(409).send({
          detail: "A backup restore is already in progress",
        });
      }
      restoreInProgress = true;
      let tempDir: string | null = null;
      try {
        let backupPath: string;
        if (request.isMultipart()) {
          const data = await request.file({
            limits: {
              fileSize: MAX_BACKUP_UPLOAD_BYTES,
              files: 1,
              fields: 0,
              parts: 1,
            },
          });
          if (!data) {
            return reply.status(400).send({
              detail: "No backup file provided",
            });
          }
          mkdirSync(deps.dataDir, { recursive: true });
          tempDir = mkdtempSync(join(deps.dataDir, ".restore-upload-"));
          const tempFilePath = safeBackupUploadPath(tempDir, data.filename);
          await pipeline(data.file, createWriteStream(tempFilePath, { flags: "wx" }));
          if (data.file.truncated) {
            await removeTemporaryUploadDirectory(tempDir);
            tempDir = null;
            return reply.status(413).send({
              detail: BACKUP_UPLOAD_TOO_LARGE_DETAIL,
            });
          }
          backupPath = tempFilePath;
        } else {
          const name = request.body?.backupName?.trim();
          if (!name) {
            return reply.status(400).send({ detail: "backupName is required" });
          }
          const backupsDir = join(deps.dataDir, "backups");
          const storedPath = safeStoredBackupPath(backupsDir, name);
          if (!storedPath) {
            return reply.status(400).send({ detail: "Invalid backup name" });
          }
          try {
            const stats = await fs.stat(storedPath);
            if (!stats.isFile()) {
              return reply.status(404).send({ detail: "Backup file not found" });
            }
          } catch {
            return reply.status(404).send({ detail: "Backup file not found" });
          }
          backupPath = storedPath;
        }

        const metadata = await restoreBackup(backupPath, deps.dataDir, deps.sqlite);
        restoreCommitted = true;
        deps.refreshDatabaseConsumers?.();
        await deps.afterDatabaseRefresh?.();
        return reply.send({
          success: true,
          message: `Backup restored successfully (${metadata.createdAt}).`,
        });
      } catch (error) {
        if (error instanceof InsufficientRestoreSpaceError) {
          return reply.status(507).send({
            detail: error.message,
            preflight: error.preflight,
          });
        }
        if (restoreCommitted) {
          try {
            deps.refreshDatabaseConsumers?.();
          } catch {
            // The restored database is already committed. Preserve the post-commit error.
          }
          const message = error instanceof Error ? error.message : String(error);
          return reply.status(500).send({
            detail: `Backup restored, but live refresh failed: ${message}`,
          });
        }
        // restoreBackup reconnects SQLite after both success and failure. Any
        // long-lived repository must follow that new connection before the
        // next request arrives.
        try {
          deps.refreshDatabaseConsumers?.();
        } catch {
          // Preserve the restore error below. The health check will expose a
          // reconnect failure without replacing the more useful root cause.
        }
        const message = error instanceof Error ? error.message : String(error);
        return reply.status(500).send({
          detail: message.startsWith("Restore failed:") ? message : `Restore failed: ${message}`,
        });
      } finally {
        restoreInProgress = false;
        if (tempDir) {
          await removeTemporaryUploadDirectory(tempDir);
        }
      }
    },
  );

  /**
   * DELETE /backups/:name
   * Deletes a specific backup file.
   */
  app.delete("/backups/:name", async (request, reply) => {
    try {
      const { name } = request.params as { name: string };

      // Prevent directory traversal
      if (name.includes("..") || name.includes("/") || name.includes("\\")) {
        return reply.status(400).send({
          detail: "Invalid backup name",
        });
      }

      const backupsDir = join(deps.dataDir, "backups");
      const backupPath = safeStoredBackupPath(backupsDir, name);
      if (!backupPath) return reply.status(404).send({ detail: "Backup file not found" });

      try {
        await fs.unlink(backupPath);
        return reply.send({ success: true });
      } catch {
        return reply.status(404).send({
          detail: "Backup file not found",
        });
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return reply.status(500).send({
        detail: `Failed to delete backup: ${message}`,
      });
    }
  });
}
