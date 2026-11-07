import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { randomUUID } from "node:crypto";
import { basename, dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { promises as fs } from "node:fs";
import * as tar from "tar";
import type { Stats } from "node:fs";
import type { ReadEntry } from "tar";
import Database from "better-sqlite3";
import type { SqliteDatabase } from "../db/client.js";
import {
  backupRootForArchivePath,
  FULL_BACKUP_ROOT_PATHS,
  FULL_BACKUP_ROOTS,
  isFullBackupRoot,
  LEGACY_BACKUP_ROOTS,
  type FullBackupRoot,
} from "./backup-scope.js";
import { readFreeDiskBytes } from "./storage-inventory.js";
import { isRecord } from "../lib/guards.js";

export type BackupDatabase = {
  readonly dbPath: string;
  ping(): boolean;
  backupToFile(destinationPath: string): Promise<void>;
};

type CreateBackupOptions = Readonly<{
  includeDataDirectories?: boolean;
  validationLimits?: BackupValidationLimits;
}>;

type BackupMetadataBase = Readonly<{
  version: string;
  createdAt: string;
  appVersion: string;
}>;

type BackupMetadataV1 = BackupMetadataBase &
  Readonly<{
    version: "1";
    formatVersion: 1;
  }>;

type BackupScope =
  | Readonly<{
      kind: "full";
      includedRoots: readonly FullBackupRoot[];
    }>
  | Readonly<{
      kind: "database-only";
      includedRoots: readonly [];
    }>;

type BackupMetadataV2 = BackupMetadataBase &
  Readonly<{
    version: "2";
    formatVersion: 2;
    scope: BackupScope;
  }>;

export type BackupMetadata = BackupMetadataV1 | BackupMetadataV2;

const BACKUP_FORMAT_VERSION = 2;
const BACKUP_METADATA_FILE = "backup-metadata.json";
const BACKUP_DATABASE_FILE = "print-partner.db";
const BACKUP_WAL_FILE = "print-partner.db-wal";
const BACKUP_CORE_FILES = new Set([
  BACKUP_METADATA_FILE,
  BACKUP_DATABASE_FILE,
  BACKUP_WAL_FILE,
]);
const SQLITE_HEADER = Buffer.from("SQLite format 3\0", "binary");
const MAX_METADATA_BYTES = 64 * 1024;
const DEFAULT_MAX_BACKUP_ENTRIES = 100_000;
const MAX_BACKUP_ENTRY_BYTES = 8 * 1024 * 1024 * 1024;
const MAX_BACKUP_EXPANDED_BYTES = 20 * 1024 * 1024 * 1024;
const DEFAULT_MAX_DECOMPRESSION_RATIO = 200;
const RESTORE_FREE_SPACE_RESERVE_BYTES = 64 * 1024 * 1024;

type BackupValidationLimits = {
  maxEntries?: number;
  maxTotalBytes?: number;
  maxEntryBytes?: number;
  maxDecompressionRatio?: number;
};

export type RestorePreflight = Readonly<{
  archiveBytes: number;
  requiredBytes: number;
  freeBytes: number;
  sufficient: boolean;
}>;

type RestoreInspectionOptions = BackupValidationLimits &
  Readonly<{
    readFreeBytes?: (path: string) => Promise<number>;
  }>;

export class InsufficientRestoreSpaceError extends Error {
  readonly preflight: RestorePreflight;

  constructor(preflight: RestorePreflight) {
    super(
      `Insufficient disk space for restore: ${preflight.requiredBytes} bytes required, ${preflight.freeBytes} bytes free`,
    );
    this.name = "InsufficientRestoreSpaceError";
    this.preflight = preflight;
  }
}

type ResolvedBackupLimits = Required<BackupValidationLimits>;

type ArchiveGuardState = {
  entries: number;
  totalBytes: number;
  seen: Set<string>;
  error: Error | null;
};

function positiveLimit(value: number | undefined, fallback: number): number {
  return value == null || !Number.isFinite(value) || value < 0 ? fallback : value;
}

function resolveBackupLimits(limits: BackupValidationLimits = {}): ResolvedBackupLimits {
  return {
    maxEntries: positiveLimit(limits.maxEntries, DEFAULT_MAX_BACKUP_ENTRIES),
    maxTotalBytes: positiveLimit(limits.maxTotalBytes, MAX_BACKUP_EXPANDED_BYTES),
    maxEntryBytes: positiveLimit(limits.maxEntryBytes, MAX_BACKUP_ENTRY_BYTES),
    maxDecompressionRatio: positiveLimit(
      limits.maxDecompressionRatio,
      DEFAULT_MAX_DECOMPRESSION_RATIO,
    ),
  };
}

function resolveCreationLimits(limits: BackupValidationLimits = {}): ResolvedBackupLimits {
  const defaults = resolveBackupLimits();
  const requested = resolveBackupLimits(limits);
  return {
    maxEntries: Math.min(requested.maxEntries, defaults.maxEntries),
    maxTotalBytes: Math.min(requested.maxTotalBytes, defaults.maxTotalBytes),
    maxEntryBytes: Math.min(requested.maxEntryBytes, defaults.maxEntryBytes),
    maxDecompressionRatio: Math.min(
      requested.maxDecompressionRatio,
      defaults.maxDecompressionRatio,
    ),
  };
}

function normalizedArchivePath(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+$/, "");
}

