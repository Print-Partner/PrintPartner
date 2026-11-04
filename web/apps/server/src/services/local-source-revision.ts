import { createHash } from "node:crypto";
import { createReadStream, rmSync } from "node:fs";
import { copyFile, lstat, mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve, sep } from "node:path";
import type { SourceSummary } from "@print-partner/contracts";
import { chargeTenantDiskBytes } from "../lib/tenant-disk-quota.js";
import {
  type AppRepository,
  type SourceActivationObservation,
} from "../db/repository.js";
import { indexSourceDocsFromDisk } from "./source-docs-index.js";
import {
  LocalSourceSnapshotStore,
  SOURCE_SNAPSHOT_MANIFEST_FILE,
  releaseSharedSnapshot,
  sharedSnapshotHolderCount,
  sourceRelativePath,
  type PublishedSourceSnapshot,
  type SnapshotFile,
  type SnapshotFileKind,
} from "./local-source-snapshot.js";
import { loadManifestYaml } from "./manifest-apply.js";
import {
  isSourceMetadataPath,
  SOURCE_MANIFEST_FILENAME,
  SOURCE_PHASE_MANIFEST_FILENAME,
} from "./source-workspace.js";
import {
  MAX_PHASE_MANIFEST_BYTES,
  PHASE_MANIFEST_TOO_LARGE_DETAIL,
} from "./upload-limits.js";
import { isRecord } from "../lib/guards.js";
import { OwnershipAdmissionError } from "../ownership/source-sync-admission.js";

export const SOURCE_PUBLICATION_UNAVAILABLE_DETAIL =
  "Source revision publication is unavailable on experimental PostgreSQL because atomic publication transactions are not supported.";

function assertSourcePublicationSupported(repo: AppRepository): void {
  if (!repo.canPublishSourceRevision()) throw new Error(SOURCE_PUBLICATION_UNAVAILABLE_DETAIL);
}

export const DEFAULT_LOCAL_SNAPSHOT_STL_LIMIT = 500;
export const DEFAULT_LOCAL_SNAPSHOT_DOCS_BYTES = 1024 * 1024 * 1024;
const DEFAULT_LOCAL_SNAPSHOT_TOTAL_BYTES = 1024 * 1024 * 1024;

type CollectedSnapshotFile = {
  relativePath: string;
  absolutePath: string;
  kind: SnapshotFileKind;
  sizeHintBytes: number;
};

type SnapshotResource = Pick<
  CollectedSnapshotFile,
  "relativePath" | "kind" | "sizeHintBytes"
>;

export class InvalidSourceManifestError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "InvalidSourceManifestError";
  }
}

export class SourceManifestContentUnavailableError extends Error {
  constructor() {
    super("Source has no local content available under the active filesystem policy");
    this.name = "SourceManifestContentUnavailableError";
  }
}

function classifySnapshotPath(path: string): SnapshotFileKind | null {
  const lower = path.toLowerCase();
  if (lower.endsWith(".stl")) return "stl";
  if (lower.endsWith(".3mf") || lower.endsWith(".zip")) return "artifact";
  if (isSourceMetadataPath(path)) return "artifact";
  if (!lower.endsWith(".md") && !lower.endsWith(".pdf")) return null;
  if (lower.endsWith(".pdf")) return "pdf";
  const base = lower.split("/").pop() ?? lower;
  if (base === "readme.md" || base.startsWith("readme.")) return "readme";
  return "md";
}

function resolveUnderRoot(root: string, relativePath: string): string | null {
  const normalized = relativePath.replaceAll("\\", "/").replace(/^\/+/, "");
  if (!normalized || normalized.split("/").includes("..")) return null;
  const base = resolve(root);
  const target = resolve(base, normalized);
  if (target !== base && !target.startsWith(`${base}${sep}`)) return null;
  return target;
}

