import { createHash, timingSafeEqual } from "node:crypto";
import {
  closeSync,
  constants,
  createReadStream,
  fstatSync,
  lstatSync,
  openSync,
  readSync,
  readdirSync,
  realpathSync,
} from "node:fs";
import type { ReadStream } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import type { AcceptedOperationalArtifact } from "../db/accepted-plan-operational.js";

export type AcceptedArtifactObservationFailure =
  | "missing"
  | "unsafe_path"
  | "ambiguous_case"
  | "symlink"
  | "not_file"
  | "empty"
  | "too_large"
  | "io_error";

export type AcceptedArtifactVerificationFailure =
  AcceptedArtifactObservationFailure | "digest_mismatch";

type AcceptedArtifactObservation =
  | { readonly kind: "available" }
  | {
      readonly kind: "unavailable";
      readonly reason: "legacy" | "untracked_source";
    }
  | { readonly kind: "unusable"; readonly reason: AcceptedArtifactObservationFailure };

type ObserveAcceptedArtifactInput = {
  readonly reposDir: string;
  readonly artifact: AcceptedOperationalArtifact;
  readonly maxBytes?: number;
};

type VerifiedAcceptedArtifactLease = {
  readonly expectedSha256: string;
  readonly size: number;
  createReadStream(): ReadStream;
  close(): void;
};

type OpenVerifiedAcceptedArtifactResult =
  | { readonly kind: "verified"; readonly lease: VerifiedAcceptedArtifactLease }
  | Extract<AcceptedArtifactObservation, { readonly kind: "unavailable" }>
  | { readonly kind: "unusable"; readonly reason: AcceptedArtifactVerificationFailure };

const HASH_BUFFER_BYTES = 64 * 1024;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;

function isSafeRelativePath(value: string): boolean {
  const segments = value.split("/");
  return (
    value.length > 0 &&
    !value.includes("\\") &&
    !value.includes("\0") &&
    !value.startsWith("/") &&
    !/^[A-Za-z]:/.test(value) &&
    segments.every((segment) => segment.length > 0 && segment !== "." && segment !== "..")
  );
}

function isContained(root: string, candidate: string): boolean {
  const relativePath = relative(root, candidate);
  return relativePath !== ".." && !relativePath.startsWith(`..${sep}`) && !isAbsolute(relativePath);
}

function fileErrorReason(error: unknown): "missing" | "symlink" | "io_error" {
  if (typeof error === "object" && error !== null && "code" in error) {
    const code = error.code;
    if (code === "ENOENT" || code === "ENOTDIR") return "missing";
    if (code === "ELOOP") return "symlink";
  }
  return "io_error";
}

type ResolvedArtifactPath =
  | { readonly kind: "resolved"; readonly path: string }
  | { readonly kind: "unusable"; readonly reason: AcceptedArtifactObservationFailure };

type ResolvedSnapshotRoot =
  | { readonly kind: "resolved"; readonly path: string }
  | { readonly kind: "unusable"; readonly reason: AcceptedArtifactObservationFailure };

function resolveSnapshotRoot(input: {
  readonly reposDir: string;
  readonly snapshotRoot: string;
}): ResolvedSnapshotRoot {
  try {
    const configuredReposRoot = resolve(input.reposDir);
    const snapshotPath = resolve(input.snapshotRoot);
    if (!isContained(configuredReposRoot, snapshotPath) || snapshotPath === configuredReposRoot) {
      return { kind: "unusable", reason: "unsafe_path" };
    }
    let snapshotCursor = configuredReposRoot;
    for (const segment of relative(configuredReposRoot, snapshotPath).split(sep)) {
      snapshotCursor = join(snapshotCursor, segment);
      const stats = lstatSync(snapshotCursor);
      if (stats.isSymbolicLink()) return { kind: "unusable", reason: "symlink" };
      if (!stats.isDirectory()) return { kind: "unusable", reason: "not_file" };
    }
    const reposRoot = realpathSync(configuredReposRoot);
    const snapshotRoot = realpathSync(snapshotCursor);
    if (!isContained(reposRoot, snapshotRoot) || snapshotRoot === reposRoot) {
      return { kind: "unusable", reason: "unsafe_path" };
    }
    return { kind: "resolved", path: snapshotRoot };
  } catch (error) {
    return { kind: "unusable", reason: fileErrorReason(error) };
  }
}

export function observeAcceptedSnapshotRoot(input: {
  readonly reposDir: string;
  readonly snapshotRoot: string;
}):
  | { readonly kind: "available" }
  | { readonly kind: "unusable"; readonly reason: AcceptedArtifactObservationFailure } {
  const result = resolveSnapshotRoot(input);
  return result.kind === "resolved"
    ? { kind: "available" }
    : { kind: "unusable", reason: result.reason };
}

