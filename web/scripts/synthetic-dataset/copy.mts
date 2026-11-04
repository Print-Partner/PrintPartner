// Copies a synthetic installation to a new directory and points every active
// Source at its snapshot inside the copy. Node stores projects.local_path as an
// absolute path, so a plain copy still reads the original directory.
// See docs/storage.md#disposable-copies.
import { cpSync, existsSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { getDb, SqliteDatabase } from "../../apps/server/src/db/client.js";
import { AppRepository } from "../../apps/server/src/db/repository.js";
import { MANIFEST_FILE } from "./shape.mjs";

// Re-activates each Source's current revision through the repository, which
// resolves the stored locator against the copy's repos directory. Returns the
// number of rebased Sources.
export function copyInstallation(from: string, to: string): number {
  const source = resolve(from);
  const target = resolve(to);
  if (!existsSync(resolve(source, MANIFEST_FILE)))
    throw new Error(`${source} has no ${MANIFEST_FILE}; only synthetic installations are copied`);
  if (existsSync(target) && readdirSync(target).length > 0)
    throw new Error(`refusing to copy into non-empty directory ${target}`);
  cpSync(source, target, { recursive: true });
  const database = new SqliteDatabase(target);
  database.connect();
  try {
    const repo = new AppRepository(getDb(database), "default", database.reposDir);
    let rebased = 0;
    for (const { id } of repo.listSources()) {
      const observed = repo.getSourceActivationObservation(id);
      if (!observed?.currentSourceRevisionId || !observed.lastCommitSha) continue;
      const before = observed.localPath;
      const activated = repo.activateSourceRevision({
        sourceId: id,
        revisionId: observed.currentSourceRevisionId,
        observed,
        sourceVersion: observed.lastCommitSha,
      });
      if (activated.local_path !== before) rebased += 1;
    }
    return rebased;
  } finally {
    database.close();
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.length !== 2) throw new Error("usage: copy.mts SOURCE_DIR NEW_DIR");
  const rebased = copyInstallation(args[0]!, args[1]!);
  process.stdout.write(`${JSON.stringify({ output: resolve(args[1]!), rebased })}\n`);
}
