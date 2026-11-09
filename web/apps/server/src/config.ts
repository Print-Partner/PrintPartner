import type {
  AiProviderId,
  DeployMode,
  RuntimeReleaseIdentity,
  SearchProviderId,
} from "@print-partner/contracts";
import { createRequire } from "node:module";
import { resolveRuntimeReleaseIdentity } from "./lib/version.js";

const SEARCH_PROVIDER_IDS: SearchProviderId[] = [
  "anthropic-native",
  "openai-native",
  "brave",
  "exa",
  "duckduckgo",
  "none",
];

function isSearchProviderId(raw: string): raw is SearchProviderId {
  return (SEARCH_PROVIDER_IDS as string[]).includes(raw);
}

export type ServerConfig = {
  deployMode: DeployMode;
  host: string;
  port: number;
  dataDir: string;
  /** Trust proxy forwarding headers. Disables unauthenticated loopback bypasses. */
  trustProxy: boolean;
  /** Shared slicer exchange volume root (host path). Empty disables managed open. */
  exchangeDir: string;
  version: string;
  releaseIdentity: RuntimeReleaseIdentity;
  corsOrigin: string | boolean | string[];
  staticDir: string | null;
  databaseUrl: string | null;
  /** Explicit opt-in for the sync-bridge Postgres backend, which is not production-ready. */
  postgresExperimental: boolean;
  /** When true, require user login (self-host or saas). Default off in self-host. */
  multiUser: boolean;
  /** Require one authenticated administrator without enabling multi-user sharing. */
  singleUserAuth: boolean;
  discordClientId: string | null;
  discordClientSecret: string | null;
  discordCallbackUrl: string | null;
  discordOAuthConfigured: boolean;
  saasBasicAuth: string | null;
  saasAllowAnonymous: boolean;
  /** When false, POST /auth/register and new OAuth identities are refused. */
  registrationOpen: boolean;
  authRequired: boolean;
  sessionSecret: string | null;
  sessionCookieSecure: boolean;
  githubClientId: string | null;
  githubClientSecret: string | null;
  githubCallbackUrl: string | null;
  githubOAuthConfigured: boolean;
  /**
   * Public Google OAuth Web client id for Drive file open/save in the SPA.
   * Not a secret — safe to expose via /health. Default: unset (Drive UI disabled).
   */
  googleClientId: string | null;
  authSuccessRedirect: string;
  basicAuthUser: string | null;
  basicAuthPass: string | null;
  /** Self-host: API key for versioned routes; /api/v1/mcp needs this or a Settings key on every host */
  integrationApiKey: string | null;
  /** When false, skip GitHub / override version checks for app updates */
  updateCheckEnabled: boolean;
  /** Canonical public URL for password reset links. Required for SMTP delivery. */
  appPublicUrl: string | null;
  smtpHost: string | null;
  smtpPort: number;
  smtpUser: string | null;
  smtpPass: string | null;
  smtpFrom: string | null;
  smtpSecure: boolean;
  smtpConfigured: boolean;
  /** When true and SMTP is off, API may return reset URL in dev/non-prod responses. */
  passwordResetDevExpose: boolean;
  /** GitHub owner/repo for release lookup (e.g. Print-Partner/PrintPartner) */
  githubRepo: string;
  /** Air-gapped override: treat this as the latest published version */
  latestVersionOverride: string | null;
  /** In-memory cache TTL for update checks (hours) */
  updateCheckCacheHours: number;
  /** Opt-in AI advisor (Phase 1: read-only chat). Requires AI_ENABLED=1. */
  aiEnabled: boolean;
  aiProvider: AiProviderId;
  anthropicApiKey: string | null;
  openaiApiKey: string | null;
  openaiBaseUrl: string | null;
  aiModel: string | null;
  ollamaUrl: string;
  aiMaxTokens: number;
  /**
   * Soft per-tenant daily chat request cap (`0` = unlimited).
   * Overridable via Settings `daily_request_budget`.
   */
  aiDailyRequestBudget: number;
  /**
   * Soft per-tenant daily estimated-token cap (`0` = unlimited).
   * Overridable via Settings `daily_token_budget`.
   */
  aiDailyTokenBudget: number;
  /**
   * Per-source budget for synced markdown/PDF docs (bytes). Default ~1 GiB.
   * Operator escape hatch: `SOURCE_DOCS_MAX_BYTES`.
   */
  sourceDocsMaxBytes: number;
  /**
   * When false, `ingest_guide_url` is disabled. Default true.
   * `ASSISTANT_ALLOW_URL_INGEST=0` to disable.
   */
  assistantAllowUrlIngest: boolean;
  /** Max bytes for a single guide URL fetch. Default 512 KiB. */
  assistantGuideIngestMaxBytes: number;
  /**
   * Explicit web search backend (`null` = auto-resolve).
   * Env: `SEARCH_PROVIDER`.
   */
  searchProvider: SearchProviderId | null;
  /**
   * API key for Brave / Exa search backends.
   * Env: `SEARCH_API_KEY`, with `BRAVE_API_KEY` / `EXA_API_KEY` as fallbacks.
   */
  searchApiKey: string | null;
};