function archiveEntryError(entry: ReadEntry, seen: Set<string>): Error | null {
  if (entry.meta) return null;
  const rawPath = entry.path;
  const path = normalizedArchivePath(rawPath);
  const parts = path.split("/");
  if (
    !path ||
    rawPath.includes("\\") ||
    rawPath.includes("\0") ||
    rawPath.startsWith("/") ||
    /^[A-Za-z]:/.test(rawPath) ||
    parts.some((part) => part === "" || part === "." || part === "..")
  ) {
    return new Error(`Unsafe backup entry path: ${rawPath}`);
  }
  if (BACKUP_CORE_FILES.has(path)) {
    if (entry.type !== "File" && entry.type !== "OldFile") {
      return new Error(`Backup root entry must be a regular file: ${rawPath}`);
    }
    if (path === BACKUP_METADATA_FILE && entry.size > MAX_METADATA_BYTES) {
      return new Error(`Backup metadata exceeds ${MAX_METADATA_BYTES} byte limit`);
    }
  } else {
    const root = backupRootForArchivePath(path);
    if (!root) return new Error(`Unexpected backup entry: ${rawPath}`);
    if (root.kind === "file") {
      if (entry.type !== "File" && entry.type !== "OldFile") {
        return new Error(`Backup root entry must be a regular file: ${rawPath}`);
      }
    } else {
      if (
        entry.type !== "File" &&
        entry.type !== "OldFile" &&
        entry.type !== "Directory"
      ) {
        return new Error(`Unsupported backup entry type for ${rawPath}: ${entry.type}`);
      }
      if (path === root.path && entry.type !== "Directory") {
        return new Error(`Backup directory root has invalid type: ${rawPath}`);
      }
    }
  }
  if (seen.has(path)) return new Error(`Duplicate backup entry: ${rawPath}`);
  seen.add(path);
  return null;
}

function guardArchiveEntry(
  entry: ReadEntry,
  state: ArchiveGuardState,
  limits: ResolvedBackupLimits,
): boolean {
  if (state.error) return false;
  state.entries += 1;
  if (state.entries > limits.maxEntries) {
    state.error = new Error(`Backup archive entry limit exceeded (${limits.maxEntries})`);
    return false;
  }
  if (!Number.isSafeInteger(entry.size) || entry.size < 0) {
    state.error = new Error(`Backup entry has invalid size: ${entry.path}`);
    return false;
  }
  if (entry.size > limits.maxEntryBytes) {
    state.error = new Error(`Backup entry exceeds decompressed size limit: ${entry.path}`);
    return false;
  }
  state.totalBytes += entry.size;
  if (!Number.isSafeInteger(state.totalBytes) || state.totalBytes > limits.maxTotalBytes) {
    state.error = new Error(
      `Backup archive decompressed byte limit exceeded (${limits.maxTotalBytes})`,
    );
    return false;
  }
  state.error = archiveEntryError(entry, state.seen);
  return state.error === null;
}