async function collectSnapshotFiles(
  root: string,
  excludeManagedSnapshots = false,
  preserveAllFiles = false,
): Promise<CollectedSnapshotFile[]> {
  try {
    const rootStat = await lstat(root);
    if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) return [];
  } catch (error) {
    if (isRecord(error) && error.code === "ENOENT") return [];
    throw error;
  }
  const files: CollectedSnapshotFile[] = [];
  const walk = async (dir: string, relative: string): Promise<void> => {
    const entries = await readdir(dir, { withFileTypes: true });
    for (const entry of entries) {
      if (!preserveAllFiles && entry.name.startsWith(".")) continue;
      const rel = relative ? `${relative}/${entry.name}` : entry.name;
      if (
        relative === ""
        && entry.name.toLocaleLowerCase("en-US") === SOURCE_SNAPSHOT_MANIFEST_FILE
      ) continue;
      const abs = join(dir, entry.name);
      if (entry.isDirectory()) {
        const segments = rel.split("/");
        if (
          excludeManagedSnapshots &&
          segments.length === 2 &&
          segments[0]?.toLocaleLowerCase("en-US") === "revisions"
        ) {
          try {
            const marker = await lstat(join(abs, SOURCE_SNAPSHOT_MANIFEST_FILE));
            if (marker.isFile() && !marker.isSymbolicLink()) continue;
          } catch (error) {
            if (!isRecord(error) || error.code !== "ENOENT") throw error;
          }
        }
        await walk(abs, rel);
        continue;
      }
      if (!entry.isFile()) continue;
      const kind = classifySnapshotPath(rel) ?? (preserveAllFiles ? "artifact" : null);
      if (!kind) continue;
      const stat = await lstat(abs);
      if (stat.isSymbolicLink() || !stat.isFile()) continue;
      sourceRelativePath(rel);
      files.push({
        relativePath: rel,
        absolutePath: abs,
        kind,
        sizeHintBytes: stat.size,
      });
    }
  };
  await walk(root, "");
  return files.sort((left, right) => left.relativePath.localeCompare(right.relativePath));
}

function assertSnapshotResourceLimits(input: {
  files: readonly SnapshotResource[];
  maxStlFiles: number;
  maxDocumentationBytes: number;
  maxTotalBytes: number;
}): void {
  const phaseManifest = input.files.find(
    (file) => file.relativePath === SOURCE_PHASE_MANIFEST_FILENAME,
  );
  if (phaseManifest && phaseManifest.sizeHintBytes > MAX_PHASE_MANIFEST_BYTES) {
    throw new Error(PHASE_MANIFEST_TOO_LARGE_DETAIL);
  }
  const stlCount = input.files.filter((file) => file.kind === "stl").length;
  if (stlCount > input.maxStlFiles) {
    throw new Error(
      `Local Source contains ${stlCount} STL files, exceeding the limit of ${input.maxStlFiles}`,
    );
  }
  const nonStlBytes = input.files
    .filter((file) => file.kind !== "stl")
    .reduce((sum, file) => sum + file.sizeHintBytes, 0);
  if (nonStlBytes > input.maxDocumentationBytes) {
    throw new Error(
      `Local Source documentation exceeds the ${input.maxDocumentationBytes} byte limit, including non-STL artifacts`,
    );
  }
  const totalBytes = input.files.reduce((sum, file) => sum + file.sizeHintBytes, 0);
  if (totalBytes > input.maxTotalBytes) {
    throw new Error(`Local Source total stored bytes exceeds the ${input.maxTotalBytes} byte limit`);
  }
}

async function digestWorkingTree(files: readonly CollectedSnapshotFile[]): Promise<string> {
  const hash = createHash("sha256");
  for (const file of files) {
    const contentHash = createHash("sha256");
    for await (const chunk of createReadStream(file.absolutePath)) {
      contentHash.update(chunk);
    }
    const contentDigest = contentHash.digest("hex");
    hash.update(
      JSON.stringify({
        path: file.relativePath,
        size: file.sizeHintBytes,
        sha256: contentDigest,
      }),
    );
    hash.update("\n");
  }
  return hash.digest("hex");
}

