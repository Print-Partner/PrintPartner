"""Run the review-gate step from .github/workflows/review-gate.yml against a fake gh.

The step's shell and jq run unchanged; only gh is replaced by a script that
answers from a JSON state file. Needs bash, jq, and PyYAML.
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

FAKE_GH = r"""
import json, os, sys
from urllib.parse import parse_qs, urlsplit
state = json.load(open(os.environ["FAKE_GH_STATE"]))
url = urlsplit([arg for arg in sys.argv[1:] if not arg.startswith("-")][1])
if "/check-runs" in url.path:
    query = parse_qs(url.query)
    assert url.path.endswith(f"/commits/{state['head']}/check-runs"), url.path
    assert query["app_id"] == ["15368"] and query["filter"] == ["latest"], query
    run = state["checks"].get(query["check_name"][0])
    print(json.dumps({"check_runs": [run] if run else []}))
elif url.path.endswith("/comments"):
    assert "--slurp" in sys.argv and "--paginate" in sys.argv
    print(json.dumps(state["pages"]))
else:
    sys.exit(f"unexpected gh call: {sys.argv}")
"""


def load(name):
    return yaml.load((WORKFLOWS / name).read_text(encoding="utf-8"), Loader=yaml.BaseLoader)


def marker(head=HEAD, reviewer="gpt-5.6-sol", findings="2", author=OWNER):
    body = f"Review recorded.\n<!-- pp-review-gate head={head} reviewer={reviewer} findings={findings} -->"
    return {"user": {"login": author}, "html_url": "https://github.com/o/r/pull/7#issuecomment-1", "body": body}


def done(conclusion):
    return {"status": "completed", "conclusion": conclusion}


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
        self.script, self.env = step["run"], dict(step["env"])
        self.path = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"

    def gate(self, checks=None, pages=None):
        state = self.folder / "state.json"
        state.write_text(json.dumps({"head": HEAD, "checks": checks or {}, "pages": pages or [[]]}))
        summary = self.folder / "summary.md"
        env = {**self.env, "PATH": self.path, "FAKE_GH_STATE": str(state), "GH_TOKEN": "x", "REPO": "o/r",
               "PR_NUMBER": "7", "HEAD_SHA": HEAD, "OWNER": OWNER, "RUNNER_TEMP": str(self.folder),
               "GITHUB_STEP_SUMMARY": str(summary)}
        result = subprocess.run(["bash", "-e", "-c", self.script], env=env, capture_output=True, text=True,
                                check=False)
        self.assertNotIn("unexpected gh call", result.stderr)
        return result

    def assertPasses(self, **state):
        result = self.gate(**state)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def assertFails(self, **state):
        result = self.gate(**state)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(f"::error::No finished review of {HEAD}", result.stdout)

    def test_no_review_fails(self):
        self.assertFails()
        self.assertFails(checks={"droid-review": done("failure"), "claude-code-review": {"status": "in_progress"}})

    def test_droid_success_passes(self):
        self.assertIn("droid-review succeeded", self.assertPasses(checks={"droid-review": done("success")}).stdout)
        self.assertFails(checks={"droid-review": done("skipped")})

    def test_claude_needs_code_success_and_security_success_or_neutral(self):
        for security in ("success", "neutral"):
            with self.subTest(security=security):
                self.assertPasses(checks={"claude-code-review": done("success"),
                                          "claude-security-review": done(security)})
        for security in ("failure", "cancelled", "skipped", None):
            with self.subTest(security=security):
                checks = {"claude-code-review": done("success")}
                if security:
                    checks["claude-security-review"] = done(security)
                self.assertFails(checks=checks)
        self.assertFails(checks={"claude-code-review": done("failure"), "claude-security-review": done("success")})
        self.assertFails(checks={"claude-security-review": done("success")})

    def test_owner_marker_for_the_head_passes(self):
        result = self.assertPasses(pages=[[marker()]], checks={"droid-review": done("failure")})
        self.assertIn("gpt-5.6-sol, 2 findings", result.stdout)
        # Markers on a later page still count.
        self.assertPasses(pages=[[{"user": {"login": OWNER}, "html_url": "u", "body": "hi"}], [marker()]])

    def test_markers_that_do_not_match_fail(self):
        for bad in (marker(head=OLD), marker(head=HEAD[:12]), marker(head=HEAD.upper()), marker(author="lane-bot"),
                    marker(reviewer=""), marker(findings="many"),
                    {**marker(), "body": f"pp-review-gate head={HEAD} reviewer=x findings=0"},
                    {**marker(), "body": None}):
            with self.subTest(body=bad["body"], author=bad["user"]["login"]):
                self.assertFails(pages=[[bad]])

    def test_a_new_head_needs_a_new_marker(self):
        self.assertFails(pages=[[marker(head=OLD)]], checks={"claude-code-review": done("success")})

    def test_summary_lists_every_path(self):
        self.gate(checks={"droid-review": done("failure")})
        summary = (self.folder / "summary.md").read_text()
        for row in ("| droid-review | failure |", "| claude-code-review | missing |",
                    "| claude-security-review | missing |", "| coordinator marker | none |"):
            self.assertIn(row, summary)


class ReviewWorkflowShapeTests(unittest.TestCase):
    def test_rerun_listens_to_the_review_workflows_and_owner_markers(self):
        rerun = load("review-gate-rerun.yml")
        names = {load(name)["name"] for name in ("droid-review.yml", "claude-review.yml")}
        self.assertEqual(set(rerun["on"]["workflow_run"]["workflows"]), names)
        condition = rerun["jobs"]["rerun"]["if"]
        self.assertIn("github.event.comment.user.login == github.repository_owner", condition)
        self.assertEqual(rerun["jobs"]["rerun"]["permissions"], {"actions": "write", "pull-requests": "read"})
        self.assertIn("review-gate.yml/runs", rerun["jobs"]["rerun"]["steps"][0]["run"])

    def test_gate_reads_the_job_names_the_review_workflows_define(self):
        script = load("review-gate.yml")["jobs"]["review-gate"]["steps"][0]["run"]
        self.assertIn("droid-review", load("droid-review.yml")["jobs"])
        self.assertEqual(set(load("claude-review.yml")["jobs"]), {"claude-code-review", "claude-security-review"})
        for name in ("droid-review", "claude-code-review", "claude-security-review"):
            self.assertIn(f"check {name}", script)

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
                self.assertIn('--allowedTools "mcp__github_inline_comment__create_inline_comment,Bash(gh pr diff:*),'
                              'Bash(gh pr view:*),Bash(gh pr comment:*),Read,Glob,Grep"', review["with"]["claude_args"])


if __name__ == "__main__":
    unittest.main()
