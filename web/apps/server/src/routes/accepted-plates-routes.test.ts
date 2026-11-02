import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import Fastify from "fastify";
import { drizzle } from "drizzle-orm/node-postgres";
import type { Pool } from "pg";
import { buildApp } from "../app.js";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { loadConfig } from "../config.js";
import { acceptedPlanBasis } from "../db/accepted-plan-progress.js";
import { AppRepository, type SchemaTables } from "../db/repository.js";
import * as pgSchema from "../db/schema-pg.js";
import { registerPostgresSyncQuery, unregisterPostgresSyncQuery } from "../db/sync-db-bridge.js";
import { saveFleet } from "../services/printer-fleet.js";
import { registerAcceptedPlateRoutes } from "./accepted-plates.js";

const cleanups: Array<() => Promise<void>> = [];

afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) await cleanup();
  delete process.env.PRINT_PARTNER_API_KEY;
});

async function fixture(apiKey?: string) {
  const root = mkdtempSync(join(tmpdir(), "pp-accepted-plate-routes-"));
  process.env.PRINT_PARTNER_DATA_DIR = root;
  if (apiKey) process.env.PRINT_PARTNER_API_KEY = apiKey;
  else delete process.env.PRINT_PARTNER_API_KEY;
  const ports = createSelfHostPorts(root);
  await ports.db.connect();
  const profile = ports.repository.createProfile("Empty Plate Plan");
  const app = await buildApp(loadConfig(), ports);
  cleanups.push(async () => {
    await app.close();
    ports.db.close();
    rmSync(root, { recursive: true, force: true });
  });
  return { app, profile };
}