function resourcesWithManifest(
  files: readonly CollectedSnapshotFile[],
  manifestYaml: string | undefined,
): SnapshotResource[] {
  if (manifestYaml === undefined) return [...files];
  return [
    ...files.filter((file) => file.relativePath !== SOURCE_MANIFEST_FILENAME),
    {
      relativePath: SOURCE_MANIFEST_FILENAME,
      kind: "artifact" as const,
      sizeHintBytes: Buffer.byteLength(manifestYaml),
    },
  ];
}

async function materializeLocalSourceWorkingTree(input: {
  beforePublish?: () => undefined;
  reposDir: string;
  sourceId: number;
  workingTree: string;
  manifestYaml?: string;
  preserveAllFiles?: boolean;
  maxStlFiles: number;
  maxDocumentationBytes: number;
  maxTotalBytes: number;
}): Promise<PublishedSourceSnapshot> {
  const sourceWorkspace = resolve(input.reposDir, String(input.sourceId));
  const liveFiles = await collectSnapshotFiles(
    input.workingTree,
    resolve(input.workingTree) === sourceWorkspace,
    input.preserveAllFiles,
  );
  assertSnapshotResourceLimits({
    files: resourcesWithManifest(liveFiles, input.manifestYaml),
    maxStlFiles: input.maxStlFiles,
    maxDocumentationBytes: input.maxDocumentationBytes,
    maxTotalBytes: input.maxTotalBytes,
  });
  input.beforePublish?.();
  await mkdir(sourceWorkspace, { recursive: true });
  input.beforePublish?.();
  const stagingRoot = await mkdtemp(join(sourceWorkspace, ".local-source-"));
  let published: PublishedSourceSnapshot | undefined;
  let workError: unknown;
  try {
    for (const file of liveFiles) {
      if (
        input.manifestYaml !== undefined &&
        file.relativePath === SOURCE_MANIFEST_FILENAME
      ) {
        continue;
      }
      const stagedPath = resolveUnderRoot(stagingRoot, file.relativePath);
      if (!stagedPath) throw new Error(`Unsafe Source snapshot path: ${file.relativePath}`);
      input.beforePublish?.();
      await mkdir(dirname(stagedPath), { recursive: true });
      input.beforePublish?.();
      chargeTenantDiskBytes(file.sizeHintBytes);
      await copyFile(file.absolutePath, stagedPath);
    }
    if (input.manifestYaml !== undefined) {
      input.beforePublish?.();
      chargeTenantDiskBytes(Buffer.byteLength(input.manifestYaml));
      await writeFile(
        join(stagingRoot, SOURCE_MANIFEST_FILENAME),
        input.manifestYaml,
        { encoding: "utf8", flag: "wx" },
      );
    }
    const collected = await collectSnapshotFiles(stagingRoot, false, input.preserveAllFiles);
    assertSnapshotResourceLimits({
      files: collected,
      maxStlFiles: input.maxStlFiles,
      maxDocumentationBytes: input.maxDocumentationBytes,
      maxTotalBytes: input.maxTotalBytes,
    });

    const upstreamRevisionKey = await digestWorkingTree(collected);
    const files: SnapshotFile[] = collected.map((file) => ({
      path: sourceRelativePath(file.relativePath),
      kind: file.kind,
      sizeHintBytes: file.sizeHintBytes,
    }));
    const store = new LocalSourceSnapshotStore({
      reposDir: input.reposDir,
      retainUntilRelease: true,
    });
    published = await store.materialize({
      beforePublish: input.beforePublish,
      sourceId: input.sourceId,
      upstreamRevisionKey,
      files,
      selection: {
        maxStlFiles: input.maxStlFiles,
        maxDocumentationBytes: input.maxDocumentationBytes,
        omittedFiles: [],
      },
      openFile: async (file) => {
        const absolutePath = resolveUnderRoot(stagingRoot, file.path);
        if (!absolutePath) throw new Error(`Unsafe Source snapshot path: ${file.path}`);
        const stat = await lstat(absolutePath);
        return {
          stream: createReadStream(absolutePath),
          contentLengthBytes: stat.size,
        };
      },
    });
  } catch (error) {
    workError = error;
  }
  try {
    await rm(stagingRoot, { recursive: true, force: true });
  } catch (error) {
    if (published) releaseSharedSnapshot(published);
    throw error;
  }
  if (workError !== undefined) throw workError;
  if (published === undefined) throw new Error("Source snapshot was not materialized");
  return published;
}

