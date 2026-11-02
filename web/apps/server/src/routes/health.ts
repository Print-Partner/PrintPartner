import { mappedDesktopTenant } from "../desktop-context.js";
import type { FastifyInstance } from "fastify";
import {
  DISCORD_OAUTH_CAPABILITY,
  GITHUB_OAUTH_CAPABILITY,
  HOSTED_PLANNING_CAPABILITY,
  INVITE_BOARD_CAPABILITY,
  TENANT_FEATURE_TOGGLES_CAPABILITY,
} from "@print-partner/contracts";
import type { ServerConfig } from "../config.js";
import type { AppPorts } from "../ports/index.js";
import { pingBundle } from "../db/database.js";
import type { SaasDbStore } from "../adapters/saas/index.js";
import { deploymentCapability } from "../lib/deployment-capability.js";
import { hostedPlanningPolicy } from "../lib/hosted-planning.js";
import { getVersionInfo, getBuildSemver } from "../lib/version.js";
import type { AuthStore } from "../services/auth-store.js";
import { desktopFeaturesEnabled } from "../lib/feature-toggles.js";

export async function registerHealthRoutes(
  app: FastifyInstance,
  config: ServerConfig,
  ports: AppPorts,
  authStore: AuthStore | null = null,
): Promise<void> {
  const mapped = mappedDesktopTenant() !== null;
  app.get("/health", async (request) => {
    let dbOk = false;
    let postgresOk: boolean | null = null;

    try {
      await ports.db.ping();
      dbOk = true;
    } catch {
      /* dbOk stays false */
    }

    const saasDb = ports.db as Partial<SaasDbStore>;
    if (saasDb.bundle) {
      try {
        const status = await pingBundle(saasDb.bundle);
        dbOk = status.app;
        postgresOk = status.postgres;
      } catch {
        /* ignore */
      }
    }

    return {
      ok: dbOk,
      version: config.version,
      semver: getBuildSemver(config.releaseIdentity),
      build: getVersionInfo(config.releaseIdentity),
      release: config.releaseIdentity,
      deploy_mode: config.deployMode,
      multi_user: config.multiUser,
      authenticated: request.sessionUser !== null,
      authentication_required: config.authRequired,
      registration_open:
        !mapped &&
        config.registrationOpen &&
        (config.multiUser || (config.singleUserAuth && (authStore?.countUsers() ?? 0) === 0)),
      data_dir: config.dataDir,
      port: config.port,
      api_version: "v1",
      capabilities: healthCapabilities(config, mapped),
      db: {
        connected: dbOk,
        driver: saasDb.bundle?.driver ?? "sqlite",
        postgres: postgresOk,
        support_status: saasDb.bundle?.driver === "postgres" ? "experimental" : "supported",
      },
      deployment: deploymentCapability({
        databaseDriver: saasDb.bundle?.driver === "postgres" ? "postgres" : "sqlite",
        multiUser: config.multiUser,
      }),
      google_drive: {
        client_id: config.googleClientId,
      },
    };
  });
}

function healthCapabilities(config: ServerConfig, mapped: boolean): string[] {
  const hosted = hostedPlanningPolicy(config.deployMode).hostedPlanning;
  return [
    "kit_planning",
    "accepted_plate_revisions",
    "accepted_plate_export",
    "jobs_ws",
    "fleet_presets",
    "integrations_api",
    ...(config.multiUser
      ? [
          ...(!mapped ? ["multi_user_auth"] : []),
          ...(hosted ? [INVITE_BOARD_CAPABILITY] : ["plan_sharing"]),
        ]
      : []),
    ...(config.singleUserAuth && !mapped ? ["single_user_auth"] : []),
    ...(config.smtpConfigured && !mapped ? ["password_reset_email"] : []),
    ...(hosted
      ? [HOSTED_PLANNING_CAPABILITY]
      : ["mcp_http", ...(!mapped ? ["backups"] : []), "api_key_management", "webhook_security"]),
    ...(config.githubOAuthConfigured && !mapped ? [GITHUB_OAUTH_CAPABILITY] : []),
    ...(config.discordOAuthConfigured && !mapped ? [DISCORD_OAUTH_CAPABILITY] : []),
    ...(config.googleClientId ? ["google_drive_manifest"] : []),
    "logging",
    ...(desktopFeaturesEnabled() ? [TENANT_FEATURE_TOGGLES_CAPABILITY] : []),
  ];
}
