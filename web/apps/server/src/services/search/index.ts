/**
 * Pluggable web search for assistant research tools.
 *
 * Resolution order (`resolveSearchProvider`):
 * 1. Explicit search provider (Settings `search_provider` override, else env `SEARCH_PROVIDER`)
 * 2. Provider-native when AI provider is anthropic/openai with a key
 * 3. DuckDuckGo HTML fallback (no key)
 * 4. `none` if somehow disabled
 *
 * HTTP backends for `web_search` tool hits: brave / exa / duckduckgo.
 * When the resolved provider is anthropic-native or openai-native, status reports
 * native, but the tool still returns DuckDuckGo hits so the tool loop always gets
 * structured results unless SEARCH_PROVIDER=brave|exa (or Settings equivalent).
 *
 * API keys: Settings `search_api_key` overrides env `SEARCH_API_KEY` / Brave/Exa aliases.
 */

import type { AiProviderId } from "@print-partner/contracts";
import type { ServerConfig } from "../../config.js";
import { safeOutboundFetch } from "../../lib/outbound-url.js";
import { searchBrave } from "./brave.js";
import { searchDuckDuckGo } from "./duckduckgo.js";
import { searchExa } from "./exa.js";
import { nativeSearchNote, nativeSearchProviderForAi } from "./provider-native.js";
import {
  isSearchProviderId,
  SEARCH_UNTRUSTED_BANNER,
  type SearchAdapterDependencies,
  type SearchProviderId,
  type SearchSetupOption,
  type SearchStatus,
  type WebSearchOptions,
  type WebSearchResult,
} from "./types.js";

const WEB_SEARCH_REQUEST_TIMEOUT_MS = 30_000;

export type { SearchProviderId } from "./types.js";

export function getSearchSetupGuidance(): SearchSetupOption[] {
  return [
    {
      id: "anthropic-native",
      label: "Anthropic native",
      summary: "Zero extra search config when AI provider is Anthropic.",
      setup:
        "Set AI_PROVIDER=anthropic (or Settings → Anthropic) with ANTHROPIC_API_KEY. web_search still returns DuckDuckGo HTTP hits unless you set SEARCH_PROVIDER=brave|exa.",
    },
    {
      id: "openai-native",
      label: "OpenAI native",
      summary: "Zero extra search config when AI provider is OpenAI.",
      setup:
        "Set AI_PROVIDER=openai (or Settings → OpenAI) with OPENAI_API_KEY. web_search still returns DuckDuckGo HTTP hits unless you set SEARCH_PROVIDER=brave|exa.",
    },
    {
      id: "brave",
      label: "Brave Search",
      summary: "Official Brave Search API (recommended paid option).",
      setup:
        "SEARCH_PROVIDER=brave and SEARCH_API_KEY (or BRAVE_API_KEY) from https://brave.com/search/api/",
    },
    {
      id: "exa",
      label: "Exa",
      summary: "Exa neural search API.",
      setup: "SEARCH_PROVIDER=exa and SEARCH_API_KEY (or EXA_API_KEY) from https://exa.ai/",
    },
    {
      id: "duckduckgo",
      label: "DuckDuckGo (HTML)",
      summary: "No API key — brittle HTML scrape fallback.",
      setup:
        "Default when no API key. Force with SEARCH_PROVIDER=duckduckgo. Prefer Brave/Exa for reliability.",
    },
    {
      id: "none",
      label: "Disabled",
      summary: "Web search tools return setup hints only.",
      setup: "SEARCH_PROVIDER=none disables HTTP search.",
    },
  ];
}

export type ResolveSearchInput = {
  searchProvider: SearchProviderId | null;
  searchApiKey: string | null;
  aiProvider: AiProviderId;
  anthropicApiKey: string | null;
  openaiApiKey: string | null;
};

