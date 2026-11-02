import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import * as fsPromises from "node:fs/promises";
import Fastify from "fastify";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import { registerRepoManifestRoutes } from "../routes/repo-manifest.js";
import {
  archiveLegacySourceManifest,
  inspectLegacySourceManifest,
} from "./legacy-source-manifest.js";
import { publishLocalSourceWorkingTree } from "./local-source-revision.js";
import { ISOLATED_SOURCE_FILESYSTEM } from "./source-filesystem-policy.js";
import {
  migrateLegacySourceManifestOverrides,
  migrateLegacySourceManifestOverridesForTenant,
} from "./source-manifest-migration.js";
import { legacySourceManifestOverridePath, sourceWorkspaceRoot } from "./source-workspace.js";
import {
  beginSourceSyncAdmission,
  packagedSourceSyncManifestPath,
} from "../ownership/source-sync-admission.js";

vi.mock("node:fs/promises", { spy: true });

const roots: string[] = [];

function fixture() {
  const dataDir = mkdtempSync(join(tmpdir(), "pp-source-manifest-migration-"));
  roots.push(dataDir);
  const sqlite = new SqliteDatabase(dataDir);
  sqlite.connect();
  return {
    dataDir,
    sqlite,
    repo: new AppRepository(getDb(sqlite), undefined, sqlite.reposDir),
  };
}

function migratedBackupPaths(legacyPath: string): string[] {
  const parent = dirname(legacyPath);
  const name = basename(legacyPath);
  return readdirSync(parent)
    .filter((entry) => entry.startsWith(`${name}.migrated-`))
    .map((entry) => join(parent, entry, `${name}.migrated`))
    .filter(existsSync)
    .sort();
}

async function trackedSource(input: { dataDir: string; repo: AppRepository; name: string }) {
  const source = input.repo.createSource({
    name: input.name,
    source_kind: "local",
  });
  const workingTree = join(input.dataDir, `incoming-${source.id}`);
  mkdirSync(workingTree, { recursive: true });
  writeFileSync(join(workingTree, "cube.stl"), "solid cube\nendsolid cube\n");
  writeFileSync(join(workingTree, "print-partner.manifest.yaml"), "project: upstream\n");
  const activated = await publishLocalSourceWorkingTree({
    repo: input.repo,
    reposDir: input.repo.reposDir,
    sourceId: source.id,
    workingTree,
  });
  return { source, activated };
}

