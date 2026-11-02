import { createHash } from "node:crypto";
import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  open,
  readdir,
  realpath,
  rename,
  rm,
} from "node:fs/promises";
import { constants } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import type { AcceptedPlanBasis } from "../db/accepted-plan-progress.js";
import type { AppRepository } from "../db/repository.js";
import type { RequiredUnitToken } from "./required-units.js";
import type { AcceptedArtifactVerificationFailure } from "./accepted-artifacts.js";
import {
  generateAcceptedPlate3mfArtifacts,
  type AcceptedPlate3mfLimit,
  type AcceptedPlate3mfLimits,
} from "./accepted-plate-3mf.js";
import { MAX_ACCEPTED_PLATES } from "@print-partner/domain";
import { chargeTenantDiskBytes } from "../lib/tenant-disk-quota.js";

export const ACCEPTED_PLATE_EXPORT_LIMITS: AcceptedPlate3mfLimits = {
  maxTotalSourceBytes: 256 * 1024 * 1024,
  maxObjects: 10_000,
  maxTriangles: 5_000_000,
  maxOutputBytes: 512 * 1024 * 1024,
  maxPlates: MAX_ACCEPTED_PLATES,
};

export type MaterializeAcceptedPlateExportCommand = Readonly<{
  profileId: number;
  expectedPlateRevisionId: number;
}>;

export type MaterializedAcceptedPlateFile = Readonly<{
  filename: string;
  relativePath: string;
  absolutePath: string;
  byteLength: number;
  sha256: string;
}>;

export type MaterializedAcceptedPlateExport = Readonly<{
  kind: "materialized";
  basis: AcceptedPlanBasis;
  plateRevisionId: number;
  plateRevisionNumber: number;
  layoutDigest: string;
  manifest: MaterializedAcceptedPlateFile;
  plates: readonly (MaterializedAcceptedPlateFile & {
    plateId: string;
    ordinal: number;
  })[];
  bundle: MaterializedAcceptedPlateFile;
}>;

type AcceptedArtifactFailure = "legacy" | "untracked_source" | AcceptedArtifactVerificationFailure;

export type MaterializeAcceptedPlateExportResult =
  | MaterializedAcceptedPlateExport
  | { readonly kind: "profile_not_found" }
  | {
      readonly kind:
        "empty_plan" | "plates_not_published" | "stale_accepted_plan" | "unplaced_units";
    }
  | { readonly kind: "plate_revision_changed" }
  | {
      readonly kind: "accepted_state_unavailable";
      readonly reason: "compatibility_dirty" | "uninitialized";
    }
  | { readonly kind: "transaction_unavailable" }
  | {
      readonly kind: "artifact_unavailable";
      readonly token: RequiredUnitToken;
      readonly reason: AcceptedArtifactFailure;
    }
  | {
      readonly kind: "invalid_stl" | "artifact_geometry_mismatch";
      readonly token: RequiredUnitToken;
    }
  | { readonly kind: "limit_exceeded"; readonly limit: AcceptedPlate3mfLimit }
  | { readonly kind: "output_conflict" };

export type MaterializeAcceptedPlateExportDependencies = Readonly<{
  repository: Pick<AppRepository, "readAcceptedPlateExportInput">;
  reposDir: string;
  tenantExportsDir: string;
  limits: AcceptedPlate3mfLimits;
}>;

type ExpectedFile = Readonly<{
  filename: string;
  relativePath: string;
  bytes: Uint8Array;
  byteLength: number;
  sha256: string;
}>;

function positiveSafeInteger(value: number): boolean {
  return Number.isSafeInteger(value) && value > 0;
}

function digest(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function expectedFile(relativePath: string, bytes: Uint8Array): ExpectedFile {
  return {
    filename: relativePath.split("/").at(-1) ?? relativePath,
    relativePath,
    bytes,
    byteLength: bytes.byteLength,
    sha256: digest(bytes),
  };
}

function isUnderRoot(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path));
}

function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("code" in error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}

