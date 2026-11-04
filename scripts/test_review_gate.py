"""Run the review-gate step from .github/workflows/review-gate.yml against a fake gh.

The step's Python runs unchanged; only gh is replaced by a script that answers
from a JSON state file. Needs PyYAML.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github/workflows"
HEAD = "0123456789abcdef0123456789abcdef01234567"
OLD = "fedcba9876543210fedcba9876543210fedcba98"
OWNER = "poitee"
DROID, CLAUDE = ".github/workflows/droid-review.yml", ".github/workflows/claude-review.yml"
# Check suite id -> (workflow path, event) of the Actions run behind it.
SUITES = {1: (".github/workflows/review-gate.yml", "pull_request_target"), 2: (DROID, "pull_request"),
          3: (CLAUDE, "pull_request"), 4: (".github/workflows/review-gate.yml", "pull_request"),
          9: (".github/workflows/evil.yml", "pull_request")}
SUITE_FOR = {"review-gate": 1, "droid-review": 2, "claude-code-review": 3, "claude-security-review": 3}

FAKE_GH = r"""
import json, os, sys
from urllib.parse import parse_qs, urlsplit
state = json.load(open(os.environ["FAKE_GH_STATE"]))
url = urlsplit([arg for arg in sys.argv[1:] if not arg.startswith("-")][1])
query, path, slurp = parse_qs(url.query), url.path, "--slurp" in sys.argv
def out(value):
    print(json.dumps(value))
if path.endswith("/pulls/7/files") and slurp:
    out([[{"filename": name} for name in state["files"]]])
elif path.endswith(f"/commits/{state['head']}/check-runs") and slurp and query["filter"] == ["all"]:
    out([{"check_runs": state["checks"]}])
elif path.endswith("/issues/7/comments") and slurp:
    out(state["pages"])
elif path.endswith("/actions/runs") and slurp:
    suite = state["suites"].get(query["check_suite_id"][0])
    out([{"workflow_runs": [{"path": suite[0], "event": suite[1]}] if suite else []}])
elif "/contents/" in path:
    sha = state["blobs"].get(path.split("/contents/", 1)[1] + "@" + query["ref"][0])
    if sha is None:
        sys.exit("HTTP 404")
    out({"sha": sha})
else:
    sys.exit(f"unexpected gh call: {sys.argv}")
