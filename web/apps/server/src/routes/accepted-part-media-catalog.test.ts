import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { buildApp } from "../app.js";
import { loadConfig } from "../config.js";
import { acceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { acceptedMediaBasis, writeAcceptedMediaPng } from "../lib/accepted-media-cache.js";
import { PLACEHOLDER_PNG } from "../lib/thumbnails.js";
import { acceptedPartMediaIdentity } from "../services/accepted-part-media.js";
import { parseRequiredUnitToken } from "../services/required-units.js";
import {
  loadRoleFilamentDefaults,
  saveRoleFilamentDefault,
} from "../services/role-filament-store.js";
import { acceptPlanForTest } from "../test/accept-plan.js";

describe("catalog color corrections for existing accepted Parts", () => {
  it.each([
    { colorId: "asa::black", customHex: null, expectedHex: "#000000", oldHex: "#bfb1a3" },
    { colorId: "asa::super-grey", customHex: null, expectedHex: "#747874", oldHex: "#baa895" },
    { colorId: "asa::black", customHex: "#123456", expectedHex: "#123456", oldHex: "#bfb1a3" },
  ])(
    "revalidates $colorId / $customHex without changing saved work",
    async ({ colorId, customHex, expectedHex, oldHex }) => {
      const root = mkdtempSync(join(tmpdir(), "pp-catalog-media-"));
      const ports = createSelfHostPorts(root);
      await ports.db.connect();
      const repo = ports.repository;
      const source = repo.createSource({ name: "Color proof", url: "https://github.com/a/b" });
      const observed = repo.getProjectRow(source.id);
      if (!observed) throw new Error("test Source is missing");
      const locator = `${source.id}/revisions/color-proof`;
      const sourceRoot = join(root, "repos", locator);
      mkdirSync(sourceRoot, { recursive: true });
      writeFileSync(join(sourceRoot, "bracket_x2.stl"), "solid color proof");
      const revision = repo.recordSourceRevision({
        sourceId: source.id,
        upstreamRevisionKey: "color-proof",
        manifestDigest: "a".repeat(64),
        snapshotLocator: locator,
        syncedAt: "2026-09-04T12:00:00.000Z",
        completeness: "complete",
      });
      repo.activateSourceRevision({
        sourceId: source.id,
        revisionId: revision.id,
        observed,
        sourceVersion: "color-proof",
      });
      const profile = repo.createProfile("Saved color proof", source.id);
      saveRoleFilamentDefault(repo, profile.id, "primary", {
        filament_color_id: colorId,
        filament_custom_hex: customHex,
        spoolman_spool_id: "spoolman:test:spool:3",
      });
      expect(acceptPlanForTest(repo, profile.id).merged).toBe(true);
      const accepted = repo.readAcceptedPlanOperationalSnapshot(profile.id);
      if (accepted.kind !== "ready") throw new Error("test Plan is not ready");
      const unit = accepted.snapshot.parts[0]?.units[0];
      if (!unit) throw new Error("test Required unit is missing");
      expect(
        repo.setAcceptedUnitCompletion({
          expected: acceptedPlanBasis(accepted.snapshot),
          token: parseRequiredUnitToken(unit.token),
          completed: true,
        }).kind,
      ).toBe("updated");
      const before = repo.readAcceptedPlanOperationalSnapshot(profile.id);
      if (before.kind !== "ready") throw new Error("test Plan is not ready");
      const part = before.snapshot.parts[0];
      if (!part || part.artifact.kind !== "tracked") throw new Error("test artifact is missing");
      const defaultsBefore = loadRoleFilamentDefaults(repo, profile.id);
      const app = await buildApp({ ...loadConfig(), dataDir: root }, ports);

      try {
        for (const variant of ["mesh", "thumbnail", "preview"] as const) {
          const oldBasis = acceptedMediaBasis({
            expectedSha256: part.artifact.expectedSha256,
            role: part.effectiveRole,
            hex: oldHex,
            variant,
          });
          if (variant !== "mesh") {
            writeAcceptedMediaPng({
              thumbsDir: join(root, "thumbs"),
              basis: oldBasis,
              png: PLACEHOLDER_PNG,
            });
          }
          const corrected = acceptedPartMediaIdentity(part, variant);
          const response = await app.inject({
            method: "GET",
            url: `/parts/${part.projectionPartId}/${variant}`,
            headers: { "if-none-match": `"${oldBasis}"` },
          });
          expect(corrected.hex).toBe(expectedHex);
          expect(corrected.basis).not.toBe(oldBasis);
          expect(response.statusCode).toBe(200);
          expect(response.headers["x-accepted-render-hex"]).toBe(expectedHex);
          if (variant === "mesh") {
            expect(response.headers.etag).toBe(`"${corrected.basis}"`);
          } else {
            expect(response.headers["x-thumbnail-placeholder"]).toBe("1");
            expect(response.headers["cache-control"]).toBe("no-store");
          }
        }

        expect(repo.readAcceptedPlanOperationalSnapshot(profile.id)).toEqual(before);
        expect(loadRoleFilamentDefaults(repo, profile.id)).toEqual(defaultsBefore);
      } finally {
        await app.close();
        rmSync(root, { recursive: true, force: true });
      }
    },
  );
});