const DEFAULT_DATA_DIR = process.env.PRINT_PARTNER_DATA_DIR ?? "./data";
const require = createRequire(import.meta.url);
const requireFromServerPackage = createRequire(
  require.resolve("@print-partner/server/package.json"),
);
const appPackage: unknown = requireFromServerPackage("../../package.json");
if (
  typeof appPackage !== "object" ||
  appPackage === null ||
  !("version" in appPackage) ||
  typeof appPackage.version !== "string"
) {
  throw new Error("Application package must contain a version string");
}
const appVersion = appPackage.version;

function parseDeployMode(raw: string | undefined): DeployMode {
  if (raw === "saas") return "saas";
  return "self-host";
}

function resolveBasicAuth(): string | null {
  if (process.env.SAAS_BASIC_AUTH) return process.env.SAAS_BASIC_AUTH;
  const user = process.env.BASIC_AUTH_USER ?? process.env.SAAS_BASIC_AUTH_USER;
  const pass = process.env.BASIC_AUTH_PASS ?? process.env.SAAS_BASIC_AUTH_PASS;
  if (user && pass) return `${user}:${pass}`;
  return null;
}

/**
 * `desktop` is true in the desktop compatibility child. Its boundary admits each
 * request only with a signed desktop principal, never a cookie session, so the
 * multi-user SESSION_SECRET rule does not apply there.
 */
export function validateProductionConfig(
  config: ServerConfig,
  options: { desktop?: boolean } = {},
): void {
  const isProd = process.env.NODE_ENV === "production";
  if (!isProd) return;
  if (config.multiUser && !config.sessionSecret && !options.desktop) {
    throw new Error("SESSION_SECRET is required when MULTI_USER is enabled");
  }
  if (config.deployMode === "saas" && !config.sessionSecret && config.githubOAuthConfigured) {
    throw new Error("SESSION_SECRET is required in production SaaS mode with OAuth enabled");
  }
  if (
    config.deployMode === "saas" &&
    config.authRequired &&
    !config.sessionSecret &&
    !config.saasBasicAuth
  ) {
    throw new Error("SESSION_SECRET or SAAS_BASIC_AUTH is required when SaaS auth is enabled");
  }
  if (config.smtpConfigured && !config.appPublicUrl) {
    throw new Error("APP_PUBLIC_URL is required when SMTP password-reset email is enabled");
  }
  if (config.deployMode === "saas" && config.databaseUrl && !config.postgresExperimental) {
    throw new Error(
      "Postgres support is experimental; set POSTGRES_EXPERIMENTAL=1 to acknowledge the sync-bridge limitations",
    );
  }
  if (config.deployMode === "saas") {
    if (config.saasAllowAnonymous) {
      throw new Error("SAAS_ALLOW_ANONYMOUS is not allowed in production hosted planning");
    }
    if (config.corsOrigin === true) {
      throw new Error("ALLOWED_ORIGINS must be a public origin in production hosted planning");
    }
    if (!config.multiUser) {
      throw new Error("MULTI_USER=1 is required in production hosted planning");
    }
  }
}

function parseCorsOrigin(raw: string | undefined): string | boolean | string[] {
  if (!raw || raw === "true") return true;
  if (raw === "false") return false;
  const origins = raw
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  if (origins.length === 1) return origins[0]!;
  if (origins.length > 1) return origins;
  return true;
}

function parseAiProvider(raw: string | undefined): AiProviderId {
  if (raw === "anthropic" || raw === "openai" || raw === "ollama" || raw === "none") {
    return raw;
  }
  return "none";
}

function parseSearchProvider(raw: string | undefined): SearchProviderId | null {
  const trimmed = raw?.trim().toLowerCase();
  if (!trimmed) return null;
  if (isSearchProviderId(trimmed)) return trimmed;
  return null;
}

