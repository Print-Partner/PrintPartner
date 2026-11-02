import { createHash } from "node:crypto";
import { createReadStream, createWriteStream, type Dirent } from "node:fs";
import { lstat, mkdir, mkdtemp, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, posix, resolve } from "node:path";
import { Readable, Transform, Writable, type TransformCallback } from "node:stream";
import { pipeline } from "node:stream/promises";
import { chargeTenantDiskBytes } from "../lib/tenant-disk-quota.js";
import { isRecord } from "../lib/guards.js";

export const SOURCE_SNAPSHOT_MANIFEST_FILE = ".printpartner-source-snapshot.json";
export const MAX_SOURCE_SNAPSHOT_BYTES = 1024 * 1024 * 1024;

const SOURCE_SNAPSHOT_MANIFEST_VERSION = 1;
const SHA256_PATTERN = /^[a-f0-9]{64}$/;
const REVISION_KEY_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,199}$/;

declare const sourceRelativePathBrand: unique symbol;

export type SourceRelativePath = string & {
  readonly [sourceRelativePathBrand]: "SourceRelativePath";
};

export type SnapshotFileKind = "stl" | "artifact" | "readme" | "md" | "pdf";
type SnapshotDocumentationKind = Exclude<SnapshotFileKind, "stl" | "artifact">;

export type SnapshotFile = {
  path: SourceRelativePath;
  kind: SnapshotFileKind;
  sizeHintBytes: number | null;
};

export type OmittedSnapshotFile = {
  path: SourceRelativePath;
  kind: SnapshotDocumentationKind;
  sizeHintBytes: number | null;
  reason: "documentation-byte-budget" | "unknown-document-size";
};

export type SnapshotSelection = {
  maxStlFiles: number;
  maxDocumentationBytes: number;
  omittedFiles: readonly OmittedSnapshotFile[];
};

type SnapshotContentFile = {
  path: SourceRelativePath;
  kind: SnapshotFileKind;
  sizeBytes: number;
  sha256: string;
};

export type PublishedSourceSnapshot = {
  upstreamRevisionKey: string;
  manifestDigest: string;
  snapshotLocator: string;
  absolutePath: string;
  files: readonly SnapshotContentFile[];
  selection: SnapshotSelection;
  publication: "created" | "reused";
};

export type SnapshotFileResponse = {
  stream: Readable;
  contentLengthBytes: number | null;
};

type MaterializeSourceSnapshotInput = {
  beforePublish?: () => undefined;
  sourceId: number;
  upstreamRevisionKey: string;
  files: readonly SnapshotFile[];
  selection: SnapshotSelection;
  openFile(file: SnapshotFile): Promise<SnapshotFileResponse>;
};

type DeriveFileReplacementInput = {
  beforePublish?: () => undefined;
  sourceId: number;
  baseRevisionKey: string;
  sourceVersion: string;
  replacement: {
    path: SourceRelativePath;
    kind: SnapshotFileKind;
    content: Buffer;
  };
};

type SnapshotManifest = {
  version: 1;
  upstreamRevisionKey: string;
  manifestDigest: string;
  selection: SnapshotSelection;
  files: readonly SnapshotContentFile[];
};

type SnapshotStoreDependencies = {
  renameDirectory(from: string, to: string): Promise<void>;
};

type SnapshotStoreOptions = {
  reposDir: string;
  dependencies?: Partial<SnapshotStoreDependencies>;
  retainUntilRelease?: boolean;
};

const defaultDependencies: SnapshotStoreDependencies = {
  renameDirectory: rename,
};

const sharedSnapshotHolds = new Map<string, number>();
const snapshotHoldKeys = new WeakMap<PublishedSourceSnapshot, string>();

function retainSharedSnapshot(absolutePath: string): string {
  const key = resolve(absolutePath);
  sharedSnapshotHolds.set(key, (sharedSnapshotHolds.get(key) ?? 0) + 1);
  return key;
}

function releaseSharedSnapshotKey(key: string): void {
  const current = sharedSnapshotHolds.get(key) ?? 0;
  if (current <= 1) sharedSnapshotHolds.delete(key);
  else sharedSnapshotHolds.set(key, current - 1);
}

function handoffSharedSnapshot(snapshot: PublishedSourceSnapshot, key: string): void {
  snapshotHoldKeys.set(snapshot, key);
}

