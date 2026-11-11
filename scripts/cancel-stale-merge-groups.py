#!/usr/bin/env python3
"""Cancel unmerged cross-OS queue runs after their ref returns HTTP 404."""

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
    main_ref = request(f'repos/{repository}/git/ref/heads/main')
    if not isinstance(main_ref, dict) or main_ref.get('ref') != 'refs/heads/main':
        raise ValueError('Cannot verify main ref access')
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
                        sha = run.get('head_sha', '')
                        if not re.fullmatch(r'[0-9a-f]{40}', sha):
                            raise ValueError('Invalid head SHA')
                        candidates[run_id] = (branch, sha)
    gone = set()
    for branch in sorted({branch for branch, _ in candidates.values()}):
        try:
            ref = request(f'repos/{repository}/git/ref/heads/{quote(branch, safe="/")}')
        except ApiError as error:
            if error.status != 404:
                raise
            gone.add(branch)
        else:
            if not isinstance(ref, dict) or ref.get('ref') != f'refs/heads/{branch}':
                raise ValueError('Unexpected ref response')
    unmerged = set()
    for sha in sorted({sha for branch, sha in candidates.values() if branch in gone}):
        comparison = request(f'repos/{repository}/compare/{sha}...main')
        if not isinstance(comparison, dict):
            raise ValueError('Invalid comparison response')
        status = comparison.get('status')
        # main is the head of this comparison: ahead means sha is its ancestor.
        if status in {'behind', 'diverged'}:
            unmerged.add(sha)
        elif status not in {'identical', 'ahead'}:
            raise ValueError('Unexpected comparison status')
    return sorted(run_id for run_id, (branch, sha) in candidates.items()
                  if branch in gone and sha in unmerged)


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
    cancelled = 0
    for run_id in runs:
        try:
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
