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

import yaml

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/cancel-stale-merge-groups.py'
spec = importlib.util.spec_from_file_location('canceller', SCRIPT)
canceller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(canceller)
REPO = 'Print-Partner/PrintPartner'
BRANCH = 'gh-readonly-queue/main/pr-1-sha'


def run(run_id=1, branch=BRANCH, event='merge_group', status='queued'):
    return {'id': run_id, 'head_branch': branch, 'event': event, 'status': status}


class Api:
    def __init__(self, runs=(), refs=None, fail_listing=False):
        self.runs, self.refs, self.fail_listing = list(runs), refs or {}, fail_listing
        self.calls = []

    def __call__(self, path, method='GET', paginate=False):
        self.calls.append((path, method, paginate))
        if '/workflows/' in path:
            if self.fail_listing:
                raise canceller.ApiError(503)
            # Multiple pages and duplicate listings must not drop or repeat cancellations.
            return [{'workflow_runs': self.runs[:1]}, {'workflow_runs': self.runs[1:]}]
        if '/git/ref/heads/' in path:
            branch = path.split('/git/ref/heads/')[1]
            status = self.refs.get(branch, 404)
            if status != 200:
                raise canceller.ApiError(status)
            return {'ref': f'refs/heads/{branch}'}
        self.assert_cancel(path, method)
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

    def test_cli_and_gh_http_status_handling(self):
        for status, expected in ((200, 0), (404, 0), (403, 1), (503, 1)):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                fake = root / 'gh'
                fake.write_text('''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps(args) + '\\n')
path = args[3]
if '/workflows/' in path:
    print(json.dumps([{'workflow_runs': [{'id': 17, 'event': 'merge_group',
          'head_branch': 'gh-readonly-queue/main/pr-1-sha', 'status': 'queued'}]}]))
elif '/git/ref/heads/' in path:
    status = int(os.environ['REF_STATUS'])
    if status != 200:
        print(f'gh: request failed (HTTP {status})', file=sys.stderr)
        sys.exit(1)
    print(json.dumps({'ref': 'refs/heads/gh-readonly-queue/main/pr-1-sha'}))
''')
                fake.chmod(0o755)
                calls = root / 'calls'
                result = subprocess.run([sys.executable, '-B', str(SCRIPT)], capture_output=True, text=True,
                                        env={**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}',
                                             'GH_REPO': REPO, 'CALLS': str(calls), 'REF_STATUS': str(status)})
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                posted = [json.loads(line) for line in calls.read_text().splitlines() if 'POST' in line]
                self.assertEqual(len(posted), int(status == 404))
                self.assertIn('Cancelled 1' if status == 404 else 'Cancelled 0' if status == 200
                              else 'cancelled nothing', result.stdout)

    def test_workflow_has_only_actions_write_and_never_checks_out_code(self):
        workflow = yaml.load((ROOT / '.github/workflows/cancel-stale-merge-groups.yml').read_text(),
                             Loader=yaml.BaseLoader)
        self.assertEqual(workflow['permissions'], {'actions': 'write'})
        self.assertEqual(set(workflow['on']), {'schedule', 'workflow_dispatch'})
        job = workflow['jobs']['cancel']
        self.assertEqual(job['if'], "github.ref == 'refs/heads/main'")
        self.assertTrue(all('uses' not in step for step in job['steps']))
        self.assertIn('/refs/heads/main/scripts/', job['steps'][0]['run'])
        self.assertNotIn('secrets.', json.dumps(workflow))
