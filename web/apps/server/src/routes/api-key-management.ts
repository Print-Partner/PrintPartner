import type { FastifyInstance } from "fastify";
import type { AppRepository } from "../db/repository.js";
import { sendProblem } from "../lib/api-error.js";
import {
  createApiKey,
  listApiKeys,
  revokeApiKey,
  regenerateApiKey,
  type ApiKeyInfo,
} from "../services/api-key-manager.js";

type RouteDeps = { repo: AppRepository };

export async function registerApiKeyManagementRoutes(
  app: FastifyInstance,
  deps: RouteDeps,
): Promise<void> {
  /**
   * GET /settings/api-keys
   * List all API keys (without showing the actual keys).
   */
  app.get("/settings/api-keys", async (_request, reply) => {
    const keys = listApiKeys(deps.repo);
    reply.send({
      keys,
      total: keys.length,
    });
  });

  /**
   * POST /settings/api-keys
   * Create a new API key (plaintext key shown only once).
   */
  app.post<{ Reply: ApiKeyInfo | { detail: string } }>(
    "/settings/api-keys",
    { config: { rateLimit: { max: 10, timeWindow: "1 minute" } } },
    async (request, reply) => {
      try {
        const keyInfo = createApiKey(deps.repo);
        reply.status(201).send(keyInfo);
      } catch (error) {
        request.log.error({ err: error }, "Failed to create API key");
        return sendProblem(reply, 500, "Internal Server Error", "Failed to create API key");
      }
    },
  );

  /**
   * POST /settings/api-keys/:id/regenerate
   * Regenerate (rotate) an existing API key.
   * The old key is marked as inactive; a new one is returned.
   */
  app.post<{
    Params: { id: string };
    Reply: ApiKeyInfo | { detail: string };
  }>("/settings/api-keys/:id/regenerate", async (request, reply) => {
    try {
      const id = (request.params as { id: string }).id;
      const keyInfo = regenerateApiKey(deps.repo, id);

      if (!keyInfo) {
        return sendProblem(reply, 404, "Not Found", "API key not found");
      }

      reply.status(201).send(keyInfo);
    } catch (error) {
      request.log.error({ err: error }, "Failed to regenerate API key");
      return sendProblem(reply, 500, "Internal Server Error", "Failed to regenerate API key");
    }
  });

  /**
   * DELETE /settings/api-keys/:id
   * Revoke an API key (makes it inactive).
   */
  app.delete<{
    Params: { id: string };
    Reply: { success: boolean } | { detail: string };
  }>("/settings/api-keys/:id", async (request, reply) => {
    try {
      const id = (request.params as { id: string }).id;
      const success = revokeApiKey(deps.repo, id);

      if (!success) {
        return sendProblem(reply, 404, "Not Found", "API key not found");
      }

      reply.send({ success });
    } catch (error) {
      request.log.error({ err: error }, "Failed to revoke API key");
      return sendProblem(reply, 500, "Internal Server Error", "Failed to revoke API key");
    }
  });
}
