import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { afterEach, describe, expect, it, vi } from "vitest";
import { THUMBNAIL_RENDERER_VERSION } from "@print-partner/contracts";

vi.mock("../services/upload-limits.js", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../services/upload-limits.js")>()),
  MAX_THUMBNAIL_UPLOAD_BYTES: 64,
}));
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { buildApp } from "../app.js";
import { loadConfig } from "../config.js";
import { AcceptedPlanOperationalIntegrityError } from "../db/accepted-plan-operational.js";
import { saveRoleFilamentDefault } from "../services/role-filament-store.js";
import {
  acceptedMediaBasis,
  acceptedMediaCachePath,
  readAcceptedMediaPng,
  writeAcceptedMediaPng,
} from "../lib/accepted-media-cache.js";
import { PLACEHOLDER_PNG } from "../lib/thumbnails.js";
import { acceptPlanForTest } from "../test/accept-plan.js";
import { acceptedPartMediaIdentity } from "../services/accepted-part-media.js";
import { acceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { parseRequiredUnitToken } from "../services/required-units.js";

const directories: string[] = [];
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  Buffer.from("accepted rendered thumbnail"),
]);

function multipartPng(boundary: string, bytes: Buffer): Buffer {
  return Buffer.concat([
    Buffer.from(
      `--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="thumbnail.png"\r\nContent-Type: image/png\r\n\r\n`,
    ),
    bytes,
    Buffer.from(`\r\n--${boundary}--\r\n`),
  ]);
}

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe("accepted Part media routes", () => {
  it("ignores legacy thumbnail PNGs and refuses old writers without changing accepted work", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-thumbnail-version-"));
    directories.push(directory);
    const ports = createSelfHostPorts(directory);
    await ports.db.connect();
    const repo = ports.repository;
    const source = repo.createSource({ name: "Renderer proof", url: "https://github.com/a/b" });
    const observed = repo.getProjectRow(source.id);
    if (!observed) throw new Error("test Source is missing");
    const locator = `${source.id}/revisions/renderer-proof`;
    const sourceRoot = join(directory, "repos", locator);
    mkdirSync(sourceRoot, { recursive: true });
    writeFileSync(join(sourceRoot, "bracket_x2.stl"), "solid renderer proof");
    const revision = repo.recordSourceRevision({
      sourceId: source.id,
      upstreamRevisionKey: "renderer-proof",
      manifestDigest: "a".repeat(64),
      snapshotLocator: locator,
      syncedAt: "2026-09-05T00:00:00.000Z",
      completeness: "complete",
    });
    repo.activateSourceRevision({
      sourceId: source.id,
      revisionId: revision.id,
      observed,
      sourceVersion: "renderer-proof",
    });
    const profile = repo.createProfile("Renderer proof", source.id);
    expect(acceptPlanForTest(repo, profile.id).merged).toBe(true);
    const accepted = repo.readAcceptedPlanOperationalSnapshot(profile.id);
    if (accepted.kind !== "ready") throw new Error("test Plan is missing");
    const unit = accepted.snapshot.parts[0]?.units[0];
    if (!unit) throw new Error("test print unit is missing");
    expect(
      repo.setAcceptedUnitCompletion({
        expected: acceptedPlanBasis(accepted.snapshot),
        token: parseRequiredUnitToken(unit.token),
        completed: true,
      }).kind,
    ).toBe("updated");
    const before = repo.readAcceptedPlanOperationalSnapshot(profile.id);
    if (before.kind !== "ready") throw new Error("test Plan is missing");
    const part = before.snapshot.parts[0];
    if (!part || part.artifact.kind !== "tracked") throw new Error("test Part is missing");
    const meshIdentity = acceptedPartMediaIdentity(part, "mesh");
    const thumbnailIdentity = acceptedPartMediaIdentity(part, "thumbnail");
    const artifactDigest = part.artifact.expectedSha256;
    const legacyBasis = (variant: "mesh" | "thumbnail") =>
      createHash("sha256")
        .update(
          [
            "accepted-media-v1",
            variant,
            artifactDigest,
            part.effectiveRole.trim().toLowerCase(),
            meshIdentity.hex ?? "",
          ].join("\0"),
        )
        .digest("hex");
    const oldThumbnailBasis = legacyBasis("thumbnail");
    const thumbsDir = join(directory, "thumbs");
    writeAcceptedMediaPng({ thumbsDir, basis: oldThumbnailBasis, png });
    const app = await buildApp(
      { ...loadConfig(), dataDir: directory, corsOrigin: "https://print.example" },
      ports,
    );
    try {
      const oldCacheProbe = await app.inject({
        method: "GET",
        url: `/parts/${part.projectionPartId}/thumbnail`,
        headers: { "if-none-match": `"${oldThumbnailBasis}"` },
      });
      expect(oldCacheProbe.statusCode).toBe(200);
      expect(oldCacheProbe.headers["x-thumbnail-placeholder"]).toBe("1");
      expect(oldCacheProbe.headers["cache-control"]).toBe("no-store");
      expect(readAcceptedMediaPng({ thumbsDir, basis: oldThumbnailBasis })).toEqual(png);
      const mesh = await app.inject({ method: "GET", url: `/parts/${part.projectionPartId}/mesh` });
      expect(mesh.headers.etag).toBe(`"${legacyBasis("mesh")}"`);

      for (const version of [undefined, "1"]) {
        const boundary = `old-renderer-${version}`;
        const response = await app.inject({
          method: "POST",
          url: `/parts/${part.projectionPartId}/thumbnail`,
          headers: {
            "content-type": `multipart/form-data; boundary=${boundary}`,
            "if-match": `"${meshIdentity.basis}"`,
            ...(version === undefined ? {} : { "x-thumbnail-renderer-version": version }),
          },
          payload: multipartPng(boundary, png),
        });
        expect(response.statusCode).toBe(409);
        expect(response.json()).toEqual({
          detail: "Thumbnail renderer is out of date; refresh the app and retry",
        });
        expect(readAcceptedMediaPng({ thumbsDir, basis: thumbnailIdentity.basis })).toBeNull();
      }
      const boundary = "current-renderer";
      const uploaded = await app.inject({
        method: "POST",
        url: `/parts/${part.projectionPartId}/thumbnail`,
        headers: {
          "content-type": `multipart/form-data; boundary=${boundary}`,
          "if-match": `"${meshIdentity.basis}"`,
          "x-thumbnail-renderer-version": THUMBNAIL_RENDERER_VERSION,
        },
        payload: multipartPng(boundary, png),
      });
      expect(uploaded.statusCode).toBe(200);
      const regenerated = await app.inject({
        method: "GET",
        url: `/parts/${part.projectionPartId}/thumbnail`,
        headers: { "if-none-match": `"${oldThumbnailBasis}"` },
      });
      expect(regenerated.statusCode).toBe(200);
      expect(regenerated.headers.etag).toBe(`"${thumbnailIdentity.basis}"`);
      expect(regenerated.rawPayload).toEqual(png);
      expect(repo.readAcceptedPlanOperationalSnapshot(profile.id)).toEqual(before);
      const preflight = await app.inject({
        method: "OPTIONS",
        url: `/parts/${part.projectionPartId}/thumbnail`,
        headers: {
          origin: "https://print.example",
          "access-control-request-method": "POST",
          "access-control-request-headers": "if-match,x-thumbnail-renderer-version",
        },
      });
      expect(preflight.statusCode).toBe(204);
      expect(preflight.headers["access-control-allow-headers"]).toContain(
        "x-thumbnail-renderer-version",
      );
    } finally {
      await app.close();
    }
  });

  it("serves, uploads, revalidates, and regenerates accepted Part media", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-accepted-part-media-"));
    directories.push(directory);
    const ports = createSelfHostPorts(directory);
    await ports.db.connect();
    const repo = ports.repository;
    const source = repo.createSource({ name: "Media Source", url: "https://github.com/a/b" });
    const observed = repo.getProjectRow(source.id);
    if (!observed) throw new Error("test Source is missing");
    const locator = `${source.id}/revisions/accepted`;
    const sourceRoot = join(directory, "repos", locator);
    const mesh = Buffer.from("solid accepted mesh");
    mkdirSync(join(sourceRoot, "parts"), { recursive: true });
    writeFileSync(join(sourceRoot, "parts", "widget.stl"), mesh);
    const sourceRevision = repo.recordSourceRevision({
      sourceId: source.id,
      upstreamRevisionKey: "accepted",
      manifestDigest: "a".repeat(64),
      snapshotLocator: locator,
      syncedAt: "2026-08-21T12:00:00.000Z",
      completeness: "complete",
    });
    repo.activateSourceRevision({
      sourceId: source.id,
      revisionId: sourceRevision.id,
      observed,
      sourceVersion: sourceRevision.upstream_revision_key,
    });
    const profile = repo.createProfile("Accepted media Plan", source.id);
    saveRoleFilamentDefault(repo, profile.id, "primary", {
      filament_color_id: null,
      filament_custom_hex: "#112233",
      spoolman_spool_id: null,
    });
    const created = repo.recomputePlanDraft({
      profileId: profile.id,
      actor: "test:user",
      idempotencyKey: "accepted-media-draft",
    });
    if (created.kind !== "created") throw new Error("test draft was not created");
    const reconciled = repo.savePlanDraftRequiredUnitReconciliation({
      profileId: profile.id,
      draftId: created.draft.id,
      expectedSnapshotDigest: created.draft.snapshotDigest,
      decisions: [],
      actorId: "test:user",
      idempotencyKey: "accepted-media-reconciliation",
    });
    if (reconciled.kind !== "saved") throw new Error("test reconciliation was not saved");
    const applied = repo.applyPlanChanges({
      profileId: profile.id,
      draftId: created.draft.id,
      expectedSnapshotDigest: reconciled.draft.snapshotDigest,
      expectedLifecycleVersion: 0,
      expectedBase: { kind: "empty", planVersion: 0 },
      actorId: "test:user",
      idempotencyKey: "accepted-media-apply",
    });
    if (applied.kind !== "applied") throw new Error("test draft was not applied");
    const part = repo.listParts(profile.id).parts[0];
    if (!part) throw new Error("test Part is missing");

    const expectedBasis = acceptedMediaBasis({
      expectedSha256: createHash("sha256").update(mesh).digest("hex"),
      role: part.role ?? "primary",
      hex: "#112233",
      variant: "mesh",
    });
    const thumbnailBasis = acceptedMediaBasis({
      expectedSha256: createHash("sha256").update(mesh).digest("hex"),
      role: part.role ?? "primary",
      hex: "#112233",
      variant: "thumbnail",
    });
    const previewBasis = acceptedMediaBasis({
      expectedSha256: createHash("sha256").update(mesh).digest("hex"),
      role: part.role ?? "primary",
      hex: "#112233",
      variant: "preview",
    });
    const readAccepted = repo.readAcceptedPlanOperationalSnapshot.bind(repo);
    let acceptedReads = 0;
    repo.readAcceptedPlanOperationalSnapshot = (profileId) => {
      acceptedReads += 1;
      return readAccepted(profileId);
    };
    const app = await buildApp({ ...loadConfig(), dataDir: directory }, ports);
    const capturedErrors: unknown[][] = [];
    app.addHook("onRequest", (request, _reply, done) => {
      request.log.error = (...args: unknown[]) => {
        capturedErrors.push(args);
      };
      done();
    });
    try {
      const response = await app.inject({ method: "GET", url: `/parts/${part.id}/mesh` });

      expect(response.statusCode).toBe(200);
      expect(response.rawPayload).toEqual(mesh);
      expect(response.headers.etag).toBe(`"${expectedBasis}"`);
      expect(response.headers["cache-control"]).toBe("private, no-cache");
      expect(response.headers["x-accepted-render-hex"]).toBe("#112233");
      expect(acceptedReads).toBe(1);

      const notModified = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/mesh`,
        headers: { "if-none-match": `"${expectedBasis}"` },
      });
      expect(notModified.statusCode).toBe(304);
      expect(notModified.rawPayload).toHaveLength(0);
      expect(acceptedReads).toBe(2);

      const listedNotModified = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/mesh`,
        headers: { "if-none-match": `"${"0".repeat(64)}", "${expectedBasis}"` },
      });
      expect(listedNotModified.statusCode).toBe(304);

      const weakValidator = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/mesh`,
        headers: { "if-none-match": `W/"${expectedBasis}"` },
      });
      expect(weakValidator.statusCode).toBe(200);
      expect(weakValidator.rawPayload).toEqual(mesh);

      const placeholder = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/thumbnail`,
      });
      expect(placeholder.statusCode).toBe(200);
      expect(placeholder.rawPayload).toEqual(PLACEHOLDER_PNG);
      expect(placeholder.headers.etag).toBeUndefined();
      expect(placeholder.headers["cache-control"]).toBe("no-store");
      expect(placeholder.headers["x-thumbnail-placeholder"]).toBe("1");
      expect(placeholder.headers["x-accepted-render-hex"]).toBe("#112233");
      expect(acceptedReads).toBe(5);

      const oversizedBoundary = "accepted-media-oversized-upload";
      const oversized = await app.inject({
        method: "POST",
        url: `/parts/${part.id}/thumbnail`,
        headers: {
          "content-type": `multipart/form-data; boundary=${oversizedBoundary}`,
          "if-match": `"${expectedBasis}"`,
          "x-thumbnail-renderer-version": THUMBNAIL_RENDERER_VERSION,
        },
        payload: multipartPng(
          oversizedBoundary,
          Buffer.concat([png, Buffer.alloc(65 - png.length)]),
        ),
      });
      expect(oversized.statusCode).toBe(413);
      expect(oversized.json()).toEqual({
        detail: "Thumbnail exceeds the 1 MiB upload limit",
      });
      expect(acceptedReads).toBe(5);

      const boundary = "accepted-media-upload";
      const uploaded = await app.inject({
        method: "POST",
        url: `/parts/${part.id}/thumbnail`,
        headers: {
          "content-type": `multipart/form-data; boundary=${boundary}`,
          "if-match": `"${expectedBasis}"`,
          "x-thumbnail-renderer-version": THUMBNAIL_RENDERER_VERSION,
        },
        payload: multipartPng(boundary, png),
      });
      expect(uploaded.statusCode).toBe(200);
      expect(uploaded.json()).toEqual({ saved: true, digest: thumbnailBasis });
      expect(acceptedReads).toBe(6);

      const placeholderValidator = placeholder.headers.etag;
      const cached = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/thumbnail`,
        headers:
          typeof placeholderValidator === "string"
            ? { "if-none-match": placeholderValidator }
            : undefined,
      });
      expect(cached.statusCode).toBe(200);
      expect(cached.rawPayload).toEqual(png);
      expect(cached.headers.etag).toBe(`"${thumbnailBasis}"`);
      expect(cached.headers["cache-control"]).toBe("private, no-cache");
      expect(acceptedReads).toBe(7);

      const cachedNotModified = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/thumbnail`,
        headers: { "if-none-match": `"${thumbnailBasis}"` },
      });
      expect(cachedNotModified.statusCode).toBe(304);
      expect(acceptedReads).toBe(8);

      const staleBoundary = "accepted-media-stale-upload";
      const stale = await app.inject({
        method: "POST",
        url: `/parts/${part.id}/thumbnail`,
        headers: {
          "content-type": `multipart/form-data; boundary=${staleBoundary}`,
          "if-match": `"${"f".repeat(64)}"`,
          "x-thumbnail-renderer-version": THUMBNAIL_RENDERER_VERSION,
        },
        payload: multipartPng(staleBoundary, Buffer.concat([png, Buffer.from("stale")])),
      });
      expect(stale.statusCode).toBe(409);
      expect(stale.json()).toEqual({ detail: "Accepted Part media basis is stale" });
      expect(
        readAcceptedMediaPng({
          thumbsDir: join(directory, "thumbs"),
          basis: thumbnailBasis,
        }),
      ).toEqual(png);
      expect(acceptedReads).toBe(9);

      const missingPreconditionBoundary = "accepted-media-missing-precondition";
      const missingPrecondition = await app.inject({
        method: "POST",
        url: `/parts/${part.id}/thumbnail`,
        headers: {
          "content-type": `multipart/form-data; boundary=${missingPreconditionBoundary}`,
          "x-thumbnail-renderer-version": THUMBNAIL_RENDERER_VERSION,
        },
        payload: multipartPng(missingPreconditionBoundary, png),
      });
      expect(missingPrecondition.statusCode).toBe(400);
      expect(missingPrecondition.json()).toEqual({ detail: "Strong If-Match header required" });

      const invalidBoundary = "accepted-media-invalid-png";
      const invalidPng = await app.inject({
        method: "POST",
        url: `/parts/${part.id}/thumbnail`,
        headers: {
          "content-type": `multipart/form-data; boundary=${invalidBoundary}`,
          "if-match": `"${expectedBasis}"`,
          "x-thumbnail-renderer-version": THUMBNAIL_RENDERER_VERSION,
        },
        payload: multipartPng(invalidBoundary, Buffer.from("not a png")),
      });
      expect(invalidPng.statusCode).toBe(400);
      expect(invalidPng.json()).toEqual({ detail: "Expected PNG image" });

      const previewPlaceholder = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/preview`,
      });
      expect(previewPlaceholder.statusCode).toBe(200);
      expect(previewPlaceholder.rawPayload).toEqual(PLACEHOLDER_PNG);
      expect(previewPlaceholder.headers.etag).toBeUndefined();
      expect(previewPlaceholder.headers["cache-control"]).toBe("no-store");
      expect(acceptedReads).toBe(11);

      writeAcceptedMediaPng({
        thumbsDir: join(directory, "thumbs"),
        basis: previewBasis,
        png,
      });
      const regenerated = await app.inject({
        method: "POST",
        url: `/plans/${profile.id}/regenerate-thumbnails`,
      });
      expect(regenerated.statusCode).toBe(200);
      expect(regenerated.json()).toEqual({ cleared: 2 });
      expect(acceptedReads).toBe(12);
      expect(
        existsSync(
          acceptedMediaCachePath({
            thumbsDir: join(directory, "thumbs"),
            basis: thumbnailBasis,
          }),
        ),
      ).toBe(false);
      expect(
        existsSync(
          acceptedMediaCachePath({ thumbsDir: join(directory, "thumbs"), basis: previewBasis }),
        ),
      ).toBe(false);

      const nextObserved = repo.getProjectRow(source.id);
      if (!nextObserved) throw new Error("next test Source is missing");
      const nextLocator = `${source.id}/revisions/next`;
      const nextSourceRoot = join(directory, "repos", nextLocator);
      const nextMesh = Buffer.from("solid next accepted mesh");
      mkdirSync(join(nextSourceRoot, "parts"), { recursive: true });
      writeFileSync(join(nextSourceRoot, "parts", "widget.stl"), nextMesh);
      const nextSourceRevision = repo.recordSourceRevision({
        sourceId: source.id,
        upstreamRevisionKey: "next",
        manifestDigest: "b".repeat(64),
        snapshotLocator: nextLocator,
        syncedAt: "2026-08-21T12:02:00.000Z",
        completeness: "complete",
      });
      repo.activateSourceRevision({
        sourceId: source.id,
        revisionId: nextSourceRevision.id,
        observed: nextObserved,
        sourceVersion: nextSourceRevision.upstream_revision_key,
      });
      const nextDraft = repo.recomputePlanDraft({
        profileId: profile.id,
        actor: "test:user",
        idempotencyKey: "accepted-media-next-draft",
      });
      if (nextDraft.kind !== "created") {
        throw new Error(`next test draft was not created: ${nextDraft.kind}`);
      }
      const decisions = nextDraft.draft.parts.map((draftPart) => {
        if (draftPart.baseRevisionPartId == null) {
          throw new Error("next test predecessor is missing");
        }
        return {
          kind: "accept_prior_completion" as const,
          targetDraftPartId: draftPart.id,
          predecessorRevisionPartId: draftPart.baseRevisionPartId,
        };
      });
      const nextReconciled = repo.savePlanDraftRequiredUnitReconciliation({
        profileId: profile.id,
        draftId: nextDraft.draft.id,
        expectedSnapshotDigest: nextDraft.draft.snapshotDigest,
        decisions,
        actorId: "test:user",
        idempotencyKey: "accepted-media-next-reconciliation",
      });
      if (nextReconciled.kind !== "saved") {
        throw new Error("next test reconciliation was not saved");
      }
      const nextApplyCommand = {
        profileId: profile.id,
        draftId: nextDraft.draft.id,
        expectedSnapshotDigest: nextReconciled.draft.snapshotDigest,
        expectedLifecycleVersion: 0,
        expectedBase: {
          kind: "revision" as const,
          revisionId: applied.receipt.revisionId,
          planVersion: applied.receipt.planVersion,
        },
        actorId: "test:user",
        idempotencyKey: "accepted-media-next-apply",
      };
      const staleApplyBoundary = "accepted-media-stale-apply";
      const staleApplyPayload = multipartPng(staleApplyBoundary, png);
      const bodyOffset = staleApplyPayload.indexOf(png);
      let nextApplied = false;
      const applyDuringMultipart = Readable.from(
        (async function* () {
          yield staleApplyPayload.subarray(0, bodyOffset);
          const appliedNext = repo.applyPlanChanges(nextApplyCommand);
          if (appliedNext.kind !== "applied") {
            throw new Error(`next test draft was not applied: ${appliedNext.kind}`);
          }
          nextApplied = true;
          yield staleApplyPayload.subarray(bodyOffset);
        })(),
      );
      const staleAfterApply = await app.inject({
        method: "POST",
        url: `/parts/${part.id}/thumbnail`,
        headers: {
          "content-type": `multipart/form-data; boundary=${staleApplyBoundary}`,
          "if-match": `"${expectedBasis}"`,
          "x-thumbnail-renderer-version": THUMBNAIL_RENDERER_VERSION,
        },
        payload: applyDuringMultipart,
      });
      expect(nextApplied).toBe(true);
      expect(staleAfterApply.statusCode).toBe(409);
      expect(staleAfterApply.json()).toEqual({ detail: "Accepted Part media basis is stale" });
      expect(
        existsSync(
          acceptedMediaCachePath({
            thumbsDir: join(directory, "thumbs"),
            basis: thumbnailBasis,
          }),
        ),
      ).toBe(false);
      const nextThumbnailBasis = acceptedMediaBasis({
        expectedSha256: createHash("sha256").update(nextMesh).digest("hex"),
        role: part.role ?? "primary",
        hex: "#112233",
        variant: "thumbnail",
      });
      expect(
        existsSync(
          acceptedMediaCachePath({
            thumbsDir: join(directory, "thumbs"),
            basis: nextThumbnailBasis,
          }),
        ),
      ).toBe(false);

      const acceptedReady = readAccepted(profile.id);
      if (acceptedReady.kind !== "ready") throw new Error("accepted test snapshot is missing");
      const currentPart = acceptedReady.snapshot.parts.find(
        (acceptedPart) => acceptedPart.filename === "widget.stl",
      );
      if (!currentPart) throw new Error("current accepted test Part is missing");
      const readsBeforeMissing = acceptedReads;
      const missingPart = await app.inject({ method: "GET", url: "/parts/999999/mesh" });
      expect(missingPart.statusCode).toBe(404);
      expect(missingPart.json()).toEqual({ detail: "Part not found" });
      expect(acceptedReads).toBe(readsBeforeMissing);

      rmSync(join(nextSourceRoot, "parts", "widget.stl"));
      const missingArtifact = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/mesh`,
      });
      expect(missingArtifact.statusCode).toBe(409);
      expect(missingArtifact.json()).toEqual({ detail: "Accepted Part artifact is unavailable" });
      writeFileSync(join(nextSourceRoot, "parts", "widget.stl"), "solid changed mesh");
      const mismatchedArtifact = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/thumbnail`,
      });
      expect(mismatchedArtifact.statusCode).toBe(409);
      expect(mismatchedArtifact.json()).toEqual({
        detail: "Accepted Part artifact is unavailable",
      });
      writeFileSync(join(nextSourceRoot, "parts", "widget.stl"), nextMesh);

      repo.readAcceptedPlanOperationalSnapshot = () => {
        acceptedReads += 1;
        return { kind: "compatibility_dirty" };
      };
      const dirty = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/preview`,
      });
      expect(dirty.statusCode).toBe(409);
      expect(dirty.json()).toEqual({ detail: "Accepted Plan requires compatibility repair" });
      repo.readAcceptedPlanOperationalSnapshot = () => {
        acceptedReads += 1;
        return { kind: "uninitialized" };
      };
      const uninitialized = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/mesh`,
      });
      expect(uninitialized.statusCode).toBe(409);
      expect(uninitialized.json()).toEqual({
        detail: "Accepted Plan operational state is not initialized",
      });
      repo.readAcceptedPlanOperationalSnapshot = () => {
        acceptedReads += 1;
        return {
          kind: "ready",
          snapshot: {
            ...acceptedReady.snapshot,
            parts: acceptedReady.snapshot.parts.map((acceptedPart) =>
              acceptedPart.projectionPartId === currentPart.projectionPartId
                ? { ...acceptedPart, artifact: { kind: "unavailable", reason: "legacy" } }
                : acceptedPart,
            ),
          },
        };
      };
      const legacy = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/mesh`,
      });
      expect(legacy.statusCode).toBe(409);
      expect(legacy.json()).toEqual({ detail: "Accepted Part media is unavailable" });

      repo.readAcceptedPlanOperationalSnapshot = () => {
        throw new AcceptedPlanOperationalIntegrityError("artifact_linkage", "private detail");
      };
      const integrity = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/mesh`,
      });
      expect(integrity.statusCode).toBe(500);
      expect(integrity.json()).toEqual({ detail: "Accepted Plan data is inconsistent" });
      repo.readAcceptedPlanOperationalSnapshot = () => {
        throw new Error(`private unexpected detail ${sourceRoot} ${expectedBasis}`);
      };
      const unexpected = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/thumbnail`,
      });
      expect(unexpected.statusCode).toBe(500);
      expect(unexpected.json()).toEqual({ detail: "Internal Server Error" });

      const getPartRow = repo.getPartRow.bind(repo);
      repo.getPartRow = () => {
        throw new Error(
          `private Part lookup detail ${join(directory, "thumbs")} ${thumbnailBasis}`,
        );
      };
      const lookupFailure = await app.inject({
        method: "GET",
        url: `/parts/${currentPart.projectionPartId}/mesh`,
      });
      expect(lookupFailure.statusCode).toBe(500);
      expect(lookupFailure.json()).toEqual({ detail: "Internal Server Error" });
      repo.getPartRow = getPartRow;
      expect(readdirSync(join(directory, "thumbs"))).toEqual([]);
      const serializedErrors = JSON.stringify(capturedErrors, (_key, value: unknown) =>
        value instanceof Error ? { message: value.message, stack: value.stack } : value,
      );
      expect(serializedErrors).not.toContain("private unexpected detail");
      expect(serializedErrors).not.toContain("private Part lookup detail");
      expect(serializedErrors).not.toContain(sourceRoot);
      expect(serializedErrors).not.toContain(join(directory, "thumbs"));
      expect(serializedErrors).not.toContain(expectedBasis);
      expect(serializedErrors).not.toContain(thumbnailBasis);
      expect(serializedErrors).not.toContain("accepted-part-media-routes.test.ts");
    } finally {
      await app.close();
    }
  });
});
