import { StatusExecution } from "./integrations/status-execution.js";
import { moonrakerHostSnapshot } from "./moonraker-host-snapshot.js";
import {
  acquireDataDirectory,
  desktopContext,
  desktopPrincipal,
  desktopRateLimit,
  mappedDesktopTenant,
  mappedDesktopDatabaseAvailable,
  registerDesktopBoundary,
  serveDesktopPipe,
} from "./desktop-context.js";
import { rustOwnedCopy } from "./db/rust-owned.js";
import Fastify, { type FastifyInstance } from "fastify";
import cors from "@fastify/cors";
import websocket from "@fastify/websocket";
import compress from "@fastify/compress";
import multipart from "@fastify/multipart";
import cookie from "@fastify/cookie";
import rateLimit from "@fastify/rate-limit";
import type { ServerConfig } from "./config.js";
import { createSelfHostPorts } from "./adapters/self-host/index.js";
import { createSaasPorts } from "./adapters/saas/index.js";
import type { AppPorts } from "./ports/index.js";
import { registerHealthRoutes } from "./routes/health.js";
import { registerJobWebSocket } from "./routes/jobs.js";
import { createJobRunner, type InProcessJobRunner } from "./services/job-runner.js";
import { registerCoreRoutes } from "./routes/core-routes.js";
import { registerBackupRoutes } from "./routes/backups.js";
import { registerLoggingRoutes } from "./routes/logging.js";
import { registerApiKeyManagementRoutes } from "./routes/api-key-management.js";
import { registerMetricsRoutes } from "./routes/metrics.js";
import {
  registerApiV1Plugin,
  registerOpenApi,
  registerOpenApiJsonRoutes,
} from "./routes/api-v1.js";
import { registerApiV2PlanPlugin } from "./routes/api-v2.js";
import { registerAcceptedPlateRoutes } from "./routes/accepted-plates.js";
import { registerPlanDraftRoutes } from "./routes/plan-drafts.js";
import { registerAuthRoutes, registerTenantMiddleware } from "./routes/auth.js";
import { createAdminPreHandler, registerApiKeyAuth } from "./middleware/api-key.js";
import { registerRequestLoggingMiddleware } from "./middleware/request-logging.js";
import { validateProductionConfig } from "./config.js";
import {
  getRequestTenantId,
  setRequestTenantId,
  tenantStorage,
} from "./middleware/tenant-context.js";
import fastifyStatic from "@fastify/static";
import { existsSync } from "node:fs";
import { rustOwnedNodeParticipants } from "./ownership/operation-cutover.js";
import {
  beginSourceSyncAdmission,
  sourceSyncOwnedByRust,
} from "./ownership/source-sync-admission.js";
import { dirname, join, resolve } from "node:path";
import { isBrowserDocumentNavigation, isSpaClientPath } from "./lib/spa-nav.js";
import type { SaasDbStore } from "./adapters/saas/index.js";
import type { SelfHostDbStore } from "./adapters/self-host/index.js";
import type { AppRepository } from "./db/repository.js";
import { getDb } from "./db/client.js";
import type { AppDrizzleDb } from "./db/sync-db-bridge.js";
import { createAuthStore, type AuthStore } from "./services/auth-store.js";
import { createBoardStore, type BoardStore } from "./services/board-store.js";
import { validateApiKey } from "./services/api-key-manager.js";
import { migrateLegacySelfHostExports } from "./services/legacy-export-migration.js";
import { prepareSqliteUpgrade } from "./db/upgrade-guard.js";
import {
  EXTERNAL_ACCESS_DEFAULT,
  externalApiAccessEnabled,
  mcpAccessEnabled,
  type ExternalAccessMode,
} from "@print-partner/contracts";
import { InvalidCommunityManifestSlugError } from "./lib/community-manifest-slug.js";
import { sendProblem } from "./lib/api-error.js";
import { hostedDeniedRouteDetail } from "./lib/hosted-planning-deny.js";
import { hostedPlanningPolicy } from "./lib/hosted-planning.js";
import { setPrivateOutboundDenied } from "./lib/outbound-url.js";
import { readExternalAccessSettings } from "./services/external-access.js";
import { listActivePrinterSendQueue } from "./services/printer-send-queue-store.js";
import { sweepExpiredTransferArtifacts } from "./services/transfer-artifact-retention.js";
import { migrateLegacySourceManifestOverridesForTenant } from "./services/source-manifest-migration.js";
import { MAX_SOURCE_UPLOAD_FILES, MAX_SOURCE_UPLOAD_PARTS } from "./services/archive-import.js";
import {
  KIT_BUNDLE_UPLOAD_TOO_LARGE_DETAIL,
  MAX_KIT_BUNDLE_UPLOAD_BYTES,
  MAX_JSON_BODY_BYTES,
  MAX_SOURCE_UPLOAD_BYTES,
} from "./services/upload-limits.js";
import { getLogger } from "./services/logger.js";
import type { ManagedProfileSyncHandle } from "./services/profile-sync-manager.js";
import {
  ISOLATED_SOURCE_FILESYSTEM,
  TRUSTED_SINGLE_USER_SOURCE_FILESYSTEM,
} from "./services/source-filesystem-policy.js";