function assertRequiredArchiveEntries(seen: Set<string>): void {
  if (!seen.has(BACKUP_METADATA_FILE)) {
    throw new Error(`Backup archive is missing ${BACKUP_METADATA_FILE}`);
  }
  if (!seen.has(BACKUP_DATABASE_FILE)) {
    throw new Error(`Backup archive is missing ${BACKUP_DATABASE_FILE}`);
  }
}

async function scanBackupArchive(
  backupPath: string,
  configuredLimits: BackupValidationLimits = {},
): Promise<number> {
  const limits = resolveBackupLimits(configuredLimits);
  const state: ArchiveGuardState = {
    entries: 0,
    totalBytes: 0,
    seen: new Set(),
    error: null,
  };
  await tar.t({
    file: resolve(backupPath),
    gzip: true,
    strict: true,
    maxDecompressionRatio: limits.maxDecompressionRatio,
    onReadEntry: (entry) => {
      guardArchiveEntry(entry, state, limits);
    },
  });
  if (state.error) throw state.error;
  assertRequiredArchiveEntries(state.seen);
  return state.totalBytes;
}

export async function inspectRestore(
  backupPath: string,
  dataDir: string,
  options: RestoreInspectionOptions = {},
): Promise<RestorePreflight> {
  await fs.mkdir(dataDir, { recursive: true });
  const archiveBytes = await scanBackupArchive(backupPath, options);
  const requiredBytes = archiveBytes + RESTORE_FREE_SPACE_RESERVE_BYTES;
  if (!Number.isSafeInteger(requiredBytes)) {
    throw new Error("Restore disk requirement exceeds the supported numeric range");
  }
  const freeBytes = await (options.readFreeBytes ?? readFreeDiskBytes)(dataDir);
  if (!Number.isSafeInteger(freeBytes) || freeBytes < 0) {
    throw new Error("Could not determine available disk space for restore");
  }
  return {
    archiveBytes,
    requiredBytes,
    freeBytes,
    sufficient: freeBytes >= requiredBytes,
  };
}

function sameRoots(left: readonly FullBackupRoot[], right: readonly FullBackupRoot[]): boolean {
  if (left.length !== right.length) return false;
  const leftSet = new Set(left);
  return leftSet.size === left.length && right.every((root) => leftSet.has(root));
}

function parseBackupMetadata(value: unknown): BackupMetadata {
  const metadata = value;
  if (
    !isRecord(metadata) ||
    typeof metadata.version !== "string" ||
    typeof metadata.createdAt !== "string" ||
    Number.isNaN(Date.parse(metadata.createdAt)) ||
    typeof metadata.appVersion !== "string" ||
    typeof metadata.formatVersion !== "number" ||
    !Number.isInteger(metadata.formatVersion) ||
    metadata.formatVersion < 1
  ) {
    throw new Error("Invalid backup metadata: missing or invalid required fields");
  }
  if (metadata.formatVersion > BACKUP_FORMAT_VERSION) {
    throw new Error(
      `Backup format version ${metadata.formatVersion} is newer than supported ${BACKUP_FORMAT_VERSION}`,
    );
  }
  if (metadata.version !== String(metadata.formatVersion)) {
    throw new Error("Invalid backup metadata: version fields do not match");
  }
  if (metadata.formatVersion === 1 && metadata.version === "1") {
    return {
      version: "1",
      createdAt: metadata.createdAt,
      appVersion: metadata.appVersion,
      formatVersion: 1,
    };
  }
  if (
    metadata.formatVersion !== 2 ||
    metadata.version !== "2" ||
    !isRecord(metadata.scope) ||
    (metadata.scope.kind !== "full" && metadata.scope.kind !== "database-only") ||
    !Array.isArray(metadata.scope.includedRoots) ||
    !metadata.scope.includedRoots.every(isFullBackupRoot)
  ) {
    throw new Error("Invalid backup metadata: format v2 requires a valid scope");
  }
  const includedRoots = metadata.scope.includedRoots;
  if (metadata.scope.kind === "database-only") {
    if (includedRoots.length !== 0) {
      throw new Error("Invalid backup metadata: scope roots do not match its kind");
    }
    return {
      version: "2",
      createdAt: metadata.createdAt,
      appVersion: metadata.appVersion,
      formatVersion: 2,
      scope: { kind: "database-only", includedRoots: [] },
    };
  }
  if (!sameRoots(includedRoots, FULL_BACKUP_ROOT_PATHS)) {
    throw new Error("Invalid backup metadata: scope roots do not match its kind");
  }
  return {
    version: "2",
    createdAt: metadata.createdAt,
    appVersion: metadata.appVersion,
    formatVersion: 2,
    scope: {
      kind: "full",
      includedRoots: [...includedRoots],
    },
  };
}

