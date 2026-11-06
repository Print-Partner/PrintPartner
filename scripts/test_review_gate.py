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
          9: (".github/workflows/evil.yml", "pull_request"),
          # Same head SHA, but the run came from PR 8 into another base, or lists both PRs, or no PR.
          6: (CLAUDE, "pull_request", [{"number": 8, "base": {"ref": "release"}}]),
          7: (DROID, "pull_request", [{"number": 8, "base": {"ref": "release"}}]),
          8: (CLAUDE, "pull_request", [{"number": 7, "base": {"ref": "main"}}, {"number": 8, "base": {"ref": "release"}}]),
          11: (DROID, "pull_request", []),
          # Stacked PR 7 into release: the runs list that base.
          12: (CLAUDE, "pull_request", [{"number": 7, "base": {"ref": "release"}}]),
          13: (DROID, "pull_request", [{"number": 7, "base": {"ref": "release"}}])}
DROID_RAN = "##[end-action id=__Factory-AI_droid-action.droid;outcome=success;conclusion=success;duration_ms=128764]"
DROID_SKIPPED = "##[end-action id=__Factory-AI_droid-action.droid;outcome=skipped;conclusion=skipped;duration_ms=0]"
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
    out([[{"filename": item, "status": "modified"} if isinstance(item, str) else item for item in state["files"]]])
elif path.endswith("/pulls/7") and not slurp:
    counter = os.environ["FAKE_GH_STATE"] + ".pulls"
    calls = int(open(counter).read()) if os.path.exists(counter) else 0
    open(counter, "w").write(str(calls + 1))
    later = calls >= state.get("moves_on_call", 1)
    out({"changed_files": state["changed_files"],
         "base": {"ref": state.get("moved_base") if later and state.get("moved_base") else state["pr_base"]},
         "head": {"sha": state.get("moved_to", state["head"]) if later else state["head"]}})
elif path.endswith(f"/commits/{state['head']}/check-runs") and slurp and query["filter"] == ["all"]:
    seq = state.get("checks_seq")
    if seq:
        counter = os.environ["FAKE_GH_STATE"] + ".n"
        calls = int(open(counter).read()) if os.path.exists(counter) else 0
        open(counter, "w").write(str(calls + 1))
        out([{"check_runs": seq[min(calls, len(seq) - 1)]}])
    else:
        out([{"check_runs": state["checks"]}])
elif path.endswith("/issues/7/comments") and slurp:
    out(state["pages"])
elif path.endswith("/actions/runs") and slurp:
    suite = state["suites"].get(query["check_suite_id"][0])
    prs = suite[2] if suite and len(suite) > 2 else [{"number": 7, "base": {"ref": "main"}}]
    out([{"workflow_runs": [{"path": suite[0], "event": suite[1], "pull_requests": prs}] if suite else []}])
