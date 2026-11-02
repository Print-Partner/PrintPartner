import { githubRefName, parseGithubUrl } from "./github-sync.js";
import { createGithubClient } from "./github-client.js";
import type { AppRepository } from "../db/repository.js";
import { requireSourceSyncParticipant } from "../ownership/source-sync-admission.js";

export const REMOTE_UPDATE_STATUS_KEY = "remote_update_status";
export const REMOTE_CHECKED_AT_KEY = "remote_checked_at";

type RemoteUpdateStatus = "up_to_date" | "updates_available" | "unknown";

async function remoteUpdateStatusOctokit(
  url: string,
  branch: string,
  lastSha: string | null,
  token?: string | null,
  tag?: string | null,
): Promise<RemoteUpdateStatus> {
  const ref = parseGithubUrl(url);
  if (!ref) return "unknown";
  try {
    const octokit = createGithubClient(token);
    const refName = tag?.trim() || githubRefName(url, branch) || ref.branch;
    const { data } = await octokit.repos.getCommit({
      owner: ref.owner,
      repo: ref.repo,
      ref: refName,
    });
    const remoteSha = data.sha;
    const compare = (lastSha ?? "").trim();
    if (!compare) return "unknown";
    return remoteSha === compare ? "up_to_date" : "updates_available";
  } catch {
    return "unknown";
  }
}

function isGitSource(row: {
  localPath: string | null;
  sourceKind: string;
  sourceType: string | null;
  url: string;
}): boolean {
  if (!row.localPath) return false;
  const kind = (row.sourceKind || "").toLowerCase();
  if (kind !== "github" && kind !== "git") return false;
  if ((row.sourceType || "git") === "local") return false;
  if ((row.url || "").startsWith("file://")) return false;
  return true;
}

export async function checkAllSourceUpdates(
  repo: AppRepository,
  participant:
    "worker.startSourceWatcher" | "job.check-source-updates" = "job.check-source-updates",
): Promise<{
  checked: Array<{ source_id: number; name: string; update_status: RemoteUpdateStatus }>;
  skipped: Array<{ source_id: number; name: string; reason: string }>;
  updates_available: number;
  checked_count: number;
}> {
  requireSourceSyncParticipant(participant);
  const token = repo.getSetting("github_pat");
  const checked: Array<{ source_id: number; name: string; update_status: RemoteUpdateStatus }> = [];
  const skipped: Array<{ source_id: number; name: string; reason: string }> = [];

  for (const source of repo.listSources()) {
    requireSourceSyncParticipant(participant);
    const row = repo.getProjectRow(source.id);
    if (!row || !isGitSource(row)) {
      skipped.push({ source_id: source.id, name: source.name, reason: "not_git" });
      continue;
    }
    const status = await remoteUpdateStatusOctokit(
      row.url,
      row.branch ?? "main",
      row.lastCommitSha,
      token,
      row.tag,
    );
    requireSourceSyncParticipant(participant);
    const now = new Date().toISOString();
    repo.updateSource(source.id, {
      metadata: {
        [REMOTE_UPDATE_STATUS_KEY]: status,
        [REMOTE_CHECKED_AT_KEY]: now,
      },
    });
    if (status === "updates_available" && source.update_status !== "updates_available") {
      repo.recordAppEvent({
        kind: "source.update_available",
        actorType: "source",
        actorId: String(source.id),
        payload: {
          source_id: source.id,
          source_name: source.name,
          source_url: source.url,
          branch: row.branch ?? "main",
          previous_sha: row.lastCommitSha,
        },
      });
    }
    checked.push({ source_id: source.id, name: source.name, update_status: status });
  }

  requireSourceSyncParticipant(participant);
  repo.setSetting("source_update_check_last_run_at", new Date().toISOString());
  return {
    checked,
    skipped,
    updates_available: checked.filter((c) => c.update_status === "updates_available").length,
    checked_count: checked.length,
  };
}
