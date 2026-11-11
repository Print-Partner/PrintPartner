"""Exercise canceller preflight, main and the real CLI with a fake gh executable."""

import contextlib
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
BRANCH = 'gh-readonly-queue/main/pr-1-sha'
SHA = 'a' * 40


def run(run_id=1, branch=BRANCH, event='merge_group', status='queued', sha=SHA):
    return {'id': run_id, 'head_branch': branch, 'head_sha': sha, 'event': event, 'status': status}


class Api:
    def __init__(self, runs=(), refs=None, fail_listing=False, comparisons=None, cancel_status=None):
        self.runs, self.refs, self.fail_listing = list(runs), refs or {}, fail_listing
        self.comparisons, self.cancel_status = comparisons or {}, cancel_status or {}
        self.calls = []

    def __call__(self, path, method='GET', paginate=False):
        self.calls.append((path, method, paginate))
        if '/workflows/' in path:
            if self.fail_listing:
                raise canceller.ApiError(503)
            # Multiple pages and duplicate listings must not drop or repeat cancellations.
            return [{'workflow_runs': self.runs[:1]}, {'workflow_runs': self.runs[1:]}]
        if '/git/ref/heads/' in path:
            branch = unquote(path.split('/git/ref/heads/')[1])
            status = self.refs.get(branch, 200 if branch == 'main' else 404)
            if status != 200:
                raise canceller.ApiError(status)
            return {'ref': f'refs/heads/{branch}'}
        if '/compare/' in path:
            sha = path.split('/compare/')[1].split('...')[0]
            status = self.comparisons.get(sha, 'diverged')
            if isinstance(status, int):
                raise canceller.ApiError(status)
            return {'status': status}
        self.assert_cancel(path, method)
        status = self.cancel_status.get(int(path.split('/runs/')[1].split('/')[0]))
        if status:
            raise canceller.ApiError(status)
        return None

    def assert_cancel(self, path, method):
        if method != 'POST' or not path.endswith('/cancel'):
            raise AssertionError((path, method))

    def cancelled(self):
        return [path for path, method, _ in self.calls if method == 'POST']


