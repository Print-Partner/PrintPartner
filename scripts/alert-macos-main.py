#!/usr/bin/env python3
"""Report failed macOS main runs in one rolling labelled issue per workflow kind."""

import json
import os
import subprocess

LABEL = "ci-macos-main"


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True).strip()


def rolling_title(kind):
    if kind == "native":
        return "macOS CI failure on main: Native macOS bundle"
    return "macOS CI failure on main: rust (macos)"


def alert(repository, run_id, attempt, kind, server):
    jobs = json.loads(gh("api", "--paginate", "--slurp",
                         f"repos/{repository}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"))
    failed = [job for page in jobs for job in page["jobs"]
              if job["conclusion"] == "failure"
              and (kind == "native" or job["name"] == "rust (macos)")]
    if not failed:
        return
    # Idempotent label creation also tolerates simultaneous alerts from other runs.
    gh("label", "create", LABEL, "--repo", repository, "--color", "D93F0B",
       "--description", "macOS CI failure on main", "--force")
    title = rolling_title(kind)
    issues = json.loads(gh("issue", "list", "--repo", repository, "--label", LABEL,
                           "--state", "open", "--search", f'"{title}" in:title',
                           "--json", "number,title", "--limit", "100"))
    issues = [issue for issue in issues if issue["title"] == title]
    url = f"{server}/{repository}/actions/runs/{run_id}/attempts/{attempt}"
    subject = "Native macOS bundle workflow" if kind == "native" else "rust (macos)"
    body = f"{subject} failed on main (run {run_id}, attempt {attempt}).\n\n" + url + "\n\n" + "\n".join(
        f"- {job['name']}: {job['html_url']}" for job in failed)
    if issues:
        gh("issue", "comment", str(issues[0]["number"]), "--repo", repository, "--body", body)
    else:
        gh("issue", "create", "--repo", repository, "--label", LABEL,
           "--title", title, "--body", body)


if __name__ == "__main__":
    alert(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_RUN_ID"],
          os.environ["GITHUB_RUN_ATTEMPT"], os.environ["ALERT_KIND"],
          os.environ["GITHUB_SERVER_URL"])
