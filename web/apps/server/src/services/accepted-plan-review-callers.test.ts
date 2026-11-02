import Fastify from "fastify";
import Database from "better-sqlite3";
import { existsSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppRepository } from "../db/repository.js";
import { getDb, SqliteDatabase } from "../db/client.js";
import {
  AcceptedPlanOperationalIntegrityError,
  type AcceptedPlanOperationalSnapshot,
  type ReadAcceptedPlanOperationalSnapshotResult,
} from "../db/accepted-plan-operational.js";
import { invokeAssistantTool } from "../assistant/tools.js";
import { registerPlanRoutes } from "../routes/plans.js";
import { summarizeAcceptedPlanReview } from "./accepted-plan-review.js";

const apps: ReturnType<typeof Fastify>[] = [];

function acceptedSnapshot(name = "Accepted Plan"): AcceptedPlanOperationalSnapshot {
  const part = (input: {
    revisionPartId: number;
    projectionPartId: number;
    filename: string;
    included: boolean;
  }) => ({
    revisionPartId: input.revisionPartId,
    projectionPartId: input.projectionPartId,
    partKey: input.filename,
    relativePath: input.filename,
    filename: input.filename,
    sourceLayer: "source:Accepted Source",
    status: "ok",
    roleInferred: "primary",
    roleOverride: null,
    effectiveRole: "primary",
    filamentColorId: null,
    filamentCustomHex: null,
    spoolmanSpoolId: null,
    quantityInferred: 1,
    quantityOverride: null,
    quantityEffective: 1,
    included: input.included,
    notes: "",
    githubBlobUrl: null,
    geometrySame: null,
    requirement: null,
    optionGroupId: null,
    manifestSource: null,
    artifact: { kind: "unavailable" as const, reason: "legacy" as const },
    units: [
      {
        unitIndex: 0,
        required: true,
        token: `part:${input.projectionPartId}:0`,
        objectName: input.filename,
        completed: false,
        assembled: false,
      },
    ],
  });
  return {
    format: "accepted-plan-operational-v1",
    profile: {
      id: 7,
      name,
      orderNumber: null,
      specialRequest: null,
      archivedAt: null,
    },
    planVersion: 1,
    revisionId: 1,
    revisionNumber: 1,
    revisionDigest: "a".repeat(64),
    acceptedAt: "2026-08-21T00:00:00.000Z",
    provenance: { kind: "legacy" },
    requiredUnitMappingDigest: "b".repeat(64),
    parts: [
      part({ revisionPartId: 1, projectionPartId: 20, filename: "z.stl", included: false }),
      part({ revisionPartId: 2, projectionPartId: 10, filename: "a.stl", included: true }),
    ],
  };
}

function repository(
  profile: { id: number; name: string } | null = { id: 7, name: "Plan" },
  accepted:
    | ReadAcceptedPlanOperationalSnapshotResult
    | (() => ReadAcceptedPlanOperationalSnapshotResult) = {
    kind: "empty",
  },
) {
  const mutations = {
    assignAcceptedFilament: vi.fn(),
  };
  return {
    reposDir: "/unused/repos",
    getOwnedProfileIdentity: vi.fn(() => profile),
    getSetting: vi.fn(() => null),
    listParts: vi.fn(() => {
      throw new Error("working Part reader must not run");
    }),
    getProfileLayers: vi.fn(() => {
      throw new Error("working layer reader must not run");
    }),
    getGlobalNaming: vi.fn(() => {
      throw new Error("working naming reader must not run");
    }),
    readAcceptedPlanOperationalSnapshot: vi.fn(() =>
      typeof accepted === "function" ? accepted() : accepted,
    ),
    ...mutations,
  } as unknown as AppRepository;
}

afterEach(async () => {
  for (const app of apps.splice(0)) await app.close();
});

