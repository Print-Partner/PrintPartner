import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
  realpathSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import Fastify from "fastify";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { registerRepoManifestRoutes } from "../routes/repo-manifest.js";
import { acceptPlanForTest } from "../test/accept-plan.js";
import {
  LocalSourceSnapshotStore,
  sourceRelativePath,
  type SnapshotFile,
} from "./local-source-snapshot.js";
import {
  findSourceManifestPath,
  legacySourceManifestOverridePath,
  revisionPdfTextCacheRoot,
  sourcePdfTextStorage,
} from "./source-workspace.js";

const cleanupDirs: string[] = [];

function createRepo() {
  const dataDir = realpathSync(mkdtempSync(join(tmpdir(), "pp-source-workspace-")));
  cleanupDirs.push(dataDir);
  const sqlite = new SqliteDatabase(dataDir);
  sqlite.connect();
  return {
    dataDir,
    sqlite,
    repo: new AppRepository(getDb(sqlite), undefined, sqlite.reposDir),
  };
}

async function activateTrackedSnapshot(input: {
  repo: AppRepository;
  sourceId: number;
  manifestYaml: string;
  sourceVersion?: string;
  revisionKey?: string;
}) {
  const sourceVersion = input.sourceVersion ?? "commit-a";
  const revisionKey = input.revisionKey ?? `${sourceVersion}.snapshot-v2`;
  const contents = new Map([
    ["cube.stl", Buffer.from("solid cube\nendsolid cube\n")],
    ["print-partner.manifest.yaml", Buffer.from(input.manifestYaml)],
  ]);
  const files: SnapshotFile[] = [...contents].map(([path, content]) => ({
    path: sourceRelativePath(path),
    kind: path.endsWith(".stl") ? "stl" : "artifact",
    sizeHintBytes: content.byteLength,
  }));
  const snapshot = await new LocalSourceSnapshotStore({
    reposDir: input.repo.reposDir,
  }).materialize({
    sourceId: input.sourceId,
    upstreamRevisionKey: revisionKey,
    files,
    selection: {
      maxStlFiles: 500,
      maxDocumentationBytes: 1024 * 1024,
      omittedFiles: [],
    },
    openFile: async (file) => {
      const content = contents.get(file.path);
      if (!content) throw new Error(`Missing test content: ${file.path}`);
      return {
        stream: Readable.from(content),
        contentLengthBytes: content.byteLength,
      };
    },
  });
  const revision = input.repo.recordSourceRevision({
    sourceId: input.sourceId,
    upstreamRevisionKey: snapshot.upstreamRevisionKey,
    manifestDigest: snapshot.manifestDigest,
    snapshotLocator: snapshot.snapshotLocator,
    syncedAt: new Date().toISOString(),
    completeness: "complete",
  });
  const observed = input.repo.getSourceActivationObservation(input.sourceId);
  if (!observed) throw new Error("Expected Source observation");
  input.repo.activateSourceRevision({
    sourceId: input.sourceId,
    revisionId: revision.id,
    observed,
    sourceVersion,
  });
  return { revision, snapshot };
}

