import { desktopContext, desktopPrincipal, mappedDesktopTenant } from "../desktop-context.js";
import type { FastifyInstance, FastifyReply, FastifyRequest } from "fastify";
import { randomBytes } from "node:crypto";
import type { ServerConfig } from "../config.js";
import { isBrowserDocumentNavigation, isSpaClientPath, isStaticAssetPath } from "../lib/spa-nav.js";
import { isMcpTransportRequest } from "../lib/mcp-transport-path.js";
import { hashPassword, validatePasswordStrength, verifyPassword } from "../services/password.js";
import type { AuthStore } from "../services/auth-store.js";
import { cancelResponseBody, readBoundedResponseBody } from "../lib/bounded-response.js";
import {
  buildPasswordResetUrl,
  deliverPasswordResetEmail,
  passwordResetPublicOrigin,
} from "../services/password-reset-mail.js";
import { toPublicUser, type SessionUser } from "./auth-types.js";
import type { AuthIdentityProvider } from "./auth-types.js";
import {
  defaultDisplayName,
  isValidEmailInput,
  normalizeEmailInput,
  parseDiscordOAuthProfile,
  parseGitHubOAuthProfile,
  parseOAuthAccessToken,
  readOAuthError,
  selectVerifiedGitHubEmail,
} from "./auth-route-model.js";

declare module "fastify" {
  interface FastifyRequest {
    tenantId: string;
    sessionUser: SessionUser | null;
  }
}

const authRateLimit = { config: { rateLimit: { max: 20, timeWindow: "1 minute" } } };
const MAX_OAUTH_RESPONSE_BYTES = 256 * 1024;
const OAUTH_PROVIDER_TIMEOUT_MS = 15_000;

class OAuthProviderRequestError extends Error {
  constructor(
    readonly kind: "timeout" | "network",
    cause: unknown,
  ) {
    super(kind === "timeout" ? "OAuth provider timed out" : "OAuth provider request failed", {
      cause,
    });
    this.name = "OAuthProviderRequestError";
  }
}

async function fetchOAuthProvider(input: string, init: RequestInit = {}): Promise<Response> {
  try {
    return await fetch(input, {
      ...init,
      signal: AbortSignal.timeout(OAUTH_PROVIDER_TIMEOUT_MS),
    });
  } catch (error) {
    const errorName = error instanceof Error ? error.name : "";
    throw new OAuthProviderRequestError(
      errorName === "TimeoutError" || errorName === "AbortError" ? "timeout" : "network",
      error,
    );
  }
}

function replyToOAuthProviderFailure(
  provider: "GitHub" | "Discord",
  error: unknown,
  request: FastifyRequest,
  reply: FastifyReply,
): FastifyReply {
  if (!(error instanceof OAuthProviderRequestError)) throw error;
  request.log.warn(
    { err: error.cause, provider, failure: error.kind },
    "OAuth provider request failed",
  );
  const timedOut = error.kind === "timeout";
  return reply.status(timedOut ? 504 : 502).send({
    detail: timedOut
      ? `${provider} OAuth provider timed out`
      : `${provider} OAuth provider unavailable`,
  });
}

function setSessionCookie(reply: FastifyReply, rawToken: string, secure: boolean): void {
  reply.setCookie("pp_session", rawToken, {
    httpOnly: true,
    path: "/",
    sameSite: "lax",
    secure,
    maxAge: 60 * 60 * 24 * 14,
  });
}

function finishOAuthLogin(
  authStore: AuthStore,
  user: ReturnType<AuthStore["upsertOAuthUser"]>,
  provider: AuthIdentityProvider,
  reply: FastifyReply,
  redirectUrl: string,
  secure: boolean,
): void {
  const raw = authStore.createSession(user.id, provider);
  setSessionCookie(reply, raw, secure);
  reply.clearCookie("oauth_state", { path: "/" });
  reply.redirect(redirectUrl);
}