async function syncDirectory(path: string): Promise<void> {
  const handle = await open(path, constants.O_RDONLY);
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function writeExclusive(path: string, bytes: Uint8Array): Promise<void> {
  const handle = await open(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL, 0o600);
  try {
    await handle.writeFile(bytes);
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function regularFileMatches(path: string, expected: ExpectedFile): Promise<boolean> {
  try {
    const before = await lstat(path);
    if (!before.isFile() || before.isSymbolicLink() || before.size !== expected.byteLength)
      return false;
    const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const opened = await handle.stat();
      if (!opened.isFile() || opened.size !== expected.byteLength) return false;
      const bytes = await handle.readFile();
      return digest(bytes) === expected.sha256;
    } finally {
      await handle.close();
    }
  } catch {
    return false;
  }
}

function expectedDirectories(expectedFiles: readonly ExpectedFile[]): string[] {
  const directories = new Set<string>();
  for (const file of expectedFiles) {
    const parts = file.relativePath.split("/").slice(0, -1);
    for (let index = 1; index <= parts.length; index += 1) {
      directories.add(parts.slice(0, index).join("/"));
    }
  }
  return [...directories].sort();
}

async function treeEntries(
  allowedRoot: string,
  directory: string,
  prefix = "",
): Promise<{ files: string[]; directories: string[] } | null> {
  const directoryStat = await lstat(directory);
  if (!directoryStat.isDirectory() || directoryStat.isSymbolicLink()) return null;
  if (!isUnderRoot(allowedRoot, await realpath(directory))) return null;
  const files: string[] = [];
  const directories: string[] = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) return null;
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isFile()) {
      files.push(path);
      continue;
    }
    if (!entry.isDirectory()) return null;
    directories.push(path);
    const nested = await treeEntries(allowedRoot, join(directory, entry.name), path);
    if (!nested) return null;
    files.push(...nested.files);
    directories.push(...nested.directories);
  }
  return { files, directories };
}

async function publishedTreeMatches(
  allowedRoot: string,
  finalDirectory: string,
  expectedFiles: readonly ExpectedFile[],
): Promise<boolean> {
  try {
    const rootStat = await lstat(finalDirectory);
    if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) return false;
    const resolved = await realpath(finalDirectory);
    if (!isUnderRoot(allowedRoot, resolved)) return false;
    const actual = await treeEntries(allowedRoot, finalDirectory);
    if (!actual) return false;
    const expectedPaths = expectedFiles.map((file) => file.relativePath).sort();
    if (actual.files.sort().join("\0") !== expectedPaths.join("\0")) return false;
    if (actual.directories.sort().join("\0") !== expectedDirectories(expectedFiles).join("\0"))
      return false;
    return (
      await Promise.all(
        expectedFiles.map((file) =>
          regularFileMatches(join(finalDirectory, ...file.relativePath.split("/")), file),
        ),
      )
    ).every(Boolean);
  } catch {
    return false;
  }
}

async function exists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    return errorCode(error) !== "ENOENT";
  }
}

async function ensureDirectoryPath(
  allowedRoot: string,
  segments: readonly string[],
): Promise<string | null> {
  let directory = allowedRoot;
  for (const segment of segments) {
    directory = join(directory, segment);
    try {
      await mkdir(directory, { mode: 0o700 });
    } catch (error) {
      if (errorCode(error) !== "EEXIST") return null;
    }
    try {
      const stat = await lstat(directory);
      if (!stat.isDirectory() || stat.isSymbolicLink()) return null;
      const resolved = await realpath(directory);
      if (!isUnderRoot(allowedRoot, resolved)) return null;
      directory = resolved;
    } catch {
      return null;
    }
  }
  return directory;
}

