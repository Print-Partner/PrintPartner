#!/usr/bin/env python3
"""Pick the runners for the Rust and packaged-smoke jobs.

Main pushes, merge groups and manual runs use pp-local for the Linux Rust jobs unless
the PP_RUST_RUNNER repository variable says hosted or the fleet looks unavailable.
The nightly schedule stays hosted. Pull requests follow the PP_PR_RUNNER variable:
blacksmith (the default when unset or unknown), hosted (GitHub's runners), or pp-iso.
blacksmith runs rust (linux) and the packaged smoke job on Blacksmith runners; hosted
puts both back on GitHub's; pp-iso runs rust (linux) on the isolated pool while the smoke
job stays on Blacksmith. rust (windows) always runs on windows-latest, and the smoke job
stays on GitHub's runners on every other event, because Blacksmith minutes cost money. Required check names do not depend on the runner. GITHUB_TOKEN cannot list runners,
so the idle-runner check needs the optional PP_RUNNER_READ_TOKEN secret. Without
it, a pp-local Rust job queued for STALE_MINUTES or more marks the fleet as
unavailable. See docs/development/ci.md.
"""

from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import sys
import urllib.error
import urllib.request

LOCAL = ["self-hosted", "linux", "x64", "pp-local"]
ISOLATED = ["self-hosted", "linux", "x64", "pp-iso"]
HOSTED = "ubuntu-latest"
# 4 vCPU: hosted 4 vCPU medians stay well under the job timeouts (docs/development/ci.md).
# pp-iso covers pull requests only for the Linux Rust jobs; Blacksmith runs rust (linux) and the
# packaged smoke job on pull requests only, and every other event stays on GitHub's free runners.
BLACKSMITH = {"linux": "blacksmith-4vcpu-ubuntu-2404", "smoke": "blacksmith-4vcpu-ubuntu-2404"}
LOCAL_EVENTS = {"push", "merge_group", "workflow_dispatch"}
STALE_MINUTES = 15
# Only these jobs run on pp-local; their names keep the required check names.
LOCAL_JOBS = ("rust (linux)",)


def idle_runners(runners):
    """Count online, idle runners that carry the pp-local label."""
    online = idle = 0
    for runner in runners:
        labels = {label.get("name", "").lower() for label in runner.get("labels", [])}
        if "pp-local" not in labels or runner.get("status") != "online":
            continue
        online += 1
        idle += not runner.get("busy", True)
    return online, idle


def stale_jobs(jobs, now, minutes=STALE_MINUTES):
    """Names of pp-local Rust jobs that have waited for a runner `minutes` or longer."""
    stale = []
    for job in jobs:
        labels = {label.lower() for label in job.get("labels", [])}
        if (job.get("status") != "queued" or "pp-local" not in labels
                or not job.get("name", "").startswith(LOCAL_JOBS)):
            continue
        created = datetime.fromisoformat(job["created_at"].replace("Z", "+00:00"))
        if now - created >= timedelta(minutes=minutes):
            stale.append(job["name"])
    return stale


def pr_mode(env):
    mode = env.get("PP_PR_RUNNER", "").strip().lower()
    return mode if mode in {"hosted", "blacksmith", "pp-iso"} else "blacksmith"


def runner_kind(runs_on):
    """The kind of runner the job steps adapt to: pp-local, pp-iso, blacksmith or hosted."""
    if isinstance(runs_on, list):
        return "pp-local" if "pp-local" in runs_on else "pp-iso"
    return "blacksmith" if runs_on.startswith("blacksmith-") else "hosted"


def choose(env, fetch, now):
    """Return (runs_on, reason). `fetch(path, token)` returns parsed JSON or raises."""
    event = env.get("EVENT_NAME", "")
    if event == "pull_request":
        mode = pr_mode(env)
        if mode == "blacksmith":
            return BLACKSMITH["linux"], "pull request with PP_PR_RUNNER=blacksmith"
        if mode == "pp-iso":
            if env.get("HEAD_REPOSITORY") != env.get("REPOSITORY"):
                return BLACKSMITH["linux"], "fork pull request; pp-iso is for same-repository pull requests"
            return ISOLATED, "pull request with PP_PR_RUNNER=pp-iso"
        return HOSTED, "pull request with PP_PR_RUNNER=hosted"
    if event not in LOCAL_EVENTS:
        return HOSTED, f"{event or 'unknown'} events stay hosted"
    mode = env.get("PP_RUST_RUNNER", "").strip().lower() or "local"
    if mode == "hosted":
        return HOSTED, "PP_RUST_RUNNER=hosted"
    if mode != "local":
        return HOSTED, f"unknown PP_RUST_RUNNER={mode!r}; using hosted"

    repo = env["REPOSITORY"]
    if env.get("RUNNER_TOKEN"):
        try:
            runners = fetch(f"repos/{repo}/actions/runners?per_page=100",
                            env["RUNNER_TOKEN"]).get("runners", [])
        except (OSError, ValueError) as error:
            print(f"::warning::cannot list runners ({error}); checking queued jobs instead")
        else:
            online, idle = idle_runners(runners)
            if idle:
                return LOCAL, f"{idle} of {online} online pp-local runners idle"
            return HOSTED, f"no idle pp-local runner ({online} online)"

    # Fallback: a pp-local Rust job that no runner picked up means the fleet is down or full.
    token = env.get("GH_TOKEN", "")
    workflow = env.get("WORKFLOW_FILE", "rust-cross-os.yml")
    since = (now - timedelta(hours=6)).strftime("%Y-%m-%dT%H:%M:%SZ")
    try:
        runs = []
        for status in ("queued", "in_progress"):
            runs += fetch(f"repos/{repo}/actions/workflows/{workflow}/runs"
                          f"?status={status}&per_page=20&created=%3E{since}",
                          token).get("workflow_runs", [])
        for run in runs:
            if str(run.get("id")) == env.get("RUN_ID"):
                continue
            jobs = fetch(f"repos/{repo}/actions/runs/{run['id']}/jobs?filter=latest&per_page=100",
                         token).get("jobs", [])
            stale = stale_jobs(jobs, now)
            if stale:
                return HOSTED, (f"run {run['id']} job {stale[0]!r} queued on pp-local for "
                                f"{STALE_MINUTES}+ minutes")
    except (OSError, ValueError) as error:
        return LOCAL, f"runner status unreadable ({error}); using PP_RUST_RUNNER default"
    return LOCAL, "no pp-local Rust job queued for 15+ minutes"


def github_fetch(path, token):
    request = urllib.request.Request(f"https://api.github.com/{path}", headers={
        "Accept": "application/vnd.github+json",
        "Authorization": f"Bearer {token}",
        "X-GitHub-Api-Version": "2022-11-28",
    })
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise OSError(f"HTTP {error.code} for {path.split('?')[0]}") from None


def main():
    runs_on, reason = choose(os.environ, github_fetch, datetime.now(timezone.utc))
    value = json.dumps(runs_on, separators=(",", ":"))
    paid = os.environ.get("EVENT_NAME") == "pull_request" and pr_mode(os.environ) != "hosted"
    smoke = json.dumps(BLACKSMITH["smoke"] if paid else "ubuntu-24.04")
    message = f"Linux Rust runner: {value} ({reason}); packaged smoke {smoke}\n"
    print(message, end="")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"linux_runs_on={value}\nlinux_kind={runner_kind(runs_on)}\n"
                     f"smoke_runs_on={smoke}\n")
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as output:
        output.write(message)


if __name__ == "__main__":
    sys.exit(main())