function assertArchiveMatchesScope(metadata: BackupMetadata, seen: Set<string>): void {
  const includedRoots = new Set<string>(
    metadata.formatVersion === 1 ? LEGACY_BACKUP_ROOTS : metadata.scope.includedRoots,
  );
  for (const path of seen) {
    if (BACKUP_CORE_FILES.has(path)) continue;
    const root = backupRootForArchivePath(path);
    if (root && !includedRoots.has(root.path)) {
      throw new Error(`Backup entry is outside the declared scope: ${path}`);
    }
  }
}

function readValidatedMetadata(tempDir: string, seen: Set<string>): BackupMetadata {
  const metadataContent = readFileSync(join(tempDir, BACKUP_METADATA_FILE), "utf-8");
  const parsedMetadata: unknown = JSON.parse(metadataContent);
  const metadata = parseBackupMetadata(parsedMetadata);
  assertArchiveMatchesScope(metadata, seen);
  const database = readFileSync(join(tempDir, BACKUP_DATABASE_FILE));
  if (
    database.length < SQLITE_HEADER.length ||
    !database.subarray(0, SQLITE_HEADER.length).equals(SQLITE_HEADER)
  ) {
    throw new Error("Backup contains an invalid SQLite database");
  }
  let sqlite: Database.Database | null = null;
  try {
    sqlite = new Database(join(tempDir, BACKUP_DATABASE_FILE), { fileMustExist: true });
    sqlite.pragma("query_only = ON");
    const result = sqlite.pragma("integrity_check", { simple: true });
    if (result !== "ok") {
      throw new Error(`SQLite integrity_check returned: ${String(result)}`);
    }
  } catch (error) {
    throw new Error(
      `Backup SQLite integrity_check failed: ${error instanceof Error ? error.message : String(error)}`,
      { cause: error },
    );
  } finally {
    sqlite?.close();
  }
  return metadata;
}

async function extractValidatedBackup(
  backupPath: string,
  tempDir: string,
  extractDataDirectories: boolean,
  configuredLimits: BackupValidationLimits = {},
): Promise<BackupMetadata> {
  const limits = resolveBackupLimits(configuredLimits);
  const state: ArchiveGuardState = {
    entries: 0,
    totalBytes: 0,
    seen: new Set(),
    error: null,
  };
  await tar.x({
    file: backupPath,
    gzip: true,
    strict: true,
    cwd: tempDir,
    maxDecompressionRatio: limits.maxDecompressionRatio,
    filter: (_path, entry) => {
      const readEntry = entry as ReadEntry;
      const safe = guardArchiveEntry(readEntry, state, limits);
      if (!safe || readEntry.meta) return false;
      const path = normalizedArchivePath(readEntry.path);
      return extractDataDirectories || BACKUP_CORE_FILES.has(path);
    },
  });
  if (state.error) throw state.error;
  assertRequiredArchiveEntries(state.seen);
  return readValidatedMetadata(tempDir, state.seen);
}

/**
 * Creates an application-consistent backup of the SQLite database and critical directories.
 * Uses SQLite's backup API to ensure consistency.
 */
