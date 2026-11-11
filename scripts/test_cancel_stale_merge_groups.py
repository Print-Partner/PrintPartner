"""Test squash-merged versus stale queue runs, preflight and the real CLI."""

import contextlib
from datetime import datetime, timedelta, timezone
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from urllib.parse import unquote

import yaml

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/cancel-stale-merge-groups.py'
spec = importlib.util.spec_from_file_location('canceller', SCRIPT)
canceller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(canceller)
REPO = 'Print-Partner/PrintPartner'
SHA = 'a' * 40
BRANCH = f'gh-readonly-queue/main/pr-1-{SHA}'
NOW = datetime(2026, 11, 11, 4, tzinfo=timezone.utc)


def run(run_id=1, branch=BRANCH, event='merge_group', status='queued', minutes=20):
    return {'id': run_id, 'head_branch': branch, 'head_sha': SHA, 'event': event, 'status': status,
            'created_at': (NOW - timedelta(minutes=minutes)).isoformat()}


class Api:
    def __init__(self, runs=(), refs=None, prs=None, cancel_status=None, fail_listing=False):
        self.runs, self.refs, self.prs = list(runs), refs or {}, prs or {}
        self.cancel_status, self.fail_listing = cancel_status or {}, fail_listing
        self.calls = []

    def __call__(self, path, method='GET', paginate=False):
        self.calls.append((path, method, paginate))
        if '/workflows/' in path:
            if self.fail_listing:
                raise canceller.ApiError(503)
            return [{'workflow_runs': self.runs[:1]}, {'workflow_runs': self.runs[1:]}]
        if '/git/ref/heads/' in path:
            branch = unquote(path.split('/git/ref/heads/')[1])
            status = self.refs.get(branch, 200 if branch == 'main' else 404)
            if isinstance(status, list):
                status = status.pop(0)
            if status != 200:
                raise canceller.ApiError(status)
            return {'ref': f'refs/heads/{branch}', 'object': {'sha': 'b' * 40}}
        if '/pulls/' in path:
            number = int(path.split('/pulls/')[1])
            pr = self.prs.get(number, {'number': number, 'state': 'open', 'merged_at': None})
            if isinstance(pr, int):
                raise canceller.ApiError(pr)
            return pr
        if method != 'POST' or not path.endswith('/cancel'):
            raise AssertionError((path, method))  # In particular, no compare endpoint is allowed.
        status = self.cancel_status.get(int(path.split('/runs/')[1].split('/')[0]))
        if status:
            raise canceller.ApiError(status)
        return None

    def cancelled(self):
        return [path for path, method, _ in self.calls if method == 'POST']


