import { StatusExecution } from "../integrations/status-execution.js";
import { acceptPlanForTest } from "../test/accept-plan.js";
import Fastify from "fastify";
import { mkdtempSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { drizzle } from "drizzle-orm/node-postgres";
import type { Pool } from "pg";
import { eq } from "drizzle-orm";
import { miloFilenameGrouping } from "@print-partner/contracts";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AcceptedPlanOperationalIntegrityError } from "../db/accepted-plan-operational.js";
import { AppRepository, type SchemaTables } from "../db/repository.js";
import * as schema from "../db/schema.js";
import * as pgSchema from "../db/schema-pg.js";
import { registerPostgresSyncQuery, unregisterPostgresSyncQuery } from "../db/sync-db-bridge.js";
import { loadConfig } from "../config.js";
import { AuthStore } from "../services/auth-store.js";
import { getLogger } from "../services/logger.js";
import { InProcessJobRunner } from "../services/job-runner.js";
import { TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";
import { registerRequestLoggingMiddleware } from "../middleware/request-logging.js";
import { registerJobRoutes } from "./jobs.js";
import { registerShareRoutes } from "./shares.js";

const webhookCapture = vi.hoisted(() => ({
  events: [] as Array<{ event: string; payload: Record<string, unknown> }>,
}));

vi.mock("../services/webhook-store.js", () => ({
  dispatchWebhooks: async (
    _repository: unknown,
    event: string,
    payload: Record<string, unknown>,
  ) => {
    webhookCapture.events.push({ event, payload });
  },
}));

const cleanups: Array<() => Promise<void> | void> = [];

afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) await cleanup();
  webhookCapture.events.splice(0);
  vi.restoreAllMocks();
});

function fixture(recompute = true) {
  const dir = mkdtempSync(join(tmpdir(), "print-partner-operational-routes-"));
  const sqlite = new SqliteDatabase(dir);
  sqlite.connect();
  const db = getDb(sqlite);
  const repo = new AppRepository(db, undefined, sqlite.reposDir);
  const source = repo.createSource({ name: "Source", url: "https://example.test/source" });
  const sourceRoot = join(dir, "repos", String(source.id));
  mkdirSync(sourceRoot, { recursive: true });
  writeFileSync(join(sourceRoot, "part.stl"), "solid");
  repo.updateSource(source.id, { local_path: sourceRoot });
  repo.updateImportRules(source.id, ["part.stl"]);
  const profile = repo.createProfile("Build", source.id);
  if (recompute) acceptPlanForTest(repo, profile.id);
  cleanups.push(() => {
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });
  return { dir, db, repo, source, profile };
}

function applyTrackedPlan(repo: AppRepository, sourceId: number, profileId: number): void {
  const observed = repo.getProjectRow(sourceId);
  if (!observed) throw new Error("Source is missing");
  const revision = repo.recordSourceRevision({
    sourceId,
    upstreamRevisionKey: `accepted-export-${profileId}`,
    manifestDigest: "d".repeat(64),
    snapshotLocator: String(sourceId),
    syncedAt: "2026-08-21T12:00:00.000Z",
    completeness: "complete",
  });
  repo.activateSourceRevision({
    sourceId,
    revisionId: revision.id,
    observed,
    sourceVersion: revision.upstream_revision_key,
  });
  const draft = repo.recomputePlanDraft({
    profileId,
    actor: "test:user",
    idempotencyKey: `accepted-export-draft-${profileId}`,
  });
  if (draft.kind !== "created") throw new Error("Plan draft was not created");
  const reconciled = repo.savePlanDraftRequiredUnitReconciliation({
    profileId,
    draftId: draft.draft.id,
    expectedSnapshotDigest: draft.draft.snapshotDigest,
    decisions: [],
    actorId: "test:user",
    idempotencyKey: `accepted-export-reconcile-${profileId}`,
  });
  if (reconciled.kind !== "saved") throw new Error("Plan reconciliation failed");
  const applied = repo.applyPlanChanges({
    profileId,
    draftId: draft.draft.id,
    expectedSnapshotDigest: reconciled.draft.snapshotDigest,
    expectedLifecycleVersion: 0,
    expectedBase: { kind: "empty", planVersion: 0 },
    actorId: "test:user",
    idempotencyKey: `accepted-export-apply-${profileId}`,
  });
  if (applied.kind !== "applied") throw new Error("Plan was not applied");
}

