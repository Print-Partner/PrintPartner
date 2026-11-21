import {
  buildSourceCategoryTree,
  categoryDepth,
  categoryLeafName,
  categoryParentPath,
  isCategoryPathWithin,
  normalizeCategoryPath,
} from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import { parseGithubUrl } from "../services/github-sync.js";

/** Sentinel accepted by `list_sources` for Sources with no category. */
export const UNCATEGORIZED_CATEGORY = "__uncategorized__";

export function sourceByName(repo: AppRepository, name: string) {
  const needle = name.trim();
  if (!needle) return null;
  const sources = repo.listSources();
  const exact = sources.find((source) => source.name === needle);
  if (exact) return exact;
  const lower = needle.toLowerCase();
  const caseInsensitive = sources.find((source) => source.name.toLowerCase() === lower);
  if (caseInsensitive) return caseInsensitive;
  // Separator-insensitive: "Example Repo" → "Example-Repo".
  const compact = lower.replace(/[\s_-]+/g, "");
  const fuzzy = sources.find(
    (source) => source.name.toLowerCase().replace(/[\s_-]+/g, "") === compact,
  );
  if (fuzzy) return fuzzy;
  // Model often appends release suffixes ("Example-Repo R2-0", "Example-Repo @ v2.1").
  // Match when either string contains the other after separator normalization; prefer the
  // longest source name so a suffixed name resolves to the most specific source.
  const norm = (value: string) =>
    value
      .toLowerCase()
      .replace(/[\s_-]+/g, " ")
      .trim();
  const needleNorm = norm(needle);
  const contains = sources.filter((source) => {
    const sourceName = norm(source.name);
    return needleNorm.includes(sourceName) || sourceName.includes(needleNorm);
  });
  if (contains.length === 1) return contains[0]!;
  if (contains.length > 1) {
    return contains.reduce((best, source) =>
      source.name.length > best.name.length ? source : best,
    );
  }
  return null;
}

function compactSourceName(value: string): string {
  return value.trim().toLowerCase().replace(/[\s_-]+/g, "");
}

/** One key per repository: GitHub `owner/repo` in any URL form, else the URL itself. */
function repositoryKey(url: string): string | null {
  const trimmed = url.trim();
  if (!trimmed) return null;
  const github = parseGithubUrl(trimmed);
  if (github) return `github.com/${github.owner}/${github.repo}`.toLowerCase();
  return trimmed.replace(/\/+$/, "").toLowerCase();
}

/**
 * The Source that adding `name` from `url` would duplicate: one whose name is equal after
 * case and separator folding, or one cloned from the same repository. Unlike
 * `sourceByName`, a name that only contains another ("A4Tv2" and "A4T") is a new Source.
 * Names made only of separators fold to "" and so equal each other: `___` duplicates `---`.
 */
export function existingSourceFor(repo: AppRepository, name: string, url?: string | null) {
  const wantedName = name.trim() ? compactSourceName(name) : null;
  const wantedRepository = url ? repositoryKey(url) : null;
  return (
    repo.listSources().find(
      (source) =>
        (wantedName != null && compactSourceName(source.name) === wantedName) ||
        (wantedRepository != null && repositoryKey(source.url) === wantedRepository),
    ) ?? null
  );
}

/** Closest source names for "did you mean" hints in tool errors (bigram Dice similarity). */
function similarSourceNames(repo: AppRepository, name: string, limit = 5): string[] {
  const compact = (value: string) => value.toLowerCase().replace(/[\s_-]+/g, "");
  const bigrams = (value: string) => {
    const out = new Set<string>();
    for (let index = 0; index < value.length - 1; index += 1) {
      out.add(value.slice(index, index + 2));
    }
    return out;
  };
  const needle = bigrams(compact(name));
  if (!needle.size) return [];
  return repo
    .listSources()
    .map((source) => {
      const target = bigrams(compact(source.name));
      let shared = 0;
      for (const bigram of needle) if (target.has(bigram)) shared += 1;
      const score = (2 * shared) / (needle.size + target.size);
      return { name: source.name, score };
    })
    .filter((row) => row.score >= 0.3)
    .sort((left, right) => right.score - left.score)
    .slice(0, limit)
    .map((row) => row.name);
}

export function categoryNotFoundError(repo: AppRepository, requested: string): string {
  return JSON.stringify({
    error: `Unknown library category: "${requested}". Call list_source_categories, or create it with propose_create_source_category.`,
    categories: repo.getSourceCategories(),
  });
}

/** Sources filed under `path` or any of its subcategories. */
export function countSourcesUnderCategory(repo: AppRepository, path: string): number {
  return repo.listSources().filter((source) => isCategoryPathWithin(source.category ?? "", path))
    .length;
}

/** Library category tree plus per-category Source counts, for MCP callers. */
export function summarizeSourceCategories(repo: AppRepository) {
  const paths = repo.getSourceCategories();
  const sources = repo.listSources();
  const direct = new Map<string, number>();
  let uncategorized = 0;
  for (const source of sources) {
    const path = normalizeCategoryPath(source.category ?? "");
    if (!path) {
      uncategorized += 1;
      continue;
    }
    const key = path.toLowerCase();
    direct.set(key, (direct.get(key) ?? 0) + 1);
  }
  return {
    separator: "/",
    note: 'Categories nest as "/"-separated paths; a Source stores one full path.',
    categories: paths.map((path) => ({
      path,
      name: categoryLeafName(path),
      parent: categoryParentPath(path),
      depth: categoryDepth(path),
      sources: direct.get(path.toLowerCase()) ?? 0,
      sources_including_subcategories: sources.filter((source) =>
        isCategoryPathWithin(source.category ?? "", path),
      ).length,
    })),
    tree: buildSourceCategoryTree(paths),
    uncategorized_sources: uncategorized,
  };
}

export function sourceNotFoundError(repo: AppRepository, sourceName: string, hint: string): string {
  const suggestions = similarSourceNames(repo, sourceName);
  const didYouMean = suggestions.length
    ? ` Did you mean: ${suggestions.map((name) => `"${name}"`).join(", ")}?`
    : "";
  return JSON.stringify({
    error: `Source not found: "${sourceName}".${didYouMean} ${hint}`,
  });
}