class CancellerTests(unittest.TestCase):
    def main(self, api):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = canceller.main({'GH_REPO': REPO}, api, NOW)
        return code, output.getvalue()

    def test_gone_ref_cancels_only_target_runs_once(self):
        api = Api([run(), run(2, status='in_progress'), run(3, branch='main'),
                   run(4, event='pull_request'), run(5, status='completed')])
        code, output = self.main(api)
        self.assertEqual(code, 0)
        self.assertEqual(api.cancelled(), [f'repos/{REPO}/actions/runs/{n}/cancel' for n in (1, 2)])
        self.assertIn('Cancelled 2 stale merge-group run(s)', output)
        self.assertEqual(sum(paginate for _, _, paginate in api.calls), 4)

    def test_squash_merged_pr_survives_even_with_diverged_queue_sha(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(['git', *args], cwd=directory, text=True).strip()
            git('init', '-q', '-b', 'main')
            git('config', 'user.name', 'Squash fixture')
            git('config', 'user.email', 'fixture@example.com')
            file = Path(directory, 'file')
            file.write_text('base')
            git('add', 'file')
            git('commit', '-qm', 'base')
            git('checkout', '-qb', 'queue')
            file.write_text('group')
            git('commit', '-qam', 'queue group')
            queue_sha = git('rev-parse', 'HEAD')
            git('checkout', '-q', 'main')
            git('merge', '--squash', 'queue')
            git('commit', '-qm', 'squash landed')
            main_sha = git('rev-parse', 'HEAD')
            self.assertEqual(git('rev-list', '--left-right', '--count', f'{queue_sha}...main').split(), ['1', '1'])
            pr = {'number': 1, 'state': 'closed', 'merged_at': '2026-11-11T03:55:00Z',
                  'merge_commit_sha': main_sha}
            fixture = {**run(), 'head_sha': queue_sha}
            api = Api([fixture], prs={1: pr})
            self.assertEqual(self.main(api)[0], 0)
            self.assertEqual(api.cancelled(), [])
            self.assertFalse(any('/compare/' in path for path, _, _ in api.calls))

    def test_open_pr_requeued_under_new_ref_cancels_only_old_run(self):
        new_ref = f'gh-readonly-queue/main/pr-1-{"c" * 40}'
        api = Api([run(), run(2, branch=new_ref)], refs={new_ref: 200})
        self.assertEqual(self.main(api)[0], 0)
        self.assertEqual(api.cancelled(), [f'repos/{REPO}/actions/runs/1/cancel'])

    def test_dequeued_open_and_closed_unmerged_prs_cancel(self):
        for state in ('open', 'closed'):
            api = Api([run()], prs={1: {'number': 1, 'state': state, 'merged_at': None}})
            self.assertEqual(self.main(api)[0], 0)
            self.assertEqual(len(api.cancelled()), 1)

    def test_young_or_future_run_is_skipped(self):
        for minutes in (-1, 0, 9, 10):
            api = Api([run(minutes=minutes)])
            self.assertEqual(self.main(api)[0], 0)
            self.assertEqual(api.cancelled(), [])

    def test_pr_error_unexpected_state_or_unparseable_ref_aborts_before_writes(self):
        other = f'gh-readonly-queue/main/pr-2-{"b" * 40}'
        for pr in (401, 403, 404, 500, {'number': 2, 'state': 'unknown', 'merged_at': None},
                   {'number': 2, 'state': 'open'}):
            api = Api([run(), run(2, branch=other)], prs={2: pr})
            self.assertEqual(self.main(api)[0], 1)
            self.assertEqual(api.cancelled(), [])
        for branch in ('gh-readonly-queue/main/pr-2-bad', 'gh-readonly-queue/other/pr-2-' + SHA):
            api = Api([run(), run(2, branch=branch)])
            self.assertEqual(self.main(api)[0], 1)
            self.assertEqual(api.cancelled(), [])

    def test_ref_errors_and_startup_main_failure_cancel_nothing(self):
        for status in (401, 403, 429, 500):
            api = Api([run()], refs={BRANCH: status})
            self.assertEqual(self.main(api)[0], 1)
            self.assertEqual(api.cancelled(), [])
        for status in (401, 403, 404, 500):
            api = Api([run()], refs={'main': status})
            self.assertEqual(self.main(api)[0], 1)
            self.assertEqual(len(api.calls), 1)
        self.assertEqual(self.main(Api([run()], fail_listing=True))[0], 1)

    def test_ref_reappearing_before_cancel_is_skipped_and_read_failure_stops(self):
        for status, expected in ((200, 0), (403, 1)):
            api = Api([run()], refs={BRANCH: [404, status]})
            self.assertEqual(self.main(api)[0], expected)
            self.assertEqual(api.cancelled(), [])

    def test_finished_run_conflict_does_not_stop_later_cancellations(self):
        api = Api([run(), run(2)], cancel_status={1: 409})
        code, output = self.main(api)
        self.assertEqual(code, 0)
        self.assertEqual(len(api.cancelled()), 2)
        self.assertIn('Skipped finished run 1', output)
        self.assertIn('Cancelled stale merge-group run 2', output)
        self.assertIn('Cancelled 1 stale merge-group run(s)', output)

    def test_special_characters_in_ref_probe_are_encoded_and_decoded(self):
        branch = 'gh-readonly-queue/main/pr-1+revision#%'
        api = Api(refs={branch: 200})
        self.assertTrue(canceller.ref_exists(REPO, branch, api))
        self.assertIn('%2Brevision%23%25', api.calls[0][0])
        # The sweep deliberately rejects this name because it isn't a queue PR/SHA ref.
        self.assertEqual(self.main(Api([run(branch=branch)]))[0], 1)

    def test_cli_outputs_with_squash_merged_stale_young_and_error_fixtures(self):
        for mode, expected, count in (('merged', 0, 0), ('old', 0, 1), ('young', 0, 0),
                                      ('pr-error', 1, 0), ('main-error', 1, 0), ('conflict', 0, 0)):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                fake = root / 'gh'
                fake.write_text('''#!/usr/bin/env python3
from datetime import datetime, timedelta, timezone
import json, os, sys
from urllib.parse import unquote
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps(args) + '\\n')
path, mode = args[3], os.environ['MODE']
def error(status):
    print(f'gh: request failed (HTTP {status})', file=sys.stderr)
    sys.exit(1)
if '/workflows/' in path:
    created = datetime.now(timezone.utc) - timedelta(minutes=1 if mode == 'young' else 20)
    print(json.dumps([{'workflow_runs': [{'id': 17, 'event': 'merge_group',
          'head_branch': 'gh-readonly-queue/main/pr-1-' + 'a' * 40,
          'created_at': created.isoformat(), 'status': 'queued'}]}]))
elif '/git/ref/heads/' in path:
    branch = unquote(path.split('/git/ref/heads/')[1])
    if branch != 'main': error(404)
    if mode == 'main-error': error(403)
    print(json.dumps({'ref': 'refs/heads/main', 'object': {'sha': 'b' * 40}}))
elif '/pulls/' in path:
    if mode == 'pr-error': error(403)
    print(json.dumps({'number': 1, 'state': 'closed' if mode == 'merged' else 'open',
                      'merged_at': '2026-11-11T03:55:00Z' if mode == 'merged' else None}))
elif '/cancel' in path:
    if mode == 'conflict': error(409)
else:
    raise AssertionError(path)
''')
                fake.chmod(0o755)
                calls = root / 'calls'
                result = subprocess.run([sys.executable, '-B', str(SCRIPT)], capture_output=True, text=True,
                                        env={**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}',
                                             'GH_REPO': REPO, 'CALLS': str(calls), 'MODE': mode})
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                posted = [json.loads(line) for line in calls.read_text().splitlines() if 'POST' in line]
                self.assertEqual(len(posted), int(mode in ('old', 'conflict')))
                self.assertIn(f'Cancelled {count} stale' if expected == 0 else 'cancelled nothing', result.stdout)

    def test_workflow_has_minimum_permissions_and_never_checks_out_code(self):
        workflow = yaml.load((ROOT / '.github/workflows/cancel-stale-merge-groups.yml').read_text(),
                             Loader=yaml.BaseLoader)
        self.assertEqual(workflow['permissions'], {'actions': 'write', 'contents': 'read', 'pull-requests': 'read'})
        self.assertEqual(set(workflow['on']), {'schedule', 'workflow_dispatch'})
        job = workflow['jobs']['cancel']
        self.assertEqual(job['if'], "github.ref == 'refs/heads/main'")
        self.assertTrue(all('uses' not in step for step in job['steps']))
        self.assertIn('/refs/heads/main/scripts/', job['steps'][0]['run'])
        self.assertNotIn('secrets.', json.dumps(workflow))