afterEach(() => {
  vi.restoreAllMocks();
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

describe("legacy Source manifest migration", () => {
  it.each([false, true])(
    "refuses legacy publication or archival after admission changes (alreadyCutOver=%s)",
    async (alreadyCutOver) => {
      const f = fixture();
      const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      try {
        const { source } = await trackedSource({ ...f, name: "Migration admission" });
        const manifest = join(f.dataDir, "ownership.json");
        const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
        writeFileSync(manifest, original);
        process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
        beginSourceSyncAdmission(f.dataDir);
        const legacyPath = legacySourceManifestOverridePath(f.repo.reposDir, source.id);
        writeFileSync(legacyPath, "project: legacy\n");
        if (alreadyCutOver) {
          await migrateLegacySourceManifestOverrides(f.repo);
          writeFileSync(legacyPath, "project: restored legacy\n");
        }
        const legacyBytes = readFileSync(legacyPath);
        const revisions = f.repo.listSourceRevisions(source.id);
        const before = f.repo.getProjectRow(source.id);
        const backups = migratedBackupPaths(legacyPath);
        await expect(
          migrateLegacySourceManifestOverrides(f.repo, {
            inspectLegacy: async (input) => {
              const observed = await inspectLegacySourceManifest(input);
              writeFileSync(manifest, `${original}\n`);
              return observed;
            },
          }),
        ).rejects.toThrow(/runtime manifest does not match/);
        expect(readFileSync(legacyPath)).toEqual(legacyBytes);
        expect(f.repo.getProjectRow(source.id)).toEqual(before);
        expect(f.repo.listSourceRevisions(source.id)).toEqual(revisions);
        expect(migratedBackupPaths(legacyPath)).toEqual(backups);
      } finally {
        if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
        else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
        f.sqlite.close();
      }
    },
  );

  it("retains legacy overrides when publication transactions are unsupported", async () => {
    const { sqlite, repo } = fixture();
    const source = repo.createSource({ name: "Unsupported migration", source_kind: "github" });
    const workspace = sourceWorkspaceRoot(repo.reposDir, source.id);
    mkdirSync(workspace, { recursive: true });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: retained\nparts: []\n");
    const before = repo.getProjectRow(source.id);
    vi.spyOn(repo, "canPublishSourceRevision").mockReturnValue(false);
    try {
      const report = await migrateLegacySourceManifestOverrides(repo);
      expect(report).toEqual({
        migrated: [],
        retained: [
          {
            sourceId: source.id,
            legacyPath,
            reason:
              "Source revision publication is unavailable on experimental PostgreSQL because atomic publication transactions are not supported.",
          },
        ],
      });
      expect(repo.getProjectRow(source.id)).toEqual(before);
      expect(repo.listSourceRevisions(source.id)).toEqual([]);
      expect(readFileSync(legacyPath, "utf8")).toBe("project: retained\nparts: []\n");
      expect(readdirSync(workspace)).toEqual(["print-partner.manifest.yaml"]);
    } finally {
      sqlite.close();
    }
  });

  it("atomically claims concurrently replaced legacy bytes without deleting them", async () => {
    const { sqlite, repo } = fixture();
    const source = repo.createSource({ name: "Atomic claim", source_kind: "local" });
    mkdirSync(sourceWorkspaceRoot(repo.reposDir, source.id), { recursive: true });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: observed\n");
    const observed = await inspectLegacySourceManifest({
      reposDir: repo.reposDir,
      sourceId: source.id,
    });
    if (observed.kind !== "file") throw new Error("Expected a legacy manifest file");
    writeFileSync(legacyPath, "project: replaced\n");

    const archived = await archiveLegacySourceManifest(observed, repo.reposDir, source.id);

    expect(archived?.matchesObservedContent).toBe(false);
    expect(readFileSync(archived!.backupPath, "utf8")).toBe("project: replaced\n");
    expect(existsSync(legacyPath)).toBe(false);
    sqlite.close();
  });

  it("materializes complete content for an untracked trusted local Source", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const workingTree = join(dataDir, "trusted-working-tree");
    mkdirSync(workingTree, { recursive: true });
    writeFileSync(join(workingTree, "cube.stl"), "solid local\nendsolid local\n");
    writeFileSync(join(workingTree, "print-partner.manifest.yaml"), "project: working tree\n");
    const source = repo.createSource({
      name: "Untracked local Source",
      source_kind: "local",
      local_path: workingTree,
    });
    const workspace = sourceWorkspaceRoot(repo.reposDir, source.id);
    mkdirSync(workspace, { recursive: true });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    const legacyYaml = "project: migrated local\nparts: []\n";
    writeFileSync(legacyPath, legacyYaml);

    const report = await migrateLegacySourceManifestOverrides(repo);

    expect(report.migrated).toHaveLength(1);
    const active = repo.getSource(source.id);
    expect(active?.current_source_revision_id).toEqual(expect.any(Number));
    expect(readFileSync(join(active!.local_path!, "cube.stl"), "utf8")).toBe(
      "solid local\nendsolid local\n",
    );
    expect(readFileSync(join(active!.local_path!, "print-partner.manifest.yaml"), "utf8")).toBe(
      legacyYaml,
    );
    expect(readFileSync(join(workingTree, "print-partner.manifest.yaml"), "utf8")).toBe(
      "project: working tree\n",
    );
    expect(readFileSync(report.migrated[0]!.backupPath!, "utf8")).toBe(legacyYaml);
    sqlite.close();
  });

  it("publishes valid bytes once and preserves the previous backup", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const { source, activated: base } = await trackedSource({
      dataDir,
      repo,
      name: "Migrated Source",
    });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    const legacyYaml = "project: migrated\r\nparts: []\r\n";
    writeFileSync(legacyPath, legacyYaml);
    writeFileSync(`${legacyPath}.migrated`, "older backup\n");

    const report = await migrateLegacySourceManifestOverrides(repo);

    expect(report.retained).toEqual([]);
    expect(report.migrated).toHaveLength(1);
    expect(report.migrated[0]).toMatchObject({ changedDuringMigration: false });
    expect(dirname(report.migrated[0]!.backupPath!)).toContain(
      "print-partner.manifest.yaml.migrated-",
    );
    expect(basename(report.migrated[0]!.backupPath!)).toBe("print-partner.manifest.yaml.migrated");
    expect(existsSync(legacyPath)).toBe(false);
    expect(readFileSync(`${legacyPath}.migrated`, "utf8")).toBe("older backup\n");
    expect(readFileSync(report.migrated[0]!.backupPath!, "utf8")).toBe(legacyYaml);
    const active = repo.getSource(source.id);
    expect(active?.current_source_revision_id).not.toBe(base.current_source_revision_id);
    expect(active?.last_commit_sha).toBe(base.last_commit_sha);
    expect(readFileSync(join(active!.local_path!, "print-partner.manifest.yaml"), "utf8")).toBe(
      legacyYaml,
    );

    const activeRevisionId = active!.current_source_revision_id;
    await expect(migrateLegacySourceManifestOverrides(repo)).resolves.toEqual({
      migrated: [],
      retained: [],
    });
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(activeRevisionId);
    sqlite.close();
  });

  it("retains invalid YAML without changing the active revision", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const { source, activated } = await trackedSource({
      dataDir,
      repo,
      name: "Invalid legacy Source",
    });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "option_groups: [");

    const report = await migrateLegacySourceManifestOverrides(repo);

    expect(report.migrated).toEqual([]);
    expect(report.retained).toEqual([expect.objectContaining({ sourceId: source.id, legacyPath })]);
    expect(readFileSync(legacyPath, "utf8")).toBe("option_groups: [");
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(
      activated.current_source_revision_id,
    );
    sqlite.close();
  });

  it("retains leaf and workspace symlinks without touching outside files", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const leafSource = repo.createSource({ name: "Leaf symlink", source_kind: "local" });
    const leafOutside = join(dataDir, "leaf-outside.yaml");
    writeFileSync(leafOutside, "project: leaf outside\n");
    mkdirSync(sourceWorkspaceRoot(repo.reposDir, leafSource.id), { recursive: true });
    symlinkSync(leafOutside, legacySourceManifestOverridePath(repo.reposDir, leafSource.id));

    const parentSource = repo.createSource({ name: "Parent symlink", source_kind: "local" });
    const parentOutside = join(dataDir, "parent-outside");
    mkdirSync(parentOutside, { recursive: true });
    writeFileSync(join(parentOutside, "print-partner.manifest.yaml"), "project: parent outside\n");
    symlinkSync(parentOutside, sourceWorkspaceRoot(repo.reposDir, parentSource.id), "dir");

    const report = await migrateLegacySourceManifestOverrides(repo);

    expect(report.migrated).toEqual([]);
    expect(report.retained.map((entry) => entry.sourceId).sort()).toEqual(
      [leafSource.id, parentSource.id].sort(),
    );
    expect(readFileSync(leafOutside, "utf8")).toBe("project: leaf outside\n");
    expect(readFileSync(join(parentOutside, "print-partner.manifest.yaml"), "utf8")).toBe(
      "project: parent outside\n",
    );
    sqlite.close();
  });

  it("fails closed on CAS loss, then lets PUT retire the legacy file without restart rollback", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const { source, activated: base } = await trackedSource({
      dataDir,
      repo,
      name: "Recovered migration",
    });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: legacy\n");
    const activate = repo.activateSourceRevision.bind(repo);
    const spy = vi.spyOn(repo, "activateSourceRevision").mockImplementationOnce((input) => {
      repo.updateSource(source.id, { branch: "release" });
      return activate(input);
    });

    await expect(migrateLegacySourceManifestOverrides(repo)).rejects.toThrow(
      "Source changed during sync",
    );
    expect(readFileSync(legacyPath, "utf8")).toBe("project: legacy\n");
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(
      base.current_source_revision_id,
    );
    spy.mockRestore();

    const app = Fastify();
    await registerRepoManifestRoutes(app, { repo });
    const response = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "project: API edit\n" },
    });
    expect(response.statusCode).toBe(200);
    expect(existsSync(legacyPath)).toBe(false);
    expect(migratedBackupPaths(legacyPath).map((path) => readFileSync(path, "utf8"))).toEqual([
      "project: legacy\n",
    ]);
    const apiRevisionId = repo.getSource(source.id)?.current_source_revision_id;
    expect(apiRevisionId).not.toBe(base.current_source_revision_id);

    writeFileSync(legacyPath, "project: stale restart value\n");
    const restart = await migrateLegacySourceManifestOverrides(repo);
    expect(restart.migrated).toHaveLength(1);
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(apiRevisionId);
    expect(
      readFileSync(
        join(repo.getSource(source.id)!.local_path!, "print-partner.manifest.yaml"),
        "utf8",
      ),
    ).toBe("project: API edit\n");
    expect(
      migratedBackupPaths(legacyPath)
        .map((path) => readFileSync(path, "utf8"))
        .sort(),
    ).toEqual(["project: legacy\n", "project: stale restart value\n"]);
    await app.close();
    sqlite.close();
  });

  it("retries an untracked workspace migration without ingesting managed revisions", async () => {
    const { sqlite, repo } = fixture();
    const source = repo.createSource({
      name: "Workspace retry",
      source_kind: "local",
    });
    const workspace = sourceWorkspaceRoot(repo.reposDir, source.id);
    mkdirSync(workspace, { recursive: true });
    writeFileSync(join(workspace, "cube.stl"), "solid root\nendsolid root\n");
    mkdirSync(join(workspace, "revisions", "v2"), { recursive: true });
    writeFileSync(join(workspace, "revisions", "v2", "revision-part.stl"), "solid revision\n");
    mkdirSync(join(workspace, "derived", "v2"), { recursive: true });
    writeFileSync(join(workspace, "derived", "v2", "derived-part.stl"), "solid derived\n");
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: root manifest\n");
    const activate = repo.activateSourceRevision.bind(repo);
    const spy = vi.spyOn(repo, "activateSourceRevision").mockImplementationOnce((input) => {
      repo.updateSource(source.id, { branch: "release" });
      return activate(input);
    });

    await expect(migrateLegacySourceManifestOverrides(repo)).rejects.toThrow(
      "Source changed during sync",
    );
    expect(existsSync(join(workspace, "revisions"))).toBe(true);
    expect(existsSync(legacyPath)).toBe(true);
    spy.mockRestore();

    const retry = await migrateLegacySourceManifestOverrides(repo);

    expect(retry.migrated).toHaveLength(1);
    const active = repo.getSource(source.id);
    expect(
      JSON.parse(
        readFileSync(join(active!.local_path!, ".printpartner-source-snapshot.json"), "utf8"),
      ),
    ).toMatchObject({
      files: [
        { path: "cube.stl" },
        { path: "derived/v2/derived-part.stl" },
        { path: "print-partner.manifest.yaml" },
        { path: "revisions/v2/revision-part.stl" },
      ],
    });
    expect(repo.listSourceRevisions(source.id)).toHaveLength(1);
    expect(readFileSync(join(active!.local_path!, "cube.stl"), "utf8")).toBe(
      "solid root\nendsolid root\n",
    );
    expect(
      readFileSync(join(active!.local_path!, "revisions", "v2", "revision-part.stl"), "utf8"),
    ).toBe("solid revision\n");
    expect(
      readFileSync(join(active!.local_path!, "derived", "v2", "derived-part.stl"), "utf8"),
    ).toBe("solid derived\n");
    sqlite.close();
  });

  it("reports a successful PUT after post-commit archival fails and repairs it on restart", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const { source } = await trackedSource({
      dataDir,
      repo,
      name: "Archive retry",
    });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: legacy\n");
    const actualFs = await vi.importActual<typeof fsPromises>("node:fs/promises");
    const renameSpy = vi.mocked(fsPromises.rename).mockImplementation(async (from, to) => {
      if (from === legacyPath) throw new Error("archive denied");
      await actualFs.rename(from, to);
    });
    const app = Fastify();
    await registerRepoManifestRoutes(app, { repo });

    const response = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "project: committed edit\n" },
    });

    expect(response.statusCode).toBe(200);
    expect(existsSync(legacyPath)).toBe(true);
    const committedRevisionId = repo.getSource(source.id)?.current_source_revision_id;
    expect(repo.getProjectRow(source.id)?.legacyManifestCutover).toBe(true);
    expect(
      readFileSync(
        join(repo.getSource(source.id)!.local_path!, "print-partner.manifest.yaml"),
        "utf8",
      ),
    ).toBe("project: committed edit\n");
    renameSpy.mockRestore();

    const syncedTree = join(dataDir, "archive-retry-synced");
    mkdirSync(syncedTree, { recursive: true });
    writeFileSync(join(syncedTree, "cube.stl"), "solid synced\n");
    writeFileSync(join(syncedTree, "print-partner.manifest.yaml"), "project: synced upstream\n");
    const synced = await publishLocalSourceWorkingTree({
      repo,
      reposDir: repo.reposDir,
      sourceId: source.id,
      workingTree: syncedTree,
    });
    expect(synced.current_source_revision_id).not.toBe(committedRevisionId);
    expect(repo.getProjectRow(source.id)?.legacyManifestCutover).toBe(true);

    const restart = await migrateLegacySourceManifestOverrides(repo);
    expect(restart.migrated).toHaveLength(1);
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(
      synced.current_source_revision_id,
    );
    expect(
      readFileSync(
        join(repo.getSource(source.id)!.local_path!, "print-partner.manifest.yaml"),
        "utf8",
      ),
    ).toBe("project: synced upstream\n");
    expect(existsSync(legacyPath)).toBe(false);
    expect(migratedBackupPaths(legacyPath).map((path) => readFileSync(path, "utf8"))).toEqual([
      "project: legacy\n",
    ]);
    await app.close();
    sqlite.close();
  });

  it("cannot overwrite a PUT that commits while legacy inspection is paused", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const { source, activated: base } = await trackedSource({
      dataDir,
      repo,
      name: "Concurrent migration",
    });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: upstream\n");
    let signalInspected: () => void = () => undefined;
    const inspected = new Promise<void>((resolve) => {
      signalInspected = resolve;
    });
    let resumeInspection: () => void = () => undefined;
    const resume = new Promise<void>((resolve) => {
      resumeInspection = resolve;
    });
    const migration = migrateLegacySourceManifestOverrides(repo, {
      inspectLegacy: async (input) => {
        const observation = await inspectLegacySourceManifest(input);
        signalInspected();
        await resume;
        return observation;
      },
    });
    await inspected;

    const app = Fastify();
    await registerRepoManifestRoutes(app, { repo });
    const put = await app.inject({
      method: "PUT",
      url: `/sources/${source.id}/repo-manifest`,
      payload: { yaml: "project: upstream\n" },
    });
    expect(put.statusCode).toBe(200);
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(
      base.current_source_revision_id,
    );
    expect(repo.getProjectRow(source.id)?.legacyManifestCutover).toBe(true);

    resumeInspection();
    await expect(migration).rejects.toThrow("Source changed during sync");
    expect(repo.getSource(source.id)?.current_source_revision_id).toBe(
      base.current_source_revision_id,
    );
    expect(
      readFileSync(
        join(repo.getSource(source.id)!.local_path!, "print-partner.manifest.yaml"),
        "utf8",
      ),
    ).toBe("project: upstream\n");
    await app.close();
    sqlite.close();
  });

  it("migrates only Sources in the active tenant context", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const sources = new Map<string, { id: number; legacyPath: string }>();
    for (const tenantId of ["tenant-a", "tenant-b"]) {
      tenantStorage.run(tenantId, () => {
        const workingTree = join(dataDir, `${tenantId}-working-tree`);
        mkdirSync(workingTree, { recursive: true });
        writeFileSync(join(workingTree, "cube.stl"), `solid ${tenantId}\n`);
        const source = repo.createSource({
          name: `${tenantId} Source`,
          source_kind: "local",
          local_path: workingTree,
        });
        mkdirSync(sourceWorkspaceRoot(repo.reposDir, source.id), { recursive: true });
        const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
        writeFileSync(legacyPath, `project: ${tenantId}\n`);
        sources.set(tenantId, { id: source.id, legacyPath });
      });
    }

    const firstReport = await tenantStorage.run("tenant-a", () =>
      migrateLegacySourceManifestOverrides(repo),
    );

    expect(firstReport.migrated.map((entry) => entry.sourceId)).toEqual([
      sources.get("tenant-a")!.id,
    ]);
    expect(existsSync(sources.get("tenant-a")!.legacyPath)).toBe(false);
    expect(existsSync(sources.get("tenant-b")!.legacyPath)).toBe(true);
    expect(
      tenantStorage.run("tenant-a", () => repo.getSource(sources.get("tenant-a")!.id))
        ?.current_source_revision_id,
    ).toEqual(expect.any(Number));
    expect(
      tenantStorage.run("tenant-b", () => repo.getSource(sources.get("tenant-b")!.id))
        ?.current_source_revision_id,
    ).toBeNull();

    const secondReport = await tenantStorage.run("tenant-b", () =>
      migrateLegacySourceManifestOverrides(repo),
    );
    expect(secondReport.migrated.map((entry) => entry.sourceId)).toEqual([
      sources.get("tenant-b")!.id,
    ]);
    expect(existsSync(sources.get("tenant-b")!.legacyPath)).toBe(false);
    sqlite.close();
  });

  it("runs a process startup migration in the requested tenant context", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const workingTree = join(dataDir, "default-working-tree");
    mkdirSync(workingTree, { recursive: true });
    writeFileSync(join(workingTree, "cube.stl"), "solid default\n");
    const source = repo.createSource({
      name: "Default Source",
      source_kind: "local",
      local_path: workingTree,
    });
    mkdirSync(sourceWorkspaceRoot(repo.reposDir, source.id), { recursive: true });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: default\n");

    const report = await tenantStorage.run("unrelated-ambient-tenant", () =>
      migrateLegacySourceManifestOverridesForTenant(repo, "default"),
    );

    expect(report.migrated.map((entry) => entry.sourceId)).toEqual([source.id]);
    expect(repo.getSource(source.id)?.current_source_revision_id).toEqual(expect.any(Number));
    expect(repo.getProjectRow(source.id)?.legacyManifestCutover).toBe(true);
    expect(existsSync(legacyPath)).toBe(false);
    sqlite.close();
  });

  it("retains an external local override when multi-user isolation hides its content", async () => {
    const { dataDir, sqlite, repo } = fixture();
    const externalWorkingTree = join(dataDir, "external-working-tree");
    mkdirSync(externalWorkingTree, { recursive: true });
    writeFileSync(join(externalWorkingTree, "cube.stl"), "solid external\n");
    const source = repo.createSource({
      name: "External local Source",
      source_kind: "local",
      local_path: externalWorkingTree,
    });
    mkdirSync(sourceWorkspaceRoot(repo.reposDir, source.id), { recursive: true });
    const legacyPath = legacySourceManifestOverridePath(repo.reposDir, source.id);
    writeFileSync(legacyPath, "project: external-local\n");
    const isolatedRepo = new AppRepository(getDb(sqlite), "default", sqlite.reposDir, undefined, {
      sourceFilesystemPolicy: ISOLATED_SOURCE_FILESYSTEM,
    });

    const report = await migrateLegacySourceManifestOverrides(isolatedRepo);

    expect(report.migrated).toEqual([]);
    expect(report.retained).toEqual([
      expect.objectContaining({
        sourceId: source.id,
        legacyPath,
        reason: expect.stringMatching(/filesystem policy/i),
      }),
    ]);
    expect(existsSync(legacyPath)).toBe(true);
    expect(repo.getSource(source.id)?.current_source_revision_id).toBeNull();
    sqlite.close();
  });
});
