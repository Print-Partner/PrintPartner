import { describe, expect, it, vi } from "vitest";
import {
  buildResolveSearchInput,
  getSearchSetupGuidance,
  getSearchStatus,
  resolveSearchProvider,
  searchConfigured,
  searchOverridesFromRuntime,
  searchWeb,
  type ResolveSearchInput,
} from "./index.js";
import { parseDuckDuckGoHtml } from "./duckduckgo.js";
import { searchBrave } from "./brave.js";
import { loadConfig } from "../../config.js";

function baseInput(over: Partial<ResolveSearchInput> = {}): ResolveSearchInput {
  return {
    searchProvider: null,
    searchApiKey: null,
    aiProvider: "none",
    anthropicApiKey: null,
    openaiApiKey: null,
    ...over,
  };
}

describe("resolveSearchProvider", () => {
  it("uses explicit SEARCH_PROVIDER when set", () => {
    expect(resolveSearchProvider(baseInput({ searchProvider: "brave", searchApiKey: "k" }))).toBe(
      "brave",
    );
    expect(resolveSearchProvider(baseInput({ searchProvider: "exa" }))).toBe("exa");
    expect(resolveSearchProvider(baseInput({ searchProvider: "duckduckgo" }))).toBe("duckduckgo");
    expect(resolveSearchProvider(baseInput({ searchProvider: "none" }))).toBe("none");
  });

  it("prefers anthropic-native when AI is anthropic with a key", () => {
    expect(
      resolveSearchProvider(baseInput({ aiProvider: "anthropic", anthropicApiKey: "sk-ant" })),
    ).toBe("anthropic-native");
  });

  it("prefers openai-native when AI is openai with a key", () => {
    expect(resolveSearchProvider(baseInput({ aiProvider: "openai", openaiApiKey: "sk-oai" }))).toBe(
      "openai-native",
    );
  });

  it("falls back to duckduckgo when no native credentials", () => {
    expect(resolveSearchProvider(baseInput({ aiProvider: "anthropic" }))).toBe("duckduckgo");
    expect(resolveSearchProvider(baseInput({ aiProvider: "ollama" }))).toBe("duckduckgo");
    expect(resolveSearchProvider(baseInput())).toBe("duckduckgo");
  });

  it("explicit provider wins over native", () => {
    expect(
      resolveSearchProvider(
        baseInput({
          searchProvider: "duckduckgo",
          aiProvider: "anthropic",
          anthropicApiKey: "sk",
        }),
      ),
    ).toBe("duckduckgo");
  });
});

describe("searchConfigured", () => {
  it("requires API key for brave and exa", () => {
    expect(searchConfigured("brave", baseInput({ searchProvider: "brave" }))).toBe(false);
    expect(
      searchConfigured("brave", baseInput({ searchProvider: "brave", searchApiKey: "k" })),
    ).toBe(true);
    expect(searchConfigured("exa", baseInput({ searchProvider: "exa" }))).toBe(false);
    expect(searchConfigured("exa", baseInput({ searchProvider: "exa", searchApiKey: "k" }))).toBe(
      true,
    );
  });

  it("duckduckgo is always configured; none is not", () => {
    expect(searchConfigured("duckduckgo", baseInput())).toBe(true);
    expect(searchConfigured("none", baseInput({ searchProvider: "none" }))).toBe(false);
  });

  it("brave without key is not configured (missing key → none-like UX)", () => {
    const input = baseInput({ searchProvider: "brave", searchApiKey: null });
    expect(resolveSearchProvider(input)).toBe("brave");
    expect(searchConfigured("brave", input)).toBe(false);
  });
});

describe("Settings search overrides", () => {
  it("buildResolveSearchInput prefers Settings provider and key over env", () => {
    const config = {
      ...loadConfig(),
      searchProvider: "exa" as const,
      searchApiKey: "env-key",
      aiProvider: "ollama" as const,
    };
    const input = buildResolveSearchInput(config, {
      searchProvider: "brave",
      searchApiKey: "settings-key",
      aiProvider: "ollama",
    });
    expect(input.searchProvider).toBe("brave");
    expect(input.searchApiKey).toBe("settings-key");
    expect(resolveSearchProvider(input)).toBe("brave");
  });

  it("searchOverridesFromRuntime omits null search fields so env applies", () => {
    const config = {
      ...loadConfig(),
      searchProvider: "brave" as const,
      searchApiKey: "env-brave",
    };
    const overrides = searchOverridesFromRuntime({
      provider: "ollama",
      anthropicApiKey: null,
      openaiApiKey: null,
      searchProvider: null,
      searchApiKey: null,
    });
    expect("searchProvider" in overrides).toBe(false);
    expect("searchApiKey" in overrides).toBe(false);
    const input = buildResolveSearchInput(config, overrides);
    expect(input.searchProvider).toBe("brave");
    expect(input.searchApiKey).toBe("env-brave");
  });

  it("getSearchStatus accepts runtime overrides object", () => {
    const config = {
      ...loadConfig(),
      searchProvider: null,
      searchApiKey: null,
      anthropicApiKey: null,
      openaiApiKey: null,
    };
    const status = getSearchStatus(config, {
      searchProvider: "none",
      aiProvider: "ollama",
    });
    expect(status.provider).toBe("none");
    expect(status.configured).toBe(false);
  });
});