async function publishTree(
  allowedRoot: string,
  finalDirectory: string,
  expectedFiles: readonly ExpectedFile[],
): Promise<boolean> {
  if (await exists(finalDirectory)) {
    return publishedTreeMatches(allowedRoot, finalDirectory, expectedFiles);
  }
  let stagedBytes = 0;
  for (const file of expectedFiles) {
    stagedBytes += file.byteLength;
    if (!Number.isSafeInteger(stagedBytes)) {
      throw new Error("Accepted Plate export exceeds the supported numeric range");
    }
  }
  chargeTenantDiskBytes(stagedBytes);
  const parent = dirname(finalDirectory);
  const tempDirectory = await mkdtemp(
    join(parent, `.${finalDirectory.split(sep).at(-1) ?? "publication"}.tmp-`),
  );
  let published = false;
  try {
    await chmod(tempDirectory, 0o700);
    const directories = expectedDirectories(expectedFiles);
    for (const directory of directories) {
      await mkdir(join(tempDirectory, ...directory.split("/")), { mode: 0o700 });
    }
    for (const file of expectedFiles) {
      await writeExclusive(join(tempDirectory, ...file.relativePath.split("/")), file.bytes);
    }
    for (const directory of directories.reverse()) {
      await syncDirectory(join(tempDirectory, ...directory.split("/")));
    }
    await syncDirectory(tempDirectory);
    try {
      await rename(tempDirectory, finalDirectory);
      published = true;
      await syncDirectory(parent);
      return true;
    } catch (error) {
      if (await publishedTreeMatches(allowedRoot, finalDirectory, expectedFiles)) return true;
      const code = errorCode(error);
      if (code === "EEXIST" || code === "ENOTEMPTY" || code === "EISDIR") return false;
      throw error;
    }
  } finally {
    if (!published) await rm(tempDirectory, { recursive: true, force: true });
  }
}

function materializedFile(
  tenantRoot: string,
  revisionRelativePath: string,
  file: ExpectedFile,
): MaterializedAcceptedPlateFile {
  return {
    filename: file.filename,
    relativePath: `${revisionRelativePath}/${file.relativePath}`,
    absolutePath: join(
      tenantRoot,
      ...revisionRelativePath.split("/"),
      ...file.relativePath.split("/"),
    ),
    byteLength: file.byteLength,
    sha256: file.sha256,
  };
}

export async function materializeAcceptedPlateExport(
  dependencies: MaterializeAcceptedPlateExportDependencies,
  command: MaterializeAcceptedPlateExportCommand,
): Promise<MaterializeAcceptedPlateExportResult> {
  if (
    !positiveSafeInteger(command.profileId) ||
    !positiveSafeInteger(command.expectedPlateRevisionId)
  ) {
    throw new RangeError("Accepted Plate export identifiers must be positive safe integers");
  }
  const tenantRoot = resolve(dependencies.tenantExportsDir);
  await mkdir(tenantRoot, { recursive: true, mode: 0o700 });
  const resolvedTenantRoot = await realpath(tenantRoot);
  const generated = await generateAcceptedPlate3mfArtifacts(
    {
      repository: dependencies.repository,
      reposDir: dependencies.reposDir,
      limits: dependencies.limits,
    },
    {
      profileId: command.profileId,
      expectedPlateRevisionId: command.expectedPlateRevisionId,
      includeBundle: true,
    },
  );
  if (generated.kind !== "generated") return generated;
  if (!generated.bundle) throw new Error("Accepted Plate bundle was not generated");

  const revisionRelativePath = `accepted-plates/profile-${command.profileId}/revision-${command.expectedPlateRevisionId}`;
  const resolvedParent = await ensureDirectoryPath(resolvedTenantRoot, [
    "accepted-plates",
    `profile-${command.profileId}`,
  ]);
  if (!resolvedParent) return { kind: "output_conflict" };
  const finalDirectory = join(resolvedParent, `revision-${command.expectedPlateRevisionId}`);
  const manifest = expectedFile("manifest.json", generated.manifest);
  const bundle = expectedFile("accepted-plates.zip", generated.bundle);
  const plates = generated.plates.map((plate) => ({
    ...expectedFile(plate.entryName, plate.bytes),
    plateId: plate.plateId,
    ordinal: plate.ordinal,
  }));
  const files = [manifest, bundle, ...plates];
  if (!(await publishTree(resolvedTenantRoot, finalDirectory, files))) {
    return { kind: "output_conflict" };
  }
  return {
    kind: "materialized",
    basis: generated.basis,
    plateRevisionId: generated.plateRevisionId,
    plateRevisionNumber: generated.plateRevisionNumber,
    layoutDigest: generated.layoutDigest,
    manifest: materializedFile(resolvedTenantRoot, revisionRelativePath, manifest),
    plates: plates.map((plate) => ({
      ...materializedFile(resolvedTenantRoot, revisionRelativePath, plate),
      plateId: plate.plateId,
      ordinal: plate.ordinal,
    })),
    bundle: materializedFile(resolvedTenantRoot, revisionRelativePath, bundle),
  };
}

