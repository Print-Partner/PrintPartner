import { OutboundUrlError } from "../../lib/outbound-url.js";
import { cancelResponseBody, readBoundedJsonResponse } from "../../lib/bounded-response.js";
import type { SearchAdapterDependencies, SearchHit, WebSearchOptions } from "./types.js";
import { isRecord } from "../../lib/guards.js";

const EXA_ENDPOINT = "https://api.exa.ai/search";
const MAX_SEARCH_RESPONSE_BYTES = 2 * 1024 * 1024;

export async function searchExa(
  options: WebSearchOptions,
  apiKey: string,
  dependencies: SearchAdapterDependencies,
): Promise<{ hits: SearchHit[]; error?: string }> {
  const { fetchFn, signal } = dependencies;
  const q = options.site ? `site:${options.site} ${options.query}` : options.query;
  const numResults = Math.min(Math.max(options.maxResults ?? 5, 1), 20);

  try {
    const res = await fetchFn(EXA_ENDPOINT, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        Authorization: `Bearer ${apiKey}`,
        "User-Agent": "PrintPartner-Search/1.0",
      },
      body: JSON.stringify({
        query: q,
        numResults,
        type: "auto",
        contents: { text: { maxCharacters: 500 } },
      }),
      signal,
    });
    if (!res.ok) {
      await cancelResponseBody(res);
      return { hits: [], error: `Exa Search HTTP ${res.status}` };
    }
    const body = await readBoundedJsonResponse(res, MAX_SEARCH_RESPONSE_BYTES);
    const results =
      isRecord(body) && Array.isArray(body.results) ? body.results.filter(isRecord) : [];
    const hits: SearchHit[] = results
      .flatMap((result) => {
        if (typeof result.url !== "string") return [];
        return [
          {
            title: typeof result.title === "string" && result.title ? result.title : result.url,
            url: result.url,
            snippet:
              typeof result.text === "string"
                ? result.text.slice(0, 500)
                : typeof result.snippet === "string"
                  ? result.snippet.slice(0, 500)
                  : "",
          },
        ];
      })
      .slice(0, numResults);
    return { hits };
  } catch (err) {
    const msg =
      err instanceof OutboundUrlError
        ? err.message
        : err instanceof Error
          ? err.message
          : String(err);
    return { hits: [], error: `Exa Search failed: ${msg}` };
  }
}