export function releaseSharedSnapshot(snapshot: PublishedSourceSnapshot): void {
  const key = snapshotHoldKeys.get(snapshot);
  if (!key) return;
  snapshotHoldKeys.delete(snapshot);
  releaseSharedSnapshotKey(key);
}

export function sharedSnapshotHolderCount(absolutePath: string): number {
  return sharedSnapshotHolds.get(resolve(absolutePath)) ?? 0;
}

export function sourceRelativePath(value: string): SourceRelativePath {
  if (
    !value ||
    value.includes("\\") ||
    value.includes("\0") ||
    posix.isAbsolute(value) ||
    value.normalize("NFC") !== value ||
    posix.normalize(value) !== value ||
    value.split("/").some((segment) => segment === "" || segment === "." || segment === "..")
  ) {
    throw new Error(`Unsafe Source snapshot path: ${value}`);
  }
  if (value === SOURCE_SNAPSHOT_MANIFEST_FILE) {
    throw new Error(`Source content cannot use reserved path: ${value}`);
  }
  return value as SourceRelativePath;
}

function hasErrorCode(value: unknown, codes: ReadonlySet<string>): boolean {
  return isRecord(value) && typeof value.code === "string" && codes.has(value.code);
}

function parseFileKind(value: unknown): SnapshotFileKind {
  switch (value) {
    case "stl":
    case "artifact":
    case "readme":
    case "md":
    case "pdf":
      return value;
    default:
      throw new Error("Snapshot manifest has an invalid file kind");
  }
}

function parseDocumentationKind(value: unknown): SnapshotDocumentationKind {
  const kind = parseFileKind(value);
  if (kind === "stl" || kind === "artifact") {
    throw new Error(
      "Snapshot manifest cannot omit a printable artifact through the documentation budget",
    );
  }
  return kind;
}