export type StageAcceptedPlateExportCommand = Readonly<{
  materialized: MaterializedAcceptedPlateExport;
  exchangeRoot: string;
  instanceId: string;
}>;

export type StageAcceptedPlateExportResult =
  | {
      readonly kind: "staged";
      readonly inboxRelativePath: string;
      readonly absoluteDirectory: string;
      readonly staged: readonly Readonly<{ ordinal: number; filename: string }>[];
    }
  | { readonly kind: "output_conflict" };

async function readMaterializedFile(
  file: MaterializedAcceptedPlateFile,
): Promise<Uint8Array | null> {
  try {
    const before = await lstat(file.absolutePath);
    if (!before.isFile() || before.isSymbolicLink() || before.size !== file.byteLength) return null;
    const handle = await open(file.absolutePath, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const opened = await handle.stat();
      if (!opened.isFile() || opened.size !== file.byteLength) return null;
      const bytes = await handle.readFile();
      return digest(bytes) === file.sha256 ? bytes : null;
    } finally {
      await handle.close();
    }
  } catch {
    return null;
  }
}

function sanitizedInstanceId(instanceId: string): string | null {
  const value = instanceId.replace(/[^a-zA-Z0-9._-]+/g, "_");
  return value && value !== "." && value !== ".." ? value : null;
}

export async function stageAcceptedPlateExport(
  command: StageAcceptedPlateExportCommand,
): Promise<StageAcceptedPlateExportResult> {
  const instanceId = sanitizedInstanceId(command.instanceId);
  if (!instanceId) return { kind: "output_conflict" };
  const { materialized } = command;
  if (
    !positiveSafeInteger(materialized.basis.profileId) ||
    !positiveSafeInteger(materialized.plateRevisionId)
  ) {
    return { kind: "output_conflict" };
  }
  const loaded = await Promise.all(
    materialized.plates.map(async (plate) => ({
      plate,
      bytes: await readMaterializedFile(plate),
    })),
  );
  const expectedFiles: ExpectedFile[] = [];
  for (const item of loaded) {
    if (!item.bytes) return { kind: "output_conflict" };
    expectedFiles.push(expectedFile(item.plate.filename, item.bytes));
  }

  const exchangeRoot = resolve(command.exchangeRoot);
  const resolvedExchangeRoot = await realpath(exchangeRoot);
  const inboxRelativePath = [
    "pp-inbox",
    instanceId,
    `profile-${materialized.basis.profileId}`,
    `revision-${materialized.plateRevisionId}`,
  ].join("/");
  const resolvedParent = await ensureDirectoryPath(resolvedExchangeRoot, [
    "pp-inbox",
    instanceId,
    `profile-${materialized.basis.profileId}`,
  ]);
  if (!resolvedParent) return { kind: "output_conflict" };
  const finalDirectory = join(resolvedParent, `revision-${materialized.plateRevisionId}`);
  if (!(await publishTree(resolvedExchangeRoot, finalDirectory, expectedFiles))) {
    return { kind: "output_conflict" };
  }
  return {
    kind: "staged",
    inboxRelativePath,
    absoluteDirectory: finalDirectory,
    staged: materialized.plates.map((plate) => ({
      ordinal: plate.ordinal,
      filename: plate.filename,
    })),
  };
}