"""


def load(name):
    return yaml.load((WORKFLOWS / name).read_text(encoding="utf-8"), Loader=yaml.BaseLoader)


def marker(head=HEAD, reviewer="gpt-5.6-sol", findings="2", author=OWNER):
    body = f"Review recorded.\n<!-- pp-review-gate head={head} reviewer={reviewer} findings={findings} -->"
    return {"user": {"login": author}, "html_url": "https://github.com/o/r/pull/7#issuecomment-1", "body": body}


RUN_IDS = iter(range(100, 100_000))


def run(name, conclusion="success", suite=None, app=15368, started="2026-11-04T15:00:00Z"):
    return {"id": next(RUN_IDS), "name": name, "status": "completed" if conclusion else "in_progress",
            "conclusion": conclusion, "app": {"id": app}, "check_suite": {"id": suite or SUITE_FOR[name]},
            "started_at": started}


class ReviewGateStepTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.folder = Path(folder.name)
        bin_dir = self.folder / "bin"
        bin_dir.mkdir()
        gh = bin_dir / "gh"
        gh.write_text(f"#!{sys.executable}\n{FAKE_GH}")
        gh.chmod(0o755)
        (step,) = load("review-gate.yml")["jobs"]["review-gate"]["steps"]
        self.assertEqual(step["shell"], "python")
        self.script, self.env = step["run"], dict(step["env"])
        self.path = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"

    def gate(self, checks=(), pages=None, files=(), author=OWNER, blobs=None):
        state = self.folder / "state.json"
        state.write_text(json.dumps({
            "head": HEAD, "checks": [run("review-gate", None), *checks], "pages": pages or [[]],
            "files": list(files), "suites": {str(key): value for key, value in SUITES.items()},
            "blobs": {f"{DROID}@{HEAD}": "d1", f"{DROID}@main": "d1", f"{CLAUDE}@{HEAD}": "c1",
                      f"{CLAUDE}@main": "c1"} if blobs is None else blobs}))
        script = self.folder / "gate.py"
        script.write_text(self.script)
        env = {**self.env, "PATH": self.path, "FAKE_GH_STATE": str(state), "GH_TOKEN": "x", "REPO": "o/r",
               "PR_NUMBER": "7", "HEAD_SHA": HEAD, "AUTHOR": author, "OWNER": OWNER, "DEFAULT_BRANCH": "main",
               "GITHUB_STEP_SUMMARY": str(self.folder / "summary.md")}
        result = subprocess.run([sys.executable, str(script)], env=env, capture_output=True, text=True, check=False)
        self.assertNotIn("unexpected gh call", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        return result

    def assertPasses(self, **state):
        result = self.gate(**state)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def assertFails(self, reason=f"::error::No finished review of {HEAD}", **state):
        result = self.gate(**state)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(reason, result.stdout)
        return result

    def test_no_review_fails(self):
        self.assertFails()
        self.assertFails(checks=[run("droid-review", "failure"), run("claude-code-review", None)])

    def test_droid_success_passes(self):
        self.assertIn("droid-review succeeded", self.assertPasses(checks=[run("droid-review")]).stdout)
        self.assertFails(checks=[run("droid-review", "skipped")])

    def test_latest_run_of_a_check_decides(self):
        older, newer = "2026-11-04T15:00:00Z", "2026-11-04T16:00:00Z"
        self.assertFails(checks=[run("droid-review", "success", started=older),
                                 run("droid-review", "failure", started=newer)])
        self.assertPasses(checks=[run("droid-review", "failure", started=older),
                                  run("droid-review", "success", started=newer)])

    def test_claude_needs_code_success_and_security_success_or_neutral(self):
        for security in ("success", "neutral"):
            with self.subTest(security=security):
                self.assertPasses(checks=[run("claude-code-review"), run("claude-security-review", security)])
        for security in ("failure", "cancelled", "skipped", None):
            with self.subTest(security=security):
                checks = [run("claude-code-review")]
                if security:
                    checks.append(run("claude-security-review", security))
                self.assertFails(checks=checks)
        self.assertFails(checks=[run("claude-code-review", "failure"), run("claude-security-review")])
        self.assertFails(checks=[run("claude-security-review")])

    def test_owner_marker_for_the_head_passes(self):
        result = self.assertPasses(pages=[[marker()]], checks=[run("droid-review", "failure")])
        self.assertIn("gpt-5.6-sol, 2 findings", result.stdout)
        self.assertPasses(pages=[[{"user": {"login": OWNER}, "html_url": "u", "body": "hi"}], [marker()]])

    def test_markers_that_do_not_match_fail(self):
        for bad in (marker(head=OLD), marker(head=HEAD[:12]), marker(head=HEAD.upper()), marker(author="lane-bot"),
                    marker(reviewer=""), marker(findings="many"),
                    {**marker(), "body": f"pp-review-gate head={HEAD} reviewer=x findings=0"},
                    {**marker(), "body": None}):
            with self.subTest(body=bad["body"], author=bad["user"]["login"]):
                self.assertFails(pages=[[bad]])

    def test_same_named_checks_from_other_workflows_block_the_gate(self):
        spoofs = {"droid-review from another file": run("droid-review", suite=9),
                  "claude review from another file": run("claude-code-review", suite=9),
                  "review-gate on pull_request": run("review-gate", "success", suite=4),
                  "droid-review from another app": run("droid-review", app=99),
                  "suite with no workflow run": run("droid-review", suite=5)}
        for label, spoof in spoofs.items():
            with self.subTest(label):
                result = self.assertFails(f"::error::check run {spoof['name']} ({spoof['id']}) comes from",
                                          checks=[spoof, run("droid-review")], pages=[[marker()]])
                self.assertNotIn("Passed", result.stdout)

    def test_non_owner_cannot_change_workflows_guard_or_zizmor_config(self):
        for name in (".github/workflows/review-gate.yml", ".github/workflows/new.yml", ".github/zizmor.yml",
                     "scripts/agent-guard.py"):
            with self.subTest(name=name):
                self.assertFails("::error::lane-bot is not the repository owner and changes",
                                 files=[name, "docs/a.md"], author="lane-bot", pages=[[marker()]])
        self.assertPasses(files=["docs/a.md", "scripts/test_review_gate.py"], author="lane-bot", pages=[[marker()]])
        self.assertPasses(files=[".github/workflows/new.yml"], pages=[[marker()]])

    def test_review_counts_only_from_the_default_branch_workflow_definition(self):
        changed = {f"{DROID}@{HEAD}": "d2", f"{DROID}@main": "d1", f"{CLAUDE}@{HEAD}": "c2", f"{CLAUDE}@main": "c1"}
        self.assertFails(checks=[run("droid-review")], files=[DROID], blobs=changed)
        self.assertFails(checks=[run("claude-code-review"), run("claude-security-review")], files=[CLAUDE],
                         blobs=changed)
        self.assertFails(checks=[run("droid-review")], files=[DROID], blobs={})
        # The PR changes the file to match the default branch, or leaves it alone while main moved on.
        self.assertPasses(checks=[run("droid-review")], files=[DROID])
        self.assertPasses(checks=[run("droid-review")], files=["docs/a.md"], blobs=changed)
        self.assertPasses(checks=[run("droid-review")], files=[DROID], blobs=changed, pages=[[marker()]])

    def test_summary_lists_every_path(self):
        self.gate(checks=[run("droid-review", "failure")])
        summary = (self.folder / "summary.md").read_text()
        for row in ("| droid-review | failure |", "| claude-code-review | missing |",
                    "| claude-security-review | missing |", "| coordinator marker | none |"):
            self.assertIn(row, summary)


class ReviewWorkflowShapeTests(unittest.TestCase):
    def test_gate_runs_the_base_branch_definition_without_checkout_or_secrets(self):
        text = (WORKFLOWS / "review-gate.yml").read_text(encoding="utf-8")
        workflow = load("review-gate.yml")
        self.assertEqual(set(workflow["on"]), {"pull_request_target"})
        job = workflow["jobs"]["review-gate"]
        self.assertEqual(job["permissions"], {"actions": "read", "checks": "read", "contents": "read",
                                              "pull-requests": "read"})
        self.assertNotIn("uses:", text)
        self.assertNotRegex(text, r"secrets\.[A-Za-z_]")

    def test_gate_origins_match_the_review_workflows(self):
        script = load("review-gate.yml")["jobs"]["review-gate"]["steps"][0]["run"]
        self.assertIn("droid-review", load("droid-review.yml")["jobs"])
        self.assertEqual(set(load("claude-review.yml")["jobs"]), {"claude-code-review", "claude-security-review"})
        for name, (path, event) in {"review-gate": SUITES[1], "droid-review": SUITES[2],
                                    "claude-code-review": SUITES[3], "claude-security-review": SUITES[3]}.items():
            self.assertIn(f'"{name}": ("{path}", "{event}")', script)
            self.assertIn(name, load(Path(path).name)["jobs"])
            self.assertIn(event, load(Path(path).name)["on"])

    def test_rerun_listens_to_the_review_workflows_and_owner_markers(self):
        rerun = load("review-gate-rerun.yml")
        names = {load(name)["name"] for name in ("droid-review.yml", "claude-review.yml")}
        self.assertEqual(set(rerun["on"]["workflow_run"]["workflows"]), names)
        condition = rerun["jobs"]["rerun"]["if"]
        self.assertIn("github.event.comment.user.login == github.repository_owner", condition)
        self.assertEqual(rerun["jobs"]["rerun"]["permissions"], {"actions": "write", "pull-requests": "read"})
        self.assertIn("review-gate.yml/runs?event=pull_request_target", rerun["jobs"]["rerun"]["steps"][0]["run"])

    def test_claude_jobs_are_limited_to_owner_prs_with_minimal_permissions(self):
        workflow = load("claude-review.yml")
        self.assertEqual(workflow["permissions"], {})
        for job_id, job in workflow["jobs"].items():
            with self.subTest(job=job_id):
                self.assertEqual(job["permissions"], {"contents": "read", "pull-requests": "write"})
                self.assertIn("github.event.pull_request.head.repo.full_name == github.repository", job["if"])
                self.assertIn("github.event.pull_request.user.login == github.repository_owner", job["if"])
                steps = job["steps"]
                self.assertIn("Neither CLAUDE_CODE_OAUTH_TOKEN nor ANTHROPIC_API_KEY is set", steps[0]["run"])
                for step in steps:
                    if "uses" in step:
                        self.assertRegex(step["uses"], r"@[0-9a-f]{40}$")
                    if step.get("uses", "").startswith("actions/checkout@"):
                        self.assertEqual(step["with"]["persist-credentials"], "false")
                (review,) = [step for step in steps if step.get("uses", "").startswith("anthropics/claude-code-action@")]
                self.assertEqual(review["with"]["claude_code_oauth_token"], "${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}")
                self.assertEqual(review["with"]["anthropic_api_key"],
                                 "${{ secrets.CLAUDE_CODE_OAUTH_TOKEN == '' && secrets.ANTHROPIC_API_KEY || '' }}")
                args = review["with"]["claude_args"]
                self.assertIn('--allowedTools "Read(./**),mcp__github_inline_comment__create_inline_comment"', args)
                denied = args.split('--disallowedTools "', 1)[1].split('"', 1)[0].split(",")
                for tool in ("Bash", "Edit", "Write", "WebFetch", "Task", "Read(./.git/**)", "Read(//proc/**)",
                             "Read(//tmp/**)", "Read(~/.*/**)", "Read(~/work/_temp/**)"):
                    self.assertIn(tool, denied)
                self.assertNotRegex(args, r"Bash\(|\bgh\b")
                self.assertEqual(review["env"], {"CLAUDE_CODE_SUBPROCESS_ENV_SCRUB": "1"})


if __name__ == "__main__":
    unittest.main()