function registerAndActivateSnapshot(input: {
  beforePublish?: () => undefined;
  repo: AppRepository;
  sourceId: number;
  observed: SourceActivationObservation;
  snapshot: PublishedSourceSnapshot;
  sourceVersion: string;
  markRemoteCurrent: boolean;
  recordLegacyManifestCutover: boolean;
  suggestedImportRules?: string[];
}): SourceSummary {
  return input.repo.transaction(() => {
    input.beforePublish?.();
    const revision = input.repo.recordSourceRevision({
      sourceId: input.sourceId,
      upstreamRevisionKey: input.snapshot.upstreamRevisionKey,
      manifestDigest: input.snapshot.manifestDigest,
      snapshotLocator: input.snapshot.snapshotLocator,
      syncedAt: new Date().toISOString(),
      completeness: "complete",
    });
    const activated = input.repo.activateSourceRevision({
      sourceId: input.sourceId,
      revisionId: revision.id,
      observed: input.observed,
      sourceVersion: input.sourceVersion,
      recordLegacyManifestCutover: input.recordLegacyManifestCutover,
    });
    if (input.markRemoteCurrent) {
      input.repo.markSourceRevisionCurrent(input.sourceId, revision.id);
    }
    const rules = input.repo.getProjectRow(input.sourceId)?.importedPaths;
    if ((!rules?.trim() || rules.trim() === "[]") && input.suggestedImportRules?.length) {
      input.repo.updateImportRules(input.sourceId, input.suggestedImportRules);
    }
    return activated;
  }, "immediate");
}

type ActivatedSourceDocsIndex =
  | { outcome: "indexed"; docCount: number; pendingPdfs: number }
  | { outcome: "cleared"; revisionId: number; reason: string }
  | { outcome: "unchanged"; revisionId: number; activeRevisionId: number | null };

export function indexDocsForActivatedRevision(input: {
  beforePublish?: () => undefined;
  repo: AppRepository;
  sourceId: number;
  localPath: string;
  revisionId: number;
}): ActivatedSourceDocsIndex {
  const activeRevisionId = (): number | null =>
    input.repo.getSourceActivationObservation(input.sourceId)?.currentSourceRevisionId ?? null;
  try {
    return input.repo.transaction(() => {
      const current = activeRevisionId();
      if (current !== input.revisionId) {
        return { outcome: "unchanged", revisionId: input.revisionId, activeRevisionId: current };
      }
      input.beforePublish?.();
      const indexed = indexSourceDocsFromDisk(input.repo, input.sourceId, input.localPath, undefined, input.beforePublish);
      return { outcome: "indexed", docCount: indexed.doc_count, pendingPdfs: indexed.pending_pdfs };
    }, "immediate");
  } catch (error) {
    if (error instanceof OwnershipAdmissionError) throw error;
    const reason = error instanceof Error ? error.message : "documentation indexing failed";
    try {
      return input.repo.transaction(() => {
        const current = activeRevisionId();
        if (current !== input.revisionId) {
          return { outcome: "unchanged", revisionId: input.revisionId, activeRevisionId: current };
        }
        input.beforePublish?.();
        input.repo.replaceSourceDocs(input.sourceId, []);
        return { outcome: "cleared", revisionId: input.revisionId, reason };
      }, "immediate");
    } catch (cleanupError) {
      if (cleanupError instanceof OwnershipAdmissionError) throw cleanupError;
      return { outcome: "unchanged", revisionId: input.revisionId, activeRevisionId: null };
    }
  }
}