elif "/actions/jobs/" in path and path.endswith("/logs") and "--allow-escape-sequences" in sys.argv:
    if state["droid_log"] is None:
        sys.exit("HTTP 410")
    print(state["droid_log"])
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

    def gate(self, checks=(), pages=None, files=(), author=OWNER, blobs=None, changed_files=None, pr_base="main",
             droid_log=f"log\n{DROID_RAN}\nlog", checks_seq=None, wait="0", moved_to=None, moved_base=None, moves_on_call=1):
        state = self.folder / "state.json"
        state.write_text(json.dumps({
            "head": HEAD, "checks": [run("review-gate", None), *checks], "pages": pages or [[]],
            "droid_log": droid_log, "pr_base": pr_base, "checks_seq": checks_seq, **({"moved_to": moved_to} if moved_to else {}),
            **({"moved_base": moved_base} if moved_base else {}), "moves_on_call": moves_on_call, "files": list(files), "changed_files": len(files) if changed_files is None else changed_files,
            "suites": {str(key): value for key, value in SUITES.items()},
            "blobs": {f"{DROID}@{HEAD}": "d1", f"{DROID}@main": "d1", f"{CLAUDE}@{HEAD}": "c1",
                      f"{CLAUDE}@main": "c1"} if blobs is None else blobs}))
        script = self.folder / "gate.py"
        script.write_text(self.script)
        env = {**self.env, "PATH": self.path, "FAKE_GH_STATE": str(state), "GH_TOKEN": "x", "REPO": "o/r",
               "PR_NUMBER": "7", "HEAD_SHA": HEAD, "AUTHOR": author, "OWNER": OWNER, "DEFAULT_BRANCH": "main",
               "PP_GATE_MAX_WAIT_SECONDS": wait, "PP_GATE_POLL_SECONDS": "0",
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

    def test_droid_counts_only_when_its_review_step_ran(self):
        stamp = "2026-11-02T23:59:24.1234567Z "
        self.assertPasses(checks=[run("droid-review")], droid_log=f"\ufeff{stamp}start\n{stamp}{DROID_RAN}\n")
        for log in (f"log\n{DROID_SKIPPED}\nSkipping action due to workflow validation", "", None,
                    # PR text echoed into the log: a second line, or the text inside a longer line.
                    f"{stamp}{DROID_SKIPPED}\n{stamp}{DROID_RAN}\n",
                    f"{stamp}echo '{DROID_RAN}'\n", f"{stamp}note {DROID_RAN}\n"):
            with self.subTest(log=log):
                result = self.assertFails(checks=[run("droid-review")], droid_log=log)
                self.assertIn("the Droid review did not run", result.stdout)

    def test_results_count_only_from_this_prs_run_into_main(self):
        cases = {"droid run from a PR into another base": [run("droid-review", suite=7)],
                 "claude runs from a PR into another base": [run("claude-code-review", suite=6),
                                                             run("claude-security-review", suite=6)],
                 "claude runs that also list a PR into another base": [run("claude-code-review", suite=8),
                                                                       run("claude-security-review", suite=8)],
                 "security run from another base": [run("claude-code-review"), run("claude-security-review", suite=6)],
                 "droid run with no pull request": [run("droid-review", suite=11)]}
        for label, checks in cases.items():
            with self.subTest(label):
                result = self.assertFails(checks=checks)
                self.assertIn("not this PR's into main", result.stdout)
        self.assertPasses(checks=[run("droid-review", suite=7)], pages=[[marker()]])

    def test_a_stacked_pr_is_judged_against_its_own_base_with_the_same_rules(self):
        stacked = [run("claude-code-review", suite=12), run("claude-security-review", suite=12)]
        self.assertPasses(checks=stacked, pr_base="release")
        self.assertPasses(checks=[run("droid-review", suite=13)], pr_base="release")
        self.assertFails(checks=[])
        self.assertFails(checks=[], pr_base="release")
        self.assertFails(checks=[run("droid-review", "failure", suite=13)], pr_base="release")
        self.assertPasses(checks=[], pr_base="release", pages=[[marker()]])
        moved = self.assertFails(checks=[run("claude-code-review"), run("claude-security-review")], pr_base="release")
        self.assertIn("not this PR's into release", moved.stdout)
        self.assertFails(checks=[run("claude-code-review", suite=12), run("claude-security-review", suite=12)])
        differs = {f"{CLAUDE}@{HEAD}": "c2", f"{CLAUDE}@main": "c1"}
        self.assertFails(checks=stacked, pr_base="release", files=[CLAUDE], blobs=differs)

    def test_the_gate_waits_for_claude_jobs_that_are_still_running(self):
        review = [run("claude-code-review", None), run("claude-security-review", None)]
        finished = [run("claude-code-review"), run("claude-security-review")]
        waited = self.assertPasses(checks_seq=[review, review, finished], checks=finished, wait="5")
        self.assertIn(f"Waiting for claude-code-review, claude-security-review on {HEAD}.", waited.stdout)
        self.assertIn("claude-code-review succeeded", waited.stdout)
        self.assertFails(checks_seq=[review], checks=review, wait="1")
        broken = [run("claude-code-review", "failure"), finished[1]]
        failed = self.assertFails(checks_seq=[review, broken], checks=broken, wait="5")
        self.assertIn("claude-code-review is failure", failed.stdout)

    def test_a_waiting_gate_stops_when_the_head_moves(self):
        review = [run("claude-code-review", None), run("claude-security-review", None)]
        result = self.gate(checks_seq=[review], checks=review, wait="5", moved_to=OLD)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("head or base moved", result.stdout)

    def test_a_waiting_gate_stops_when_the_base_moves(self):
        review = [run("claude-code-review", None, suite=12), run("claude-security-review", None, suite=12)]
        done = [run("claude-code-review", suite=12), run("claude-security-review", suite=12)]
        result = self.gate(checks_seq=[review, done], checks=done, wait="5", pr_base="release", moved_base="main")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("head or base moved", result.stdout)

    def test_a_retarget_during_judgement_is_not_passed_on_the_old_base(self):
        done = [run("claude-code-review", suite=12), run("claude-security-review", suite=12)]
        result = self.gate(checks=done, pr_base="release", moved_base="main", moves_on_call=2)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("moved while judging", result.stdout)

    def test_waiting_cannot_create_a_pass_from_a_spoofed_run(self):
        review = [run("claude-code-review", None, suite=9), run("claude-security-review", None)]
        spoofed = [run("claude-code-review", suite=9), run("claude-security-review")]
        self.assertFails(checks_seq=[review, spoofed], checks=spoofed, wait="5", reason="comes from")

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
        crlf = {**marker(), "body": marker()["body"].replace("\n", "\r\n") + "\r\nThanks.\r\n"}
        self.assertPasses(pages=[[crlf]])
        result = self.assertPasses(pages=[[marker()]], checks=[run("droid-review", "failure")])
        self.assertIn("gpt-5.6-sol, 2 findings", result.stdout)
        self.assertPasses(pages=[[{"user": {"login": OWNER}, "html_url": "u", "body": "hi"}], [marker()]])

    def test_markers_that_do_not_match_fail(self):
        for bad in (marker(head=OLD), marker(head=HEAD[:12]), marker(head=HEAD.upper()), marker(author="lane-bot"),
                    marker(reviewer=""), marker(findings="many"),
                    {**marker(), "body": f"pp-review-gate head={HEAD} reviewer=x findings=0"},
                    {**marker(), "body": None},
                    # Only the exact line the coordinator posts counts.
                    {**marker(), "body": f"<!--  pp-review-gate head={HEAD} reviewer=x findings=0 -->"},
                    {**marker(), "body": f"<!--\tpp-review-gate head={HEAD} reviewer=x findings=0 -->"},
                    {**marker(), "body": f"<!--pp-review-gate head={HEAD} reviewer=x findings=0-->"},
                    {**marker(), "body": f"<!-- PP-REVIEW-GATE head={HEAD} reviewer=x findings=0 -->"},
                    {**marker(), "body": f"<!-- pp-review-gate reviewer=x head={HEAD} findings=0 -->"},
                    {**marker(), "body": f"Done <!-- pp-review-gate head={HEAD} reviewer=x findings=0 -->"},
                    {**marker(), "body": f"&lt;!-- pp-review-gate head={HEAD} reviewer=x findings=0 --&gt;"},
                    {**marker(), "body": f"\\u003c!-- pp-review-gate head={HEAD} reviewer=x findings=0 --\\u003e"}):
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

    def test_non_owner_protected_changes_need_the_marker_and_an_automated_review(self):
        for name in (".github/workflows/review-gate.yml", ".github/workflows/new.yml", ".github/zizmor.yml",
                     "scripts/agent-guard.py", ".factory/hooks.json", ".factory/hooks/guard.py",
                     ".claude/hooks/guard.py", ".claude/settings.json"):
            with self.subTest(name=name):
                files = [name, "docs/a.md"]
                self.assertFails("::error::lane-bot is not the repository owner and changes", files=files,
                                 author="lane-bot", checks=[run("droid-review"), run("claude-code-review"),
                                                            run("claude-security-review")])
                self.assertFails("marker: yes, automated review: no", files=files, author="lane-bot",
                                 pages=[[marker()]])
                self.assertPasses(files=files, author="lane-bot", pages=[[marker()]], checks=[run("droid-review")])
                self.assertPasses(files=files, author="lane-bot", pages=[[marker()]], checks=[run("claude-code-review"), run("claude-security-review")])
        self.assertPasses(files=["docs/a.md"], author="lane-bot", checks=[run("droid-review")])
        self.assertPasses(files=[".github/workflows/new.yml"], checks=[run("droid-review")])

    def test_renames_and_deletes_of_protected_files_count(self):
        changes = {"rename out of the guard": {"filename": "scripts/old-guard.py", "status": "renamed",
                                               "previous_filename": "scripts/agent-guard.py"},
                   "rename a hook out of .claude": {"filename": "docs/guard.py", "status": "renamed",
                                                    "previous_filename": ".claude/hooks/guard.py"},
                   "rename a workflow away": {"filename": "docs/review-gate.yml.txt", "status": "renamed",
                                              "previous_filename": ".github/workflows/review-gate.yml"},
                   "rename into workflows": {"filename": ".github/workflows/new.yml", "status": "renamed",
                                             "previous_filename": "docs/new.yml"},
                   "delete the guard": {"filename": "scripts/agent-guard.py", "status": "removed"},
                   "delete a workflow": {"filename": ".github/workflows/droid-review.yml", "status": "removed"}}
        for label, change in changes.items():
            with self.subTest(label):
                self.assertFails("::error::lane-bot is not the repository owner and changes", files=[change],
                                 author="lane-bot", checks=[run("droid-review"), run("claude-code-review"),
                                                            run("claude-security-review")])
                self.assertFails("marker: yes, automated review: no", files=[change], author="lane-bot",
                                 pages=[[marker()]])
                self.assertPasses(files=[change], author="lane-bot", pages=[[marker()]], checks=[run("claude-code-review"), run("claude-security-review")])
        moved = {"filename": "docs/droid.yml", "status": "renamed", "previous_filename": DROID}
        self.assertFails(checks=[run("droid-review")], files=[moved], blobs={f"{DROID}@main": "d1"})

    def test_dependabot_workflow_bump_needs_the_owner_marker_and_an_automated_review(self):
        bump = {"files": [".github/workflows/web-ci.yml"], "author": "dependabot[bot]"}
        claude = [run("claude-code-review"), run("claude-security-review")]
        self.assertFails("::error::dependabot[bot] is not the repository owner and changes", **bump)
        self.assertFails("marker: no, automated review: yes", checks=claude, **bump)
        self.assertFails("marker: yes, automated review: no", pages=[[marker()]], **bump)
        self.assertFails("marker: no, automated review: yes", checks=claude, pages=[[marker(head=OLD)]], **bump)
        self.assertFails("marker: no, automated review: yes", checks=claude,
                         pages=[[marker(author="dependabot[bot]")]], **bump)
        result = self.assertPasses(pages=[[marker()]], checks=claude, **bump)
        self.assertIn("and an automated review cover dependabot[bot]'s changes to .github/workflows/web-ci.yml",
                      result.stdout)
        self.assertPasses(pages=[[marker()]], checks=[run("droid-review")], **bump)

    def test_incomplete_file_list_fails_closed(self):
        for listed, changed in ((["docs/a.md"] * 1, 5), ([f"f{i}" for i in range(3000)], 3000),
                                ([f"f{i}" for i in range(3000)], 4000)):
            with self.subTest(listed=len(listed), changed=changed):
                result = self.assertFails(f"::error::GitHub listed {len(listed)} of the PR's {changed} changed files",
                                          files=listed, changed_files=changed, pages=[[marker()]],
                                          checks=[run("droid-review")])
                self.assertIn("Split the PR", result.stdout)

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


SYMLINK_STEP = (
    "links=$(find . -path ./.git -prune -o -type l -print)\n"
    "if [ -n \"$links\" ]; then\n"
    "  echo \"::error::The checkout contains symlinks, which Claude's Read tool could follow:\"\n"
    "  echo \"$links\"\n"
    "  exit 1\n"
    "fi\n")


class SymlinkStepTests(unittest.TestCase):
    """Run the Claude jobs' symlink step on scratch checkouts."""

    def check(self, make_links):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / ".git").mkdir()
            (root / ".git/config").write_text("[remote]\n")
            (root / "src").mkdir()
            (root / "src/a.py").write_text("x = 1\n")
            make_links(root)
            return subprocess.run(["bash", "-e", "-c", SYMLINK_STEP], cwd=root, capture_output=True, text=True,
                                  env={**os.environ, "GITHUB_WORKSPACE": folder})

    def test_a_tree_without_symlinks_passes(self):
        self.assertEqual(self.check(lambda root: None).returncode, 0)

    def test_any_symlink_fails(self):
        targets = {"into .git": ".git/config", "inside the workspace": "src/a.py", "proc": "/proc/self/environ",
                   "home credentials": os.path.expanduser("~/.git-credentials"), "dangling": "missing"}
        for label, target in targets.items():
            with self.subTest(label):
                result = self.check(lambda root, target=target: (root / "src/link").symlink_to(target))
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("::error::The checkout contains symlinks", result.stdout)
                self.assertIn("./src/link", result.stdout)

    def test_repository_tracks_no_symlinks(self):
        listed = subprocess.run(["git", "ls-files", "-s"], cwd=ROOT, capture_output=True, text=True, check=True).stdout
        self.assertEqual([line for line in listed.splitlines() if line.startswith("120000 ")], [])


