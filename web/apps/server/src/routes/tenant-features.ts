import type { FastifyInstance, FastifyReply } from "fastify";
import { featurePatchSchema, type FeatureSettings } from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import {
  desktopFeaturesEnabled,
  featureKeys,
  parseFeatureLayer,
  resolveFeatures,
} from "../lib/feature-toggles.js";

function invalidStorage(reply: FastifyReply) {
  return reply.status(500).send({
    detail: "Stored feature toggles are invalid.",
    code: "FEATURE_TOGGLES_INVALID",
  });
}

export async function registerTenantFeatureRoutes(
  app: FastifyInstance,
  repo: AppRepository,
): Promise<void> {
  app.get("/settings/features", async (_request, reply) => {
    if (!desktopFeaturesEnabled()) return reply.status(404).send({ detail: "Not found" });
    const user = parseFeatureLayer(repo.getSettingSnapshot("feature_toggles"));
    if (!user) return invalidStorage(reply);
    return {
      choices: user.choices,
      effective: resolveFeatures(user.choices, {}),
    } satisfies FeatureSettings;
  });

  app.patch("/settings/features", async (request, reply) => {
    if (!desktopFeaturesEnabled()) return reply.status(404).send({ detail: "Not found" });
    const patch = featurePatchSchema.safeParse(request.body);
    if (!patch.success) return reply.status(400).send({ detail: "Invalid feature settings" });
    for (let attempt = 0; attempt < 5; attempt += 1) {
      const snapshot = repo.getSettingSnapshot("feature_toggles");
      const user = parseFeatureLayer(snapshot);
      if (!user) return invalidStorage(reply);
      const raw = { ...user.raw };
      const choices = { ...user.choices };
      for (const key of featureKeys) {
        const value = patch.data[key];
        if (value === null) {
          delete raw[key];
          delete choices[key];
        } else if (value !== undefined) {
          raw[key] = value;
          choices[key] = value;
        }
      }
      if (
        repo.compareAndSetSetting({
          key: "feature_toggles",
          expected: snapshot,
          value: JSON.stringify(raw),
        })
      ) {
        return { choices, effective: resolveFeatures(choices, {}) } satisfies FeatureSettings;
      }
    }
    return reply.status(409).send({ detail: "Feature settings changed; reload and retry" });
  });
}
