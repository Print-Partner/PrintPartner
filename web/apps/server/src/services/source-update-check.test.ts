import { join } from "node:path";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import {
  beginSourceSyncAdmission,
  packagedSourceSyncManifestPath,
} from "../ownership/source-sync-admission.js";

const github = vi.hoisted(() => ({ getCommit: vi.fn() }));

vi.mock("@octokit/rest", () => ({
  Octokit: class {
    readonly repos = { getCommit: github.getCommit };
  },
}));

import { checkAllSourceUpdates } from "./source-update-check.js";

const cleanups: Array<() => void> = [];

function makeRepo() {
  const dir = mkdtempSync(join(tmpdir(), "pp-source-update-"));
  const sqlite = new SqliteDatabase(dir);
  sqlite.connect();
  cleanups.push(() => {
    sqlite.close();
    rmSync(dir, { recursive: true, force: true });
  });
  return new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
}

afterEach(() => {
  vi.clearAllMocks();
  while (cleanups.length > 0) cleanups.pop()?.();
});

describe("source update checks", () => {
  it.each(["worker.startSourceWatcher", "job.check-source-updates"] as const)(
    "refuses writes after a manifest replacement during a remote lookup for %s",
    async (participant) => {
      const repo = makeRepo();
      const first = repo.createSource({
        name: "A first",
        url: "https://github.com/example/first",
        source_kind: "github",
        local_path: join(repo.reposDir, "first"),
      });
      const second = repo.createSource({
        name: "B second",
        url: "https://github.com/example/second",
        source_kind: "github",
        local_path: join(repo.reposDir, "second"),
      });
      repo.markSourceSynced(first.id, "old-sha");
      repo.markSourceSynced(second.id, "old-sha");
      const dataDir = join(repo.reposDir, "..");
      const manifest = join(dataDir, "manifest.json");
      const original = readFileSync(packagedSourceSyncManifestPath(), "utf8");
      const previous = process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
      try {
        writeFileSync(manifest, original);
        process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = manifest;
        beginSourceSyncAdmission(dataDir);
        const before = [repo.getProjectRow(first.id), repo.getProjectRow(second.id)];
        github.getCommit.mockImplementation(async () => {
          writeFileSync(manifest, `${original}\n`);
          return { data: { sha: "new-sha" } };
        });
        await expect(checkAllSourceUpdates(repo, participant)).rejects.toThrow(
          /runtime manifest does not match/,
        );
        expect(github.getCommit).toHaveBeenCalledOnce();
        expect([repo.getProjectRow(first.id), repo.getProjectRow(second.id)]).toEqual(before);
        expect(repo.listAppEvents({ kinds: ["source.update_available"] })).toEqual([]);
        expect(repo.getSetting("source_update_check_last_run_at")).toBeNull();
      } finally {
        if (previous === undefined) delete process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST;
        else process.env.PP_SOURCE_SYNC_OWNERSHIP_MANIFEST = previous;
      }
    },
  );

  it("records one in-app alert when a GitHub source first moves out of date", async () => {
    github.getCommit.mockResolvedValue({ data: { sha: "new-sha" } });
    const repo = makeRepo();
    const source = repo.createSource({
      name: "Tracked repository",
      url: "https://github.com/example/tracked",
      source_kind: "github",
      local_path: join(repo.reposDir, "tracked"),
    });
    repo.markSourceSynced(source.id, "old-sha");

    await expect(checkAllSourceUpdates(repo)).resolves.toMatchObject({
      checked_count: 1,
      updates_available: 1,
    });
    await checkAllSourceUpdates(repo);

    const alerts = repo.listAppEvents({ kinds: ["source.update_available"] });
    expect(alerts).toHaveLength(1);
    expect(JSON.parse(alerts[0]!.payloadJson ?? "{}")).toMatchObject({
      source_id: source.id,
      source_name: "Tracked repository",
      previous_sha: "old-sha",
    });
  });

  it("does not ask GitHub about tracked model pages", async () => {
    const repo = makeRepo();
    repo.createSource({
      name: "Tracked model",
      url: "https://www.printables.com/model/123-example",
      source_kind: "printables",
      local_path: join(repo.reposDir, "model"),
    });

    await expect(checkAllSourceUpdates(repo)).resolves.toMatchObject({
      checked_count: 0,
      updates_available: 0,
      skipped: [expect.objectContaining({ reason: "not_git" })],
    });
    expect(github.getCommit).not.toHaveBeenCalled();
  });
});