describe("accepted Plan Review callers", () => {
  it("leaves database tables and the accepted cache unchanged for both callers", async () => {
    const dataDir = mkdtempSync(join(tmpdir(), "print-partner-review-no-write-"));
    const database = new SqliteDatabase(dataDir);
    database.connect();
    const repo = new AppRepository(getDb(database), undefined, database.reposDir);
    const profile = repo.createProfile("No write");
    const thumbsDir = join(dataDir, "thumbs");
    const raw = new Database(join(dataDir, "print-partner.db"), { readonly: true });
    const snapshotTables = () => {
      const tableNames = raw
        .prepare(
          "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .pluck()
        .all() as string[];
      return tableNames.map((name) => ({
        name,
        rows: raw.prepare(`SELECT * FROM "${name}"`).all(),
      }));
    };
    const before = snapshotTables();
    const cacheSnapshot = () => (existsSync(thumbsDir) ? readdirSync(thumbsDir).sort() : null);
    const cacheBefore = cacheSnapshot();
    const app = Fastify({ logger: false });
    apps.push(app);
    await registerPlanRoutes(app, {
      repo,
      dataDir,
      reposDir: database.reposDir,
      thumbsDir,
    });

    try {
      const http = await app.inject({ method: "GET", url: `/plans/${profile.id}/review` });
      const assistant = await invokeAssistantTool(
        "get_plan_review",
        { plan_id: profile.id },
        { repo, activePlanId: null, dataDir, thumbsDir },
      );

      expect(http.statusCode).toBe(200);
      expect(JSON.parse(assistant.content).plan_id).toBe(profile.id);
      expect(snapshotTables()).toEqual(before);
      expect(cacheSnapshot()).toEqual(cacheBefore);
    } finally {
      raw.close();
      await app.close();
      apps.splice(apps.indexOf(app), 1);
      database.close();
      rmSync(dataDir, { recursive: true, force: true });
    }
  });

  it("HTTP returns the exact empty body after one ownership lookup and one accepted read", async () => {
    const app = Fastify({ logger: false });
    apps.push(app);
    const repo = repository();
    await registerPlanRoutes(app, {
      repo,
      dataDir: "/unused/data",
      reposDir: "/unused/repos",
      thumbsDir: "/unused/thumbs",
    });

    const response = await app.inject({ method: "GET", url: "/plans/7/review" });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toEqual({
      profile_id: 7,
      accepted_basis: null,
      plan_name: "Plan",
      layers: [],
      totals: { included_parts: 0, total_print_units: 0, by_role: {}, by_filament: {} },
      issues: [
        {
          severity: "blocker",
          code: "no_included_parts",
          message: "No parts are included in this build.",
          link_hint: "build",
        },
      ],
      has_blockers: true,
      part_groups: [],
    });
    expect(repo.getOwnedProfileIdentity).toHaveBeenCalledTimes(1);
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(1);
  });

  it("assistant summarizes the same body with one ownership lookup and one accepted read", async () => {
    const repo = repository();

    const response = await invokeAssistantTool(
      "get_plan_review",
      { plan_id: 7 },
      { repo, activePlanId: null },
    );

    expect(JSON.parse(response.content)).toEqual({
      plan_id: 7,
      plan_name: "Plan",
      has_blockers: true,
      blocker_count: 1,
      warning_count: 0,
      issue_codes: ["no_included_parts"],
      sample_issues: [
        {
          severity: "blocker",
          code: "no_included_parts",
          message: "No parts are included in this build.",
        },
      ],
      totals: { included_parts: 0, total_print_units: 0, by_role: {}, by_filament: {} },
      layers: [],
    });
    expect(repo.getOwnedProfileIdentity).toHaveBeenCalledTimes(1);
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(1);
  });

  it("keeps exact ready HTTP JSON, include_excluded variants, and production assistant parity", async () => {
    const snapshot = acceptedSnapshot();
    const repo = repository({ id: 7, name: "Working Plan" }, { kind: "ready", snapshot });
    const app = Fastify({ logger: false });
    apps.push(app);
    await registerPlanRoutes(app, {
      repo,
      dataDir: "/unused/data",
      reposDir: "/unused/repos",
      thumbsDir: "/unused/thumbs",
    });

    const standard = await app.inject({ method: "GET", url: "/plans/7/review" });
    const numeric = await app.inject({ method: "GET", url: "/plans/7/review?include_excluded=1" });
    const textual = await app.inject({
      method: "GET",
      url: "/plans/7/review?include_excluded=true",
    });
    const falseValue = await app.inject({
      method: "GET",
      url: "/plans/7/review?include_excluded=false",
    });

    expect(standard.statusCode).toBe(200);
    expect(standard.json()).toEqual({
      profile_id: 7,
      accepted_basis: {
        profile_id: 7,
        plan_version: 1,
        plan_revision_id: 1,
        plan_revision_digest: "a".repeat(64),
        required_unit_mapping_digest: "b".repeat(64),
      },
      plan_name: "Accepted Plan",
      layers: [],
      totals: {
        included_parts: 1,
        total_print_units: 1,
        by_role: { primary: 1 },
        by_filament: { Unassigned: 1 },
      },
      issues: [
        {
          severity: "blocker",
          code: "missing_stl",
          message: "STL not found on disk: a.stl",
          link_hint: "sources",
        },
      ],
      has_blockers: true,
      part_groups: [
        {
          folder: "(root)",
          source_layer: "source:Accepted Source",
          parts: [
            {
              id: 10,
              match_key: "a.stl",
              relative_path: "a.stl",
              filename: "a.stl",
              source_layer: "source:Accepted Source",
              status: "ok",
              role: "primary",
              requirement: null,
              option_group_id: null,
              included: true,
              filament_color_id: null,
              filament_custom_hex: null,
              spoolman_spool_id: null,
              filament_display: "",
              filament_hex: null,
              quantity_auto: 1,
              quantity_override: null,
              quantity_effective: 1,
              printed_count: 0,
              print_units: [false],
              assembled_units: [false],
              missing: true,
              stl_missing: true,
              thumb_empty: false,
            },
          ],
        },
      ],
    });
    expect(numeric.json().part_groups[0].parts.map((part: { id: number }) => part.id)).toEqual([
      10, 20,
    ]);
    expect(textual.json()).toEqual(numeric.json());
    expect(falseValue.json()).toEqual(standard.json());

    const assistant = await invokeAssistantTool(
      "get_plan_review",
      { plan_id: 7 },
      {
        repo,
        activePlanId: null,
        dataDir: "/unused/data",
        thumbsDir: "/unused/thumbs",
      },
    );
    expect(JSON.parse(assistant.content)).toEqual(summarizeAcceptedPlanReview(standard.json()));
    expect(repo.listParts).not.toHaveBeenCalled();
    expect(repo.getProfileLayers).not.toHaveBeenCalled();
    expect(repo.getGlobalNaming).not.toHaveBeenCalled();
  });

  it.each([
    ["compatibility_dirty", "Accepted Plan requires compatibility repair"],
    ["uninitialized", "Accepted Plan operational state is not initialized"],
  ] as const)("maps %s exactly for HTTP and assistant", async (kind, detail) => {
    const routeRepo = repository(undefined, { kind });
    const app = Fastify({ logger: false });
    apps.push(app);
    await registerPlanRoutes(app, {
      repo: routeRepo,
      dataDir: "/unused/data",
      reposDir: "/unused/repos",
      thumbsDir: "/unused/thumbs",
    });

    const response = await app.inject({ method: "GET", url: "/plans/7/review" });
    expect(response.statusCode).toBe(409);
    expect(response.json()).toEqual({ detail });

    const assistantRepo = repository(undefined, { kind });
    const assistant = await invokeAssistantTool(
      "get_plan_review",
      { plan_id: 7 },
      { repo: assistantRepo, activePlanId: null },
    );
    expect(JSON.parse(assistant.content)).toEqual({ error: detail });
  });

  it.each([
    [
      "integrity",
      () => new AcceptedPlanOperationalIntegrityError("revision", "sentinel /private/root"),
      500,
      { detail: "Accepted Plan data is inconsistent" },
      { error: "Accepted Plan data is inconsistent" },
      { code: "revision", profileId: 7 },
    ],
    [
      "unexpected",
      () => new Error("sentinel /private/root digest-aaaaaaaa filename.stl cache.png"),
      500,
      { detail: "Internal Server Error" },
      { error: "Internal Server Error" },
      { failure: "unexpected", profileId: 7 },
    ],
  ] as const)(
    "redacts %s Review failures at both boundaries",
    async (_label, failure, status, httpBody, assistantBody, logFields) => {
      const routeRepo = repository();
      vi.mocked(routeRepo.readAcceptedPlanOperationalSnapshot).mockImplementation(() => {
        throw failure();
      });
      const app = Fastify({ logger: false });
      apps.push(app);
      let logSpy: ReturnType<typeof vi.spyOn> | null = null;
      app.addHook("onRequest", async (request) => {
        logSpy = vi.spyOn(request.log, "error").mockImplementation(() => undefined);
      });
      await registerPlanRoutes(app, {
        repo: routeRepo,
        dataDir: "/unused/data",
        reposDir: "/unused/repos",
        thumbsDir: "/unused/thumbs",
      });

      const response = await app.inject({ method: "GET", url: "/plans/7/review" });
      expect(response.statusCode).toBe(status);
      expect(response.json()).toEqual(httpBody);
      expect(logSpy).not.toBeNull();
      expect(logSpy?.mock.calls[0]?.[0]).toEqual(logFields);
      const publicAndLog = `${response.body}\n${JSON.stringify(logSpy?.mock.calls)}`;
      for (const secret of [
        "sentinel",
        "/private/root",
        "digest-aaaaaaaa",
        "filename.stl",
        "cache.png",
        "stack",
      ]) {
        expect(publicAndLog).not.toContain(secret);
      }

      const assistantRepo = repository();
      vi.mocked(assistantRepo.readAcceptedPlanOperationalSnapshot).mockImplementation(() => {
        throw failure();
      });
      const assistant = await invokeAssistantTool(
        "get_plan_review",
        { plan_id: 7 },
        { repo: assistantRepo, activePlanId: null },
      );
      expect(JSON.parse(assistant.content)).toEqual(assistantBody);
      expect(assistant.content).not.toContain("sentinel");
      expect(assistant.content).not.toContain("/private/root");
    },
  );

  it("returns coherent old then new accepted Reviews with one read each and no writes", async () => {
    const snapshots = [acceptedSnapshot("Old accepted"), acceptedSnapshot("New accepted")];
    let index = 0;
    const repo = repository(undefined, () => ({ kind: "ready", snapshot: snapshots[index++]! }));
    const before = JSON.stringify(snapshots);

    const oldResult = await invokeAssistantTool(
      "get_plan_review",
      { plan_id: 7 },
      { repo, activePlanId: null },
    );
    const newResult = await invokeAssistantTool(
      "get_plan_review",
      { plan_id: 7 },
      { repo, activePlanId: null },
    );

    expect(JSON.parse(oldResult.content).plan_name).toBe("Old accepted");
    expect(JSON.parse(newResult.content).plan_name).toBe("New accepted");
    expect(repo.readAcceptedPlanOperationalSnapshot).toHaveBeenCalledTimes(2);
    expect(repo.assignAcceptedFilament).not.toHaveBeenCalled();
    expect(JSON.stringify(snapshots)).toBe(before);
  });

  it("missing ownership performs no accepted read for both callers", async () => {
    const app = Fastify({ logger: false });
    apps.push(app);
    const routeRepo = repository(null);
    await registerPlanRoutes(app, {
      repo: routeRepo,
      dataDir: "/unused/data",
      reposDir: "/unused/repos",
      thumbsDir: "/unused/thumbs",
    });

    const response = await app.inject({ method: "GET", url: "/plans/7/review" });
    expect(response.statusCode).toBe(404);
    expect(response.json()).toEqual({ detail: "Profile not found" });
    expect(routeRepo.readAcceptedPlanOperationalSnapshot).not.toHaveBeenCalled();

    const assistantRepo = repository(null);
    const assistant = await invokeAssistantTool(
      "get_plan_review",
      { plan_id: 7 },
      { repo: assistantRepo, activePlanId: null },
    );
    expect(JSON.parse(assistant.content)).toEqual({ error: "Plan not found" });
    expect(assistantRepo.readAcceptedPlanOperationalSnapshot).not.toHaveBeenCalled();
  });
});
