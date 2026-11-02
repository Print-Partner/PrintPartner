import { acceptPlanForTest, editAcceptedPartsForTest } from "../test/accept-plan.js";
import Database from "better-sqlite3";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { afterEach, describe, expect, it } from "vitest";
import Fastify from "fastify";
import { drizzle } from "drizzle-orm/node-postgres";
import type { Pool } from "pg";
import { buildApp } from "../app.js";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { loadConfig } from "../config.js";
import { acceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { parseRequiredUnitToken } from "../services/required-units.js";
import { AppRepository, type SchemaTables } from "../db/repository.js";
import * as pgSchema from "../db/schema-pg.js";
import { registerPostgresSyncQuery, unregisterPostgresSyncQuery } from "../db/sync-db-bridge.js";
import { registerPartRoutes } from "./parts.js";
import { registerPlanRoutes } from "./plans.js";

const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe("accepted Plan read routes", () => {
  it("returns 503 before any PostgreSQL query at every accepted mutation route", async () => {
    const postgres = drizzle({} as Pool, { schema: pgSchema });
    const statements: string[] = [];
    registerPostgresSyncQuery(postgres, ({ sql: query }) => {
      statements.push(query);
      return { rows: [], rowCount: 0 };
    });
    const repo = new AppRepository(
      postgres,
      "default",
      "/tmp/unused-progress-routes",
      pgSchema as unknown as SchemaTables,
    );
    const app = Fastify();
    const register = async (instance: ReturnType<typeof Fastify>) => {
      await registerPartRoutes(instance, {
        repo,
        reposDir: "/tmp/unused-progress-routes/repos",
        thumbsDir: "/tmp/unused-progress-routes/thumbs",
      });
      await registerPlanRoutes(instance, {
        repo,
        dataDir: "/tmp/unused-progress-routes",
        reposDir: "/tmp/unused-progress-routes/repos",
        thumbsDir: "/tmp/unused-progress-routes/thumbs",
      });
    };
    await app.register(register);
    await app.register(register, { prefix: "/api/v1" });
    await app.ready();
    try {
      for (const request of [
        {
          method: "PATCH" as const,
          url: "/parts/1/progress",
          payload: { unit_index: 0, completed: true },
        },
        {
          method: "PATCH" as const,
          url: "/api/v1/parts/1/progress",
          payload: { unit_index: 0, completed: true },
        },
        {
          method: "PATCH" as const,
          url: "/parts/1/assembled",
          payload: { unit_index: 0, assembled: true },
        },
        {
          method: "PATCH" as const,
          url: "/api/v1/parts/1/assembled",
          payload: { unit_index: 0, assembled: true },
        },
        { method: "POST" as const, url: "/plans/1/archive" },
        { method: "POST" as const, url: "/api/v1/plans/1/archive" },
        {
          method: "PATCH" as const,
          url: "/plans/1",
          payload: { archived: true },
        },
      ]) {
        const response = await app.inject(request);
        expect(response.statusCode, `${request.method} ${request.url}`).toBe(503);
        expect(response.json()).toEqual({ detail: "Accepted Plan update is unavailable" });
      }
      expect(statements).toEqual([]);
    } finally {
      await app.close();
      unregisterPostgresSyncQuery(postgres);
    }
  });

  it("uses one accepted read per Checkoff and assembled request and redacts failures", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-accepted-read-routes-"));
    directories.push(directory);
    const ports = createSelfHostPorts(directory);
    await ports.db.connect();
    const repo = ports.repository;
    const source = repo.createSource({
      name: "Repo",
      url: "https://github.com/a/b",
    });
    const sourceRoot = join(directory, "repos", String(source.id));
    mkdirSync(join(sourceRoot, "parts"), { recursive: true });
    writeFileSync(join(sourceRoot, "parts", "widget.stl"), "solid widget");
    repo.updateSource(source.id, { local_path: sourceRoot });
    repo.updateImportRules(source.id, ["parts/"]);
    const profile = repo.createProfile("Accepted route Plan", source.id);
    acceptPlanForTest(repo, profile.id);
    const priorPart = repo.listParts(profile.id).parts[0];
    if (!priorPart) throw new Error("test Part is missing");
    const remappedPartId = editAcceptedPartsForTest(repo, profile.id, [
      {
        projectionPartId: priorPart.id,
        quantityOverride: 1,
      },
    ]).get(priorPart.id)!;
    const part = repo
      .listParts(profile.id)
      .parts.find((candidate) => candidate.id === remappedPartId)!;

    const initialAccepted = repo.readAcceptedPlanOperationalSnapshot(profile.id);
    if (initialAccepted.kind !== "ready") throw new Error("accepted Plan is not ready");
    const initialToken = parseRequiredUnitToken(initialAccepted.snapshot.parts[0]!.units[0]!.token);
    repo.setAcceptedUnitCompletion({
      expected: acceptedPlanBasis(initialAccepted.snapshot),
      token: initialToken,
      completed: true,
    });
    repo.setAcceptedUnitAssembly({
      expected: acceptedPlanBasis(initialAccepted.snapshot),
      token: initialToken,
      assembled: true,
    });

    const progressBeforeRead = new Database(join(directory, "print-partner.db"), {
      readonly: true,
    });
    const progressBeforeRequests = progressBeforeRead
      .prepare("SELECT * FROM print_progress WHERE part_id = ? ORDER BY id")
      .all(part.id);
    progressBeforeRead.close();

    const readAccepted = repo.readAcceptedPlanOperationalSnapshot.bind(repo);
    let acceptedReadCount = 0;
    repo.readAcceptedPlanOperationalSnapshot = (profileId) => {
      acceptedReadCount += 1;
      return readAccepted(profileId);
    };

    const app = await buildApp(loadConfig(), ports);
    const capturedErrors: unknown[][] = [];
    app.addHook("onRequest", (request, _reply, done) => {
      request.log.error = (...args: unknown[]) => {
        capturedErrors.push(args);
      };
      done();
    });
    try {
      const negative = await app.inject({
        method: "PATCH",
        url: `/parts/${part.id}/progress`,
        payload: { unit_index: -1, completed: false },
      });
      expect(negative.statusCode).toBe(400);
      expect(negative.json()).toEqual({
        detail: "unit_index must be a non-negative integer and completed a boolean",
      });
      expect(acceptedReadCount).toBe(0);

      const fractional = await app.inject({
        method: "PATCH",
        url: `/parts/${part.id}/progress`,
        payload: { unit_index: 0.5, completed: true },
      });
      expect(fractional.statusCode).toBe(400);
      expect(acceptedReadCount).toBe(0);

      const outOfRange = await app.inject({
        method: "PATCH",
        url: `/parts/${part.id}/progress`,
        payload: { unit_index: 1, completed: true },
      });
      expect(outOfRange.statusCode).toBe(400);
      expect(outOfRange.json()).toEqual({ detail: "unit_index out of range" });
      expect(acceptedReadCount).toBe(1);

      const completion = await app.inject({
        method: "PATCH",
        url: `/parts/${part.id}/progress`,
        payload: { unit_index: 0, completed: true },
      });
      expect(completion.statusCode).toBe(200);
      expect(completion.json()).toEqual({
        part_id: part.id,
        printed_count: 1,
        print_units: [true],
        assembled_units: [true],
        missing: false,
      });
      expect(acceptedReadCount).toBe(2);

      const assembledApi = await app.inject({
        method: "PATCH",
        url: `/api/v1/parts/${part.id}/assembled`,
        payload: { unit_index: 0, assembled: true },
      });
      expect(assembledApi.statusCode).toBe(200);
      expect(assembledApi.json()).toEqual({
        part_id: part.id,
        assembled_count: 1,
        assembled_units: [true],
      });
      expect(acceptedReadCount).toBe(3);

      const checkoff = await app.inject({
        method: "GET",
        url: `/plans/${profile.id}/checkoff`,
      });
      expect(checkoff.statusCode).toBe(200);
      expect(checkoff.json()).toEqual({
        profile_id: profile.id,
        summary: "1/1 parts fully printed · 1/1 units",
        parts: [
          {
            id: part.id,
            filename: "widget.stl",
            match_key: part.match_key,
            relative_path: part.relative_path,
            source_layer: part.source_layer,
            role: part.role,
            quantity_effective: part.quantity_effective,
            printed_count: 1,
            print_units: [true],
            missing: false,
            filament_display: "",
            filament_hex: null,
          },
        ],
      });
      expect(acceptedReadCount).toBe(4);

      const assembled = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/assembled`,
      });
      expect(assembled.statusCode).toBe(200);
      expect(assembled.json()).toEqual({
        part_id: part.id,
        assembled_count: 1,
        assembled_units: [true],
      });
      expect(acceptedReadCount).toBe(5);

      const archivedApi = await app.inject({
        method: "POST",
        url: `/api/v1/plans/${profile.id}/archive`,
      });
      expect(archivedApi.statusCode).toBe(200);
      expect(archivedApi.json()).toMatchObject({
        id: profile.id,
        name: "Accepted route Plan",
        archived_at: expect.any(String),
      });
      expect(acceptedReadCount).toBe(6);

      const archivedFlat = await app.inject({
        method: "POST",
        url: `/plans/${profile.id}/archive`,
      });
      expect(archivedFlat.statusCode).toBe(200);
      const {
        remaining_units: archivedRemainingUnits,
        total_units: archivedTotalUnits,
        ...archivedV1
      } = archivedApi.json();
      expect(archivedFlat.json()).toEqual({
        ...archivedV1,
        accepted_progress: {
          kind: "ready",
          remaining_units: archivedRemainingUnits,
          total_units: archivedTotalUnits,
        },
      });
      expect(acceptedReadCount).toBe(6);

      const archivedUncheck = await app.inject({
        method: "PATCH",
        url: `/parts/${part.id}/progress`,
        payload: { unit_index: 0, completed: false },
      });
      expect(archivedUncheck.statusCode).toBe(409);
      expect(archivedUncheck.json()).toEqual({
        detail: "Archived Plan Progress cannot be changed",
      });
      expect(acceptedReadCount).toBe(7);

      const emptyProfile = repo.createProfile("Empty accepted route Plan");
      const empty = await app.inject({
        method: "GET",
        url: `/plans/${emptyProfile.id}/checkoff`,
      });
      expect(empty.statusCode).toBe(200);
      expect(empty.json()).toEqual({
        profile_id: emptyProfile.id,
        summary: "0/0 parts fully printed · 0/0 units",
        parts: [],
      });
      expect(acceptedReadCount).toBe(8);

      const dirtyProfile = repo.createProfile("Dirty accepted route Plan", source.id);
      acceptPlanForTest(repo, dirtyProfile.id);
      const dirtyPart = repo.listParts(dirtyProfile.id).parts[0];
      if (!dirtyPart) throw new Error("dirty test Part is missing");
      const dirtyDb = new Database(join(directory, "print-partner.db"));
      dirtyDb.prepare("UPDATE parts SET notes = 'dirty' WHERE id = ?").run(dirtyPart.id);
      dirtyDb.close();
      const dirtyCheckoff = await app.inject({
        method: "GET",
        url: `/plans/${dirtyProfile.id}/checkoff`,
      });
      expect(dirtyCheckoff.statusCode).toBe(409);
      expect(dirtyCheckoff.json()).toEqual({
        detail: "Accepted Plan requires compatibility repair",
      });
      const dirtyAssembled = await app.inject({
        method: "GET",
        url: `/parts/${dirtyPart.id}/assembled`,
      });
      expect(dirtyAssembled.statusCode).toBe(409);
      expect(dirtyAssembled.json()).toEqual({
        detail: "Accepted Plan requires compatibility repair",
      });
      expect(acceptedReadCount).toBe(10);

      const missingProfile = await app.inject({
        method: "GET",
        url: "/plans/999999/checkoff",
      });
      expect(missingProfile.statusCode).toBe(404);
      expect(missingProfile.json()).toEqual({ detail: "Profile not found" });
      const missingPart = await app.inject({
        method: "GET",
        url: "/parts/999999/assembled",
      });
      expect(missingPart.statusCode).toBe(404);
      expect(missingPart.json()).toEqual({ detail: "Part not found" });
      expect(acceptedReadCount).toBe(10);

      const observedSource = repo.getProjectRow(source.id);
      if (!observedSource) throw new Error("test Source is missing");
      const historicalLocator = `${source.id}/revisions/historical`;
      const historicalRoot = join(directory, "repos", historicalLocator);
      mkdirSync(join(historicalRoot, "parts"), { recursive: true });
      writeFileSync(join(historicalRoot, "parts", "widget.stl"), "solid widget");
      const historicalRevision = repo.recordSourceRevision({
        sourceId: source.id,
        upstreamRevisionKey: "historical",
        manifestDigest: "a".repeat(64),
        snapshotLocator: historicalLocator,
        syncedAt: "2026-08-21T06:02:00.000Z",
        completeness: "complete",
      });
      repo.activateSourceRevision({
        sourceId: source.id,
        revisionId: historicalRevision.id,
        observed: observedSource,
        sourceVersion: historicalRevision.upstream_revision_key,
      });
      const uninitializedProfile = repo.createProfile(
        "Uninitialized accepted route Plan",
        source.id,
      );
      acceptPlanForTest(repo, uninitializedProfile.id);
      const repairRaw = new Database(join(directory, "print-partner.db"));
      repairRaw.pragma("foreign_keys = ON");
      const inputSetId = repairRaw
        .prepare("SELECT input_set_id FROM plan_accepted_input_sets WHERE profile_id = ?")
        .pluck()
        .get(uninitializedProfile.id) as number;
      repairRaw
        .prepare(
          `UPDATE plan_revision_inputs
              SET source_layer = 'legacy:' || source_id,
                  layer_order = 0,
                  tracking_kind = 'revision',
                  effective_naming_digest = NULL
            WHERE input_set_id = ?`,
        )
        .run(inputSetId);
      const historicalInputs = repairRaw
        .prepare(
          `SELECT source_revision_id, manifest_digest
             FROM plan_revision_inputs
            WHERE input_set_id = ?
            ORDER BY source_revision_id`,
        )
        .all(inputSetId);
      const inputSetDigest = createHash("sha256")
        .update(JSON.stringify(historicalInputs))
        .digest("hex");
      repairRaw
        .prepare(
          "UPDATE plan_revision_input_sets SET format_version = 1, input_set_digest = ? WHERE id = ?",
        )
        .run(inputSetDigest, inputSetId);
      repairRaw.close();
      const uninitialized = await app.inject({
        method: "GET",
        url: `/plans/${uninitializedProfile.id}/checkoff`,
      });
      expect(uninitialized.statusCode).toBe(409);
      expect(uninitialized.json()).toEqual({
        detail: "Accepted Plan operational state is not initialized",
      });
      expect(acceptedReadCount).toBe(11);

      const corruptRaw = new Database(join(directory, "print-partner.db"));
      corruptRaw.pragma("foreign_keys = ON");
      corruptRaw.exec("DROP TRIGGER trg_plan_revisions_immutable_update");
      corruptRaw
        .prepare(
          `UPDATE plan_revisions
            SET snapshot_digest = ?
          WHERE id = (SELECT accepted_plan_revision_id FROM build_profiles WHERE id = ?)`,
        )
        .run("f".repeat(64), profile.id);
      corruptRaw.close();
      const corrupt = await app.inject({
        method: "GET",
        url: `/plans/${profile.id}/checkoff`,
      });
      expect(corrupt.statusCode).toBe(500);
      expect(corrupt.json()).toEqual({
        detail: "Accepted Plan data is inconsistent",
      });
      expect(acceptedReadCount).toBe(12);

      repo.readAcceptedPlanOperationalSnapshot = () => {
        acceptedReadCount += 1;
        throw new Error(`private failure detail ${directory} ${"e".repeat(64)}`);
      };
      const unexpected = await app.inject({
        method: "GET",
        url: `/plans/${emptyProfile.id}/checkoff`,
      });
      expect(unexpected.statusCode).toBe(500);
      expect(unexpected.json()).toEqual({ detail: "Internal Server Error" });
      expect(acceptedReadCount).toBe(13);
      repo.readAcceptedPlanOperationalSnapshot = (profileId) => {
        acceptedReadCount += 1;
        return readAccepted(profileId);
      };

      const readAcceptedProfileSummary = repo.readAcceptedProfileSummary.bind(repo);
      repo.readAcceptedProfileSummary = () => {
        throw new Error(`private summary lookup failure ${directory}`);
      };
      const archiveIdentityFailure = await app.inject({
        method: "POST",
        url: `/plans/${emptyProfile.id}/archive`,
      });
      expect(archiveIdentityFailure.statusCode).toBe(500);
      expect(archiveIdentityFailure.json()).toEqual({ detail: "Internal Server Error" });
      const patchArchiveIdentityFailure = await app.inject({
        method: "PATCH",
        url: `/plans/${emptyProfile.id}`,
        payload: { archived: true },
      });
      expect(patchArchiveIdentityFailure.statusCode).toBe(500);
      expect(patchArchiveIdentityFailure.json()).toEqual({ detail: "Internal Server Error" });
      const summaryLookupFailure = await app.inject({
        method: "GET",
        url: `/plans/${emptyProfile.id}`,
      });
      expect(summaryLookupFailure.statusCode).toBe(500);
      expect(summaryLookupFailure.json()).toEqual({
        detail: "Internal Server Error",
      });
      expect(acceptedReadCount).toBe(13);
      repo.readAcceptedProfileSummary = readAcceptedProfileSummary;

      repo.getPartRow = () => {
        throw new Error(`private Part lookup failure ${directory}`);
      };
      const partLookupFailure = await app.inject({
        method: "GET",
        url: `/parts/${part.id}/assembled`,
      });
      expect(partLookupFailure.statusCode).toBe(500);
      expect(partLookupFailure.json()).toEqual({
        detail: "Internal Server Error",
      });

      const progressAfterFailures = new Database(join(directory, "print-partner.db"), {
        readonly: true,
      });
      expect(
        progressAfterFailures
          .prepare("SELECT * FROM print_progress WHERE part_id = ? ORDER BY id")
          .all(part.id),
      ).toEqual(progressBeforeRequests);
      progressAfterFailures.close();
      const serializedErrors = JSON.stringify(capturedErrors, (_key, value: unknown) =>
        value instanceof Error ? { message: value.message, stack: value.stack } : value,
      );
      expect(serializedErrors).not.toContain("private failure detail");
      expect(serializedErrors).not.toContain("private identity lookup failure");
      expect(serializedErrors).not.toContain("private profile lookup failure");
      expect(serializedErrors).not.toContain("private Part lookup failure");
      expect(serializedErrors).not.toContain(directory);
      expect(serializedErrors).not.toContain("e".repeat(64));
      expect(serializedErrors).not.toContain("accepted-plan-read-routes.test.ts");
      expect(serializedErrors).toContain("Plan summary read failed");
    } finally {
      await app.close();
    }
  });
});