export async function createBackup(
  sqlite: BackupDatabase | null,
  dataDir: string,
  appVersion: string,
  outputPath: string,
  options: CreateBackupOptions = {},
): Promise<void> {
  if (!sqlite) {
    throw new Error("SQLite database not available; backups require self-host mode");
  }

  // Ensure database is connected and healthy
  if (!sqlite.ping()) {
    throw new Error("SQLite database connection failed; cannot create backup");
  }

  const full = options.includeDataDirectories !== false;
  const scope: BackupScope = full
    ? { kind: "full", includedRoots: [...FULL_BACKUP_ROOT_PATHS] }
    : { kind: "database-only", includedRoots: [] };
  const metadata: BackupMetadataV2 = {
    version: "2",
    createdAt: new Date().toISOString(),
    appVersion,
    formatVersion: BACKUP_FORMAT_VERSION,
    scope,
  };

  mkdirSync(dataDir, { recursive: true });
  const outputDir = dirname(resolve(outputPath));
  mkdirSync(outputDir, { recursive: true });
  const workDir = mkdtempSync(join(dataDir, ".backup-work-"));
  const temporaryArchivePath = join(
    outputDir,
    `.${basename(outputPath)}.tmp-${randomUUID()}`,
  );

  try {
    const backupDbPath = join(workDir, BACKUP_DATABASE_FILE);
    await sqlite.backupToFile(backupDbPath);
    writeFileSync(
      join(workDir, BACKUP_METADATA_FILE),
      JSON.stringify(metadata, null, 2),
    );

    const archiveEntries: string[] = [];
    for (const root of full ? FULL_BACKUP_ROOTS : []) {
      const source = join(dataDir, root.path);
      try {
        const stats = await fs.lstat(source);
        if (stats.isSymbolicLink()) {
          throw new Error(`Backup root cannot be a symbolic link: ${root.path}`);
        }
        if (
          (root.kind === "directory" && !stats.isDirectory()) ||
          (root.kind === "file" && !stats.isFile())
        ) {
          throw new Error(`Backup root has the wrong file type: ${root.path}`);
        }
      } catch (error) {
        if (isMissingPathError(error)) continue;
        throw error;
      }
      archiveEntries.push(root.path);
    }

    const snapshotRelativePath = join(
      basename(workDir),
      BACKUP_DATABASE_FILE,
    ).replace(/\\/g, "/");
    const metadataRelativePath = join(
      basename(workDir),
      BACKUP_METADATA_FILE,
    ).replace(/\\/g, "/");
    await tar.c(
      {
        file: temporaryArchivePath,
        gzip: true,
        cwd: dataDir,
        strict: true,
        portable: true,
        filter: (_path, value) => {
          const stats = value as Stats;
          return !stats.isSymbolicLink() && (stats.isFile() || stats.isDirectory());
        },
        onWriteEntry: (entry) => {
          if (entry.path === snapshotRelativePath) entry.path = BACKUP_DATABASE_FILE;
          else if (entry.path === metadataRelativePath) entry.path = BACKUP_METADATA_FILE;
        },
      },
      [snapshotRelativePath, metadataRelativePath, ...archiveEntries],
    );
    // Windows flushes only a handle with write access; a read-only sync fails with EPERM.
    const archiveHandle = await fs.open(temporaryArchivePath, "r+");
    try {
      await archiveHandle.sync();
    } finally {
      await archiveHandle.close();
    }
    await validateBackup(
      temporaryArchivePath,
      resolveCreationLimits(options.validationLimits),
    );
    await fs.rename(temporaryArchivePath, resolve(outputPath));
  } finally {
    try {
      await fs.rm(temporaryArchivePath, { force: true });
    } catch {
      // Ignore cleanup errors.
    }
    try {
      await fs.rm(workDir, { recursive: true, force: true });
    } catch {
      // Ignore cleanup errors.
    }
  }
}

/**
 * Validates archive entries, metadata, and SQLite integrity in an isolated
 * temporary directory without mutating live application data.
 */