function oauthRegistrationAllowed(
  config: ServerConfig,
  authStore: AuthStore,
  provider: AuthIdentityProvider,
  providerUserId: string,
): boolean {
  if (authStore.findIdentity(provider, providerUserId)) return true;
  return config.registrationOpen;
}

function singleUserOAuthCanContinue(
  config: ServerConfig,
  authStore: AuthStore,
  provider: AuthIdentityProvider,
  providerUserId: string,
): boolean {
  if (!config.singleUserAuth || authStore.countUsers() === 0) return true;
  return Boolean(authStore.findIdentity(provider, providerUserId));
}

async function readJsonResponse(response: Response): Promise<unknown> {
  try {
    const bytes = await readBoundedResponseBody(response, MAX_OAUTH_RESPONSE_BYTES);
    return JSON.parse(new TextDecoder().decode(bytes)) as unknown;
  } catch {
    return null;
  }
}

export function registerAuthRoutes(
  app: FastifyInstance,
  config: ServerConfig,
  authStore: AuthStore | null,
): void {
  app.get("/auth/me", async (request, reply) => {
    if (!request.sessionUser) {
      if (!config.multiUser && !config.singleUserAuth && config.deployMode === "self-host") {
        return {
          user: {
            user_id: "local",
            login: "local",
            display_name: "Local",
            email: null,
            provider: "anonymous",
            is_admin: true,
          },
          multi_user: false,
        };
      }
      return reply.status(401).send({ detail: "Not authenticated" });
    }
    return { user: toPublicUser(request.sessionUser), multi_user: config.multiUser };
  });

  if (mappedDesktopTenant()) return;

  app.post("/auth/logout", async (request, reply) => {
    const cookie = request.cookies?.pp_session;
    if (cookie && authStore) authStore.deleteSession(cookie);
    reply.clearCookie("pp_session", { path: "/" });
    return { ok: true };
  });

  if ((config.multiUser || config.singleUserAuth) && authStore) {
    app.post("/auth/register", authRateLimit, async (request, reply) => {
      if (!config.registrationOpen) {
        return reply.status(403).send({ detail: "Registration is closed" });
      }
      if (config.singleUserAuth && authStore.countUsers() > 0) {
        return reply.status(403).send({ detail: "The single-user administrator already exists" });
      }
      const body = request.body as { email?: unknown; password?: unknown; display_name?: unknown };
      const emailRaw = normalizeEmailInput(body.email);
      const password = typeof body.password === "string" ? body.password : "";
      const displayName = defaultDisplayName({
        displayName: body.display_name,
        email: emailRaw,
      });
      if (!isValidEmailInput(emailRaw)) {
        return reply.status(400).send({ detail: "Valid email is required" });
      }
      const pwErr = validatePasswordStrength(password);
      if (pwErr) return reply.status(400).send({ detail: pwErr });
      if (authStore.findUserByEmail(emailRaw)) {
        return reply.status(409).send({ detail: "Email already registered" });
      }
      const passwordHash = hashPassword(password);
      const user = authStore.createUser({ email: emailRaw, displayName, passwordHash });
      const raw = authStore.createSession(user.id, "email");
      setSessionCookie(reply, raw, config.sessionCookieSecure);
      const sessionUser: SessionUser = {
        user_id: user.id,
        tenant_id: user.id,
        login: emailRaw,
        display_name: user.displayName,
        email: emailRaw,
        provider: "email",
        hasPassword: true,
        is_admin: user.isAdmin,
      };
      return { user: toPublicUser(sessionUser) };
    });

    app.post("/auth/login", authRateLimit, async (request, reply) => {
      const body = request.body as { email?: unknown; password?: unknown };
      const emailRaw = normalizeEmailInput(body.email);
      const password = typeof body.password === "string" ? body.password : "";
      if (!emailRaw || !password) {
        return reply.status(400).send({ detail: "Email and password are required" });
      }
      const user = authStore.findUserByEmail(emailRaw);
      if (!user?.passwordHash || !verifyPassword(password, user.passwordHash)) {
        return reply.status(401).send({ detail: "Invalid email or password" });
      }
      const raw = authStore.createSession(user.id, "email");
      setSessionCookie(reply, raw, config.sessionCookieSecure);
      return {
        user: toPublicUser({
          user_id: user.id,
          tenant_id: user.id,
          login: emailRaw,
          display_name: user.displayName,
          email: emailRaw,
          provider: "email",
          hasPassword: true,
          is_admin: user.isAdmin,
        }),
      };
    });

    app.post("/auth/forgot-password", authRateLimit, async (request, reply) => {
      const body = request.body as { email?: unknown };
      const emailRaw = normalizeEmailInput(body.email);
      if (!isValidEmailInput(emailRaw)) {
        return reply.status(400).send({ detail: "Valid email is required" });
      }

      const user = authStore.findUserByEmail(emailRaw);
      let devResetUrl: string | undefined;
      if (user?.email) {
        const origin = passwordResetPublicOrigin(config, request);
        if (origin) {
          authStore.invalidatePasswordResetTokens(user.id);
          const raw = authStore.createPasswordResetToken(user.id);
          const resetUrl = buildPasswordResetUrl(origin, raw);
          const delivery = await deliverPasswordResetEmail(config, request.log, {
            to: user.email,
            resetUrl,
          });
          if (delivery.errorClass) {
            request.log.warn(
              `Password reset mail delivery failed; provider=smtp error_class=${delivery.errorClass}`,
            );
          } else {
            devResetUrl = delivery.devResetUrl;
          }
        } else {
          request.log.error(
            "Password reset email is unavailable because APP_PUBLIC_URL is not configured",
          );
        }
      }

      const response: { ok: true; message: string; dev_reset_url?: string } = {
        ok: true,
        message: "If an account exists for that email, a reset link was sent.",
      };
      if (devResetUrl) response.dev_reset_url = devResetUrl;
      return response;
    });

    app.post("/auth/reset-password", authRateLimit, async (request, reply) => {
      const body = request.body as { token?: unknown; password?: unknown };
      const token = typeof body.token === "string" ? body.token.trim() : "";
      const password = typeof body.password === "string" ? body.password : "";
      if (!token) return reply.status(400).send({ detail: "Reset token is required" });
      const pwErr = validatePasswordStrength(password);
      if (pwErr) return reply.status(400).send({ detail: pwErr });

      const userId = authStore.consumePasswordResetToken(token);
      if (!userId) {
        return reply.status(400).send({ detail: "Invalid or expired reset link" });
      }

      authStore.updatePasswordHash(userId, hashPassword(password));
      authStore.deleteAllUserSessions(userId);

      const user = authStore.findUserById(userId);
      if (!user) return reply.status(500).send({ detail: "User not found" });

      const raw = authStore.createSession(user.id, "email");
      setSessionCookie(reply, raw, config.sessionCookieSecure);
      return {
        ok: true,
        user: toPublicUser({
          user_id: user.id,
          tenant_id: user.id,
          login: user.email ?? user.displayName,
          display_name: user.displayName,
          email: user.email,
          provider: "email",
          hasPassword: true,
          is_admin: user.isAdmin,
        }),
      };
    });

    app.post("/auth/change-password", authRateLimit, async (request, reply) => {
      if (!request.sessionUser) {
        return reply.status(401).send({ detail: "Authentication required" });
      }
      const body = request.body as { current_password?: unknown; new_password?: unknown };
      const currentPassword =
        typeof body.current_password === "string" ? body.current_password : "";
      const newPassword = typeof body.new_password === "string" ? body.new_password : "";
      if (!currentPassword || !newPassword) {
        return reply.status(400).send({ detail: "Current and new passwords are required" });
      }
      const pwErr = validatePasswordStrength(newPassword);
      if (pwErr) return reply.status(400).send({ detail: pwErr });

      const user = authStore.findUserById(request.sessionUser.user_id);
      if (!user?.passwordHash) {
        return reply.status(400).send({ detail: "This account uses OAuth sign-in only" });
      }
      if (!verifyPassword(currentPassword, user.passwordHash)) {
        return reply.status(401).send({
          code: "current_password_rejected",
          detail: "Current password is incorrect",
        });
      }

      authStore.updatePasswordHash(user.id, hashPassword(newPassword));
      authStore.deleteAllUserSessions(user.id);
      const raw = authStore.createSession(user.id, "email");
      setSessionCookie(reply, raw, config.sessionCookieSecure);
      return { ok: true };
    });
  }

  if (config.githubOAuthConfigured && authStore) {
    app.get("/auth/github", async (_request, reply) => {
      const state = randomBytes(16).toString("hex");
      const params = new URLSearchParams({
        client_id: config.githubClientId!,
        redirect_uri: config.githubCallbackUrl!,
        scope: "read:user user:email",
        state,
      });
      reply.setCookie("oauth_state", state, {
        httpOnly: true,
        path: "/",
        secure: config.sessionCookieSecure,
        maxAge: 600,
      });
      return reply.redirect(`https://github.com/login/oauth/authorize?${params}`);
    });

    app.get("/auth/callback", async (request, reply) => {
      const query = request.query as { code?: string; state?: string };
      const stateCookie = request.cookies?.oauth_state;
      if (!query.code || !query.state || query.state !== stateCookie) {
        return reply.status(400).send({ detail: "Invalid OAuth state" });
      }
      try {
        const tokenRes = await fetchOAuthProvider("https://github.com/login/oauth/access_token", {
          method: "POST",
          headers: { Accept: "application/json", "Content-Type": "application/json" },
          body: JSON.stringify({
            client_id: config.githubClientId,
            client_secret: config.githubClientSecret,
            code: query.code,
            redirect_uri: config.githubCallbackUrl,
          }),
        });
        const tokenJson = await readJsonResponse(tokenRes);
        const accessToken = parseOAuthAccessToken(tokenJson);
        if (!tokenRes.ok || !accessToken) {
          return reply.status(401).send({ detail: readOAuthError(tokenJson) ?? "OAuth failed" });
        }
        const userRes = await fetchOAuthProvider("https://api.github.com/user", {
          headers: { Authorization: `Bearer ${accessToken}`, Accept: "application/json" },
        });
        const ghUser = parseGitHubOAuthProfile(await readJsonResponse(userRes));
        if (!userRes.ok || !ghUser) {
          return reply.status(502).send({ detail: "GitHub returned an invalid user profile" });
        }
        const githubIdentityIsLinked = Boolean(
          authStore.findIdentity("github", ghUser.providerUserId),
        );
        let email: string | null = null;
        if (!githubIdentityIsLinked) {
          const emailsRes = await fetchOAuthProvider("https://api.github.com/user/emails", {
            headers: { Authorization: `Bearer ${accessToken}`, Accept: "application/json" },
          });
          if (emailsRes.ok) {
            email = selectVerifiedGitHubEmail(await readJsonResponse(emailsRes));
          } else {
            await cancelResponseBody(emailsRes);
          }
        }
        if (!singleUserOAuthCanContinue(config, authStore, "github", ghUser.providerUserId)) {
          return reply.status(403).send({ detail: "The single-user administrator already exists" });
        }
        if (!oauthRegistrationAllowed(config, authStore, "github", ghUser.providerUserId)) {
          return reply.status(403).send({ detail: "Registration is closed" });
        }
        const user = authStore.upsertOAuthUser({
          provider: "github",
          providerUserId: ghUser.providerUserId,
          displayName: ghUser.displayName,
          email,
        });
        finishOAuthLogin(
          authStore,
          user,
          "github",
          reply,
          config.authSuccessRedirect,
          config.sessionCookieSecure,
        );
      } catch (error) {
        return replyToOAuthProviderFailure("GitHub", error, request, reply);
      }
    });
  } else {
    app.get("/auth/github", async (_request, reply) => {
      return reply.status(501).send({
        detail:
          "GitHub OAuth not configured. Set GITHUB_CLIENT_ID, GITHUB_CLIENT_SECRET, GITHUB_CALLBACK_URL.",
      });
    });
    app.get("/auth/callback", async (_request, reply) => {
      return reply.status(501).send({ detail: "GitHub OAuth not configured" });
    });
  }

  if (config.discordOAuthConfigured && authStore) {
    app.get("/auth/discord", async (_request, reply) => {
      const state = randomBytes(16).toString("hex");
      const params = new URLSearchParams({
        client_id: config.discordClientId!,
        redirect_uri: config.discordCallbackUrl!,
        response_type: "code",
        scope: "identify email",
        state,
      });
      reply.setCookie("oauth_state", state, {
        httpOnly: true,
        path: "/",
        secure: config.sessionCookieSecure,
        maxAge: 600,
      });
      return reply.redirect(`https://discord.com/api/oauth2/authorize?${params}`);
    });

    app.get("/auth/discord/callback", async (request, reply) => {
      const query = request.query as { code?: string; state?: string };
      const stateCookie = request.cookies?.oauth_state;
      if (!query.code || !query.state || query.state !== stateCookie) {
        return reply.status(400).send({ detail: "Invalid OAuth state" });
      }
      try {
        const body = new URLSearchParams({
          client_id: config.discordClientId!,
          client_secret: config.discordClientSecret!,
          grant_type: "authorization_code",
          code: query.code,
          redirect_uri: config.discordCallbackUrl!,
        });
        const tokenRes = await fetchOAuthProvider("https://discord.com/api/oauth2/token", {
          method: "POST",
          headers: { "Content-Type": "application/x-www-form-urlencoded" },
          body,
        });
        const tokenJson = await readJsonResponse(tokenRes);
        const accessToken = parseOAuthAccessToken(tokenJson);
        if (!tokenRes.ok || !accessToken) {
          return reply
            .status(401)
            .send({ detail: readOAuthError(tokenJson) ?? "Discord OAuth failed" });
        }
        const userRes = await fetchOAuthProvider("https://discord.com/api/users/@me", {
          headers: { Authorization: `Bearer ${accessToken}` },
        });
        const dcUser = parseDiscordOAuthProfile(await readJsonResponse(userRes));
        if (!userRes.ok || !dcUser) {
          return reply.status(502).send({ detail: "Discord returned an invalid user profile" });
        }
        if (!singleUserOAuthCanContinue(config, authStore, "discord", dcUser.providerUserId)) {
          return reply.status(403).send({ detail: "The single-user administrator already exists" });
        }
        if (!oauthRegistrationAllowed(config, authStore, "discord", dcUser.providerUserId)) {
          return reply.status(403).send({ detail: "Registration is closed" });
        }
        const user = authStore.upsertOAuthUser({
          provider: "discord",
          providerUserId: dcUser.providerUserId,
          displayName: dcUser.displayName,
          email: dcUser.email,
        });
        finishOAuthLogin(
          authStore,
          user,
          "discord",
          reply,
          config.authSuccessRedirect,
          config.sessionCookieSecure,
        );
      } catch (error) {
        return replyToOAuthProviderFailure("Discord", error, request, reply);
      }
    });
  } else {
    app.get("/auth/discord", async (_request, reply) => {
      return reply.status(501).send({ detail: "Discord OAuth not configured" });
    });
  }

  if (process.env.NODE_ENV !== "production") {
    app.post("/auth/dev-login", async (request, reply) => {
      if (config.deployMode !== "saas" && !config.multiUser && !config.singleUserAuth) {
        return reply.status(404).send({ detail: "Not available" });
      }
      if (!authStore) return reply.status(503).send({ detail: "Auth store unavailable" });
      const body = request.body as { tenant_id?: string; login?: string; email?: string };
      const user = authStore.createUser({
        email: body.email ?? `${body.login ?? "dev"}@dev.local`,
        displayName: body.login ?? "dev",
      });
      const raw = authStore.createSession(user.id, "email");
      setSessionCookie(reply, raw, config.sessionCookieSecure);
      return {
        user: toPublicUser({
          user_id: user.id,
          tenant_id: user.id,
          login: user.email ?? user.displayName,
          display_name: user.displayName,
          email: user.email,
          provider: "anonymous",
          is_admin: user.isAdmin,
        }),
      };
    });
  }
}