/** Optional Settings / runtime overrides on top of env `ServerConfig`. */
type SearchResolveOverrides = {
  /** When set (including `null` meaning Auto), overrides env SEARCH_PROVIDER. Pass `undefined` to keep env. */
  searchProvider?: SearchProviderId | null;
  searchApiKey?: string | null;
  aiProvider?: AiProviderId;
  anthropicApiKey?: string | null;
  openaiApiKey?: string | null;
};

/**
 * Build search resolution input: Settings/runtime overrides → env (`ServerConfig`).
 * - `searchProvider` present on overrides (even `null`) replaces env; omit the key to keep env.
 * - `searchApiKey` uses override when a non-empty string; otherwise env.
 */
export function buildResolveSearchInput(
  config: ServerConfig,
  overrides?: SearchResolveOverrides,
): ResolveSearchInput {
  return {
    searchProvider:
      overrides && "searchProvider" in overrides
        ? (overrides.searchProvider ?? null)
        : config.searchProvider,
    searchApiKey:
      overrides?.searchApiKey != null && overrides.searchApiKey !== ""
        ? overrides.searchApiKey
        : config.searchApiKey,
    aiProvider: overrides?.aiProvider ?? config.aiProvider,
    anthropicApiKey: overrides?.anthropicApiKey ?? config.anthropicApiKey,
    openaiApiKey: overrides?.openaiApiKey ?? config.openaiApiKey,
  };
}

/**
 * Merge assistant runtime search fields into overrides for `buildResolveSearchInput`.
 * Runtime `searchProvider` / `searchApiKey` of `null` mean "no Settings override" → env.
 */
export function searchOverridesFromRuntime(runtime: {
  provider: AiProviderId;
  anthropicApiKey: string | null;
  openaiApiKey: string | null;
  searchProvider: SearchProviderId | null;
  searchApiKey: string | null;
}): SearchResolveOverrides {
  const overrides: SearchResolveOverrides = {
    aiProvider: runtime.provider,
    anthropicApiKey: runtime.anthropicApiKey,
    openaiApiKey: runtime.openaiApiKey,
  };
  if (runtime.searchProvider != null) {
    overrides.searchProvider = runtime.searchProvider;
  }
  if (runtime.searchApiKey != null) {
    overrides.searchApiKey = runtime.searchApiKey;
  }
  return overrides;
}

export function resolveSearchProvider(input: ResolveSearchInput): SearchProviderId {
  const explicit = input.searchProvider;
  if (explicit && isSearchProviderId(explicit)) {
    return explicit;
  }

  const native = nativeSearchProviderForAi(input.aiProvider);
  if (native === "anthropic-native" && input.anthropicApiKey) return "anthropic-native";
  if (native === "openai-native" && input.openaiApiKey) return "openai-native";

  return "duckduckgo";
}

export function searchConfigured(provider: SearchProviderId, input: ResolveSearchInput): boolean {
  if (provider === "none") return false;
  if (provider === "duckduckgo") return true;
  if (provider === "brave" || provider === "exa") return Boolean(input.searchApiKey);
  if (provider === "anthropic-native") return Boolean(input.anthropicApiKey);
  if (provider === "openai-native") return Boolean(input.openaiApiKey);
  return false;
}

export function getSearchStatus(
  config: ServerConfig,
  aiProviderOrOverrides?: AiProviderId | SearchResolveOverrides,
): SearchStatus {
  const overrides: SearchResolveOverrides =
    typeof aiProviderOrOverrides === "string" || aiProviderOrOverrides === undefined
      ? { aiProvider: aiProviderOrOverrides }
      : aiProviderOrOverrides;
  const input = buildResolveSearchInput(config, overrides);
  const provider = resolveSearchProvider(input);
  return {
    provider,
    configured: searchConfigured(provider, input),
    options: getSearchSetupGuidance(),
  };
}