async function trackedFixture(
  stl = `solid tracked
facet normal 0 0 1
outer loop
vertex 0 0 0
vertex 30 0 0
vertex 0 20 10
endloop
endfacet
endsolid tracked`,
) {
  const root = mkdtempSync(join(tmpdir(), "pp-accepted-plate-route-tracked-"));
  process.env.PRINT_PARTNER_DATA_DIR = root;
  delete process.env.PRINT_PARTNER_API_KEY;
  const ports = createSelfHostPorts(root);
  await ports.db.connect();
  const repo = ports.repository;
  const source = repo.createSource({
    name: "Tracked Plate source",
    url: "https://example.test/tracked-plate-route",
    source_kind: "github",
  });
  const observed = repo.getProjectRow(source.id);
  if (!observed) throw new Error("tracked Source is missing");
  const locator = `${source.id}/revisions/accepted`;
  const snapshotRoot = join(root, "repos", locator);
  mkdirSync(snapshotRoot, { recursive: true });
  writeFileSync(join(snapshotRoot, "bracket.stl"), stl);
  const sourceRevision = repo.recordSourceRevision({
    sourceId: source.id,
    upstreamRevisionKey: "accepted",
    manifestDigest: "d".repeat(64),
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
  const profile = repo.createProfile("Tracked Plate Build", source.id);
  const created = repo.recomputePlanDraft({
    profileId: profile.id,
    actor: "test:user",
    idempotencyKey: "plate-route-draft",
  });
  if (created.kind !== "created") throw new Error("tracked draft was not created");
  const reconciled = repo.savePlanDraftRequiredUnitReconciliation({
    profileId: profile.id,
    draftId: created.draft.id,
    expectedSnapshotDigest: created.draft.snapshotDigest,
    decisions: [],
    actorId: "test:user",
    idempotencyKey: "plate-route-reconciliation",
  });
  if (reconciled.kind !== "saved") throw new Error("tracked reconciliation failed");
  const applied = repo.applyPlanChanges({
    profileId: profile.id,
    draftId: created.draft.id,
    expectedSnapshotDigest: reconciled.draft.snapshotDigest,
    expectedLifecycleVersion: 0,
    expectedBase: { kind: "empty", planVersion: 0 },
    actorId: "test:user",
    idempotencyKey: "plate-route-apply",
  });
  if (applied.kind !== "applied") throw new Error("tracked Plan was not applied");
  const accepted = repo.readAcceptedPlanOperationalSnapshot(profile.id);
  if (accepted.kind !== "ready") throw new Error("tracked accepted Plan is unavailable");
  const unit = accepted.snapshot.parts
    .filter((part) => part.included)
    .flatMap((part) => part.units)
    .find((candidate) => candidate.required);
  if (!unit) throw new Error("tracked Required unit is missing");
  saveFleet(repo, [
    {
      id: "printer-one",
      name: "Printer One",
      model: "Model One",
      bed_width_mm: 250,
      bed_depth_mm: 210,
      bed_height_mm: 200,
      margin_mm: 4,
      max_filament_slots: 1,
      loaded_filaments: [{ slot: 1, filament_color_id: "wrong", label: "Wrong" }],
    },
    {
      id: "printer-two",
      name: "Printer Two",
      model: "Model Two",
      bed_width_mm: 220,
      bed_depth_mm: 220,
      bed_height_mm: 250,
      margin_mm: 4,
      max_filament_slots: 1,
      loaded_filaments: [],
    },
  ]);
  const app = await buildApp(loadConfig(), ports);
  cleanups.push(async () => {
    await app.close();
    ports.db.close();
    rmSync(root, { recursive: true, force: true });
  });
  return { app, profile, basis: acceptedPlanBasis(accepted.snapshot), token: unit.token };
}

describe("accepted Plate routes", () => {
  it("mounts the same flat contract unversioned and in v2, but never in v1", async () => {
    const { app, profile } = await fixture();

    const browser = await app.inject({ method: "GET", url: `/plans/${profile.id}/plates` });
    const v2 = await app.inject({ method: "GET", url: `/api/v2/plans/${profile.id}/plates` });
    const v1 = await app.inject({ method: "GET", url: `/api/v1/plans/${profile.id}/plates` });

    expect(browser.statusCode).toBe(200);
    expect(browser.json()).toEqual({ kind: "empty_plan" });
    expect(v2.statusCode).toBe(200);
    expect(v2.json()).toEqual(browser.json());
    expect(v1.statusCode).toBe(404);

    const removed = [
      { method: "GET" as const, path: `/plans/${profile.id}/print-plan` },
      { method: "PUT" as const, path: `/plans/${profile.id}/print-plan` },
      { method: "GET" as const, path: `/plans/${profile.id}/print-groups` },
      { method: "PUT" as const, path: `/plans/${profile.id}/print-assignments` },
      { method: "GET" as const, path: `/plans/${profile.id}/plate-workspace` },
      { method: "POST" as const, path: `/plans/${profile.id}/print-plan/prepare-missing` },
      { method: "POST" as const, path: "/jobs/export-3mf" },
      { method: "POST" as const, path: "/jobs/pack-preview" },
      { method: "POST" as const, path: "/jobs/auto-slice" },
      { method: "POST" as const, path: "/slicer-instances/missing/open-plates" },
    ];
    for (const route of removed) {
      for (const prefix of ["", "/api/v1"]) {
        const response = await app.inject({ method: route.method, url: `${prefix}${route.path}` });
        expect(response.statusCode, `${route.method} ${prefix}${route.path}`).toBe(404);
      }
    }

    const openapi = await app.inject({ method: "GET", url: "/api/v1/openapi.json" });
    const paths = Object.keys(openapi.json().paths as Record<string, unknown>);
    for (const route of removed) {
      expect(paths).not.toContain(route.path);
      expect(paths).not.toContain(`/api/v1${route.path}`);
    }
    expect(paths).toContain("/jobs/export-accepted-plate-3mf");
    expect(paths).toContain("/slicer-instances/{id}/open-accepted-plates");

    const health = await app.inject({ method: "GET", url: "/health" });
    const capabilities = health.json().capabilities as string[];
    expect(capabilities).toEqual(
      expect.arrayContaining(["accepted_plate_revisions", "accepted_plate_export"]),
    );
    expect(capabilities).not.toEqual(
      expect.arrayContaining(["export-3mf", "pack-preview", "auto-slice"]),
    );
  });

  it("returns stable boundary codes for missing and malformed Plan IDs", async () => {
    const { app } = await fixture();

    const missing = await app.inject({ method: "GET", url: "/plans/999999/plates" });
    expect(missing.statusCode).toBe(404);
    expect(missing.json()).toEqual({ detail: "Plan not found", code: "profile_not_found" });

    const malformed = await app.inject({ method: "GET", url: "/plans/not-a-number/plates" });
    expect(malformed.statusCode).toBe(400);
    expect(malformed.json()).toEqual({ detail: "Request is invalid", code: "invalid_request" });
  });

  it("requires the configured API key for a remote v2 request", async () => {
    const { app, profile } = await fixture("accepted-plate-key");

    const denied = await app.inject({
      method: "GET",
      url: `/api/v2/plans/${profile.id}/plates`,
      remoteAddress: "203.0.113.10",
    });
    const allowed = await app.inject({
      method: "GET",
      url: `/api/v2/plans/${profile.id}/plates`,
      remoteAddress: "203.0.113.10",
      headers: { authorization: "Bearer accepted-plate-key" },
    });

    expect(denied.statusCode).toBe(401);
    expect(allowed.statusCode).toBe(200);
  });

  it("returns 503 on PostgreSQL without issuing an accepted Plate query", async () => {
    const postgres = drizzle({} as Pool, { schema: pgSchema });
    const statements: string[] = [];
    registerPostgresSyncQuery(postgres, ({ sql }) => {
      statements.push(sql);
      return { rows: [], rowCount: 0 };
    });
    const repo = new AppRepository(
      postgres,
      "default",
      "/tmp/unused-accepted-plate-routes",
      pgSchema as unknown as SchemaTables,
    );
    const app = Fastify();
    await registerAcceptedPlateRoutes(app, {
      repo,
      reposDir: "/tmp/unused-accepted-plate-routes",
    });
    await app.ready();
    try {
      const response = await app.inject({ method: "GET", url: "/plans/1/plates" });
      expect(response.statusCode).toBe(503);
      expect(response.json()).toEqual({
        detail: "Accepted Plate update is unavailable",
        code: "accepted_plate_update_unavailable",
      });
      expect(statements).toEqual([]);
    } finally {
      await app.close();
      unregisterPostgresSyncQuery(postgres);
    }
  });

  it("initializes exact tracked geometry, replays unchanged, and persists a valid move", async () => {
    const { app, profile, basis, token } = await trackedFixture();
    const expected = {
      profile_id: basis.profileId,
      plan_version: basis.planVersion,
      plan_revision_id: basis.revisionId,
      plan_revision_digest: basis.revisionDigest,
      required_unit_mapping_digest: basis.requiredUnitMappingDigest,
    };
    const payload = {
      expected,
      expected_plate_revision_id: null,
      assignments: [{ token, printer_id: "printer-one" }],
    };

    const malformed = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/plates/initialize`,
      payload: { ...payload, assignments: [{ token: "bad", printer_id: "printer-one" }] },
    });
    expect(malformed.statusCode).toBe(400);
    expect(malformed.json()).toEqual({ detail: "Request is invalid", code: "invalid_request" });

    const unassigned = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/plates/initialize`,
      payload: { ...payload, assignments: [{ token, printer_id: null }] },
    });
    expect(unassigned.statusCode).toBe(422);
    expect(unassigned.json()).toEqual({
      detail: "Printer assignments are incomplete",
      code: "unassigned_units",
      tokens: [token],
    });
    const stillSetup = await app.inject({ method: "GET", url: `/plans/${profile.id}/plates` });
    expect(stillSetup.json()).toMatchObject({ kind: "setup", expected_plate_revision_id: null });

    const initialized = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/plates/initialize`,
      payload,
    });
    expect(initialized.statusCode).toBe(200);
    const ready = initialized.json();
    expect(ready).toMatchObject({
      kind: "ready",
      plate_revision_number: 1,
      plates: [
        {
          printer: { id: "printer-one", model: "Model One" },
          units: [
            {
              token,
              x_um: 4_000,
              y_um: 4_000,
              width_um: 30_000,
              depth_um: 20_000,
              height_um: 10_000,
            },
          ],
        },
      ],
    });

    const replay = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/plates/initialize`,
      payload,
    });
    expect(replay.statusCode).toBe(200);
    expect(replay.json()).toEqual(ready);

    const plate = ready.plates[0];
    const moved = await app.inject({
      method: "PATCH",
      url: `/plans/${profile.id}/plates/${plate.plate_id}/units/${token}`,
      payload: {
        expected,
        expected_plate_revision_id: ready.plate_revision_id,
        x_um: 50_000,
        y_um: 40_000,
      },
    });
    expect(moved.statusCode).toBe(200);
    expect(moved.json()).toMatchObject({ plate_revision_number: 2 });

    const reloaded = await app.inject({ method: "GET", url: `/plans/${profile.id}/plates` });
    expect(reloaded.json()).toMatchObject({
      kind: "ready",
      plate_revision_id: moved.json().plate_revision_id,
      plates: [{ units: [{ token, x_um: 50_000, y_um: 40_000 }] }],
    });

    const missingUnit = await app.inject({
      method: "PATCH",
      url: `/plans/${profile.id}/plates/${plate.plate_id}/units/ppu_${"f".repeat(32)}`,
      payload: {
        expected,
        expected_plate_revision_id: moved.json().plate_revision_id,
        x_um: 50_000,
        y_um: 40_000,
      },
    });
    expect(missingUnit.statusCode).toBe(422);
    expect(missingUnit.json()).toEqual({
      detail: "Accepted Plate unit is invalid",
      code: "unit_not_found",
    });

    const outside = await app.inject({
      method: "PATCH",
      url: `/plans/${profile.id}/plates/${plate.plate_id}/units/${token}`,
      payload: {
        expected,
        expected_plate_revision_id: moved.json().plate_revision_id,
        x_um: 0,
        y_um: 0,
      },
    });
    expect(outside.statusCode).toBe(422);
    expect(outside.json()).toEqual({
      detail: "Accepted Plate geometry is invalid",
      code: "outside_build_area",
    });
  });

  it("returns a stable 422 code for parseable degenerate accepted geometry", async () => {
    const { app, profile, basis, token } = await trackedFixture(`solid degenerate
facet normal 0 0 1
outer loop
vertex 0 0 0
vertex 30 0 0
vertex 0 20 0
endloop
endfacet
endsolid degenerate`);

    const response = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/plates/initialize`,
      payload: {
        expected: {
          profile_id: basis.profileId,
          plan_version: basis.planVersion,
          plan_revision_id: basis.revisionId,
          plan_revision_digest: basis.revisionDigest,
          required_unit_mapping_digest: basis.requiredUnitMappingDigest,
        },
        expected_plate_revision_id: null,
        assignments: [{ token, printer_id: "printer-one" }],
      },
    });

    expect(response.statusCode).toBe(422);
    expect(response.json()).toEqual({
      detail: "Accepted artifact geometry cannot be arranged",
      code: "degenerate_geometry",
      token,
    });
    const workspace = await app.inject({ method: "GET", url: `/plans/${profile.id}/plates` });
    expect(workspace.json()).toMatchObject({ kind: "setup", expected_plate_revision_id: null });
  });

  it("transfers a unit to a new Plate on the selected Printer", async () => {
    const { app, profile, basis, token } = await trackedFixture();
    const expected = {
      profile_id: basis.profileId,
      plan_version: basis.planVersion,
      plan_revision_id: basis.revisionId,
      plan_revision_digest: basis.revisionDigest,
      required_unit_mapping_digest: basis.requiredUnitMappingDigest,
    };
    const initialized = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/plates/initialize`,
      payload: {
        expected,
        expected_plate_revision_id: null,
        assignments: [{ token, printer_id: "printer-one" }],
      },
    });
    const ready = initialized.json() as {
      plate_revision_id: number;
      plates: Array<{ plate_id: string }>;
    };
    const plateId = ready.plates[0]?.plate_id;
    if (!plateId) throw new Error("initialized Plate is missing");
    const transferred = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/plates/${plateId}/units/${token}/transfer`,
      payload: {
        expected,
        expected_plate_revision_id: ready.plate_revision_id,
        target_printer_id: "printer-two",
      },
    });
    expect(transferred.statusCode).toBe(200);
    const workspace = await app.inject({ method: "GET", url: `/plans/${profile.id}/plates` });
    expect(workspace.json()).toMatchObject({
      kind: "ready",
      plates: [{ printer: { id: "printer-two" } }],
      unplaced: [{ token, printer_id: "printer-two" }],
    });
    expect(workspace.json().plates[0].plate_id).not.toBe(plateId);
  });
});