type RuntimePorts = AppPorts & {
  repository?: AppRepository;
  reposDir?: string;
  sourcesDir?: string;
  getRepository?: (tenantId: string) => AppRepository;
  db: AppPorts["db"] & {
    bundle?: unknown;
    defaultRepository?: AppRepository | null;
  };
};

export function createPorts(config: ServerConfig): RuntimePorts {
  if (config.deployMode === "saas") {
    return createSaasPorts(config.dataDir) as RuntimePorts;
  }
  return createSelfHostPorts(
    config.dataDir,
    config.multiUser ? ISOLATED_SOURCE_FILESYSTEM : TRUSTED_SINGLE_USER_SOURCE_FILESYSTEM,
    mappedDesktopTenant() ? "existing-current-schema" : "migrate",
  );
}

function resolveRepository(ports: RuntimePorts): AppRepository | null {
  if (ports.repository) return ports.repository;
  if (ports.db && "defaultRepository" in ports.db) {
    const repo = (ports.db as SaasDbStore).defaultRepository;
    if (repo) return repo;
  }
  if (ports.getRepository) return ports.getRepository("default");
  return null;
}

type StoreDb = Readonly<{ db: AppDrizzleDb; driver: "sqlite" | "postgres" }>;

function resolveStoreDb(ports: RuntimePorts): StoreDb | null {
  const db = ports.db;
  if ("sqlite" in db) {
    const sqlite = (db as SelfHostDbStore).sqlite;
    if (sqlite?.drizzle) return { db: getDb(sqlite), driver: "sqlite" };
  }
  if ("bundle" in db) {
    const bundle = (db as SaasDbStore).bundle;
    if (bundle.postgres?.drizzle) return { db: bundle.postgres.drizzle, driver: "postgres" };
    if (bundle.sqlite?.drizzle) return { db: getDb(bundle.sqlite), driver: "sqlite" };
  }
  return null;
}

function resolveAuthStore(ports: RuntimePorts, config: ServerConfig): AuthStore | null {
  if (!config.multiUser && !config.singleUserAuth) return null;
  const store = resolveStoreDb(ports);
  if (!store) return null;
  return createAuthStore(store.db, store.driver, {
    claimDefaultTenantForFirstUser:
      mappedDesktopTenant() === null && !config.singleUserAuth && config.deployMode !== "saas",
  });
}

function resolveBoardStore(ports: RuntimePorts, config: ServerConfig): BoardStore | null {
  if (!hostedPlanningPolicy(config.deployMode).hostedPlanning) return null;
  const store = resolveStoreDb(ports);
  return store ? createBoardStore(store.db, store.driver) : null;
}

