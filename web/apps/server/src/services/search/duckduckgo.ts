/**
 * DuckDuckGo HTML scrape fallback — brittle, no API key required.
 * Parses html.duckduckgo.com result markup with simple regexes.
 */

import { OutboundUrlError } from "../../lib/outbound-url.js";
import { cancelResponseBody, readBoundedResponseText } from "../../lib/bounded-response.js";
import type { SearchAdapterDependencies, SearchHit, WebSearchOptions } from "./types.js";

const DDG_HTML = "https://html.duckduckgo.com/html/";
const MAX_SEARCH_RESPONSE_BYTES = 4 * 1024 * 1024;

function decodeHtmlEntities(s: string): string {
  return s
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#x27;/gi, "'")
    .replace(/&#(\d+);/g, (_, n) => String.fromCharCode(Number(n)));
}

function stripTags(s: string): string {
  return decodeHtmlEntities(
    s
      .replace(/<[^>]+>/g, " ")
      .replace(/\s+/g, " ")
      .trim(),
  );
}

/** Unwrap DuckDuckGo redirect URLs when present. */
function unwrapDdgUrl(href: string): string {
  try {
    const u = new URL(href, DDG_HTML);
    if (u.hostname.includes("duckduckgo.com") && u.pathname.includes("l/")) {
      const uddg = u.searchParams.get("uddg");
      if (uddg) return decodeURIComponent(uddg);
    }
    return u.toString();
  } catch {
    return href;
  }
}

export function parseDuckDuckGoHtml(html: string, maxResults: number): SearchHit[] {
  const hits: SearchHit[] = [];
  const seen = new Set<string>();

  // Classic result__a links
  const linkRe = /<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/gi;
  let m: RegExpExecArray | null;
  const anchors: Array<{ url: string; title: string; index: number }> = [];
  while ((m = linkRe.exec(html)) != null) {
    anchors.push({
      url: unwrapDdgUrl(m[1]!),
      title: stripTags(m[2]!),
      index: m.index,
    });
  }

  for (const a of anchors) {
    if (!a.url.startsWith("http") || seen.has(a.url)) continue;
    seen.add(a.url);
    // Snippet: look for result__snippet near this anchor
    const window = html.slice(a.index, a.index + 1200);
    const snipMatch =
      /class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)<\/(?:a|td|div|span)>/i.exec(window);
    const snippet = snipMatch ? stripTags(snipMatch[1]!).slice(0, 500) : "";
    hits.push({ title: a.title || a.url, url: a.url, snippet });
    if (hits.length >= maxResults) break;
  }

  return hits;
}

export async function searchDuckDuckGo(
  options: WebSearchOptions,
  dependencies: SearchAdapterDependencies,
): Promise<{ hits: SearchHit[]; error?: string }> {
  const { fetchFn, signal } = dependencies;
  const q = options.site ? `site:${options.site} ${options.query}` : options.query;
  const maxResults = Math.min(Math.max(options.maxResults ?? 5, 1), 20);
  const url = `${DDG_HTML}?q=${encodeURIComponent(q)}`;

  try {
    const res = await fetchFn(url, {
      headers: {
        Accept: "text/html",
        "User-Agent": "PrintPartner-Search/1.0",
      },
      signal,
    });
    if (!res.ok) {
      await cancelResponseBody(res);
      return { hits: [], error: `DuckDuckGo HTTP ${res.status}` };
    }
    const html = await readBoundedResponseText(res, MAX_SEARCH_RESPONSE_BYTES);
    const hits = parseDuckDuckGoHtml(html, maxResults);
    if (!hits.length) {
      return {
        hits: [],
        error:
          "DuckDuckGo returned no parseable results (HTML scrape is brittle; try SEARCH_PROVIDER=brave with SEARCH_API_KEY)",
      };
    }
    return { hits };
  } catch (err) {
    const msg =
      err instanceof OutboundUrlError
        ? err.message
        : err instanceof Error
          ? err.message
          : String(err);
    return { hits: [], error: `DuckDuckGo search failed: ${msg}` };
  }
}