export async function validateBackup(
  backupPath: string,
  limits: BackupValidationLimits = {},
): Promise<BackupMetadata> {
  const absoluteBackupPath = resolve(backupPath);
  const tempDir = mkdtempSync(join(tmpdir(), "pp-backup-validate-"));

  try {
    // Validation writes only metadata/database files. All other entries are
    // parsed and bounded, but not extracted.
    return await extractValidatedBackup(absoluteBackupPath, tempDir, false, limits);
  } finally {
    try {
      await fs.rm(tempDir, { recursive: true, force: true });
    } catch {
      // Ignore cleanup errors
    }
  }
}

type RestoreReplacement =
  | Readonly<{
      kind: "publish";
      livePath: string;
      previousPath: string;
      stagedPath: string;
    }>
  | Readonly<{
      kind: "remove";
      livePath: string;
      previousPath: string;
    }>;

type PreviousRestorePath =
  | Readonly<{ kind: "absent" }>
  | Readonly<{ kind: "moved"; path: string }>;

type ActivatedRestorePath = Readonly<{
  failedPath: string;
  livePath: string;
  previous: PreviousRestorePath;
}>;

type RestoreFileTransaction = Readonly<{
  activate(): Promise<void>;
  rollback(): Promise<Error[]>;
}>;

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isMissingPathError(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}

async function pathExists(path: string): Promise<boolean> {
  try {
    await fs.lstat(path);
    return true;
  } catch (error) {
    if (isMissingPathError(error)) return false;
    throw error;
  }
}

function restoreFailure(primaryError: unknown, recoveryErrors: readonly Error[]): Error {
  const recoveryDetail = recoveryErrors.length
    ? ` Recovery also failed: ${recoveryErrors.map((error) => error.message).join("; ")}`
    : "";
  const cause = primaryError instanceof Error ? primaryError : new Error(String(primaryError));
  return new Error(`Restore failed: ${cause.message}.${recoveryDetail}`, { cause });
}

function createRestoreFileTransaction(
  replacements: readonly RestoreReplacement[],
): RestoreFileTransaction {
  const activated: ActivatedRestorePath[] = [];

  return {
    async activate() {
      for (const replacement of replacements) {
        let previous: PreviousRestorePath = { kind: "absent" };
        if (await pathExists(replacement.livePath)) {
          await fs.rename(replacement.livePath, replacement.previousPath);
          previous = { kind: "moved", path: replacement.previousPath };
        }
        activated.push({
          failedPath: `${replacement.previousPath}.failed`,
          livePath: replacement.livePath,
          previous,
        });
        if (replacement.kind === "publish") {
          await fs.rename(replacement.stagedPath, replacement.livePath);
        }
      }
    },

    async rollback() {
      const errors: Error[] = [];
      for (let index = activated.length - 1; index >= 0; index -= 1) {
        const entry = activated[index];
        if (!entry) continue;
        let livePathExists: boolean;
        try {
          livePathExists = await pathExists(entry.livePath);
        } catch (error) {
          errors.push(
            new Error(`Could not inspect failed restored path ${entry.livePath}: ${errorMessage(error)}`, {
              cause: error,
            }),
          );
          continue;
        }
        if (livePathExists) {
          try {
            await fs.rename(entry.livePath, entry.failedPath);
          } catch (error) {
            errors.push(
              new Error(`Could not move failed restored path ${entry.livePath}: ${errorMessage(error)}`, {
                cause: error,
              }),
            );
            continue;
          }
        }
        if (entry.previous.kind === "moved") {
          try {
            await fs.rename(entry.previous.path, entry.livePath);
          } catch (error) {
            errors.push(
              new Error(`Could not restore previous path ${entry.livePath}: ${errorMessage(error)}`, {
                cause: error,
              }),
            );
          }
        }
      }
      return errors;
    },
  };
}

