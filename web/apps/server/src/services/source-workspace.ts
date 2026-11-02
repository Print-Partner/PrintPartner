import { join } from "node:path";
import type { AppRepository, ProjectRow } from "../db/repository.js";
import { resolvedFileUnderRoot } from "../lib/secure-path.js";

export const SOURCE_MANIFEST_FILENAME = "print-partner.manifest.yaml";
export const SOURCE_PHASE_MANIFEST_FILENAME = "pp-phases.json";

export function isSourceMetadataPath(path: string): boolean {
  return path === SOURCE_MANIFEST_FILENAME || path === SOURCE_PHASE_MANIFEST_FILENAME;
}

const LEGACY_DERIVED_KEY = "legacy";

function requireSourceId(sourceId: number): number {
  if (!Number.isSafeInteger(sourceId) || sourceId <= 0) {
    throw new Error("Source ID must be a positive integer");
  }
  return sourceId;
}

function requireManifestDigest(manifestDigest: string): string {
  const normalized = manifestDigest.trim().toLowerCase();
  if (!/^[a-f0-9]{64}$/.test(normalized)) {
    throw new Error("Manifest digest must be a SHA-256 hex digest");
  }
  return normalized;
}

export function sourceWorkspaceRoot(reposDir: string, sourceId: number): string {
  return join(reposDir, String(requireSourceId(sourceId)));
}

export function legacySourceManifestOverridePath(reposDir: string, sourceId: number): string {
  return join(sourceWorkspaceRoot(reposDir, sourceId), SOURCE_MANIFEST_FILENAME);
}

export function findSourceManifestPath(contentRoot: string): string | null {
  return resolvedFileUnderRoot(contentRoot, join(contentRoot, SOURCE_MANIFEST_FILENAME));
}

export function revisionPdfTextCacheRoot(input: {
  reposDir: string;
  sourceId: number;
  manifestDigest: string;
}): string {
  return join(
    sourceWorkspaceRoot(input.reposDir, input.sourceId),
    "derived",
    requireManifestDigest(input.manifestDigest),
    "pdf-text",
  );
}

function activeRevisionId(row: ProjectRow): number | null {
  const value = row.currentSourceRevisionId;
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : null;
}

function activeManifestDigest(
  repo: AppRepository,
  sourceId: number,
  row: ProjectRow,
): string | null {
  const revisionId = activeRevisionId(row);
  if (revisionId != null) {
    const revision = repo.getSourceRevision(revisionId);
    if (revision?.source_id === sourceId) return revision.manifest_digest;
  }

  if (!row.lastCommitSha) return null;
  const revision = repo
    .listSourceRevisions(sourceId)
    .find((candidate) => candidate.upstream_revision_key === row.lastCommitSha);
  return revision?.manifest_digest ?? null;
}

type SourcePdfTextStorage = {
  cacheRoot: string;
  legacyCacheRoots: readonly string[];
};

export function sourcePdfTextStorage(
  repo: AppRepository,
  sourceId: number,
  contentRoot: string,
): SourcePdfTextStorage {
  const row = repo.getProjectRow(sourceId);
  if (!row) throw new Error("Source not found");

  const workspaceRoot = sourceWorkspaceRoot(repo.reposDir, sourceId);
  const digest = activeManifestDigest(repo, sourceId, row);
  const cacheRoot = digest
    ? revisionPdfTextCacheRoot({ reposDir: repo.reposDir, sourceId, manifestDigest: digest })
    : join(workspaceRoot, "derived", LEGACY_DERIVED_KEY, "pdf-text");
  const legacyCacheRoots = [
    join(workspaceRoot, ".docs-text"),
    join(contentRoot, ".docs-text"),
  ].filter((path, index, paths) => paths.indexOf(path) === index);

  return { cacheRoot, legacyCacheRoots };
}
