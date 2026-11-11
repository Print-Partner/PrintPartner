#!/usr/bin/env python3
"""Cancel cross-OS merge-group runs only after the queue ref returns HTTP 404."""

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


def stale_runs(repository, request):
    """Preflight every listing and ref before issuing any cancellation."""
    candidates = {}
    for workflow in WORKFLOWS:
        for status in STATUSES:
            pages = request(f'repos/{repository}/actions/workflows/{workflow}/runs'
                            f'?status={status}&per_page=100', paginate=True)
            for page in pages:
                for run in page['workflow_runs']:
                    branch = run.get('head_branch') or ''
                    if (run.get('event') == 'merge_group' and run.get('status') in STATUSES
                            and branch.startswith('gh-readonly-queue/')):
                        run_id = run['id']
                        if not isinstance(run_id, int) or isinstance(run_id, bool) or run_id <= 0:
                            raise ValueError('Invalid run id')
                        candidates[run_id] = branch
    gone = set()
    for branch in sorted(set(candidates.values())):
        try:
            ref = request(f'repos/{repository}/git/ref/heads/{quote(branch, safe="/")}')
        except ApiError as error:
            if error.status != 404:
                raise
            gone.add(branch)
        else:
            if ref.get('ref') != f'refs/heads/{branch}':
                raise ValueError('Unexpected ref response')
    return sorted(run_id for run_id, branch in candidates.items() if branch in gone)


def main(env=None, request=None):
    env = os.environ if env is None else env
    request = api if request is None else request
    repository = env.get('GH_REPO', '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        print('::error::GH_REPO must be owner/repository')
        return 1
    try:
        runs = stale_runs(repository, request)
    except (ApiError, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print(f'::error::Preflight failed; cancelled nothing: {error}')
        return 1
    for run_id in runs:
        try:
            request(f'repos/{repository}/actions/runs/{run_id}/cancel', method='POST')
        except (ApiError, OSError, ValueError, subprocess.TimeoutExpired) as error:
            print(f'::error::Could not cancel run {run_id}: {error}')
            return 1
        print(f'Cancelled stale merge-group run {run_id}')
    print(f'Cancelled {len(runs)} stale merge-group run(s)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
