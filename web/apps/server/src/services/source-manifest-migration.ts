import type { AppRepository } from "../db/repository.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import {
  OwnershipAdmissionError,
  requireSourceSyncParticipant,
} from "../ownership/source-sync-admission.js";
import {
  archiveLegacySourceManifest,
  inspectLegacySourceManifest,
} from "./legacy-source-manifest.js";
import {
  InvalidSourceManifestError,
  SourceManifestContentUnavailableError,
  SOURCE_PUBLICATION_UNAVAILABLE_DETAIL,
  publishSourceManifestRevision,
} from "./local-source-revision.js";

type LegacySourceManifestMigration = Readonly<{
  sourceId: number;
  legacyPath: string;
  backupPath: string | null;
  revisionId: number;
  changedDuringMigration: boolean;
}>;

type RetainedLegacySourceManifest = Readonly<{
  sourceId: number;
  legacyPath: string;
  reason: string;
}>;

type LegacySourceManifestMigrationReport = Readonly<{
  migrated: readonly LegacySourceManifestMigration[];
  retained: readonly RetainedLegacySourceManifest[];
}>;

export function migrateLegacySourceManifestOverridesForTenant(
  repo: AppRepository,
  tenantId: string,
): Promise<LegacySourceManifestMigrationReport> {
  return tenantStorage.run(tenantId, () => migrateLegacySourceManifestOverrides(repo));
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export async function migrateLegacySourceManifestOverrides(
  repo: AppRepository,
  dependencies: {
    inspectLegacy?: typeof inspectLegacySourceManifest;
  } = {},
): Promise<LegacySourceManifestMigrationReport> {
  const checkAdmission = () =>
    requireSourceSyncParticipant("migration.migrateLegacySourceManifestOverridesForTenant");
  checkAdmission();
  const migrated: LegacySourceManifestMigration[] = [];
  const retained: RetainedLegacySourceManifest[] = [];

  for (const source of repo.listSources()) {
    const observedSource = repo.getSourceActivationObservation(source.id);
    if (!observedSource) continue;
    const observedLegacy = await (dependencies.inspectLegacy ?? inspectLegacySourceManifest)({
      reposDir: repo.reposDir,
      sourceId: source.id,
    });
    if (observedLegacy.kind === "absent") continue;
    if (observedLegacy.kind === "unsafe") {
      retained.push({
        sourceId: source.id,
        legacyPath: observedLegacy.legacyPath,
        reason: observedLegacy.reason,
      });
      continue;
    }

    if (!repo.canPublishSourceRevision()) {
      retained.push({
        sourceId: source.id,
        legacyPath: observedLegacy.legacyPath,
        reason: SOURCE_PUBLICATION_UNAVAILABLE_DETAIL,
      });
      continue;
    }

    if (observedSource.legacyManifestCutover) {
      try {
        const revisionId = observedSource.currentSourceRevisionId;
        if (revisionId == null) {
          throw new Error("Source manifest cutover has no active revision");
        }
        const activeRevision = repo.getSourceRevision(revisionId);
        if (!activeRevision || activeRevision.source_id !== source.id) {
          throw new Error("Source manifest cutover revision is unavailable");
        }
        const archived = await archiveLegacySourceManifest(
          observedLegacy,
          repo.reposDir,
          source.id,
          checkAdmission,
        );
        migrated.push({
          sourceId: source.id,
          legacyPath: observedLegacy.legacyPath,
          backupPath: archived?.backupPath ?? null,
          revisionId,
          changedDuringMigration: archived?.matchesObservedContent === false,
        });
      } catch (error) {
        if (error instanceof OwnershipAdmissionError) throw error;
        retained.push({
          sourceId: source.id,
          legacyPath: observedLegacy.legacyPath,
          reason: errorMessage(error),
        });
      }
      continue;
    }

    const yaml = observedLegacy.content.toString("utf8");
    if (!Buffer.from(yaml, "utf8").equals(observedLegacy.content)) {
      retained.push({
        sourceId: source.id,
        legacyPath: observedLegacy.legacyPath,
        reason: "Legacy Source manifest is not valid UTF-8",
      });
      continue;
    }

    let activated;
    try {
      activated = await publishSourceManifestRevision({
        beforePublish: checkAdmission,
        repo,
        sourceId: source.id,
        manifestYaml: yaml,
        observed: observedSource,
      });
    } catch (error) {
      if (error instanceof InvalidSourceManifestError) {
        retained.push({
          sourceId: source.id,
          legacyPath: observedLegacy.legacyPath,
          reason: error.message,
        });
        continue;
      }
      if (error instanceof SourceManifestContentUnavailableError) {
        retained.push({
          sourceId: source.id,
          legacyPath: observedLegacy.legacyPath,
          reason: error.message,
        });
        continue;
      }
      throw error;
    }
    const revisionId = activated.current_source_revision_id;
    if (revisionId == null) {
      throw new Error("Migrated Source manifest has no active revision");
    }
    try {
      const archived = await archiveLegacySourceManifest(
        observedLegacy,
        repo.reposDir,
        source.id,
        checkAdmission,
      );
      migrated.push({
        sourceId: source.id,
        legacyPath: observedLegacy.legacyPath,
        backupPath: archived?.backupPath ?? null,
        revisionId,
        changedDuringMigration: archived?.matchesObservedContent === false,
      });
    } catch (error) {
      if (error instanceof OwnershipAdmissionError) throw error;
      retained.push({
        sourceId: source.id,
        legacyPath: observedLegacy.legacyPath,
        reason: errorMessage(error),
      });
    }
  }

  return { migrated, retained };
}
