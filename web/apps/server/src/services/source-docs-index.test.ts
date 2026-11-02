import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import Fastify from "fastify";
import { eq } from "drizzle-orm";
import { afterEach, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import * as schema from "../db/schema.js";
import { registerSourceDocsRoutes } from "../routes/source-docs.js";
import { extractPendingPdfsForSource, indexSourceDocsFromDisk } from "./source-docs-index.js";
import * as pdf from "./pdf-text-extract.js";
import {
  beginSourceSyncAdmission,
  packagedSourceSyncManifestPath,
} from "../ownership/source-sync-admission.js";
import { createJobRunner } from "./job-runner.js";

const cleanups: Array<() => Promise<void> | void> = [];

afterEach(async () => {
  vi.restoreAllMocks();
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

async function fixture(indexed = true) {
  const directory = mkdtempSync(join(tmpdir(), "pp-doc-extraction-race-"));
  const sqlite = new SqliteDatabase(directory);
  sqlite.connect();
  cleanups.push(() => {
    sqlite.close();
    rmSync(directory, { recursive: true, force: true });
  });
  const db = getDb(sqlite);
  const repo = new AppRepository(db, undefined, sqlite.reposDir);
  const source = repo.createSource({ name: "Docs", url: "https://example.test/docs" });
  const oldRoot = join(directory, "old");
  const newRoot = join(directory, "new");
  for (const [root, contents] of [
    [oldRoot, "old PDF bytes"],
    [newRoot, "replacement PDF bytes"],
  ]) {
    mkdirSync(root, { recursive: true });
    writeFileSync(join(root, "manual.pdf"), contents);
  }
  repo.updateSource(source.id, { local_path: oldRoot });
  if (indexed) indexSourceDocsFromDisk(repo, source.id, oldRoot);
  const app = Fastify();
  cleanups.push(() => app.close());
  await registerSourceDocsRoutes(app, { repo });
  const docRow = () =>
    db.select().from(schema.sourceDocs).where(eq(schema.sourceDocs.projectId, source.id)).get();
  const replace = () => {
    repo.updateSource(source.id, { local_path: newRoot });
    indexSourceDocsFromDisk(repo, source.id, newRoot);
    return docRow();
  };
  return { repo, source, app, oldRoot, newRoot, docRow, replace };
}

function extracted(root: string, pageCount = 1): Awaited<ReturnType<typeof pdf.extractPdfText>> {
  return {
    status: "ready",
    hash: pdf.contentHashForFile(join(root, "manual.pdf")),
    pageCount,
    text: "extracted manual",
    chunks: [],
    cachePath: null,
  };
}

function deferredExtraction() {
  let resolve: ((result: Awaited<ReturnType<typeof pdf.extractPdfText>>) => void) | undefined;
  const promise = new Promise<Awaited<ReturnType<typeof pdf.extractPdfText>>>((onResolve) => {
    resolve = onResolve;
  });
  if (!resolve) throw new Error("Extraction promise was not initialized");
  return { promise, resolve };
}

it("refuses a deferred PDF job's database result after admission changes during parsing", async () => {
  const f = await fixture();
  const dataDir = dirname(f.repo.reposDir);
  const manifest = join(dataDir, "manifest.json");
  const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
  const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
  try {
    writeFileSync(manifest, original);
    process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
    beginSourceSyncAdmission(dataDir);
    const before = f.docRow();
    const extraction = deferredExtraction();
    const parse = vi.spyOn(pdf, "extractPdfText").mockImplementationOnce(() => extraction.promise);
    const jobs = createJobRunner(() => f.repo, dataDir);
    const jobId = await jobs.start("extract-source-docs", { project_id: f.source.id });
    await vi.waitFor(() => expect(parse).toHaveBeenCalledOnce());
    writeFileSync(manifest, `${original}\n`);
    extraction.resolve(extracted(f.oldRoot));
    await vi.waitFor(async () => {
      expect(await jobs.get(jobId, "default")).toMatchObject({
        status: "error",
        error: expect.stringMatching(/runtime manifest does not match/),
      });
    });
    expect(f.docRow()).toEqual(before);
  } finally {
    if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
    else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
  }
});

it.each(["background", "request"] as const)(
  "ignores a replaced PDF row when a late %s extraction completes",
  async (caller) => {
    const f = await fixture();
    const oldRow = f.docRow();
    const previous = deferredExtraction();
    const extract = vi.spyOn(pdf, "extractPdfText").mockReturnValueOnce(previous.promise);
    const pending =
      caller === "background"
        ? extractPendingPdfsForSource(f.repo, f.source.id, f.oldRoot)
        : f.app.inject(`/sources/${f.source.id}/docs/manual.pdf`);
    await vi.waitFor(() => expect(extract).toHaveBeenCalledTimes(1));
    const replacement = f.replace();
    expect(replacement?.id).not.toBe(oldRow?.id);

    previous.resolve(extracted(f.oldRoot));
    const result = await pending;

    expect(f.docRow()).toEqual(replacement);
    if (caller === "background") expect(result).toEqual({ extracted: 0, errors: 0 });
    extract.mockResolvedValueOnce(extracted(f.newRoot, 2));
    expect(await extractPendingPdfsForSource(f.repo, f.source.id, f.newRoot)).toEqual({
      extracted: 1,
      errors: 0,
    });
    expect(f.docRow()).toMatchObject({
      id: replacement?.id,
      extractStatus: "ready",
      pageCount: 2,
      contentHash: pdf.contentHashForFile(join(f.newRoot, "manual.pdf")),
    });
  },
);

it.each(["background", "request"] as const)(
  "updates an unchanged indexed PDF from a %s extraction",
  async (caller) => {
    const f = await fixture();
    const original = f.docRow();
    vi.spyOn(pdf, "extractPdfText").mockResolvedValueOnce(extracted(f.oldRoot, 3));

    if (caller === "background") {
      expect(await extractPendingPdfsForSource(f.repo, f.source.id, f.oldRoot)).toEqual({
        extracted: 1,
        errors: 0,
      });
    } else {
      const response = await f.app.inject(`/sources/${f.source.id}/docs/manual.pdf`);
      expect(response.statusCode).toBe(200);
      expect(response.json().markdown).toBe("extracted manual");
    }

    expect(f.docRow()).toMatchObject({ id: original?.id, extractStatus: "ready", pageCount: 3 });
  },
);

it("does not update a row indexed after an unindexed PDF request began", async () => {
  const f = await fixture(false);
  const previous = deferredExtraction();
  const extract = vi.spyOn(pdf, "extractPdfText").mockReturnValueOnce(previous.promise);
  const pending = f.app.inject(`/sources/${f.source.id}/docs/manual.pdf`);
  await vi.waitFor(() => expect(extract).toHaveBeenCalledTimes(1));
  const replacement = f.replace();

  previous.resolve(extracted(f.oldRoot));
  const response = await pending;

  expect(response.statusCode).toBe(200);
  expect(f.docRow()).toEqual(replacement);
});