describe("accepted operational export routes", () => {
  it.each(["compatibility_dirty", "uninitialized"] as const)(
    "returns unavailable accepted state from filename grouping: %s",
    async (kind) => {
      const { dir, repo, profile } = fixture();
      vi.spyOn(repo, "readAcceptedPlanOperationalSnapshot").mockReturnValue({ kind });
      const runner = new InProcessJobRunner({
        getRepo: () => repo,
        reposDir: join(dir, "repos"),
        exportsDir: join(dir, "exports"),
        dataDir: dir,
      });
      const app = Fastify();
      cleanups.push(() => app.close());
      await registerJobRoutes(app, runner, { statusExecution: new StatusExecution() });

      const response = await app.inject(`/plans/${profile.id}/filename-grouping`);

      expect(response.statusCode).toBe(409);
      expect(response.json()).toEqual({
        code: "accepted_state_unavailable",
        detail: "Accepted Plan state is unavailable. Apply or repair the Plan, then export again.",
      });
    },
  );

  it("returns a sanitized accepted integrity failure from filename grouping", async () => {
    const { dir, repo, profile } = fixture();
    vi.spyOn(repo, "readAcceptedPlanOperationalSnapshot").mockImplementation(() => {
      throw new AcceptedPlanOperationalIntegrityError(
        "progress",
        "private SQL and token ppu_secret",
      );
    });
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: join(dir, "repos"),
      exportsDir: join(dir, "exports"),
      dataDir: dir,
    });
    const app = Fastify();
    cleanups.push(() => app.close());
    await registerJobRoutes(app, runner, { statusExecution: new StatusExecution() });

    const response = await app.inject(`/plans/${profile.id}/filename-grouping`);

    expect(response.statusCode).toBe(500);
    expect(response.json()).toEqual({
      code: "accepted_integrity",
      detail: "Accepted Plan export failed integrity verification.",
    });
    expect(response.body).not.toContain("ppu_secret");
    expect(response.body).not.toContain("private SQL");
  });

  it("redacts unexpected filename grouping read errors from the response and request logs", async () => {
    const { dir, repo, profile } = fixture();
    const sentinel = "private-path-/secret/token-ppu_grouping_leak";
    vi.spyOn(repo, "readAcceptedPlanOperationalSnapshot").mockImplementation(() => {
      throw new Error(sentinel);
    });
    const workflowLog = vi.spyOn(getLogger(), "logWorkflow").mockImplementation(() => undefined);
    const requestLog: string[] = [];
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: join(dir, "repos"),
      exportsDir: join(dir, "exports"),
      dataDir: dir,
    });
    const app = Fastify({
      logger: { level: "error", stream: { write: (line) => requestLog.push(line) } },
    });
    cleanups.push(() => app.close());
    await registerRequestLoggingMiddleware(app);
    await registerJobRoutes(app, runner, { statusExecution: new StatusExecution() });

    const response = await app.inject(`/plans/${profile.id}/filename-grouping`);

    expect(response.statusCode).toBe(500);
    expect.soft(response.json()).toEqual({
      code: "unexpected",
      detail: "Accepted Plan export failed.",
    });
    expect.soft(response.body).not.toContain(sentinel);
    expect.soft(requestLog.join("\n")).not.toContain(sentinel);
    expect.soft(JSON.stringify(workflowLog.mock.calls)).not.toContain(sentinel);
    expect(workflowLog).toHaveBeenCalledWith(
      expect.objectContaining({
        method: "GET",
        url: `/plans/${profile.id}/filename-grouping`,
        statusCode: 500,
      }),
    );
  });

  it.each([false, true])(
    "preserves filename grouping success with accepted Parts: %s",
    async (apply) => {
      const { dir, repo, source, profile } = fixture(false);
      if (apply) applyTrackedPlan(repo, source.id, profile.id);
      const runner = new InProcessJobRunner({
        getRepo: () => repo,
        reposDir: join(dir, "repos"),
        exportsDir: join(dir, "exports"),
        dataDir: dir,
      });
      const app = Fastify();
      cleanups.push(() => app.close());
      await registerJobRoutes(app, runner, { statusExecution: new StatusExecution() });

      const response = await app.inject(`/plans/${profile.id}/filename-grouping`);

      expect(response.statusCode).toBe(200);
      const body = response.json();
      expect(body.definition).toEqual(miloFilenameGrouping);
      if (apply) {
        expect(body.parts).toEqual([
          {
            relativePath: "part.stl",
            sourceLayer: "base:Source",
            role: "primary",
            units: [{ token: expect.stringMatching(/^ppu_[0-9a-f]{32}$/), completed: false }],
          },
        ]);
      } else {
        expect(body.parts).toEqual([]);
      }
      expect((await app.inject("/plans/999999/filename-grouping")).statusCode).toBe(404);
    },
  );

  it.each([false, true])(
    "preserves quota errors in kit export jobs with print progress: %s",
    async (includePrintProgress) => {
      const { dir, repo, source, profile } = fixture(false);
      applyTrackedPlan(repo, source.id, profile.id);
      const runner = new InProcessJobRunner({
        getRepo: () => repo,
        reposDir: join(dir, "repos"),
        exportsDir: join(dir, "exports"),
        dataDir: dir,
        tenantDiskQuotaBytes: 128,
      });

      const jobId = await runner.start(
        "export-kit-bundle",
        { profile_id: profile.id, include_print_progress: includePrintProgress },
        "default",
      );
      const job = await runner.waitForTerminal(jobId, 20_000, "default");

      expect(job.status).toBe("error");
      expect(job.error).toBe(new TenantDiskQuotaError().message);
      expect(job.result).toBeNull();
      expect(
        readdirSync(join(dir, "exports"), { recursive: true, encoding: "utf8" }).filter(
          (entry) => entry.endsWith(".zip") || entry.endsWith(".tmp"),
        ),
      ).toEqual([]);
    },
  );

  it("returns the fixed explicit-progress job failure without private integrity detail", async () => {
    const { dir, repo, profile } = fixture();
    repo.readAcceptedPlanOperationalSnapshot = () => {
      throw new AcceptedPlanOperationalIntegrityError(
        "progress",
        "private SQL and token ppu_secret",
      );
    };
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: join(dir, "repos"),
      exportsDir: join(dir, "exports"),
      dataDir: dir,
    });

    const jobId = await runner.start(
      "export-kit-bundle",
      { profile_id: profile.id, include_print_progress: true },
      "default",
    );
    const job = await runner.waitForTerminal(jobId, 20_000, "default");

    expect(job.status).toBe("error");
    expect(job.error).toBe("Accepted Plan export failed integrity verification.");
    expect(JSON.stringify(job)).not.toContain("ppu_secret");
    expect(JSON.stringify(job)).not.toContain("private SQL");
  });

  it("maps kit publication failures to the fixed output error", async () => {
    const { dir, repo, profile } = fixture();
    const blockedExports = join(dir, "blocked-exports");
    writeFileSync(blockedExports, "not a directory");
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: join(dir, "repos"),
      exportsDir: blockedExports,
      dataDir: dir,
    });

    const jobId = await runner.start(
      "export-kit-bundle",
      { profile_id: profile.id, include_print_progress: false },
      "default",
    );
    const job = await runner.waitForTerminal(jobId, 20_000, "default");

    expect(job.status).toBe("error");
    expect(job.error).toBe("Accepted Plan export could not be published safely.");
  });

  it("preserves successful checklist and explicit-progress kit result keys", async () => {
    const { dir, repo, source, profile } = fixture(false);
    applyTrackedPlan(repo, source.id, profile.id);
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: join(dir, "repos"),
      exportsDir: join(dir, "exports"),
      dataDir: dir,
    });

    const checklistId = await runner.start(
      "export-checklist-html",
      { profile_id: profile.id },
      "default",
    );
    const checklist = await runner.waitForTerminal(checklistId, 20_000, "default");
    expect(checklist.status).toBe("done");
    expect(Object.keys(checklist.result ?? {}).sort()).toEqual([
      "download_url",
      "part_count",
      "path",
      "plan_version",
      "revision_id",
      "thumb_count",
    ]);

    const kitId = await runner.start(
      "export-kit-bundle",
      { profile_id: profile.id, include_print_progress: true },
      "default",
    );
    const kit = await runner.waitForTerminal(kitId, 20_000, "default");
    expect(kit.status).toBe("done");
    expect(Object.keys(kit.result ?? {}).sort()).toEqual([
      "download_url",
      "path",
      "plan_version",
      "profile_id",
      "revision_id",
    ]);

    const stlId = await runner.start(
      "export-stl-pack",
      { profile_id: profile.id, missing_only: false, group_by: "color" },
      "default",
    );
    const stl = await runner.waitForTerminal(stlId, 20_000, "default");
    expect(stl.status).toBe("done");
    expect(Object.keys(stl.result ?? {}).sort()).toEqual([
      "download_url",
      "file_counts",
      "file_total",
      "missing_only",
      "plan_version",
      "revision_id",
      "root_path",
      "warnings",
      "zip_counts",
    ]);
  });

  it("redacts an unexpected sentinel from job state, webhooks, and logs", async () => {
    const { dir, repo, profile } = fixture();
    const sentinel = "private-path-/secret/token-ppu_leak";
    repo.readAcceptedPlanOperationalSnapshot = () => {
      throw new Error(sentinel);
    };
    const log = vi.spyOn(getLogger(), "log").mockImplementation(() => undefined);
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: join(dir, "repos"),
      exportsDir: join(dir, "exports"),
      dataDir: dir,
    });

    const jobId = await runner.start(
      "export-checklist-html",
      { profile_id: profile.id },
      "default",
    );
    const job = await runner.waitForTerminal(jobId, 20_000, "default");

    expect(job.status).toBe("error");
    expect(job.error).toBe("Accepted Plan export failed.");
    expect(JSON.stringify(job)).not.toContain(sentinel);
    expect(JSON.stringify(webhookCapture.events)).not.toContain(sentinel);
    expect(JSON.stringify(log.mock.calls)).not.toContain(sentinel);
    expect(webhookCapture.events).toEqual(
      expect.arrayContaining([expect.objectContaining({ event: "job.error" })]),
    );
  });

  it("maps a twice-changing PostgreSQL accepted read to fixed integrity failure", async () => {
    const postgres = drizzle({} as Pool, { schema: pgSchema });
    let terminalReads = 0;
    registerPostgresSyncQuery(postgres, ({ sql: query }) => {
      const normalized = query.toLowerCase();
      if (normalized.includes('left join "plan_accepted_input_sets"')) {
        terminalReads += 1;
        const version = terminalReads === 1 ? 0 : terminalReads < 4 ? 1 : 2;
        return { rows: [[null, version, null, null, null]], rowCount: 1 };
      }
      if (normalized.includes('from "build_profiles"')) {
        const version = terminalReads < 2 ? 0 : terminalReads < 4 ? 1 : 2;
        return {
          rows: [
            [1, "default", "PostgreSQL Build", null, null, null, null, null, null, null, version],
          ],
          rowCount: 1,
        };
      }
      return { rows: [], rowCount: 0 };
    });
    const repo = new AppRepository(
      postgres,
      "default",
      "/tmp/unused-operational-export",
      pgSchema as unknown as SchemaTables,
    );
    repo.getOwnedProfileIdentity = () => ({
      id: 1,
      name: "PostgreSQL Build",
      orderNumber: null,
      archivedAt: null,
    });
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: "/tmp/unused-operational-export",
      exportsDir: "/tmp/unused-operational-export",
      dataDir: "/tmp/unused-operational-export",
    });
    cleanups.push(() => unregisterPostgresSyncQuery(postgres));

    const jobId = await runner.start("export-checklist-html", { profile_id: 1 }, "default");
    const job = await runner.waitForTerminal(jobId, 20_000, "default");

    expect(job.status).toBe("error");
    expect(job.error).toBe("Accepted Plan export failed integrity verification.");
    expect(job.error).not.toContain("transaction_unavailable");
    expect(terminalReads).toBe(4);
  });

  it("exports after a stable terminal PostgreSQL accepted read", async () => {
    const postgres = drizzle({} as Pool, { schema: pgSchema });
    let terminalReads = 0;
    registerPostgresSyncQuery(postgres, ({ sql: query }) => {
      const normalized = query.toLowerCase();
      if (normalized.includes('left join "plan_accepted_input_sets"')) {
        terminalReads += 1;
        return { rows: [[null, 0, null, null, null]], rowCount: 1 };
      }
      if (normalized.includes('from "build_profiles"')) {
        return {
          rows: [[1, "default", "PostgreSQL Build", null, null, null, null, null, null, null, 0]],
          rowCount: 1,
        };
      }
      return { rows: [], rowCount: 0 };
    });
    const repo = new AppRepository(
      postgres,
      "default",
      "/tmp/unused-operational-export",
      pgSchema as unknown as SchemaTables,
    );
    repo.getOwnedProfileIdentity = () => ({
      id: 1,
      name: "PostgreSQL Build",
      orderNumber: null,
      archivedAt: null,
    });
    const root = mkdtempSync(join(tmpdir(), "print-partner-pg-operational-export-"));
    const runner = new InProcessJobRunner({
      getRepo: () => repo,
      reposDir: join(root, "repos"),
      exportsDir: join(root, "exports"),
      dataDir: root,
    });
    cleanups.push(() => {
      unregisterPostgresSyncQuery(postgres);
      rmSync(root, { recursive: true, force: true });
    });

    const jobId = await runner.start("export-checklist-html", { profile_id: 1 }, "default");
    const job = await runner.waitForTerminal(jobId, 20_000, "default");

    expect(job.status).toBe("done");
    expect(job.error).toBeNull();
    expect(terminalReads).toBe(2);
  });

  it("maps unavailable accepted share progress to 409 before creating a share", async () => {
    const { db, repo, profile } = fixture();
    const part = repo.listParts(profile.id).parts[0];
    if (!part) throw new Error("test Part is missing");
    db.update(schema.parts).set({ notes: "dirty" }).where(eq(schema.parts.id, part.id)).run();
    const authStore = new AuthStore(db);
    const createShare = vi.spyOn(authStore, "createPlanShare");
    const app = Fastify();
    app.decorateRequest("sessionUser", null);
    app.addHook("onRequest", async (request) => {
      request.sessionUser = {
        user_id: "user-1",
        tenant_id: "default",
        login: "sender@example.com",
        display_name: "Sender",
        email: "sender@example.com",
        provider: "email",
        is_admin: true,
      };
    });
    registerShareRoutes(app, {
      repo,
      authStore,
      config: { ...loadConfig(), multiUser: true },
    });
    cleanups.push(() => app.close());

    const response = await app.inject({
      method: "POST",
      url: `/plans/${profile.id}/shares`,
      payload: { include_print_progress: true },
    });

    expect(response.statusCode).toBe(409);
    expect(response.json()).toEqual({
      detail: "Accepted Plan state is unavailable. Apply or repair the Plan, then export again.",
    });
    expect(createShare).not.toHaveBeenCalled();
  });
});