function configuredTenantIds(config: ServerConfig, authStore: AuthStore | null): string[] {
  const mapped = mappedDesktopTenant();
  if (mapped) return mappedDesktopDatabaseAvailable() ? [mapped] : [];
  const tenantIds = config.multiUser ? (authStore?.listTenantIds() ?? []) : ["default"];
  if (config.deployMode === "saas" && config.saasBasicAuth) {
    const [login] = config.saasBasicAuth.split(":");
    tenantIds.push(`basic-${login ?? "basic"}`);
  }
  if (config.deployMode === "saas" && config.saasAllowAnonymous) {
    tenantIds.push("anonymous");
  }
  if (tenantIds.length === 0) tenantIds.push("default");
  return [...new Set(tenantIds)];
}

function activePrinterUploadDirectories(
  repository: AppRepository,
  tenantIds: readonly string[],
): ReadonlySet<string> {
  const directories = new Set<string>();
  for (const tenantId of tenantIds) {
    tenantStorage.run(tenantId, () => {
      for (const item of listActivePrinterSendQueue(repository)) {
        directories.add(resolve(dirname(item.artifact_path)));
      }
    });
  }
  return directories;
}

const ADMIN_ROUTE_PREFIXES = [
  "/admin",
  "/backups",
  "/settings/api-keys",
  "/settings/logging",
  "/slicer-instances",
  "/api/v1/integrations",
  "/api/v1/webhooks",
];

function isAdministrativeRoute(url: string): boolean {
  const path = url.split("?")[0] ?? url;
  return ADMIN_ROUTE_PREFIXES.some((prefix) => path === prefix || path.startsWith(`${prefix}/`));
}

export async function buildApp(config: ServerConfig, ports: RuntimePorts) {
  const app = Fastify({
    logger: desktopContext() ? { redact: ["req", "res", "err"] } : true,
    bodyLimit: MAX_JSON_BODY_BYTES,
    trustProxy: config.trustProxy,
  });
  registerDesktopBoundary(app);
  if (mappedDesktopTenant()) {
    app.addHook("onRequest", async (request, reply) => {
      const path = decodeURIComponent(new URL(request.url, "http://desktop").pathname);
      if (/^\/(?:api\/v[12]\/)?backups(?:\/|$)/.test(path)) {
        return reply.code(403).send({ detail: "Backups require an unmapped runtime" });
      }
    });
  }
  try {
    const tenant = mappedDesktopTenant();
    if (tenant) await tenantStorage.run(tenant, () => configureApp(app, config, ports));
    else await configureApp(app, config, ports);
    return app;
  } catch (error) {
    await app.close();
    throw error;
  }
}

