import { randomBytes } from "node:crypto";
import {
  closeSync,
  constants,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  realpathSync,
  renameSync,
  rmSync,
  writeSync,
} from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { chargeTenantDiskBytes } from "../lib/tenant-disk-quota.js";

function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("code" in error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}

function isContained(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path));
}

function isSafeSegment(segment: string): boolean {
  return (
    segment.length > 0 &&
    segment !== "." &&
    segment !== ".." &&
    !segment.includes("/") &&
    !segment.includes("\\") &&
    !segment.includes("\0")
  );
}

function ensureAcceptedExportDirectory(root: string, segments: readonly string[]): string {
  const exportRoot = resolve(root);
  mkdirSync(exportRoot, { recursive: true, mode: 0o700 });
  const rootStats = lstatSync(exportRoot);
  if (!rootStats.isDirectory() || rootStats.isSymbolicLink()) {
    throw new Error("Invalid accepted export root");
  }
  const resolvedRoot = realpathSync(exportRoot);
  let current = exportRoot;
  for (const segment of segments) {
    if (!isSafeSegment(segment)) throw new Error("Invalid accepted export segment");
    current = join(current, segment);
    try {
      mkdirSync(current, { mode: 0o700 });
    } catch (error) {
      if (errorCode(error) !== "EEXIST") throw error;
    }
    const stats = lstatSync(current);
    if (!stats.isDirectory() || stats.isSymbolicLink()) {
      throw new Error("Invalid accepted export directory");
    }
    if (!isContained(resolvedRoot, realpathSync(current))) {
      throw new Error("Accepted export directory escaped its root");
    }
  }
  return current;
}

export function writeAcceptedExportFile(
  input: Readonly<{
    root: string;
    directorySegments: readonly string[];
    filename: string;
    bytes: Uint8Array;
  }>,
): string {
  if (!isSafeSegment(input.filename)) throw new Error("Invalid accepted export filename");
  const directory = ensureAcceptedExportDirectory(input.root, input.directorySegments);
  const target = join(directory, input.filename);
  const temporary = join(
    directory,
    `.${input.filename}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`,
  );
  let descriptor: number | null = null;
  let renamed = false;
  try {
    chargeTenantDiskBytes(input.bytes.byteLength);
    descriptor = openSync(
      temporary,
      constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
      0o600,
    );
    let position = 0;
    while (position < input.bytes.length) {
      const written = writeSync(
        descriptor,
        input.bytes,
        position,
        input.bytes.length - position,
        position,
      );
      if (written === 0) throw new Error("Accepted export write made no progress");
      position += written;
    }
    fsyncSync(descriptor);
    closeSync(descriptor);
    descriptor = null;
    renameSync(temporary, target);
    renamed = true;
    return target;
  } finally {
    if (descriptor != null) closeSync(descriptor);
    if (!renamed) rmSync(temporary, { force: true });
  }
}