function setupHintFor(provider: SearchProviderId, missingKey?: boolean): string {
  const options = getSearchSetupGuidance();
  if (provider === "brave" || provider === "exa") {
    const opt = options.find((o) => o.id === provider);
    return missingKey ? `Missing API key. ${opt?.setup ?? ""}` : (opt?.setup ?? "");
  }
  if (provider === "none") {
    return options.find((o) => o.id === "none")!.setup;
  }
  return "Set SEARCH_PROVIDER=brave|exa with SEARCH_API_KEY for reliable results, or leave unset for DuckDuckGo HTML fallback.";
}

/**
 * Run web search via the active HTTP backend.
 * Native providers report as native but use DuckDuckGo for HTTP hits.
 */
export async function searchWeb(
  options: WebSearchOptions,
  config: ServerConfig,
  deps?: SearchResolveOverrides & {
    fetchFn?: typeof safeOutboundFetch;
    signal?: AbortSignal;
    timeoutMs?: number;
  },
): Promise<WebSearchResult> {
  const query = options.query.trim();
  if (!query) {
    return {
      provider: "none",
      hits: [],
      untrusted_banner: SEARCH_UNTRUSTED_BANNER,
      error: "query required",
      setup_hint: setupHintFor("duckduckgo"),
    };
  }

  const {
    fetchFn: fetchOverride,
    signal: callerSignal,
    timeoutMs = WEB_SEARCH_REQUEST_TIMEOUT_MS,
    ...overrides
  } = deps ?? {};
  const input = buildResolveSearchInput(config, overrides);
  const provider = resolveSearchProvider(input);
  const fetchFn = fetchOverride ?? safeOutboundFetch;

  if (provider === "none") {
    return {
      provider,
      hits: [],
      untrusted_banner: SEARCH_UNTRUSTED_BANNER,
      error: "Web search disabled (SEARCH_PROVIDER=none)",
      setup_hint: setupHintFor("none"),
    };
  }

  const deadline = AbortSignal.timeout(timeoutMs);
  const adapterDependencies: SearchAdapterDependencies = {
    fetchFn,
    signal: callerSignal ? AbortSignal.any([callerSignal, deadline]) : deadline,
  };

  if (provider === "brave") {
    if (!input.searchApiKey) {
      return {
        provider,
        hits: [],
        untrusted_banner: SEARCH_UNTRUSTED_BANNER,
        error: "Brave Search requires SEARCH_API_KEY or BRAVE_API_KEY",
        setup_hint: setupHintFor("brave", true),
      };
    }
    const { hits, error } = await searchBrave(options, input.searchApiKey, adapterDependencies);
    return {
      provider,
      hits,
      untrusted_banner: SEARCH_UNTRUSTED_BANNER,
      http_backend: "brave",
      ...(error ? { error, setup_hint: setupHintFor("brave") } : {}),
    };
  }

  if (provider === "exa") {
    if (!input.searchApiKey) {
      return {
        provider,
        hits: [],
        untrusted_banner: SEARCH_UNTRUSTED_BANNER,
        error: "Exa Search requires SEARCH_API_KEY or EXA_API_KEY",
        setup_hint: setupHintFor("exa", true),
      };
    }
    const { hits, error } = await searchExa(options, input.searchApiKey, adapterDependencies);
    return {
      provider,
      hits,
      untrusted_banner: SEARCH_UNTRUSTED_BANNER,
      http_backend: "exa",
      ...(error ? { error, setup_hint: setupHintFor("exa") } : {}),
    };
  }

  // anthropic-native / openai-native / duckduckgo → DuckDuckGo HTTP
  const { hits, error } = await searchDuckDuckGo(options, adapterDependencies);
  const note =
    provider === "anthropic-native" || provider === "openai-native"
      ? nativeSearchNote(provider)
      : undefined;
  return {
    provider,
    hits,
    untrusted_banner: SEARCH_UNTRUSTED_BANNER,
    http_backend: "duckduckgo",
    ...(error ? { error, setup_hint: setupHintFor(provider) } : note ? { setup_hint: note } : {}),
  };
}