async function configureApp(
  app: FastifyInstance,
  config: ServerConfig,
  ports: RuntimePorts,
): Promise<void> {
  app.addHook("onSend", async (_request, reply, payload) => {
    if (!reply.hasHeader("Cache-Control")) {
      reply.header("Cache-Control", "private, no-store");
    }
    return payload;
  });
  const authStore = resolveAuthStore(ports, config);
  const boardStore = resolveBoardStore(ports, config);
  const repository = resolveRepository(ports);
  // Private UDS-only snapshot for the Rust status watcher. The desktop boundary
  // authenticates the signed principal; the public gateway does not admit this path.
  if (desktopContext() && repository) {
    app.get("/__desktop/moonraker-hosts", async (request, reply) => {
      if (!desktopPrincipal(request))
        return reply.code(403).send({ detail: "Desktop principal required" });
      const raw = repository.getSetting("integrations");
      const hosts = moonrakerHostSnapshot(raw);
      // 4xx: malformed stored settings fail closed immediately. 5xx stays reserved
      // for a snapshot the gateway may keep using through the grace period.
      if (!hosts) return reply.code(400).send({ detail: "Host snapshot invalid" });
      return hosts;
    });
  }
  const planning = hostedPlanningPolicy(config.deployMode);
  setPrivateOutboundDenied(!planning.allowPrivateOutbound);
  if (planning.hostedPlanning) {
    app.addHook("onRequest", async (request, reply) => {
      const detail = hostedDeniedRouteDetail(request.method, request.url);
      if (detail) {
        return sendProblem(reply, 403, "Forbidden", detail);
      }
    });
  }

  // Register request logging middleware early
  await registerRequestLoggingMiddleware(app);

  await app.register(cookie);
  registerTenantMiddleware(app, config, authStore);
  app.addHook("onRequest", async (request) => {
    setRequestTenantId(request.tenantId ?? "default");
  });
  registerAuthRoutes(app, config, authStore);
  const externalAccessMode = (): ExternalAccessMode => {
    if (planning.hostedPlanning) return "off";
    return repository === null
      ? EXTERNAL_ACCESS_DEFAULT
      : readExternalAccessSettings(repository).mode;
  };
  const validateRequestApiKey = registerApiKeyAuth(
    app,
    config,
    (rawKey) => repository !== null && validateApiKey(repository, rawKey) !== null,
    {
      apiKeysEnabled: () => externalApiAccessEnabled(externalAccessMode()),
      mcpEnabled: () => mcpAccessEnabled(externalAccessMode()),
    },
  );

  await app.register(cors, { origin: config.corsOrigin, credentials: true });
  // Media and application traffic have separate bounded budgets per IP.
  await app.register(rateLimit, {
    global: !desktopContext()?.gateway_rate_limit,
    max: 1000,
    timeWindow: "1 minute",
    cache: 10000, // Store limit info for max 10k IPs
    keyGenerator: (request) => {
      const path = request.url.split("?", 1)[0];
      const media = /^(?:\/api\/v[12])?\/parts\/\d+\/(?:mesh|thumbnail)$/.test(path);
      return `${request.ip}:${media ? "media" : "app"}`;
    },
    allowList: (request) => /^(?:\/api\/v[12])?\/health(?:\?|$)/.test(request.url),
    redis: undefined, // Use in-memory store for single-instance deployments
  });
  if (desktopContext()?.gateway_rate_limit) {
    app.addHook("onRoute", (route) => {
      if (route.config?.rateLimit != null) return;
      if (Array.isArray(route.onRequest)) {
        if (!route.onRequest.includes(desktopRateLimit)) route.onRequest.push(desktopRateLimit);
      } else if (route.onRequest) route.onRequest = [route.onRequest, desktopRateLimit];
      else route.onRequest = [desktopRateLimit];
    });
  }
  // Compress SPA assets and API JSON; event streams are excluded so MCP and
  // job streams stay unbuffered. Uncompressed JS alone is ~1 MB on first load.
  await app.register(compress, {
    global: true,
    encodings: ["br", "gzip"],
    customTypes:
      /^application\/(json|javascript|manifest\+json|xml|wasm)|^text\/(?!event-stream)|^image\/svg\+xml|^font\/ttf/,
  });
  await app.register(websocket);
  await app.register(multipart, {
    limits: {
      fileSize: MAX_SOURCE_UPLOAD_BYTES,
      files: MAX_SOURCE_UPLOAD_FILES,
      parts: MAX_SOURCE_UPLOAD_PARTS,
    },
  });

  const requireAdmin = createAdminPreHandler(config, validateRequestApiKey);
  app.addHook("preHandler", async (request, reply) => {
    const mappedPath = mappedDesktopTenant()
      ? decodeURIComponent(new URL(request.url, "http://desktop").pathname)
      : null;
    const mappedAdminRoute =
      mappedPath &&
      (isAdministrativeRoute(mappedPath) ||
        isAdministrativeRoute(mappedPath.replace(/^\/api\/v[12](?=\/)/, "")));
    if (mappedAdminRoute && !request.sessionUser?.is_admin) {
      return reply.code(403).send({ detail: "Administrator access required" });
    }
    if (isAdministrativeRoute(request.url)) {
      return requireAdmin(request, reply);
    }
  });

  if (config.staticDir && existsSync(config.staticDir)) {
    app.addHook("preHandler", async (request, reply) => {
      if (isSpaClientPath(request.url) && isBrowserDocumentNavigation(request)) {
        return reply.sendFile("index.html", config.staticDir!);
      }
    });
  }

  await registerHealthRoutes(app, config, ports, authStore);
  await registerOpenApi(app, config);

  if (repository) {
    const statusExecution = new StatusExecution();
    app.addHook("preClose", async () => {
      await statusExecution.stop();
    });
    // Rust owns a fenced operation's participants and every route alias; Node runs none of them.
    const rustOwned = rustOwnedNodeParticipants(config.dataDir);
    beginSourceSyncAdmission(config.dataDir, rustOwned);
    // The migration rewrites queue paths, so it moves with the printer send queue.
    if (
      config.deployMode === "self-host" &&
      !mappedDesktopTenant() &&
      !rustOwned.has("migration.migrateLegacySelfHostExports")
    ) {
      migrateLegacySelfHostExports(config.dataDir, repository);
    }
    const backgroundTenantIds = () => configuredTenantIds(config, authStore);
    const migrateLegacySourceManifests = async (): Promise<void> => {
      // The migration is a Source Sync participant; Rust runs it after cutover.
      if (sourceSyncOwnedByRust()) return;
      for (const tenantId of backgroundTenantIds()) {
        const report = await migrateLegacySourceManifestOverridesForTenant(repository, tenantId);
        if (report.migrated.length > 0) {
          app.log.info(
            { tenantId, sourceIds: report.migrated.map((item) => item.sourceId) },
            "Migrated legacy Source manifest overrides",
          );
        }
        for (const migrated of report.migrated) {
          if (!migrated.changedDuringMigration) continue;
          app.log.warn(
            {
              tenantId,
              sourceId: migrated.sourceId,
              backupPath: migrated.backupPath,
            },
            "Legacy Source manifest changed during migration; the changed bytes remain archived",
          );
        }
        for (const retained of report.retained) {
          app.log.warn(
            {
              tenantId,
              sourceId: retained.sourceId,
              legacyPath: retained.legacyPath,
              reason: retained.reason,
            },
            "Retained legacy Source manifest override",
          );
        }
      }
    };
    if (!mappedDesktopTenant()) {
      await migrateLegacySourceManifests();
      sweepExpiredTransferArtifacts(join(config.dataDir, "exports"), {
        protectedDirectories: activePrinterUploadDirectories(repository, backgroundTenantIds()),
      });
    }
    const thumbsDir = join(config.dataDir, "thumbs");
    const coversDir = join(config.dataDir, "covers");
    const getRepo = () => repository;
    const jobs =
      (ports.jobs as InProcessJobRunner) ??
      createJobRunner(getRepo, config.dataDir, {
        tenantDiskQuotaBytes: planning.tenantDiskQuotaBytes,
      });

    // Extract SQLite instance for backup/restore
    let sqlite = null;
    if ("sqlite" in ports.db) {
      sqlite = (ports.db as SelfHostDbStore).sqlite ?? null;
    }

    let profileSyncHandle: ManagedProfileSyncHandle | null = null;
    if (
      config.deployMode === "self-host" &&
      !config.multiUser &&
      !rustOwned.has("worker.startManagedProfileSync")
    ) {
      const { startManagedProfileSync } = await import("./services/profile-sync-manager.js");
      const { broadcastProfileSync, registerProfileSyncWebSocket } =
        await import("./services/profile-sync-broadcast.js");
      registerProfileSyncWebSocket(app);
      profileSyncHandle = startManagedProfileSync({
        repository,
        listTenantIds: backgroundTenantIds,
        emit: (tenantId, event) => broadcastProfileSync(tenantId, event),
        prepareTenant: () => repository.seedStockSlicerInstancesIfEmpty(process.env),
      });
      app.addHook("onClose", async () => {
        await profileSyncHandle?.stop();
      });
    }

    const coreDeps = {
      statusExecution,
      repo: repository,
      reposDir: ports.reposDir ?? join(config.dataDir, "repos"),
      sourcesDir: ports.sourcesDir ?? join(config.dataDir, "sources"),
      thumbsDir,
      coversDir,
      dataDir: config.dataDir,
      config,
      jobs,
      authStore,
      boardStore,
      ...(profileSyncHandle
        ? {
            reloadProfileSync: async () => {
              const tenantId = getRequestTenantId();
              try {
                await profileSyncHandle.reloadTenant(tenantId);
              } catch (error) {
                getLogger().log(
                  "warn",
                  `[profile-sync] reload failed for tenant ${tenantId}: ${error instanceof Error ? error.message : String(error)}`,
                );
              }
            },
          }
        : {}),
    };

    await registerCoreRoutes(app, coreDeps, { planSummaryContract: "accepted" });
    await registerPlanDraftRoutes(app, coreDeps);
    await registerAcceptedPlateRoutes(app, {
      repo: repository,
      reposDir: coreDeps.reposDir,
    });

    // Start background source watcher unless Rust owns Source Sync.
    if (!sourceSyncOwnedByRust()) {
      const { startSourceWatcher } = await import("./services/source-watcher.js");
      const watcherHandle = startSourceWatcher({
        repo: repository,
        reposDir: coreDeps.reposDir,
        listTenantIds: backgroundTenantIds,
        tenantDiskQuotaBytes: planning.tenantDiskQuotaBytes,
        getSettings: () => {
          const webhookUrl = repository.getSetting("discord_notify_webhook_url") || null;
          const notifyOnUpdate = repository.getSetting("discord_notify_on_update", "1") !== "0";
          const notifyOnSync = repository.getSetting("discord_notify_on_sync", "0") !== "0";
          const autoSyncUpdates = repository.getSetting("discord_auto_sync_updates", "1") !== "0";
          return { discordWebhookUrl: webhookUrl, notifyOnUpdate, notifyOnSync, autoSyncUpdates };
        },
      });
      app.addHook("onClose", async () => {
        watcherHandle.stop();
      });
    }

    // Register backup routes (available regardless of auth mode)
    if (!mappedDesktopTenant())
      await registerBackupRoutes(app, {
        dataDir: config.dataDir,
        sqlite,
        appVersion: config.version,
        ...(sqlite
          ? {
              refreshDatabaseConsumers: () => {
                const restoredDb = getDb(sqlite);
                repository.replaceDatabase(restoredDb);
                authStore?.replaceDatabase(restoredDb);
                boardStore?.replaceDatabase(restoredDb);
              },
              afterDatabaseRefresh: migrateLegacySourceManifests,
            }
          : {}),
      });

    // Register logging routes
    await registerLoggingRoutes(app);

    // Register API key management routes
    await registerApiKeyManagementRoutes(app, { repo: repository });

    // Register metrics endpoint
    await registerMetricsRoutes(app, {
      repo: repository,
      validateApiKey: validateRequestApiKey,
      authRequired: config.authRequired,
      version: config.version,
    });

    await app.register(
      async (v1) => {
        await registerApiV1Plugin(v1, coreDeps, validateRequestApiKey);
      },
      { prefix: "/api/v1" },
    );
    await app.register(
      async (v2) => {
        await registerApiV2PlanPlugin(v2, coreDeps);
      },
      { prefix: "/api/v2" },
    );

    app.post(
      "/admin/import-kit-bundle",
      { config: { rateLimit: { max: 10, timeWindow: "1 minute" } } },
      async (request, reply) => {
        if (config.deployMode === "saas" && config.authRequired && !request.sessionUser) {
          return reply.status(401).send({ detail: "Authentication required" });
        }
        const body = request.body as { path?: unknown; new_name?: unknown };
        const { DataDirFileTooLargeError, readBufferUnderDataDir, trimmedString } =
          await import("./lib/secure-path.js");
        const path = trimmedString(body.path);
        if (!path) return reply.status(400).send({ detail: "path is required" });
        const { KIT_JSON_TOO_LARGE_DETAIL, KitJsonTooLargeError, parseKitBundleBuffer } =
          await import("./services/export-kit.js");
        try {
          const buf = readBufferUnderDataDir(config.dataDir, path, MAX_KIT_BUNDLE_UPLOAD_BYTES);
          const data = parseKitBundleBuffer(buf, path);
          return repository.importKitBundle(data, trimmedString(body.new_name) || null);
        } catch (error) {
          if (error instanceof DataDirFileTooLargeError) {
            return reply.status(413).send({ detail: KIT_BUNDLE_UPLOAD_TOO_LARGE_DETAIL });
          }
          if (error instanceof KitJsonTooLargeError) {
            return reply.status(413).send({ detail: KIT_JSON_TOO_LARGE_DETAIL });
          }
          if (error instanceof InvalidCommunityManifestSlugError) {
            return reply.status(400).send({ detail: error.message });
          }
          throw error;
        }
      },
    );

    if (config.staticDir && existsSync(config.staticDir)) {
      await app.register(fastifyStatic, {
        root: config.staticDir,
        prefix: "/",
        wildcard: false,
        // Vite content-hashes all asset filenames (index-AbCdEf.js) so they
        // are safe to cache forever. index.html must NOT be cached (no hash).
        setHeaders: (reply, filePath) => {
          // Vite hashes look like index-BGIfkiEk.js — a base64ish suffix after
          // a dash, not dot-separated hex.
          if (/\/assets\/[^/]+-[\w-]{8,}\.(js|css|woff2?|png|svg|webp)$/i.test(filePath)) {
            void reply.header("Cache-Control", "public, max-age=31536000, immutable");
          } else {
            void reply.header("Cache-Control", "no-cache");
          }
        },
      });
      app.setNotFoundHandler((request, reply) => {
        if (
          request.method === "GET" &&
          !request.url.includes(".") &&
          isBrowserDocumentNavigation(request)
        ) {
          return reply.sendFile("index.html", config.staticDir!);
        }
        return reply.status(404).send({ detail: "Not found" });
      });
    }

    registerJobWebSocket(app, jobs);
  } else {
    registerJobWebSocket(
      app,
      createJobRunner(() => {
        throw new Error("Database not available");
      }, config.dataDir),
    );
  }

  registerOpenApiJsonRoutes(app);
}