function releaseUnreferencedSnapshot(
  repo: AppRepository,
  reposDir: string,
  sourceId: number,
  snapshot: PublishedSourceSnapshot,
): void {
  releaseSharedSnapshot(snapshot);
  if (snapshot.publication !== "created") return;
  const expected = resolve(reposDir, String(sourceId), "revisions", snapshot.upstreamRevisionKey);
  if (resolve(snapshot.absolutePath) !== expected) return;
  repo.transaction(() => {
    if (sharedSnapshotHolderCount(expected) > 0) return;
    const referenced = repo.sourceRevisionSnapshotLocators(sourceId)
      .some((locator) => locator === snapshot.snapshotLocator);
    if (referenced) return;
    if (resolve(snapshot.absolutePath) !== expected) return;
    rmSync(expected, { recursive: true, force: true });
  }, "immediate");
}

/** Activate a Git snapshot on the shared rollback path. A failed activation removes only a snapshot this call created and nothing still referenced. */
export function activateSyncedSourceSnapshot(input: {
  beforePublish?: () => undefined;
  repo: AppRepository;
  reposDir: string;
  sourceId: number;
  observed: SourceActivationObservation;
  snapshot: PublishedSourceSnapshot;
  sourceVersion: string;
}): SourceSummary {
  return activateMaterializedSnapshot({
    beforePublish: input.beforePublish,
    repo: input.repo,
    reposDir: input.reposDir,
    sourceId: input.sourceId,
    observed: input.observed,
    snapshot: input.snapshot,
    sourceVersion: input.sourceVersion,
    markRemoteCurrent: true,
    recordLegacyManifestCutover: false,
  });
}

function activateMaterializedSnapshot(input: {
  beforePublish?: () => undefined;
  repo: AppRepository;
  reposDir: string;
  sourceId: number;
  observed: SourceActivationObservation;
  snapshot: PublishedSourceSnapshot;
  sourceVersion: string;
  markRemoteCurrent: boolean;
  recordLegacyManifestCutover: boolean;
  suggestedImportRules?: string[];
}): SourceSummary {
  try {
    return registerAndActivateSnapshot({
      beforePublish: input.beforePublish,
      repo: input.repo,
      sourceId: input.sourceId,
      observed: input.observed,
      snapshot: input.snapshot,
      sourceVersion: input.sourceVersion,
      markRemoteCurrent: input.markRemoteCurrent,
      recordLegacyManifestCutover: input.recordLegacyManifestCutover,
      suggestedImportRules: input.suggestedImportRules,
    });
  } catch (error) {
    try {
      releaseUnreferencedSnapshot(input.repo, input.reposDir, input.sourceId, input.snapshot);
    } catch (cleanupError) {
      if (error instanceof Error && cleanupError instanceof Error && error.cause === undefined) {
        error.cause = cleanupError;
      }
    }
    throw error;
  } finally {
    releaseSharedSnapshot(input.snapshot);
  }
}

export async function publishLocalSourceWorkingTree(input: {
  beforePublish?: () => undefined;
  repo: AppRepository;
  reposDir: string;
  sourceId: number;
  workingTree: string;
  observed?: SourceActivationObservation;
  preserveAllFiles?: boolean;
  suggestedImportRules?: string[];
  maxStlFiles?: number;
  maxDocumentationBytes?: number;
  maxTotalBytes?: number;
}): Promise<SourceSummary> {
  input.beforePublish?.();
  assertSourcePublicationSupported(input.repo);
  const observed = input.observed ?? input.repo.getSourceActivationObservation(input.sourceId);
  if (!observed) throw new Error("Source not found");

  const maxStlFiles = input.maxStlFiles ?? DEFAULT_LOCAL_SNAPSHOT_STL_LIMIT;
  const maxDocumentationBytes = input.maxDocumentationBytes ?? DEFAULT_LOCAL_SNAPSHOT_DOCS_BYTES;
  const maxTotalBytes = input.maxTotalBytes ?? DEFAULT_LOCAL_SNAPSHOT_TOTAL_BYTES;
  const snapshot = await materializeLocalSourceWorkingTree({
    beforePublish: input.beforePublish,
    reposDir: input.reposDir,
    sourceId: input.sourceId,
    workingTree: input.workingTree,
    preserveAllFiles: input.preserveAllFiles,
    maxStlFiles,
    maxDocumentationBytes,
    maxTotalBytes,
  });
  return activateMaterializedSnapshot({
    beforePublish: input.beforePublish,
    repo: input.repo,
    reposDir: input.reposDir,
    sourceId: input.sourceId,
    observed,
    snapshot,
    sourceVersion: snapshot.upstreamRevisionKey,
    markRemoteCurrent: true,
    recordLegacyManifestCutover: false,
    suggestedImportRules: input.suggestedImportRules,
  });
}

