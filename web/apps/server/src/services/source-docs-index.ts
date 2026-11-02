import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { AppRepository } from "../db/repository.js";
import type { SyncDocEntry } from "./github-sync.js";
import { PDF_BG_EXTRACT_BYTES, contentHashForFile, extractPdfText } from "./pdf-text-extract.js";
import { walkSourceDocs } from "./source-docs-scan.js";

function hashOfFile(repoRoot: string, relativePath: string): string | null {
  const abs = resolve(repoRoot, relativePath);
  if (!existsSync(abs)) return null;
  try {
    return contentHashForFile(abs);
  } catch {
    try {
      const buf = readFileSync(abs);
      return createHash("sha256").update(buf).digest("hex").slice(0, 24);
    } catch {
      return null;
    }
  }
}

/**
 * Rebuild `source_docs` rows from disk (and optional sync metadata).
 * Large PDFs stay `pending` for the background extraction job.
 */
export function indexSourceDocsFromDisk(
  repo: AppRepository,
  projectId: number,
  repoRoot: string,
  syncDocs?: SyncDocEntry[],
  beforePublish?: () => undefined,
): { doc_count: number; pending_pdfs: number } {
  const discovered = walkSourceDocs(repoRoot);
  const sizeByPath = new Map((syncDocs ?? []).map((d) => [d.path, d.sizeBytes] as const));

  const rows = discovered.map((d) => {
    const size = sizeByPath.get(d.path) || d.sizeBytes;
    const hash = hashOfFile(repoRoot, d.path);
    if (d.kind === "pdf") {
      const needsBg = size >= PDF_BG_EXTRACT_BYTES;
      return {
        path: d.path,
        kind: d.kind,
        sizeBytes: size,
        contentHash: hash,
        extractStatus: needsBg ? "pending" : "pending",
        pageCount: null as number | null,
      };
    }
    return {
      path: d.path,
      kind: d.kind,
      sizeBytes: size,
      contentHash: hash,
      extractStatus: "na",
      pageCount: null as number | null,
    };
  });

  beforePublish?.();
  repo.replaceSourceDocs(projectId, rows);
  const pending = rows.filter((r) => r.kind === "pdf").length;
  return { doc_count: rows.length, pending_pdfs: pending };
}

/**
 * Extract pending PDFs for a source. Small PDFs extract inline; callers can
 * limit to pending rows only.
 */
export async function extractPendingPdfsForSource(
  repo: AppRepository,
  projectId: number,
  repoRoot: string,
  options?: {
    beforePublish?: () => undefined;
    onProgress?: (msg: string, progress: number) => void;
    /** Only extract PDFs at or below this size (bytes). */
    maxSizeBytes?: number;
    /** Only extract PDFs at or above this size (bytes). */
    minSizeBytes?: number;
    /** Writable derived-data directory for extracted PDF text. */
    cacheRoot?: string;
    /** Read-only cache directories used before immutable Source revisions. */
    legacyCacheRoots?: readonly string[];
  },
): Promise<{ extracted: number; errors: number }> {
  const pending = repo.listSourceDocs(projectId).filter((d) => {
    if (d.kind !== "pdf") return false;
    if (d.extract_status === "ready") return false;
    if (options?.maxSizeBytes != null && d.size_bytes > options.maxSizeBytes) return false;
    if (options?.minSizeBytes != null && d.size_bytes < options.minSizeBytes) return false;
    return true;
  });
  let extracted = 0;
  let errors = 0;
  for (let i = 0; i < pending.length; i++) {
    options?.beforePublish?.();
    const doc = pending[i]!;
    options?.onProgress?.(
      `Extracting ${doc.path} (${i + 1}/${pending.length})`,
      Math.round(((i + 1) / Math.max(pending.length, 1)) * 100),
    );
    const result = await extractPdfText(repoRoot, doc.path, {
      beforePublish: options?.beforePublish,
      perPage: doc.size_bytes >= PDF_BG_EXTRACT_BYTES,
      cacheRoot: options?.cacheRoot,
      legacyCacheRoots: options?.legacyCacheRoots,
      onProgress: (cur, total) => {
        options?.onProgress?.(
          `Extracting ${doc.path} page ${cur}/${total}`,
          Math.round(((i + cur / Math.max(total, 1)) / Math.max(pending.length, 1)) * 100),
        );
      },
    });
    options?.beforePublish?.();
    const updated = repo.updateSourceDocExtract(
      projectId,
      doc.path,
      {
        extractStatus: result.status,
        contentHash: result.hash || null,
        pageCount: result.pageCount || null,
        extractError: result.error ?? null,
      },
      doc.id,
    );
    if (!updated) continue;
    if (result.status === "ready") extracted += 1;
    else errors += 1;
  }
  return { extracted, errors };
}
