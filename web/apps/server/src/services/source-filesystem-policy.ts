import { lstatSync, realpathSync, statSync } from "node:fs";
import { resolve, sep } from "node:path";

export type SourceFilesystemPolicy =
  Readonly<{ kind: "isolated" }> | Readonly<{ kind: "trusted_single_user" }>;

export const ISOLATED_SOURCE_FILESYSTEM: SourceFilesystemPolicy = Object.freeze({
  kind: "isolated",
});

export const TRUSTED_SINGLE_USER_SOURCE_FILESYSTEM: SourceFilesystemPolicy = Object.freeze({
  kind: "trusted_single_user",
});

export class UserSourceLocalPathNotAllowedError extends Error {
  constructor() {
    super("local_path is unavailable in multi-user and SaaS deployments");
    this.name = "UserSourceLocalPathNotAllowedError";
  }
}

export function allowsUserSourceLocalPath(policy: SourceFilesystemPolicy): boolean {
  return policy.kind === "trusted_single_user";
}

export function sourceWorkspaceRoot(reposDir: string, sourceId: number): string {
  return resolve(reposDir, String(sourceId));
}

export function resolveSourceFilesystemRoot(
  policy: SourceFilesystemPolicy,
  reposDir: string,
  sourceId: number,
  storedPath: string | null,
): string | null {
  if (!storedPath) return null;
  if (policy.kind === "trusted_single_user") return resolve(storedPath);

  const workspace = sourceWorkspaceRoot(reposDir, sourceId);
  const candidate = resolve(storedPath);

  try {
    const canonicalConfiguredWorkspace = sourceWorkspaceRoot(realpathSync(reposDir), sourceId);
    if (
      candidate !== workspace &&
      !candidate.startsWith(`${workspace}${sep}`) &&
      candidate !== canonicalConfiguredWorkspace &&
      !candidate.startsWith(`${canonicalConfiguredWorkspace}${sep}`)
    )
      return null;
    const workspaceStat = lstatSync(workspace);
    if (workspaceStat.isSymbolicLink() || !workspaceStat.isDirectory()) return null;
    const canonicalWorkspace = realpathSync(workspace);
    const canonicalCandidate = realpathSync(candidate);
    if (
      canonicalCandidate !== canonicalWorkspace &&
      !canonicalCandidate.startsWith(`${canonicalWorkspace}${sep}`)
    ) {
      return null;
    }
    if (!statSync(canonicalCandidate).isDirectory()) return null;
    return canonicalCandidate;
  } catch {
    return null;
  }
}
