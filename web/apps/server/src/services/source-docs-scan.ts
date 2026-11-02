import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { basename, join, relative, resolve } from "node:path";
import type { SyncDocKind } from "./github-sync.js";
import { DOCS_TEXT_DIR, docTitleFromPath } from "./pdf-text-extract.js";
import { resolvedFileUnderRoot } from "../lib/secure-path.js";

type DiscoveredDoc = {
  path: string;
  kind: SyncDocKind;
  sizeBytes: number;
  title: string;
};

function classify(path: string): SyncDocKind | null {
  const lower = path.toLowerCase();
  if (lower.endsWith(".pdf")) return "pdf";
  if (!lower.endsWith(".md")) return null;
  const base = basename(lower);
  if (base === "readme.md" || base.startsWith("readme.")) return "readme";
  return "md";
}

/**
 * Walk a synced repo tree for markdown + PDF docs (skips `.docs-text`).
 */
export function walkSourceDocs(repoRoot: string): DiscoveredDoc[] {
  if (!existsSync(repoRoot)) return [];
  const absRoot = resolve(repoRoot);
  const out: DiscoveredDoc[] = [];

  const walk = (dir: string) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (entry.name === DOCS_TEXT_DIR || entry.name === ".git" || entry.name === "node_modules") {
        continue;
      }
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
        continue;
      }
      if (!entry.isFile()) continue;
      const rel = relative(absRoot, full).replace(/\\/g, "/");
      const kind = classify(rel);
      if (!kind) continue;
      let sizeBytes = 0;
      try {
        sizeBytes = statSync(full).size;
      } catch {
        /* ignore */
      }
      out.push({
        path: rel,
        kind,
        sizeBytes,
        title: docTitleFromPath(rel),
      });
    }
  };

  walk(absRoot);
  return out.sort((a, b) => {
    // README first, then path.
    if (a.kind === "readme" && b.kind !== "readme") return -1;
    if (b.kind === "readme" && a.kind !== "readme") return 1;
    return a.path.localeCompare(b.path);
  });
}

export function readMarkdownDoc(repoRoot: string, relativePath: string): string | null {
  const normalized = relativePath.replace(/\\/g, "/").replace(/^\/+/, "");
  if (!normalized || normalized.includes("..")) return null;
  const absRoot = resolve(repoRoot);
  const full = resolvedFileUnderRoot(absRoot, resolve(absRoot, normalized));
  if (!full) return null;
  try {
    return readFileSync(full, "utf8");
  } catch {
    return null;
  }
}

/** Simple keyword relevance: score by how many query tokens appear (case-insensitive). */
export function keywordFilterScore(text: string, query: string | null | undefined): number {
  const q = (query ?? "").trim().toLowerCase();
  if (!q) return 1;
  const tokens = q.split(/\s+/).filter((t) => t.length > 1);
  if (tokens.length === 0) return 1;
  const hay = text.toLowerCase();
  let hits = 0;
  for (const t of tokens) {
    if (hay.includes(t)) hits += 1;
  }
  return hits / tokens.length;
}
