#!/usr/bin/env python3
"""Cancel unmerged cross-OS queue runs after their ref returns HTTP 404."""

from datetime import datetime, timedelta, timezone
import json
import os
import re
import subprocess
import sys
from urllib.parse import quote

WORKFLOWS = ('rust-cross-os.yml', 'desktop-native-macos.yml')
STATUSES = ('queued', 'in_progress')


class ApiError(Exception):
    def __init__(self, status):
        self.status = status
        super().__init__(f'GitHub API error (HTTP {status or "unknown"})')


def api(path, method='GET', paginate=False):
    command = ['gh', 'api', '--method', method, path]
    if paginate:
        command += ['--paginate', '--slurp']
    result = subprocess.run(command, capture_output=True, text=True, timeout=60, check=False)
    if result.returncode:
        match = re.search(r'\(HTTP (\d{3})\)', result.stderr)
        raise ApiError(int(match[1]) if match else None)
    return json.loads(result.stdout) if result.stdout.strip() else None


def ref_exists(repository, branch, request):
    try:
        ref = request(f'repos/{repository}/git/ref/heads/{quote(branch, safe="/")}')
    except ApiError as error:
        if error.status == 404:
            return False
        raise
    if not isinstance(ref, dict) or ref.get('ref') != f'refs/heads/{branch}':
        raise ValueError('Unexpected ref response')
    return True


def merged(repository, number, request):
    pr = request(f'repos/{repository}/pulls/{number}')
    if (not isinstance(pr, dict) or pr.get('number') != number
            or pr.get('state') not in {'open', 'closed'} or 'merged_at' not in pr):
        raise ValueError('Unexpected PR response')
    stamp = pr['merged_at']
    if stamp is not None and (not isinstance(stamp, str) or not stamp or pr['state'] != 'closed'):
        raise ValueError('Unexpected merged PR state')
    return stamp is not None


def stale_runs(repository, request, now):
    """Preflight all run/ref/PR reads before writes; squash ancestry is irrelevant."""
    if not ref_exists(repository, 'main', request):
        raise ValueError('Cannot verify main ref access')
    candidates = {}
    for workflow in WORKFLOWS:
        for status in STATUSES:
            pages = request(f'repos/{repository}/actions/workflows/{workflow}/runs'
                            f'?status={status}&per_page=100', paginate=True)
            for page in pages:
                for run in page['workflow_runs']:
                    branch = run.get('head_branch') or ''
                    if (run.get('event') != 'merge_group' or run.get('status') not in STATUSES
                            or not branch.startswith('gh-readonly-queue/')):
                        continue
                    match = re.fullmatch(r'gh-readonly-queue/main/pr-([1-9][0-9]*)-[0-9a-f]{40}', branch)
                    if not match:
                        raise ValueError('Unparseable queue ref')
                    run_id = run['id']
                    if not isinstance(run_id, int) or isinstance(run_id, bool) or run_id <= 0:
                        raise ValueError('Invalid run id')
                    created = datetime.fromisoformat(run['created_at'].replace('Z', '+00:00'))
                    if created.utcoffset() is None:
                        raise ValueError('Run creation time must include timezone')
                    candidates[run_id] = (branch, int(match[1]), created)
    gone = {branch for branch in sorted({branch for branch, _, _ in candidates.values()})
            if not ref_exists(repository, branch, request)}
    landed = {number: merged(repository, number, request)
              for number in sorted({number for branch, number, _ in candidates.values() if branch in gone})}
    return [(run_id, branch) for run_id, (branch, number, created) in sorted(candidates.items())
            if branch in gone and not landed[number] and now - created > timedelta(minutes=10)]


def main(env=None, request=None, now=None):
    env = os.environ if env is None else env
    request = api if request is None else request
    repository = env.get('GH_REPO', '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        print('::error::GH_REPO must be owner/repository')
        return 1
    try:
        runs = stale_runs(repository, request, now or datetime.now(timezone.utc))
    except (ApiError, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print(f'::error::Preflight failed; cancelled nothing: {error}')
        return 1
    cancelled = 0
    for run_id, branch in runs:
        try:
            if ref_exists(repository, branch, request):
                print(f'Skipped run {run_id}: queue ref reappeared')
                continue
            request(f'repos/{repository}/actions/runs/{run_id}/cancel', method='POST')
        except ApiError as error:
            if error.status == 409:
                print(f'Skipped finished run {run_id} (HTTP 409)')
                continue
            print(f'::error::Could not cancel run {run_id}: {error}')
            return 1
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            print(f'::error::Could not cancel run {run_id}: {error}')
            return 1
        cancelled += 1
        print(f'Cancelled stale merge-group run {run_id}')
    print(f'Cancelled {cancelled} stale merge-group run(s)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