describe("getSearchSetupGuidance", () => {
  it("lists all backends", () => {
    const ids = getSearchSetupGuidance().map((o) => o.id);
    expect(ids).toContain("brave");
    expect(ids).toContain("exa");
    expect(ids).toContain("duckduckgo");
    expect(ids).toContain("anthropic-native");
  });
});

describe("parseDuckDuckGoHtml", () => {
  it("extracts result__a links and snippets", () => {
    const html = `
      <div class="result">
        <a class="result__a" href="https://example.com/kit">Example Kit</a>
        <a class="result__snippet">A great kit guide</a>
      </div>
      <div class="result">
        <a class="result__a" href="https://github.com/org/repo">GitHub Repo</a>
        <a class="result__snippet">README for the repo</a>
      </div>
    `;
    const hits = parseDuckDuckGoHtml(html, 5);
    expect(hits).toHaveLength(2);
    expect(hits[0]).toMatchObject({
      title: "Example Kit",
      url: "https://example.com/kit",
    });
    expect(hits[0]!.snippet).toMatch(/great kit/i);
  });
});

describe("remote search response limits", () => {
  it("parses a bounded Brave response", async () => {
    const response = new Response(
      JSON.stringify({
        web: {
          results: [
            {
              title: "Voron documentation",
              url: "https://docs.vorondesign.com/",
              description: "Build documentation",
            },
          ],
        },
      }),
    );

    const result = await searchBrave({ query: "Voron" }, "search-key", {
      fetchFn: async () => response,
      signal: new AbortController().signal,
    });

    expect(result).toEqual({
      hits: [
        {
          title: "Voron documentation",
          url: "https://docs.vorondesign.com/",
          snippet: "Build documentation",
        },
      ],
    });
  });

  it("cancels a Brave response whose declared size exceeds the limit", async () => {
    const cancel = vi.fn();
    const response = new Response(new ReadableStream({ cancel }), {
      headers: { "Content-Length": String(2 * 1024 * 1024 + 1) },
    });

    const result = await searchBrave({ query: "Voron" }, "search-key", {
      fetchFn: async () => response,
      signal: new AbortController().signal,
    });

    expect(result.hits).toEqual([]);
    expect(result.error).toContain("Response body exceeds 2097152 bytes");
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("stops a search when its caller aborts", async () => {
    const controller = new AbortController();
    let requestSignal: AbortSignal | null | undefined;
    const fetchFn = vi.fn<typeof fetch>().mockImplementation(async (_input, init) => {
      requestSignal = init?.signal;
      return await new Promise<Response>((resolve, reject) => {
        const fallback = setTimeout(
          () => resolve(new Response(JSON.stringify({ web: { results: [] } }))),
          250,
        );
        requestSignal?.addEventListener(
          "abort",
          () => {
            clearTimeout(fallback);
            reject(requestSignal?.reason);
          },
          { once: true },
        );
      });
    });
    const config = {
      ...loadConfig(),
      searchProvider: "brave" as const,
      searchApiKey: "search-key",
      aiProvider: "none" as const,
    };

    const pending = searchWeb({ query: "Voron" }, config, {
      fetchFn,
      signal: controller.signal,
      timeoutMs: 1_000,
    });
    controller.abort(new Error("caller stopped"));

    const result = await pending;
    expect(result.hits).toEqual([]);
    expect(result.error).toContain("caller stopped");
    expect(requestSignal?.aborted).toBe(true);
  });

  it("keeps the search deadline active while reading the response body", async () => {
    let requestSignal: AbortSignal | null | undefined;
    const fetchFn = vi.fn<typeof fetch>().mockImplementation(async (_input, init) => {
      requestSignal = init?.signal;
      return new Response(
        new ReadableStream({
          start(streamController) {
            const fallback = setTimeout(
              () => streamController.error(new Error("test stream fallback")),
              250,
            );
            requestSignal?.addEventListener(
              "abort",
              () => {
                clearTimeout(fallback);
                streamController.error(requestSignal?.reason);
              },
              { once: true },
            );
          },
        }),
        { headers: { "Content-Type": "application/json" } },
      );
    });
    const config = {
      ...loadConfig(),
      searchProvider: "brave" as const,
      searchApiKey: "search-key",
      aiProvider: "none" as const,
    };

    const result = await searchWeb({ query: "Voron" }, config, { fetchFn, timeoutMs: 25 });

    expect(result.hits).toEqual([]);
    expect(result.error).toMatch(/aborted|timeout/i);
    expect(result.error).not.toContain("test stream fallback");
    expect(requestSignal?.aborted).toBe(true);
  });
});