function resolveSearchApiKey(): string | null {
  return (
    process.env.SEARCH_API_KEY?.trim() ||
    process.env.BRAVE_API_KEY?.trim() ||
    process.env.EXA_API_KEY?.trim() ||
    null
  );
}

function defaultAiModel(provider: AiProviderId, explicit: string | null): string | null {
  if (explicit) return explicit;
  if (provider === "anthropic") return "claude-sonnet-4-20250514";
  if (provider === "openai") return "gpt-4o-mini";
  if (provider === "ollama") return "llama3.1";
  return null;
}

function aiCredentialsPresent(
  provider: AiProviderId,
  anthropicApiKey: string | null,
  openaiApiKey: string | null,
): boolean {
  if (provider === "anthropic") return Boolean(anthropicApiKey);
  if (provider === "openai") return Boolean(openaiApiKey);
  if (provider === "ollama") return true;
  return false;
}

export function loadConfig(): ServerConfig {
  const deployMode = parseDeployMode(process.env.DEPLOY_MODE);
  const releaseIdentity = resolveRuntimeReleaseIdentity({
    packageVersion: appVersion,
    deployMode,
    env: process.env,
  });
  const port = Number(process.env.PORT ?? 18765);
  const host = process.env.HOST ?? "127.0.0.1";
  const dataDir =
    deployMode === "saas"
      ? (process.env.SAAS_DATA_DIR ?? DEFAULT_DATA_DIR)
      : (process.env.PRINT_PARTNER_DATA_DIR ?? DEFAULT_DATA_DIR);

  const githubClientId = process.env.GITHUB_CLIENT_ID ?? null;
  const githubClientSecret = process.env.GITHUB_CLIENT_SECRET ?? null;
  const githubCallbackUrl = process.env.GITHUB_CALLBACK_URL ?? null;
  /** Public GIS/Drive client id (not a secret). See DEPLOY.md. */
  const googleClientId = process.env.GOOGLE_CLIENT_ID?.trim() || null;
  const discordClientId = process.env.DISCORD_CLIENT_ID ?? null;
  const discordClientSecret = process.env.DISCORD_CLIENT_SECRET ?? null;
  const discordCallbackUrl = process.env.DISCORD_CALLBACK_URL ?? null;
  const multiUser =
    process.env.MULTI_USER === "1" || (deployMode === "saas" && process.env.MULTI_USER !== "0");
  const singleUserAuth = deployMode === "self-host" && process.env.SINGLE_USER_AUTH === "1";

  const saasBasicAuth = resolveBasicAuth();
  const saasAllowAnonymous = process.env.SAAS_ALLOW_ANONYMOUS === "1";
  const oauthConfigured = Boolean(githubClientId || discordClientId);
  const authRequired =
    multiUser ||
    singleUserAuth ||
    (deployMode === "saas" && !saasAllowAnonymous && Boolean(saasBasicAuth || oauthConfigured));

  const basicUser = process.env.BASIC_AUTH_USER ?? null;
  const basicPass = process.env.BASIC_AUTH_PASS ?? null;

  const smtpHost = process.env.SMTP_HOST?.trim() || null;
  const smtpPort = Number(process.env.SMTP_PORT ?? 587);
  const smtpUser = process.env.SMTP_USER?.trim() || null;
  const smtpPass = process.env.SMTP_PASS ?? null;
  const smtpFrom = process.env.SMTP_FROM?.trim() || null;
  const smtpSecure = process.env.SMTP_SECURE === "1" || smtpPort === 465;
  const smtpConfigured = Boolean(smtpHost && smtpFrom);
  const isProd = process.env.NODE_ENV === "production";
  const passwordResetDevExpose =
    process.env.PASSWORD_RESET_DEV_EXPOSE === "1" ||
    (!isProd && process.env.PASSWORD_RESET_DEV_EXPOSE !== "0");

  const aiProvider = parseAiProvider(process.env.AI_PROVIDER);
  const anthropicApiKey = process.env.ANTHROPIC_API_KEY?.trim() || null;
  const openaiApiKey = process.env.OPENAI_API_KEY?.trim() || null;
  const openaiBaseUrl = process.env.OPENAI_BASE_URL?.trim() || null;
  const aiModel = defaultAiModel(aiProvider, process.env.AI_MODEL?.trim() || null);
  const ollamaUrl = (process.env.OLLAMA_URL?.trim() || "http://127.0.0.1:11434").replace(/\/$/, "");
  const aiMaxTokens = Number(process.env.AI_MAX_TOKENS ?? 2048);
  const aiDailyRequestBudget = Number(process.env.AI_DAILY_REQUEST_BUDGET ?? 0);
  const aiDailyTokenBudget = Number(process.env.AI_DAILY_TOKEN_BUDGET ?? 0);
  const aiOptIn = process.env.AI_ENABLED === "1";
  const aiEnabled =
    aiOptIn &&
    aiProvider !== "none" &&
    aiCredentialsPresent(aiProvider, anthropicApiKey, openaiApiKey);

  return {
    deployMode,
    host,
    port,
    dataDir,
    trustProxy: process.env.TRUST_PROXY === "1",
    exchangeDir: (process.env.PP_EXCHANGE_DIR ?? "").trim() || "/exchange",
    version: releaseIdentity.runtime_version,
    releaseIdentity,
    corsOrigin: parseCorsOrigin(process.env.ALLOWED_ORIGINS ?? process.env.CORS_ORIGIN),
    staticDir: process.env.STATIC_DIR ?? null,
    databaseUrl: process.env.DATABASE_URL ?? null,
    postgresExperimental: process.env.POSTGRES_EXPERIMENTAL === "1",
    multiUser,
    singleUserAuth,
    discordClientId,
    discordClientSecret,
    discordCallbackUrl,
    discordOAuthConfigured: Boolean(discordClientId && discordClientSecret && discordCallbackUrl),
    saasBasicAuth,
    saasAllowAnonymous,
    registrationOpen: process.env.REGISTRATION_OPEN !== "0",
    authRequired,
    sessionSecret: process.env.SESSION_SECRET?.trim() || null,
    sessionCookieSecure:
      process.env.SESSION_COOKIE_SECURE === "1" ||
      (deployMode === "saas" && process.env.SESSION_COOKIE_SECURE !== "0"),
    githubClientId,
    githubClientSecret,
    githubCallbackUrl,
    githubOAuthConfigured: Boolean(githubClientId && githubClientSecret && githubCallbackUrl),
    googleClientId,
    authSuccessRedirect: process.env.AUTH_SUCCESS_REDIRECT ?? "/",
    basicAuthUser: basicUser,
    basicAuthPass: basicPass,
    integrationApiKey:
      process.env.INTEGRATION_API_KEY?.trim() || process.env.PRINT_PARTNER_API_KEY?.trim() || null,
    updateCheckEnabled: process.env.PRINT_PARTNER_UPDATE_CHECK !== "0",
    githubRepo: process.env.GITHUB_REPO?.trim() || "Print-Partner/PrintPartner",
    latestVersionOverride: process.env.PRINT_PARTNER_LATEST_VERSION?.trim() || null,
    updateCheckCacheHours: Number(process.env.PRINT_PARTNER_UPDATE_CHECK_CACHE_HOURS ?? 12),
    appPublicUrl: process.env.APP_PUBLIC_URL?.trim() || null,
    smtpHost,
    smtpPort,
    smtpUser,
    smtpPass,
    smtpFrom,
    smtpSecure,
    smtpConfigured,
    passwordResetDevExpose,
    aiEnabled,
    aiProvider,
    anthropicApiKey,
    openaiApiKey,
    openaiBaseUrl,
    aiModel,
    ollamaUrl,
    aiMaxTokens: Number.isFinite(aiMaxTokens) && aiMaxTokens > 0 ? aiMaxTokens : 2048,
    aiDailyRequestBudget:
      Number.isFinite(aiDailyRequestBudget) && aiDailyRequestBudget > 0
        ? Math.trunc(aiDailyRequestBudget)
        : 0,
    aiDailyTokenBudget:
      Number.isFinite(aiDailyTokenBudget) && aiDailyTokenBudget > 0
        ? Math.trunc(aiDailyTokenBudget)
        : 0,
    sourceDocsMaxBytes: (() => {
      const raw = Number(process.env.SOURCE_DOCS_MAX_BYTES ?? 1024 * 1024 * 1024);
      return Number.isFinite(raw) && raw > 0 ? Math.trunc(raw) : 1024 * 1024 * 1024;
    })(),
    assistantAllowUrlIngest: process.env.ASSISTANT_ALLOW_URL_INGEST !== "0",
    assistantGuideIngestMaxBytes: (() => {
      const raw = Number(process.env.ASSISTANT_GUIDE_INGEST_MAX_BYTES ?? 512 * 1024);
      return Number.isFinite(raw) && raw > 0 ? Math.trunc(raw) : 512 * 1024;
    })(),
    searchProvider: parseSearchProvider(process.env.SEARCH_PROVIDER),
    searchApiKey: resolveSearchApiKey(),
  };
}
