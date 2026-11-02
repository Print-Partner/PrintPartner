import { OutboundUrlError } from "../../lib/outbound-url.js";
import { cancelResponseBody, readBoundedJsonResponse } from "../../lib/bounded-response.js";
import type { SearchAdapterDependencies, SearchHit, WebSearchOptions } from "./types.js";
import { isRecord } from "../../lib/guards.js";

const BRAVE_ENDPOINT = "https://api.search.brave.com/res/v1/web/search";
const MAX_SEARCH_RESPONSE_BYTES = 2 * 1024 * 1024;

export async function searchBrave(
  options: WebSearchOptions,
  apiKey: string,
  dependencies: SearchAdapterDependencies,
): Promise<{ hits: SearchHit[]; error?: string }> {
  const { fetchFn, signal } = dependencies;
  const q = options.site ? `site:${options.site} ${options.query}` : options.query;
  const count = Math.min(Math.max(options.maxResults ?? 5, 1), 20);
  const url = `${BRAVE_ENDPOINT}?q=${encodeURIComponent(q)}&count=${count}`;

  try {
    const res = await fetchFn(url, {
      headers: {
        Accept: "application/json",
        "X-Subscription-Token": apiKey,
        "User-Agent": "PrintPartner-Search/1.0",
      },
      signal,
    });
    if (!res.ok) {
      await cancelResponseBody(res);
      return { hits: [], error: `Brave Search HTTP ${res.status}` };
    }
    const body = await readBoundedJsonResponse(res, MAX_SEARCH_RESPONSE_BYTES);
    const web = isRecord(body) && isRecord(body.web) ? body.web : null;
    const results = Array.isArray(web?.results) ? web.results.filter(isRecord) : [];
    const hits: SearchHit[] = results
      .flatMap((result) => {
        if (typeof result.url !== "string" || typeof result.title !== "string") return [];
        return [
          {
            title: result.title,
            url: result.url,
            snippet: typeof result.description === "string" ? result.description.slice(0, 500) : "",
          },
        ];
      })
      .slice(0, count);
    return { hits };
  } catch (err) {
    const msg =
      err instanceof OutboundUrlError
        ? err.message
        : err instanceof Error
          ? err.message
          : String(err);
    return { hits: [], error: `Brave Search failed: ${msg}` };
  }
}