class ReviewWorkflowShapeTests(unittest.TestCase):
    def test_gate_runs_the_base_branch_definition_without_checkout_or_secrets(self):
        text = (WORKFLOWS / "review-gate.yml").read_text(encoding="utf-8")
        workflow = load("review-gate.yml")
        self.assertEqual(set(workflow["on"]), {"pull_request_target"})
        self.assertEqual(workflow["on"]["pull_request_target"]["types"],
                         ["opened", "synchronize", "reopened", "ready_for_review", "edited"])
        job = workflow["jobs"]["review-gate"]
        self.assertNotIn("if", job)
        group = workflow["concurrency"]
        self.assertIn("github.event.action != 'edited'", group["cancel-in-progress"])
        self.assertIn("github.event.changes.base != null", group["cancel-in-progress"])
        self.assertIn("github.run_id", group["group"])
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
                self.assertIn("github.event.pull_request.user.login == 'dependabot[bot]'", job["if"])
                steps = job["steps"]
                self.assertIn("Neither CLAUDE_CODE_OAUTH_TOKEN nor ANTHROPIC_API_KEY is set", steps[0]["run"])
                for step in steps:
                    if "uses" in step:
                        self.assertRegex(step["uses"], r"@[0-9a-f]{40}$")
                    if step.get("uses", "").startswith("actions/checkout@"):
                        self.assertEqual(step["with"]["persist-credentials"], "false")
                (review,) = [step for step in steps if step.get("uses", "").startswith("anthropics/claude-code-action@")]
                self.assertEqual(review["with"]["claude_code_oauth_token"], "${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}")
                self.assertEqual(review["with"]["allowed_bots"], "dependabot")
                self.assertEqual(review["with"]["anthropic_api_key"],
                                 "${{ secrets.CLAUDE_CODE_OAUTH_TOKEN == '' && secrets.ANTHROPIC_API_KEY || '' }}")
                args = review["with"]["claude_args"]
                self.assertIn('--allowedTools "Read(./**),mcp__github_inline_comment__create_inline_comment"', args)
                denied = args.split('--disallowedTools "', 1)[1].split('"', 1)[0].split(",")
                names = [step.get("name", step.get("uses", "")) for step in steps]
                self.assertLess(names.index("Refuse symlinks in the checkout"), names.index(review["name"]))
                self.assertEqual(steps[names.index("Refuse symlinks in the checkout")]["run"], SYMLINK_STEP)
                for tool in ("Bash", "Edit", "Write", "WebFetch", "Task", "Read(./.git/**)", "Read(//proc/**)",
                             "Read(//proc/self/**)", "Read(//tmp/**)", "Read(~/.*/**)", "Read(~/.git-credentials)",
                             "Read(~/.config/**)", "Read(~/work/_temp/**)"):
                    self.assertIn(tool, denied)
                self.assertNotRegex(args, r"Bash\(|\bgh\b")
                self.assertEqual(review["env"], {"CLAUDE_CODE_SUBPROCESS_ENV_SCRUB": "1"})


if __name__ == "__main__":
    unittest.main()
