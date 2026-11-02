import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify from "fastify";
import { drizzle } from "drizzle-orm/node-postgres";
import pg from "pg";
import { expect, it } from "vitest";
import { AppRepository, type SchemaTables } from "../db/repository.js";
import * as pgSchema from "../db/schema-pg.js";
import { registerPostgresSyncQuery, unregisterPostgresSyncQuery } from "../db/sync-db-bridge.js";
import { registerSourceRoutes } from "../routes/sources.js";
import { registerRepoManifestRoutes } from "../routes/repo-manifest.js";
import { applyAssistantAction } from "../assistant/tools.js";
import { InProcessJobRunner } from "./job-runner.js";
import {
  publishLocalSourceWorkingTree,
  publishSourceManifestRevision,
  SOURCE_PUBLICATION_UNAVAILABLE_DETAIL,
} from "./local-source-revision.js";

it("rejects PostgreSQL uploads and manifest publication before SQL or filesystem mutation", async () => {
  const dir = mkdtempSync(join(tmpdir(), "pp-pg-publication-"));
  const reposDir = join(dir, "repos");
  const sourcesDir = join(dir, "sources");
  mkdirSync(sourcesDir);
  writeFileSync(join(sourcesDir, "previous.stl"), "previous bytes");
  const pool = new pg.Pool();
  const postgres = drizzle(pool, { schema: pgSchema });
  const statements: string[] = [];
  registerPostgresSyncQuery(postgres, ({ sql }) => {
    statements.push(sql);
    throw new Error("Publication must stop before the database bridge");
  });
  const repo = new AppRepository(
    postgres,
    "default",
    reposDir,
    pgSchema as unknown as SchemaTables,
  );
  const app = Fastify();
  const jobs = new InProcessJobRunner({
    getRepo: () => repo,
    reposDir,
    exportsDir: join(dir, "exports"),
    dataDir: dir,
  });
  try {
    await registerSourceRoutes(app, {
      repo,
      reposDir,
      sourcesDir,
      dataDir: dir,
      hostedPlanning: false,
      thumbsDir: join(dir, "thumbs"),
      coversDir: join(dir, "covers"),
    });
    await registerRepoManifestRoutes(app, { repo });
    for (const endpoint of ["upload-files", "upload-zip"]) {
      const result = await app.inject({
        method: "POST",
        url: `/sources/1/${endpoint}`,
        payload: {},
      });
      expect(result.statusCode).toBe(503);
      expect(result.json()).toEqual({ detail: SOURCE_PUBLICATION_UNAVAILABLE_DETAIL });
    }
    const manifest = await app.inject({
      method: "PUT",
      url: "/sources/1/repo-manifest",
      payload: { yaml: "project: test\nparts: []\n" },
    });
    expect(manifest.statusCode).toBe(503);
    expect(manifest.json()).toEqual({ detail: SOURCE_PUBLICATION_UNAVAILABLE_DETAIL });
    const result = await applyAssistantAction(
      {
        id: "pg-upload",
        type: "propose_import_source_files",
        plan_id: 0,
        label: "Import",
        summary: "Import",
        params: { source_id: 1, files: [] },
      },
      { repo, jobs, dataDir: dir, sourcesDir },
    );
    expect(result).toMatchObject({ ok: false, status: 503 });
    await expect(
      publishLocalSourceWorkingTree({
        repo,
        reposDir,
        sourceId: 1,
        workingTree: sourcesDir,
      }),
    ).rejects.toThrow("atomic publication transactions are not supported");
    await expect(
      publishSourceManifestRevision({
        repo,
        sourceId: 1,
        manifestYaml: "project: test\nparts: []\n",
      }),
    ).rejects.toThrow("atomic publication transactions are not supported");
    expect(statements).toEqual([]);
    expect(readdirSync(dir)).toEqual(["sources"]);
    expect(readdirSync(sourcesDir)).toEqual(["previous.stl"]);
    expect(readFileSync(join(sourcesDir, "previous.stl"), "utf8")).toBe("previous bytes");
  } finally {
    await app.close();
    unregisterPostgresSyncQuery(postgres);
    await pool.end();
    rmSync(dir, { recursive: true, force: true });
  }
});