function parseNonNegativeInteger(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${field} must be a non-negative safe integer`);
  }
  return value;
}

function parseOptionalByteCount(value: unknown, field: string): number | null {
  if (value === null) return null;
  return parseNonNegativeInteger(value, field);
}

function compareByPath<T extends { path: string }>(left: T, right: T): number {
  if (left.path < right.path) return -1;
  if (left.path > right.path) return 1;
  return 0;
}

function compareStrings(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function normalizeFiles(files: readonly SnapshotFile[]): SnapshotFile[] {
  const normalized = files.map((file) => ({
    path: sourceRelativePath(file.path),
    kind: parseFileKind(file.kind),
    sizeHintBytes: parseOptionalByteCount(file.sizeHintBytes, `Size hint for ${file.path}`),
  }));
  const exactPaths = new Set<string>();
  const foldedPaths = new Set<string>();
  for (const file of normalized) {
    const foldedPath = file.path.toLocaleLowerCase("en-US");
    if (exactPaths.has(file.path) || foldedPaths.has(foldedPath)) {
      throw new Error(`Duplicate Source snapshot path: ${file.path}`);
    }
    exactPaths.add(file.path);
    foldedPaths.add(foldedPath);
  }
  return normalized.sort(compareByPath);
}

function normalizeSelection(
  selection: SnapshotSelection,
  selectedFiles: readonly SnapshotFile[],
): SnapshotSelection {
  const maxStlFiles = parseNonNegativeInteger(selection.maxStlFiles, "STL file limit");
  const maxDocumentationBytes = parseNonNegativeInteger(
    selection.maxDocumentationBytes,
    "Documentation byte limit",
  );
  const selectedPaths = new Set(selectedFiles.map((file) => file.path));
  const omittedPaths = new Set<string>();
  const foldedPaths = new Set<string>();
  const omittedFiles = selection.omittedFiles.map((file) => {
    const path = sourceRelativePath(file.path);
    const foldedPath = path.toLocaleLowerCase("en-US");
    if (omittedPaths.has(path) || foldedPaths.has(foldedPath) || selectedPaths.has(path)) {
      throw new Error(`Duplicate selected or omitted Source snapshot path: ${path}`);
    }
    if (file.reason !== "documentation-byte-budget" && file.reason !== "unknown-document-size") {
      throw new Error(`Invalid omission reason for ${path}`);
    }
    omittedPaths.add(path);
    foldedPaths.add(foldedPath);
    return {
      path,
      kind: parseDocumentationKind(file.kind),
      sizeHintBytes: parseOptionalByteCount(file.sizeHintBytes, `Size hint for ${path}`),
      reason: file.reason,
    } satisfies OmittedSnapshotFile;
  });

  return {
    maxStlFiles,
    maxDocumentationBytes,
    omittedFiles: omittedFiles.sort(compareByPath),
  };
}

function validateSourceId(sourceId: number): void {
  if (!Number.isSafeInteger(sourceId) || sourceId <= 0) {
    throw new Error("Source ID must be a positive safe integer");
  }
}

function validateRevisionKey(upstreamRevisionKey: string): void {
  if (
    !REVISION_KEY_PATTERN.test(upstreamRevisionKey) ||
    upstreamRevisionKey === "." ||
    upstreamRevisionKey === ".."
  ) {
    throw new Error(`Unsafe upstream revision key: ${upstreamRevisionKey}`);
  }
}

function canonicalContent(files: readonly SnapshotContentFile[]): string {
  return JSON.stringify(files);
}

function manifestDigest(files: readonly SnapshotContentFile[]): string {
  return createHash("sha256").update(canonicalContent(files)).digest("hex");
}

function sameContentFiles(
  left: readonly SnapshotContentFile[],
  right: readonly SnapshotContentFile[],
): boolean {
  return canonicalContent(left) === canonicalContent(right);
}

function byteMeter(
  maxBytes = Number.POSITIVE_INFINITY,
  options: { chargeBytes?: boolean } = {},
): {
  stream: Transform;
  result(): { sizeBytes: number; sha256: string };
} {
  const hash = createHash("sha256");
  let sizeBytes = 0;
  const stream = new Transform({
    transform(chunk: unknown, encoding: BufferEncoding, callback: TransformCallback): void {
      let buffer: Buffer;
      if (Buffer.isBuffer(chunk)) buffer = chunk;
      else if (chunk instanceof Uint8Array) buffer = Buffer.from(chunk);
      else if (typeof chunk === "string") buffer = Buffer.from(chunk, encoding);
      else {
        callback(new TypeError("Source file stream emitted a non-byte chunk"));
        return;
      }
      sizeBytes += buffer.byteLength;
      if (sizeBytes > maxBytes) {
        callback(
          new Error(
            `Source snapshot exceeds the ${MAX_SOURCE_SNAPSHOT_BYTES} byte stored-content limit`,
          ),
        );
        return;
      }
      if (options.chargeBytes && buffer.byteLength > 0) {
        try {
          chargeTenantDiskBytes(buffer.byteLength);
        } catch (error) {
          callback(error instanceof Error ? error : new Error(String(error)));
          return;
        }
      }
      hash.update(buffer);
      callback(null, buffer);
    },
  });
  return {
    stream,
    result: () => ({ sizeBytes, sha256: hash.digest("hex") }),
  };
}

async function writeSnapshotFile(args: {
  candidateDir: string;
  file: SnapshotFile;
  openFile(file: SnapshotFile): Promise<SnapshotFileResponse>;
  maxBytes: number;
  beforePublish?: () => undefined;
}): Promise<SnapshotContentFile> {
  const destination = join(args.candidateDir, ...args.file.path.split("/"));
  await mkdir(dirname(destination), { recursive: true });
  const response = await args.openFile(args.file);
  let contentLengthBytes: number | null;
  try {
    contentLengthBytes = parseOptionalByteCount(
      response.contentLengthBytes,
      `Content-Length for ${args.file.path}`,
    );
    if (contentLengthBytes != null && contentLengthBytes > args.maxBytes) {
      throw new Error(
        `Source snapshot exceeds the ${MAX_SOURCE_SNAPSHOT_BYTES} byte stored-content limit`,
      );
    }
    args.beforePublish?.();
  } catch (error) {
    response.stream.destroy();
    throw error;
  }
  const meter = byteMeter(args.maxBytes, { chargeBytes: true });
  await pipeline(response.stream, meter.stream, createWriteStream(destination, { flags: "wx" }));
  const measured = meter.result();
  if (contentLengthBytes !== null && measured.sizeBytes !== contentLengthBytes) {
    throw new Error(
      `Source response length mismatch for ${args.file.path}: expected ${contentLengthBytes}, received ${measured.sizeBytes}`,
    );
  }
  return {
    path: args.file.path,
    kind: args.file.kind,
    sizeBytes: measured.sizeBytes,
    sha256: measured.sha256,
  };
}

async function hashFile(path: string): Promise<{ sizeBytes: number; sha256: string }> {
  const meter = byteMeter();
  const discard = new Writable({
    write(_chunk: unknown, _encoding: BufferEncoding, callback): void {
      callback();
    },
  });
  await pipeline(createReadStream(path), meter.stream, discard);
  return meter.result();
}

async function inspectDirectory(args: {
  root: string;
  relativeDir?: string;
}): Promise<{ files: SourceRelativePath[]; directories: SourceRelativePath[] }> {
  const relativeDir = args.relativeDir ?? "";
  let entries: Dirent[];
  try {
    entries = await readdir(relativeDir ? join(args.root, ...relativeDir.split("/")) : args.root, {
      withFileTypes: true,
    });
  } catch (error) {
    throw new Error(`Cannot inspect Source snapshot directory ${args.root}`, { cause: error });
  }

  const files: SourceRelativePath[] = [];
  const directories: SourceRelativePath[] = [];
  for (const entry of entries.sort((left, right) => compareStrings(left.name, right.name))) {
    const relativePath = relativeDir ? `${relativeDir}/${entry.name}` : entry.name;
    if (entry.isSymbolicLink()) {
      throw new Error(`Source snapshot contains a symbolic link: ${relativePath}`);
    }
    if (entry.isDirectory()) {
      const directoryPath = sourceRelativePath(relativePath);
      directories.push(directoryPath);
      const nested = await inspectDirectory({ root: args.root, relativeDir: directoryPath });
      files.push(...nested.files);
      directories.push(...nested.directories);
      continue;
    }
    if (!entry.isFile()) {
      throw new Error(`Source snapshot contains a non-file entry: ${relativePath}`);
    }
    if (!relativeDir && entry.name === SOURCE_SNAPSHOT_MANIFEST_FILE) continue;
    files.push(sourceRelativePath(relativePath));
  }
  return { files, directories };
}

function expectedDirectories(files: readonly SnapshotFile[]): Set<string> {
  const directories = new Set<string>();
  for (const file of files) {
    let directory = posix.dirname(file.path);
    while (directory !== ".") {
      directories.add(directory);
      directory = posix.dirname(directory);
    }
  }
  return directories;
}

async function validateDirectoryContent(
  directory: string,
  expectedFiles: readonly SnapshotFile[],
): Promise<SnapshotContentFile[]> {
  const inspected = await inspectDirectory({ root: directory });
  const actualPaths = inspected.files.map(String).sort(compareStrings);
  const expectedPaths = expectedFiles.map((file) => String(file.path));
  if (JSON.stringify(actualPaths) !== JSON.stringify(expectedPaths)) {
    throw new Error("Source snapshot files do not match the selected file set");
  }

  const allowedDirectories = expectedDirectories(expectedFiles);
  const actualDirectories = inspected.directories.map(String).sort();
  const wantedDirectories = [...allowedDirectories].sort();
  if (JSON.stringify(actualDirectories) !== JSON.stringify(wantedDirectories)) {
    throw new Error("Source snapshot directories do not match the selected file set");
  }

  const content: SnapshotContentFile[] = [];
  for (const file of expectedFiles) {
    const measured = await hashFile(join(directory, ...file.path.split("/")));
    content.push({
      path: file.path,
      kind: file.kind,
      sizeBytes: measured.sizeBytes,
      sha256: measured.sha256,
    });
  }
  return content.sort(compareByPath);
}

function parseManifestFile(value: unknown): SnapshotContentFile {
  if (!isRecord(value)) throw new Error("Snapshot manifest has an invalid content entry");
  const path = sourceRelativePath(typeof value.path === "string" ? value.path : "");
  const sha256 = value.sha256;
  if (typeof sha256 !== "string" || !SHA256_PATTERN.test(sha256)) {
    throw new Error(`Snapshot manifest has an invalid SHA-256 for ${path}`);
  }
  return {
    path,
    kind: parseFileKind(value.kind),
    sizeBytes: parseNonNegativeInteger(value.sizeBytes, `Stored bytes for ${path}`),
    sha256,
  };
}

function parseOmittedFile(value: unknown): OmittedSnapshotFile {
  if (!isRecord(value)) throw new Error("Snapshot manifest has an invalid omitted entry");
  const path = sourceRelativePath(typeof value.path === "string" ? value.path : "");
  if (value.reason !== "documentation-byte-budget" && value.reason !== "unknown-document-size") {
    throw new Error(`Snapshot manifest has an invalid omission reason for ${path}`);
  }
  return {
    path,
    kind: parseDocumentationKind(value.kind),
    sizeHintBytes: parseOptionalByteCount(value.sizeHintBytes, `Size hint for ${path}`),
    reason: value.reason,
  };
}

function parseManifest(raw: string): SnapshotManifest {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch (error) {
    throw new Error("Source snapshot manifest is not valid JSON", { cause: error });
  }
  if (!isRecord(value) || value.version !== SOURCE_SNAPSHOT_MANIFEST_VERSION) {
    throw new Error("Source snapshot manifest has an unsupported version");
  }
  if (typeof value.upstreamRevisionKey !== "string") {
    throw new Error("Source snapshot manifest has no upstream revision key");
  }
  if (typeof value.manifestDigest !== "string" || !SHA256_PATTERN.test(value.manifestDigest)) {
    throw new Error("Source snapshot manifest has an invalid digest");
  }
  if (!isRecord(value.selection) || !Array.isArray(value.selection.omittedFiles)) {
    throw new Error("Source snapshot manifest has an invalid selection record");
  }
  if (!Array.isArray(value.files)) {
    throw new Error("Source snapshot manifest has an invalid content list");
  }
  const files = value.files.map(parseManifestFile).sort(compareByPath);
  const selection: SnapshotSelection = {
    maxStlFiles: parseNonNegativeInteger(value.selection.maxStlFiles, "STL file limit"),
    maxDocumentationBytes: parseNonNegativeInteger(
      value.selection.maxDocumentationBytes,
      "Documentation byte limit",
    ),
    omittedFiles: value.selection.omittedFiles.map(parseOmittedFile).sort(compareByPath),
  };
  const normalizedFiles = normalizeFiles(
    files.map((file) => ({
      path: file.path,
      kind: file.kind,
      sizeHintBytes: file.sizeBytes,
    })),
  );
  normalizeSelection(selection, normalizedFiles);
  return {
    version: SOURCE_SNAPSHOT_MANIFEST_VERSION,
    upstreamRevisionKey: value.upstreamRevisionKey,
    manifestDigest: value.manifestDigest,
    selection,
    files,
  };
}

function validateStoredManifest(manifest: SnapshotManifest, revisionKey: string): void {
  if (manifest.upstreamRevisionKey !== revisionKey) {
    throw new Error("Source snapshot manifest revision does not match its directory");
  }
  if (manifestDigest(manifest.files) !== manifest.manifestDigest) {
    throw new Error("Existing Source snapshot manifest digest is invalid");
  }
}

async function loadExistingSnapshot(args: { finalDir: string; revisionKey: string }): Promise<{
  digest: string;
  files: readonly SnapshotContentFile[];
  selection: SnapshotSelection;
}> {
  const manifestPath = join(args.finalDir, SOURCE_SNAPSHOT_MANIFEST_FILE);
  const manifest = parseManifest(await readFile(manifestPath, "utf8"));
  validateStoredManifest(manifest, args.revisionKey);
  const storedFiles: SnapshotFile[] = manifest.files.map((file) => ({
    path: file.path,
    kind: file.kind,
    sizeHintBytes: file.sizeBytes,
  }));
  const actualFiles = await validateDirectoryContent(args.finalDir, storedFiles);
  if (!sameContentFiles(actualFiles, manifest.files)) {
    throw new Error("Existing Source snapshot content does not match its manifest");
  }
  return {
    digest: manifest.manifestDigest,
    files: manifest.files,
    selection: manifest.selection,
  };
}

async function pathExists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if (hasErrorCode(error, new Set(["ENOENT"]))) return false;
    throw error;
  }
}

export class LocalSourceSnapshotStore {
  readonly #reposDir: string;
  readonly #dependencies: SnapshotStoreDependencies;
  readonly #retainUntilRelease: boolean;

  constructor(options: SnapshotStoreOptions) {
    this.#reposDir = resolve(options.reposDir);
    this.#retainUntilRelease = options.retainUntilRelease === true;
    this.#dependencies = {
      renameDirectory: options.dependencies?.renameDirectory ?? defaultDependencies.renameDirectory,
    };
  }

  async #reuseExistingSnapshot(input: {
    finalDir: string;
    revisionKey: string;
    snapshotLocator: string;
  }): Promise<PublishedSourceSnapshot | null> {
    let heldKey = this.#retainUntilRelease ? retainSharedSnapshot(input.finalDir) : null;
    try {
      if (!(await pathExists(input.finalDir))) {
        if (heldKey) releaseSharedSnapshotKey(heldKey);
        heldKey = null;
        return null;
      }
      const existing = await loadExistingSnapshot({
        finalDir: input.finalDir,
        revisionKey: input.revisionKey,
      });
      const snapshot: PublishedSourceSnapshot = {
        upstreamRevisionKey: input.revisionKey,
        manifestDigest: existing.digest,
        snapshotLocator: input.snapshotLocator,
        absolutePath: input.finalDir,
        files: existing.files,
        selection: existing.selection,
        publication: "reused",
      };
      if (heldKey) handoffSharedSnapshot(snapshot, heldKey);
      heldKey = null;
      return snapshot;
    } catch (error) {
      if (heldKey) releaseSharedSnapshotKey(heldKey);
      throw error;
    }
  }

  async deriveFileReplacement(input: DeriveFileReplacementInput): Promise<PublishedSourceSnapshot> {
    validateSourceId(input.sourceId);
    validateRevisionKey(input.baseRevisionKey);
    if (!input.sourceVersion.trim()) throw new Error("Source version is required");
    const replacementPath = sourceRelativePath(input.replacement.path);
    const replacementKind = parseFileKind(input.replacement.kind);
    const replacementContent = Buffer.from(input.replacement.content);
    const baseDir = join(
      this.#reposDir,
      String(input.sourceId),
      "revisions",
      input.baseRevisionKey,
    );
    const base = await loadExistingSnapshot({
      finalDir: baseDir,
      revisionKey: input.baseRevisionKey,
    });
    const replacementFile: SnapshotContentFile = {
      path: replacementPath,
      kind: replacementKind,
      sizeBytes: replacementContent.byteLength,
      sha256: createHash("sha256").update(replacementContent).digest("hex"),
    };
    const derivedContent = [
      ...base.files.filter((file) => file.path !== replacementPath),
      replacementFile,
    ].sort(compareByPath);
    const derivedDigest = manifestDigest(derivedContent);
    if (derivedDigest === base.digest) {
      return {
        upstreamRevisionKey: input.baseRevisionKey,
        manifestDigest: base.digest,
        snapshotLocator: posix.join(String(input.sourceId), "revisions", input.baseRevisionKey),
        absolutePath: baseDir,
        files: base.files,
        selection: base.selection,
        publication: "reused",
      };
    }

    const derivedIdentityDigest = createHash("sha256")
      .update(
        JSON.stringify({
          manifestDigest: derivedDigest,
          selection: base.selection,
          sourceVersion: input.sourceVersion,
        }),
      )
      .digest("hex");
    const upstreamRevisionKey = `derived-${derivedIdentityDigest}`;
    const files: SnapshotFile[] = derivedContent.map((file) => ({
      path: file.path,
      kind: file.kind,
      sizeHintBytes: file.sizeBytes,
    }));
    return this.materialize({
      beforePublish: input.beforePublish,
      sourceId: input.sourceId,
      upstreamRevisionKey,
      files,
      selection: base.selection,
      openFile: async (file) => {
        if (file.path === replacementPath) {
          return {
            stream: Readable.from(replacementContent),
            contentLengthBytes: replacementContent.byteLength,
          };
        }
        const baseFile = base.files.find((candidate) => candidate.path === file.path);
        if (!baseFile) throw new Error(`Base Source snapshot file is missing: ${file.path}`);
        return {
          stream: createReadStream(join(baseDir, ...file.path.split("/"))),
          contentLengthBytes: baseFile.sizeBytes,
        };
      },
    });
  }

  async materialize(input: MaterializeSourceSnapshotInput): Promise<PublishedSourceSnapshot> {
    input.beforePublish?.();
    validateSourceId(input.sourceId);
    validateRevisionKey(input.upstreamRevisionKey);
    const files = normalizeFiles(input.files);
    const selection = normalizeSelection(input.selection, files);
    const snapshotLocator = posix.join(
      String(input.sourceId),
      "revisions",
      input.upstreamRevisionKey,
    );
    const revisionsRoot = join(this.#reposDir, String(input.sourceId), "revisions");
    const finalDir = join(revisionsRoot, input.upstreamRevisionKey);
    await mkdir(revisionsRoot, { recursive: true });

    const reused = await this.#reuseExistingSnapshot({
      finalDir,
      revisionKey: input.upstreamRevisionKey,
      snapshotLocator,
    });
    if (reused) return reused;

    input.beforePublish?.();
    const candidateDir = await mkdtemp(
      join(revisionsRoot, `.candidate-${input.upstreamRevisionKey}-`),
    );
    const publishCandidate = async (): Promise<PublishedSourceSnapshot> => {
      let storedBytes = 0;
      for (const file of files) {
        input.beforePublish?.();
        const written = await writeSnapshotFile({
          candidateDir,
          file,
          openFile: input.openFile,
          beforePublish: input.beforePublish,
          maxBytes: MAX_SOURCE_SNAPSHOT_BYTES - storedBytes,
        });
        storedBytes += written.sizeBytes;
      }
      const content = await validateDirectoryContent(candidateDir, files);
      const stlCount = content.filter((file) => file.kind === "stl").length;
      if (stlCount > selection.maxStlFiles) {
        throw new Error(
          `Source snapshot contains ${stlCount} STL files, exceeding the limit of ${selection.maxStlFiles}`,
        );
      }
      const nonStlBytes = content
        .filter((file) => file.kind !== "stl")
        .reduce((sum, file) => sum + file.sizeBytes, 0);
      if (nonStlBytes > selection.maxDocumentationBytes) {
        throw new Error(
          `Source documentation exceeds the ${selection.maxDocumentationBytes} byte limit after download, including non-STL artifacts`,
        );
      }
      const digest = manifestDigest(content);
      const manifest: SnapshotManifest = {
        version: SOURCE_SNAPSHOT_MANIFEST_VERSION,
        upstreamRevisionKey: input.upstreamRevisionKey,
        manifestDigest: digest,
        selection,
        files: content,
      };
      const manifestBytes = `${JSON.stringify(manifest, null, 2)}\n`;
      input.beforePublish?.();
      chargeTenantDiskBytes(Buffer.byteLength(manifestBytes));
      await writeFile(join(candidateDir, SOURCE_SNAPSHOT_MANIFEST_FILE), manifestBytes, {
        encoding: "utf8",
        flag: "wx",
      });

      try {
        input.beforePublish?.();
        await this.#dependencies.renameDirectory(candidateDir, finalDir);
      } catch (error) {
        if (hasErrorCode(error, new Set(["EXDEV"]))) {
          throw new Error(
            "Cannot publish Source snapshot across filesystems (EXDEV); no copy fallback is allowed",
            { cause: error },
          );
        }
        if (hasErrorCode(error, new Set(["EEXIST", "ENOTEMPTY"]))) {
          const reused = await this.#reuseExistingSnapshot({
            finalDir,
            revisionKey: input.upstreamRevisionKey,
            snapshotLocator,
          });
          if (reused) return reused;
        }
        throw error;
      }

      const snapshot: PublishedSourceSnapshot = {
        upstreamRevisionKey: input.upstreamRevisionKey,
        manifestDigest: digest,
        snapshotLocator,
        absolutePath: finalDir,
        files: content,
        selection,
        publication: "created",
      };
      if (this.#retainUntilRelease) handoffSharedSnapshot(snapshot, retainSharedSnapshot(finalDir));
      return snapshot;
    };

    let outcome: { ok: true; value: PublishedSourceSnapshot } | { ok: false; error: unknown };
    try {
      outcome = { ok: true, value: await publishCandidate() };
    } catch (error) {
      outcome = { ok: false, error };
    }
    let cleanupError: unknown;
    try {
      await rm(candidateDir, { recursive: true, force: true });
    } catch (error) {
      cleanupError = error;
    }
    if (!outcome.ok) throw outcome.error;
    if (cleanupError !== undefined) {
      releaseSharedSnapshot(outcome.value);
      throw cleanupError;
    }
    return outcome.value;
  }
}