export async function startServer(config: ServerConfig) {
  const releaseOwner = acquireDataDirectory(config.dataDir);
  try {
    beginSourceSyncAdmission(config.dataDir, rustOwnedNodeParticipants(config.dataDir));
    validateProductionConfig(config, { desktop: desktopContext() !== null });
    const ownership = rustOwnedCopy(config.dataDir);
    if (!mappedDesktopTenant() && !config.databaseUrl && ownership.kind !== "foreign") {
      const preparation = await prepareSqliteUpgrade({
        dataDir: config.dataDir,
        appVersion: config.version,
      });
      if (preparation.kind === "backup-created") {
        console.info(
          `[upgrade] protected schema ${preparation.fromVersion} at ${preparation.backupPath} before starting schema ${preparation.toVersion}`,
        );
      }
    }
    const ports = createPorts(config);
    const closeDatabase = ports.db.close.bind(ports.db);
    ports.db.close = async () => {
      await closeDatabase();
      releaseOwner();
    };
    let app: FastifyInstance | null = null;
    try {
      await ports.db.connect();

      // One-time migration: move print_outcomes blob → print_job_parts SQL rows.
      if (ports.repository && !mappedDesktopTenant()) {
        try {
          const { migratePrintOutcomesBlob } = await import("./services/printer-outcomes-store.js");
          migratePrintOutcomesBlob(ports.repository);
        } catch (err) {
          console.warn("[print-outcomes] blob migration skipped:", err);
        }
      }

      // Best-effort: upsert Advisor notes from shipped/imported domain pack onto matching sources.
      if (ports.repository && !mappedDesktopTenant()) {
        try {
          const { backfillAdvisorNotesFromDomainPack } = await import("./assistant/domain-pack.js");
          const result = backfillAdvisorNotesFromDomainPack(ports.repository, config.dataDir);
          if (result.notes_upserted > 0) {
            console.info(
              `[assistant-domain] backfilled ${result.notes_upserted} advisor note(s) across ${result.sources_matched} source(s)`,
            );
          }
        } catch (err) {
          console.warn("[assistant-domain] note backfill skipped:", err);
        }
      }

      app = await buildApp(config, ports);
      const desktop = desktopContext();
      if (desktop && process.platform === "win32") {
        await serveDesktopPipe(app, desktop.socket_path);
      } else {
        await app.listen(
          desktop ? { path: desktop.socket_path } : { host: config.host, port: config.port },
        );
      }
      return { app, ports };
    } catch (err) {
      await app?.close();
      await ports.db.close();
      throw err;
    }
  } catch (error) {
    releaseOwner();
    throw error;
  }
}