function resolveRequestAuth(
  request: FastifyRequest,
  config: ServerConfig,
  authStore: AuthStore | null,
): SessionUser | null {
  if (desktopContext()) return desktopPrincipal(request);
  if (!config.multiUser && !config.singleUserAuth && config.deployMode === "self-host") {
    return {
      user_id: "local",
      tenant_id: "default",
      login: "local",
      display_name: "Local",
      email: null,
      provider: "anonymous",
      is_admin: true,
    };
  }

  const basic = config.saasBasicAuth;
  if (basic && config.deployMode === "saas") {
    const header = request.headers.authorization ?? "";
    const expected = `Basic ${Buffer.from(basic).toString("base64")}`;
    if (header === expected) {
      const [login] = basic.split(":");
      return {
        user_id: `basic-${login}`,
        tenant_id: `basic-${login}`,
        login: login ?? "basic",
        display_name: login ?? "basic",
        email: null,
        provider: "basic",
        is_admin: false,
      };
    }
  }

  const sid = request.cookies?.pp_session;
  if (sid && authStore) {
    const user = authStore.resolveSession(sid);
    if (user) return config.singleUserAuth ? { ...user, tenant_id: "default" } : user;
  }

  if (config.saasAllowAnonymous && config.deployMode === "saas") {
    return {
      user_id: "anonymous",
      tenant_id: "anonymous",
      login: "anonymous",
      display_name: "Anonymous",
      email: null,
      provider: "anonymous",
      is_admin: false,
    };
  }

  return null;
}