export async function publishSourceManifestRevision(input: {
  beforePublish?: () => undefined;
  repo: AppRepository;
  sourceId: number;
  manifestYaml: string;
  observed?: SourceActivationObservation;
}): Promise<SourceSummary> {
  input.beforePublish?.();
  assertSourcePublicationSupported(input.repo);
  try {
    loadManifestYaml(input.manifestYaml);
  } catch (error) {
    throw new InvalidSourceManifestError(
      error instanceof Error ? error.message : "Source manifest is invalid",
      { cause: error },
    );
  }

  const observed = input.observed
    ?? input.repo.getSourceActivationObservation(input.sourceId);
  if (!observed) throw new Error("Source not found");
  const source = input.repo.getProjectRow(input.sourceId);
  if (!source?.localPath) throw new SourceManifestContentUnavailableError();

  let snapshot: PublishedSourceSnapshot;
  if (observed.currentSourceRevisionId != null) {
    if (!observed.lastCommitSha) {
      throw new Error("Tracked Source has no upstream version");
    }
    const activeRevision = input.repo.getSourceRevision(observed.currentSourceRevisionId);
    if (!activeRevision || activeRevision.source_id !== input.sourceId) {
      throw new Error("Active Source revision is unavailable");
    }
    const expectedLocator = `${input.sourceId}/revisions/${activeRevision.upstream_revision_key}`;
    if (activeRevision.snapshot_locator !== expectedLocator) {
      throw new Error("Active Source revision has an unsupported snapshot locator");
    }
    snapshot = await new LocalSourceSnapshotStore({
      reposDir: input.repo.reposDir,
      retainUntilRelease: true,
    })
      .deriveFileReplacement({
        beforePublish: input.beforePublish,
        sourceId: input.sourceId,
        baseRevisionKey: activeRevision.upstream_revision_key,
        sourceVersion: observed.lastCommitSha,
        replacement: {
          path: sourceRelativePath(SOURCE_MANIFEST_FILENAME),
          kind: "artifact",
          content: Buffer.from(input.manifestYaml),
        },
      });
  } else {
    snapshot = await materializeLocalSourceWorkingTree({
      beforePublish: input.beforePublish,
      reposDir: input.repo.reposDir,
      sourceId: input.sourceId,
      workingTree: source.localPath,
      manifestYaml: input.manifestYaml,
      maxStlFiles: DEFAULT_LOCAL_SNAPSHOT_STL_LIMIT,
      maxDocumentationBytes: DEFAULT_LOCAL_SNAPSHOT_DOCS_BYTES,
      maxTotalBytes: DEFAULT_LOCAL_SNAPSHOT_TOTAL_BYTES,
    });
  }

  return activateMaterializedSnapshot({
    beforePublish: input.beforePublish,
    repo: input.repo,
    reposDir: input.repo.reposDir,
    sourceId: input.sourceId,
    observed,
    snapshot,
    sourceVersion: observed.lastCommitSha ?? snapshot.upstreamRevisionKey,
    markRemoteCurrent: false,
    recordLegacyManifestCutover: true,
  });
}

/** Drop a pre-snapshot hosted working tree after activation. Never removes the active path. */
export async function removeObsoleteHostedWorkingTree(input: {
  beforePublish?: () => undefined;
  sourcesDir: string;
  sourceId: number;
  activePath: string | null | undefined;
}): Promise<void> {
  const legacy = resolve(join(input.sourcesDir, String(input.sourceId)));
  if (input.activePath) {
    const active = resolve(input.activePath);
    if (active === legacy || active.startsWith(legacy + sep)) return;
  }
  input.beforePublish?.();
  await rm(legacy, { recursive: true, force: true });
}