async function restoreReplacements(
  tempDir: string,
  dataDir: string,
  databasePath: string,
  metadata: BackupMetadata,
): Promise<RestoreReplacement[]> {
  const previousRoot = join(tempDir, ".previous");
  await fs.mkdir(previousRoot);
  const replacements: RestoreReplacement[] = [];

  const roots =
    metadata.formatVersion === 1
      ? LEGACY_BACKUP_ROOTS
      : metadata.scope.includedRoots;
  for (const root of roots) {
    const stagedPath = join(tempDir, root);
    const staged = await pathExists(stagedPath);
    if (metadata.formatVersion === 1 && !staged) continue;
    replacements.push(
      staged
        ? {
            kind: "publish",
            stagedPath,
            livePath: join(dataDir, root),
            previousPath: join(previousRoot, root),
          }
        : {
            kind: "remove",
            livePath: join(dataDir, root),
            previousPath: join(previousRoot, root),
          },
    );
  }

  replacements.push({
    kind: "publish",
    stagedPath: join(tempDir, BACKUP_DATABASE_FILE),
    livePath: databasePath,
    previousPath: join(previousRoot, BACKUP_DATABASE_FILE),
  });

  const stagedWalPath = join(tempDir, BACKUP_WAL_FILE);
  replacements.push(
    (await pathExists(stagedWalPath))
      ? {
          kind: "publish",
          stagedPath: stagedWalPath,
          livePath: `${databasePath}-wal`,
          previousPath: join(previousRoot, `${BACKUP_DATABASE_FILE}-wal`),
        }
      : {
          kind: "remove",
          livePath: `${databasePath}-wal`,
          previousPath: join(previousRoot, `${BACKUP_DATABASE_FILE}-wal`),
        },
    {
      kind: "remove",
      livePath: `${databasePath}-shm`,
      previousPath: join(previousRoot, `${BACKUP_DATABASE_FILE}-shm`),
    },
  );

  return replacements;
}

/**
 * Restores a backup archive, replacing current data.
 * Should only be called when the application is stopped or in maintenance mode.
 */
export async function restoreBackup(
  backupPath: string,
  dataDir: string,
  sqlite: SqliteDatabase | null,
  options: RestoreInspectionOptions = {},
): Promise<BackupMetadata> {
  if (!sqlite) {
    throw new Error("SQLite database not available; restore requires self-host mode");
  }

  mkdirSync(dataDir, { recursive: true });
  const preflight = await inspectRestore(backupPath, dataDir, options);
  if (!preflight.sufficient) {
    throw new InsufficientRestoreSpaceError(preflight);
  }
  const tempDir = mkdtempSync(join(dataDir, ".restore-tmp-"));
  let preserveRecoveryFiles = false;

  try {
    let metadata: BackupMetadata;
    let transaction: RestoreFileTransaction;
    try {
      metadata = await extractValidatedBackup(resolve(backupPath), tempDir, true, options);
      transaction = createRestoreFileTransaction(
        await restoreReplacements(tempDir, dataDir, sqlite.dbPath, metadata),
      );
    } catch (error) {
      throw restoreFailure(error, []);
    }

    try {
      sqlite.close();
    } catch (error) {
      throw restoreFailure(error, []);
    }

    try {
      await transaction.activate();
      sqlite.connect();
      return metadata;
    } catch (error) {
      const recoveryErrors: Error[] = [];
      try {
        sqlite.close();
      } catch (closeError) {
        recoveryErrors.push(
          new Error(`Could not close the failed restored database: ${errorMessage(closeError)}`, {
            cause: closeError,
          }),
        );
      }

      const rollbackErrors = await transaction.rollback();
      recoveryErrors.push(...rollbackErrors);
      preserveRecoveryFiles = rollbackErrors.length > 0;

      try {
        sqlite.connect();
      } catch (reconnectError) {
        recoveryErrors.push(
          new Error(`Could not reconnect the previous database: ${errorMessage(reconnectError)}`, {
            cause: reconnectError,
          }),
        );
      }

      throw restoreFailure(error, recoveryErrors);
    }
  } finally {
    if (!preserveRecoveryFiles) {
      try {
        await fs.rm(tempDir, { recursive: true, force: true });
      } catch {
        // A completed restore or rollback does not depend on workspace cleanup.
      }
    }
  }
}