class CancellerTests(unittest.TestCase):
    def main(self, api):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = canceller.main({'GH_REPO': REPO}, api)
        return code, output.getvalue()

    def test_gone_ref_cancels_only_target_runs_once(self):
        api = Api([run(), run(2, status='in_progress'), run(3, branch='main'),
                   run(4, event='pull_request'), run(5, status='completed')])
        code, output = self.main(api)
        self.assertEqual(code, 0)
        self.assertEqual(api.cancelled(), [f'repos/{REPO}/actions/runs/{n}/cancel' for n in (1, 2)])
        self.assertIn('Cancelled 2 stale merge-group run(s)', output)
        listings = [path for path, _, paginate in api.calls if paginate]
        self.assertEqual(len(listings), 4)
        for workflow in canceller.WORKFLOWS:
            for status in canceller.STATUSES:
                self.assertIn(f'repos/{REPO}/actions/workflows/{workflow}/runs?status={status}&per_page=100', listings)

    def test_live_ref_is_never_cancelled(self):
        api = Api([run()], {BRANCH: 200})
        self.assertEqual(self.main(api)[0], 0)
        self.assertEqual(api.cancelled(), [])

    def test_api_error_cancels_nothing_even_after_a_gone_ref(self):
        for status in (401, 403, 429, 500):
            api = Api([run(), run(2, BRANCH + '-later')], {BRANCH + '-later': status})
            code, output = self.main(api)
            self.assertEqual(code, 1)
            self.assertIn('cancelled nothing', output)
            self.assertEqual(api.cancelled(), [])
        api = Api([run()], fail_listing=True)
        self.assertEqual(self.main(api)[0], 1)
        self.assertEqual(api.cancelled(), [])

    def test_startup_main_ref_must_be_readable(self):
        for status in (401, 403, 404, 500):
            api = Api([run()], {'main': status})
            code, output = self.main(api)
            self.assertEqual(code, 1)
            self.assertIn('cancelled nothing', output)
            self.assertEqual(api.calls, [(f'repos/{REPO}/git/ref/heads/main', 'GET', False)])
            self.assertEqual(api.cancelled(), [])
        def wrong_ref(*args, **kwargs):
            return {'ref': 'refs/heads/other'}
        self.assertEqual(self.main(wrong_ref)[0], 1)

    def test_merged_groups_survive_and_superseded_groups_cancel(self):
        for status, cancelled in (('identical', False), ('ahead', False),
                                  ('behind', True), ('diverged', True)):
            with self.subTest(status=status):
                api = Api([run()], comparisons={SHA: status})
                self.assertEqual(self.main(api)[0], 0)
                self.assertEqual(bool(api.cancelled()), cancelled)
                self.assertIn((f'repos/{REPO}/compare/{SHA}...main', 'GET', False), api.calls)

    def test_comparison_failure_aborts_all_cancellations(self):
        for status in (403, 404, 503, 'unknown'):
            api = Api([run(), run(2, sha='b' * 40)], comparisons={'b' * 40: status})
            self.assertEqual(self.main(api)[0], 1)
            self.assertEqual(api.cancelled(), [])

    def test_finished_run_conflict_does_not_stop_later_cancellations(self):
        api = Api([run(), run(2)], cancel_status={1: 409})
        code, output = self.main(api)
        self.assertEqual(code, 0)
        self.assertEqual(len(api.cancelled()), 2)
        self.assertIn('Skipped finished run 1', output)
        self.assertIn('Cancelled stale merge-group run 2', output)
        self.assertIn('Cancelled 1 stale merge-group run(s)', output)
        api = Api([run(), run(2)], cancel_status={1: 403})
        self.assertEqual(self.main(api)[0], 1)
        self.assertEqual(len(api.cancelled()), 1)

    def test_special_characters_in_queue_branch_are_encoded_and_decoded(self):
        branch = 'gh-readonly-queue/main/pr-1+revision#?%'
        for status in (200, 404):
            api = Api([run(branch=branch)], {branch: status})
            self.assertEqual(self.main(api)[0], 0)
            self.assertEqual(bool(api.cancelled()), status == 404)
            self.assertIn((f'repos/{REPO}/git/ref/heads/gh-readonly-queue/main/pr-1%2Brevision%23%3F%25',
                           'GET', False), api.calls)

    def test_cli_and_gh_http_status_handling(self):
        for status, main_status, comparison, cancel_status, expected, cancelled in (
            (200, 200, 'diverged', 202, 0, 0),
            (404, 200, 'diverged', 202, 0, 1),
            (403, 200, 'diverged', 202, 1, 0),
            (503, 200, 'diverged', 202, 1, 0),
            (404, 403, 'diverged', 202, 1, 0),
            (404, 404, 'diverged', 202, 1, 0),
            (404, 200, 'ahead', 202, 0, 0),
            (404, 200, 'identical', 202, 0, 0),
            (404, 200, 'diverged', 409, 0, 0),
        ):
            with self.subTest(status=status, main=main_status, comparison=comparison, cancel=cancel_status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                fake = root / 'gh'
                fake.write_text('''#!/usr/bin/env python3
import json, os, sys
from urllib.parse import unquote
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps(args) + '\\n')
path = args[3]
if '/workflows/' in path:
    print(json.dumps([{'workflow_runs': [{'id': 17, 'event': 'merge_group',
          'head_branch': 'gh-readonly-queue/main/pr-1-sha', 'head_sha': 'a' * 40, 'status': 'queued'}]}]))
elif '/git/ref/heads/' in path:
    branch = unquote(path.split('/git/ref/heads/')[1])
    status = int(os.environ['MAIN_STATUS'] if branch == 'main' else os.environ['REF_STATUS'])
    if status != 200:
        print(f'gh: request failed (HTTP {status})', file=sys.stderr)
        sys.exit(1)
    print(json.dumps({'ref': 'refs/heads/' + branch}))
elif '/compare/' in path:
    print(json.dumps({'status': os.environ['COMPARISON']}))
elif '/cancel' in path and os.environ['CANCEL_STATUS'] != '202':
    print('gh: request failed (HTTP ' + os.environ['CANCEL_STATUS'] + ')', file=sys.stderr)
    sys.exit(1)
''')
                fake.chmod(0o755)
                calls = root / 'calls'
                result = subprocess.run([sys.executable, '-B', str(SCRIPT)], capture_output=True, text=True,
                                        env={**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}',
                                             'GH_REPO': REPO, 'CALLS': str(calls), 'REF_STATUS': str(status),
                                             'MAIN_STATUS': str(main_status), 'COMPARISON': comparison,
                                             'CANCEL_STATUS': str(cancel_status)})
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                posted = [json.loads(line) for line in calls.read_text().splitlines() if 'POST' in line]
                self.assertEqual(len(posted), int(status == 404 and main_status == 200
                                                  and comparison == 'diverged'))
                self.assertIn(f'Cancelled {cancelled} stale' if expected == 0 else 'cancelled nothing',
                              result.stdout)

    def test_workflow_has_minimum_permissions_and_never_checks_out_code(self):
        workflow = yaml.load((ROOT / '.github/workflows/cancel-stale-merge-groups.yml').read_text(),
                             Loader=yaml.BaseLoader)
        self.assertEqual(workflow['permissions'], {'actions': 'write', 'contents': 'read'})
        self.assertEqual(set(workflow['on']), {'schedule', 'workflow_dispatch'})
        job = workflow['jobs']['cancel']
        self.assertEqual(job['if'], "github.ref == 'refs/heads/main'")
        self.assertTrue(all('uses' not in step for step in job['steps']))
        self.assertIn('/refs/heads/main/scripts/', job['steps'][0]['run'])
        self.assertNotIn('secrets.', json.dumps(workflow))