function resolveArtifactPath(input: {
  readonly reposDir: string;
  readonly snapshotRoot: string;
  readonly relativePath: string;
  readonly directoryEntries?: Map<string, string[]>;
}): ResolvedArtifactPath {
  if (!isSafeRelativePath(input.relativePath)) {
    return { kind: "unusable", reason: "unsafe_path" };
  }
  const root = resolveSnapshotRoot(input);
  if (root.kind === "unusable") return root;
  try {
    let current = root.path;
    const segments = input.relativePath.split("/");
    for (const [index, segment] of segments.entries()) {
      let entries = input.directoryEntries?.get(current);
      if (!entries) {
        entries = readdirSync(current);
        input.directoryEntries?.set(current, entries);
      }
      const matches = entries.filter((entry) => entry.toLowerCase() === segment.toLowerCase());
      const [match] = matches;
      if (!match) return { kind: "unusable", reason: "missing" };
      if (matches.length > 1) {
        return { kind: "unusable", reason: "ambiguous_case" };
      }
      current = join(current, match);
      const stats = lstatSync(current);
      if (stats.isSymbolicLink()) return { kind: "unusable", reason: "symlink" };
      const final = index === segments.length - 1;
      if (!final && !stats.isDirectory()) {
        return { kind: "unusable", reason: "not_file" };
      }
    }
    return { kind: "resolved", path: current };
  } catch (error) {
    return { kind: "unusable", reason: fileErrorReason(error) };
  }
}

/** One synchronous observation batch only; never used to authorize file reads. */
export function createAcceptedArtifactObserver() {
  const directoryEntries = new Map<string, string[]>();
  return (input: ObserveAcceptedArtifactInput): AcceptedArtifactObservation =>
    observeArtifact(input, directoryEntries);
}

function observeArtifact(
  input: ObserveAcceptedArtifactInput,
  directoryEntries?: Map<string, string[]>,
): AcceptedArtifactObservation {
  if (input.artifact.kind === "unavailable") {
    return { kind: "unavailable", reason: input.artifact.reason };
  }
  const resolved = resolveArtifactPath({
    reposDir: input.reposDir,
    snapshotRoot: input.artifact.snapshotRoot,
    relativePath: input.artifact.relativePath,
    directoryEntries,
  });
  if (resolved.kind === "unusable") return resolved;
  try {
    const stats = lstatSync(resolved.path);
    if (!stats.isFile()) return { kind: "unusable", reason: "not_file" };
    if (stats.size === 0) return { kind: "unusable", reason: "empty" };
    if (input.maxBytes != null && stats.size > input.maxBytes) {
      return { kind: "unusable", reason: "too_large" };
    }
    return { kind: "available" };
  } catch (error) {
    return { kind: "unusable", reason: fileErrorReason(error) };
  }
}

function sha256Descriptor(descriptor: number): {
  readonly digest: Buffer;
  readonly hashedBytes: number;
} {
  const hash = createHash("sha256");
  const bytes = Buffer.allocUnsafe(HASH_BUFFER_BYTES);
  let position = 0;
  for (;;) {
    const bytesRead = readSync(descriptor, bytes, 0, bytes.length, position);
    if (bytesRead === 0) return { digest: hash.digest(), hashedBytes: position };
    hash.update(bytes.subarray(0, bytesRead));
    position += bytesRead;
  }
}

export function openVerifiedAcceptedArtifact(
  input: ObserveAcceptedArtifactInput,
): OpenVerifiedAcceptedArtifactResult {
  if (input.artifact.kind === "unavailable") {
    return { kind: "unavailable", reason: input.artifact.reason };
  }
  if (!SHA256_PATTERN.test(input.artifact.expectedSha256)) {
    return { kind: "unusable", reason: "digest_mismatch" };
  }
  const resolved = resolveArtifactPath({
    reposDir: input.reposDir,
    snapshotRoot: input.artifact.snapshotRoot,
    relativePath: input.artifact.relativePath,
  });
  if (resolved.kind === "unusable") return resolved;

  let descriptor: number | null = null;
  try {
    const beforeOpen = lstatSync(resolved.path);
    if (beforeOpen.isSymbolicLink()) return { kind: "unusable", reason: "symlink" };
    descriptor = openSync(resolved.path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = fstatSync(descriptor);
    if (!opened.isFile()) return { kind: "unusable", reason: "not_file" };
    if (beforeOpen.dev !== opened.dev || beforeOpen.ino !== opened.ino) {
      return { kind: "unusable", reason: "io_error" };
    }
    if (opened.size === 0) return { kind: "unusable", reason: "empty" };
    if (input.maxBytes != null && opened.size > input.maxBytes) {
      return { kind: "unusable", reason: "too_large" };
    }

    const hashed = sha256Descriptor(descriptor);
    if (hashed.hashedBytes === 0) return { kind: "unusable", reason: "empty" };
    if (input.maxBytes != null && hashed.hashedBytes > input.maxBytes) {
      return { kind: "unusable", reason: "too_large" };
    }
    const expectedDigest = Buffer.from(input.artifact.expectedSha256, "hex");
    if (!timingSafeEqual(hashed.digest, expectedDigest)) {
      return { kind: "unusable", reason: "digest_mismatch" };
    }

    const leasedDescriptor = descriptor;
    descriptor = null;
    let closed = false;
    const lease: VerifiedAcceptedArtifactLease = {
      expectedSha256: input.artifact.expectedSha256,
      size: hashed.hashedBytes,
      createReadStream() {
        if (closed) throw new Error("Accepted artifact lease is closed");
        return createReadStream(resolved.path, {
          fd: leasedDescriptor,
          autoClose: false,
          start: 0,
          end: hashed.hashedBytes - 1,
        });
      },
      close() {
        if (closed) return;
        closed = true;
        closeSync(leasedDescriptor);
      },
    };
    return { kind: "verified", lease };
  } catch (error) {
    return { kind: "unusable", reason: fileErrorReason(error) };
  } finally {
    if (descriptor != null) closeSync(descriptor);
  }
}