export function registerTenantMiddleware(
  app: FastifyInstance,
  config: ServerConfig,
  authStore: AuthStore | null,
): void {
  app.decorateRequest("tenantId", "default");
  app.decorateRequest("sessionUser", null);

  app.addHook("onRequest", async (request, reply) => {
    const path = request.url.split("?")[0] ?? request.url;

    const user = resolveRequestAuth(request, config, authStore);
    request.sessionUser = user;
    request.tenantId = user?.tenant_id ?? "default";

    if (path === "/health") return;
    if (isStaticAssetPath(path)) return;
    if (path.startsWith("/auth/")) return;
    if (isSpaClientPath(path) && isBrowserDocumentNavigation(request)) return;

    if (
      config.deployMode === "self-host" &&
      config.basicAuthUser &&
      config.basicAuthPass &&
      !config.multiUser
    ) {
      const header = request.headers.authorization ?? "";
      const expected = `Basic ${Buffer.from(`${config.basicAuthUser}:${config.basicAuthPass}`).toString("base64")}`;
      if (header !== expected) {
        reply.header("WWW-Authenticate", 'Basic realm="Print Partner"');
        return reply.status(401).send({ detail: "Authentication required" });
      }
    }

    // Single-user self-host exposes MCP to bearer-key clients that have no
    // browser session. Only the exact transport route skips the session gate;
    // the API-key hook and the MCP pre-handler still authorise the request, and
    // multi-user/SaaS deployments keep their session requirement so tenant
    // resolution is never bypassed.
    const mcpTransport =
      config.deployMode === "self-host" &&
      config.singleUserAuth &&
      !config.multiUser &&
      isMcpTransportRequest(request.method, path);

    if (!user && config.authRequired && !mcpTransport) {
      return reply.status(401).send({ detail: "Authentication required" });
    }
  });
}