afterEach(() => {
  for (const dir of cleanupDirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

describe("Source workspace derived paths", () => {
  it("keys PDF text by the active revision digest outside revision content", () => {
    const { dataDir, sqlite, repo } = createRepo();
    const source = repo.createSource({
      name: "Trident",
      url: "https://github.com/VoronDesign/Voron-Trident",
    });
    const digest = "a".repeat(64);
    const revisionRoot = join(dataDir, "repos", String(source.id), "revisions", "commit-a");
    const revision = repo.recordSourceRevision({
      sourceId: source.id,
      upstreamRevisionKey: "commit-a",
      manifestDigest: digest,
      snapshotLocator: `${source.id}/revisions/commit-a`,
      syncedAt: "2026-08-20T12:00:00.000Z",
      completeness: "complete",
    });
    const observed = repo.getProjectRow(source.id);
    if (!observed) throw new Error("Expected Source row");
    repo.activateSourceRevision({
      sourceId: source.id,
      revisionId: revision.id,
      observed,
      sourceVersion: revision.upstream_revision_key,
    });

    const storage = sourcePdfTextStorage(repo, source.id, revisionRoot);

    expect(storage.cacheRoot).toBe(
      revisionPdfTextCacheRoot({
        reposDir: repo.reposDir,
        sourceId: source.id,
        manifestDigest: digest,
      }),
    );
    expect(storage.cacheRoot.startsWith(revisionRoot)).toBe(false);
    expect(storage.legacyCacheRoots).toContain(join(revisionRoot, ".docs-text"));
    sqlite.close();
  });

  it("reads the active revision manifest even when a legacy workspace override exists", () => {
    const { dataDir, sqlite, repo } = createRepo();
    const source = repo.createSource({
      name: "Stealthburner",
      url: "https://github.com/VoronDesign/Voron-Stealthburner",
    });
    const revisionRoot = join(dataDir, "repos", String(source.id), "revisions", "commit-a");
    const legacyPath = join(revisionRoot, "print-partner.manifest.yaml");
    mkdirSync(revisionRoot, { recursive: true });
    writeFileSync(legacyPath, "project: legacy\n", "utf8");

    expect(findSourceManifestPath(revisionRoot)).toBe(legacyPath);

    const editablePath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(editablePath, "project: editable\n", "utf8");
    expect(findSourceManifestPath(revisionRoot)).toBe(legacyPath);
    sqlite.close();
  });
});

describe("repo manifest routes", () => {
  it("publishes immutable content without following an escaped legacy symlink", async () => {
    const { dataDir, sqlite, repo } = createRepo();
    const source = repo.createSource({
      name: "Escaped manifest",
      source_kind: "local",
    });
    const outside = join(dataDir, "outside.yaml");
    writeFileSync(outside, "secret: outside\n", "utf8");
    mkdirSync(join(repo.reposDir, String(source.id)), { recursive: true });
    symlinkSync(outside, legacySourceManifestOverridePath(repo.reposDir, source.id));

    const app = Fastify();
    await registerRepoManifestRoutes(app, { repo });
    const getResponse = await app.inject({
      method: "GET",
      url: `/sources/${source.id}/repo-manifest`,
    });
    const putResponse = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "project: overwritten\n" },
    });

    expect(getResponse.json()).toMatchObject({ exists: false });
    expect(getResponse.body).not.toContain("secret: outside");
    expect(putResponse.statusCode).toBe(200);
    expect(readFileSync(outside, "utf8")).toBe("secret: outside\n");
    const active = repo.getSource(source.id);
    expect(active?.current_source_revision_id).toEqual(expect.any(Number));
    expect(readFileSync(join(active!.local_path!, "print-partner.manifest.yaml"), "utf8")).toBe(
      "project: overwritten\n",
    );

    await app.close();
    sqlite.close();
  });

  it("publishes a derived revision, preserves upstream identity, and makes accepted Plans stale", async () => {
    const { sqlite, repo } = createRepo();
    const source = repo.createSource({
      name: "LDO Trident",
      url: "https://github.com/example/ldo-trident",
    });
    const base = await activateTrackedSnapshot({
      repo,
      sourceId: source.id,
      manifestYaml: "project: upstream\n",
    });
    const plan = repo.createProfile("Tracked build", source.id);
    expect(acceptPlanForTest(repo, plan.id).merged).toBe(true);

    const app = Fastify();
    await registerRepoManifestRoutes(app, { repo });
    const response = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "project: edited\n" },
    });

    expect(response.statusCode).toBe(200);
    expect(response.json()).toMatchObject({
      source_id: source.id,
      path: "print-partner.manifest.yaml",
      saved: true,
      yaml: "project: edited\n",
      document: { format: "print-partner-manifest-v2", version: 2 },
    });
    expect(
      readFileSync(join(base.snapshot.absolutePath, "print-partner.manifest.yaml"), "utf8"),
    ).toBe("project: upstream\n");
    expect(readFileSync(join(base.snapshot.absolutePath, "cube.stl"), "utf8")).toBe(
      "solid cube\nendsolid cube\n",
    );
    expect(existsSync(legacySourceManifestOverridePath(repo.reposDir, source.id))).toBe(false);
    const active = repo.getSource(source.id);
    expect(active?.current_source_revision_id).not.toBe(base.revision.id);
    expect(active?.last_commit_sha).toBe("commit-a");
    expect(readFileSync(join(active!.local_path!, "print-partner.manifest.yaml"), "utf8")).toBe(
      "project: edited\n",
    );
    expect(repo.getProfileHeader(plan.id)?.freshness).toMatchObject({
      status: "stale",
      reasons: [{ kind: "source_revision_changed", source_id: source.id }],
    });

    const getResponse = await app.inject({
      method: "GET",
      url: `/sources/${source.id}/repo-manifest`,
    });
    expect(getResponse.json()).toMatchObject({ exists: true, yaml: "project: edited\n" });

    const secondResponse = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "project: edited twice\n" },
    });
    expect(secondResponse.statusCode).toBe(200);
    expect(readFileSync(join(active!.local_path!, "print-partner.manifest.yaml"), "utf8")).toBe(
      "project: edited\n",
    );
    const twiceEdited = repo.getSource(source.id);
    expect(twiceEdited?.last_commit_sha).toBe("commit-a");
    expect(twiceEdited?.current_source_revision_id).not.toBe(active!.current_source_revision_id);
    expect(
      readFileSync(join(twiceEdited!.local_path!, "print-partner.manifest.yaml"), "utf8"),
    ).toBe("project: edited twice\n");

    await app.close();
    sqlite.close();
  });

  it("rejects invalid YAML without publishing a revision", async () => {
    const { sqlite, repo } = createRepo();
    const source = repo.createSource({
      name: "Invalid edit",
      url: "https://github.com/example/invalid-edit",
    });
    const base = await activateTrackedSnapshot({
      repo,
      sourceId: source.id,
      manifestYaml: "project: upstream\n",
    });
    const app = Fastify();
    await registerRepoManifestRoutes(app, { repo });

    const response = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "option_groups: [" },
    });

    expect(response.statusCode).toBe(400);
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(base.revision.id);
    expect(repo.listSourceRevisions(source.id)).toHaveLength(1);
    expect(
      readFileSync(join(base.snapshot.absolutePath, "print-partner.manifest.yaml"), "utf8"),
    ).toBe("project: upstream\n");
    await app.close();
    sqlite.close();
  });

  it("returns 409 and leaves the active revision unchanged after a lost activation CAS", async () => {
    const { sqlite, repo } = createRepo();
    const source = repo.createSource({
      name: "Concurrent edit",
      url: "https://github.com/example/concurrent-edit",
    });
    const base = await activateTrackedSnapshot({
      repo,
      sourceId: source.id,
      manifestYaml: "project: upstream\n",
    });
    const observe = repo.getSourceActivationObservation.bind(repo);
    vi.spyOn(repo, "getSourceActivationObservation").mockImplementationOnce((id) => {
      const observed = observe(id);
      repo.updateSource(source.id, { branch: "release" });
      return observed;
    });
    const app = Fastify();
    await registerRepoManifestRoutes(app, { repo });

    const response = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "project: edited\n" },
    });

    expect(response.statusCode).toBe(409);
    expect(repo.getSource(source.id)).toMatchObject({
      current_source_revision_id: base.revision.id,
      last_commit_sha: "commit-a",
      branch: "release",
    });
    expect(
      readFileSync(join(base.snapshot.absolutePath, "print-partner.manifest.yaml"), "utf8"),
    ).toBe("project: upstream\n");
    await app.close();
    sqlite.close();
  });
});
