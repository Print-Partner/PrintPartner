import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[1]
EVENT = json.loads((ROOT / "scripts/fixtures/merge_group/checks_requested.json").read_text())


def evaluate_if(expression, contexts):
    """Evaluate an `a.b == 'x' && c != 'y'` step condition; reject any other shape."""
    result = True
    for clause in expression.split("&&"):
        left, op, right = clause.strip().partition(" == ")
        if not op:
            left, op, right = clause.strip().partition(" != ")
        if not op or not (right.startswith("'") and right.endswith("'")):
            raise ValueError(f"unsupported condition clause: {clause!r}")
        value = contexts
        for key in left.split("."):
            value = value.get(key, "") if isinstance(value, dict) else ""
        result = result and ((value == right[1:-1]) == (op == " == "))
    return result


class MergeGroupWorkflowTests(unittest.TestCase):
    def test_drafts_skip_every_pr_ci_job_and_ready_events_restart_them(self):
        guard = "github.event_name != 'pull_request' || !github.event.pull_request.draft"
        count = 0
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            workflow = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
            events = workflow.get("on", {})
            if not isinstance(events, dict) or "pull_request" not in events:
                continue
            count += 1
            with self.subTest(workflow=path.name):
                types = events["pull_request"]["types"]
                self.assertIn("ready_for_review", types)
                self.assertIn("converted_to_draft", types)
                for job_id, job in workflow["jobs"].items():
                    with self.subTest(job=job_id):
                        condition = job.get("if", "")
                        self.assertTrue(condition == guard or condition.startswith(f"({guard}) && ("),
                                        f"{path.name}:{job_id} allocates a runner on draft PRs")
        self.assertGreater(count, 0)

    def test_required_workflows_still_emit_job_checks_for_drafts(self):
        for filename, names in (("actionlint.yml", {"actionlint", "zizmor"}),
                                ("rust-cross-os.yml", {"rust (linux)", "rust (windows)"}),
                                ("pr-evidence.yml", {"Lint Evidence block"})):
            workflow = yaml.load((ROOT / ".github/workflows" / filename).read_text(),
                                 Loader=yaml.BaseLoader)
            self.assertNotIn("paths", workflow["on"]["pull_request"])
            self.assertNotIn("paths-ignore", workflow["on"]["pull_request"])
            self.assertTrue(names <= {job.get("name", job_id)
                                      for job_id, job in workflow["jobs"].items()})

    def test_required_workflows_accept_merge_group_fixture(self):
        for filename in ("actionlint.yml", "rust-cross-os.yml", "desktop-native-macos.yml",
                         "pr-evidence.yml"):
            with self.subTest(workflow=filename):
                workflow = yaml.load(
                    (ROOT / ".github/workflows" / filename).read_text(), Loader=yaml.BaseLoader
                )
                self.assertEqual(workflow["on"].get("merge_group"), {"types": [EVENT["action"]]})

    def test_merge_group_evidence_runs_and_passes_without_a_pr_body(self):
        workflow = yaml.load((ROOT / ".github/workflows/pr-evidence.yml").read_text(),
                             Loader=yaml.BaseLoader)
        self.assertEqual(workflow["on"]["merge_group"], {"types": ["checks_requested"]})
        self.assertEqual(workflow["concurrency"], {
            "group": "${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}-"
                     "${{ github.event_name == 'pull_request' && 'pr' || github.run_id }}",
            "cancel-in-progress": "${{ github.event_name == 'pull_request' }}",
        })
        job = workflow["jobs"]["lint-pr-body"]
        self.assertEqual(job["if"],
                         "github.event_name != 'pull_request' || !github.event.pull_request.draft")
        self.assertNotIn("needs", job)
        steps = job["steps"]
        self.assertEqual(steps[0]["if"], "github.event_name == 'merge_group'")
        result = subprocess.run(["bash", "-e", "-c", steps[0]["run"]],
                                env={"PATH": os.environ["PATH"]},
                                capture_output=True, text=True, check=True)
        self.assertIn("Merge group check passed.", result.stdout)
        for step in steps[1:]:
            if step.get("uses", "").startswith("actions/setup-node@") or step.get("name") == "Verify Node version":
                # check-ci-tool-pins requires the Node pin and its assertion to run unconditionally.
                self.assertNotIn("if", step)
            elif step.get("name") == "Lint pull request Evidence block":
                # Dependabot PRs carry generated bodies with no Evidence block, so only they skip the lint.
                self.assertEqual(
                    step["if"],
                    "github.event_name == 'pull_request' && github.event.pull_request.user.login != 'dependabot[bot]'",
                )
            else:
                self.assertEqual(step["if"], "github.event_name == 'pull_request'")
        for event in ("pull_request", "merge_group"):
            self.assertNotIn("paths", workflow["on"][event])
            self.assertNotIn("paths-ignore", workflow["on"][event])

    def test_only_dependabot_pull_requests_skip_the_evidence_lint(self):
        workflow = yaml.load((ROOT / ".github/workflows/pr-evidence.yml").read_text(),
                             Loader=yaml.BaseLoader)
        lint = next(step for step in workflow["jobs"]["lint-pr-body"]["steps"]
                    if step.get("name") == "Lint pull request Evidence block")
        for event, login, runs in (("pull_request", "dependabot[bot]", False),
                                   ("pull_request", "poitee", True),
                                   ("pull_request", "github-actions[bot]", True),
                                   ("pull_request", "dependabot", True),
                                   ("merge_group", "dependabot[bot]", False)):
            with self.subTest(event=event, login=login):
                contexts = {"github": {"event_name": event, "event": {
                    "pull_request": {"user": {"login": login}}}}}
                self.assertEqual(evaluate_if(lint["if"], contexts), runs)

    def test_required_lint_and_rust_jobs_use_the_filter(self):
        for filename, kind, contexts in (
            ("actionlint.yml", "actions", {"actionlint": "actionlint", "zizmor": "zizmor"}),
            # Path filter runs on rust-linux-build; rust-linux is the required
            # aggregate check over build + optional rebuild partitions.
            ("rust-cross-os.yml", "rust", {"rust-linux-build": "rust (linux) build",
                                           "rust-windows": "rust (windows)"}),
        ):
            with self.subTest(workflow=filename):
                workflow = yaml.load(
                    (ROOT / ".github/workflows" / filename).read_text(), Loader=yaml.BaseLoader
                )
                jobs = workflow["jobs"]
                select = next(step for step in jobs["changes"]["steps"]
                              if step.get("id") == "select")
                self.assertIn(f"python3 scripts/ci-path-filter.py {kind}", select["run"].splitlines())
                self.assertEqual(select["env"], {
                    "EVENT_NAME": "${{ github.event_name }}",
                    "BASE_SHA": "${{ github.event.pull_request.base.sha }}",
                    "HEAD_SHA": "${{ github.event.pull_request.head.sha }}",
                })
                self.assertEqual(jobs["changes"]["outputs"]["relevant"],
                                 "${{ steps.select.outputs.relevant }}")
                for job_id, context in contexts.items():
                    job = jobs[job_id]
                    self.assertEqual(job.get("name", job_id), context)
                    self.assertEqual(job["needs"], "changes")
                    self.assertEqual(job["if"], "(github.event_name != 'pull_request' || "
                                     "!github.event.pull_request.draft) && (!cancelled() && "
                                     "(needs.changes.result != 'success' || "
                                     "needs.changes.outputs.relevant != 'false'))")
                    checkout = next(step for step in job["steps"]
                                    if step.get("uses", "").startswith("actions/checkout@"))
                    self.assertNotIn("ref", checkout.get("with", {}))
                if filename == "rust-cross-os.yml":
                    self.assertIn("python3 scripts/ci-affected-nextest-filter.py",
                                  select["run"].splitlines())
                    self.assertEqual(jobs["changes"]["outputs"]["nextest_filter"],
                                     "${{ steps.select.outputs.nextest_filter }}")
                    for job_id in ("rust-linux-build", "rust-macos", "rust-windows"):
                        self.assertEqual(
                            jobs[job_id]["env"]["NEXTEST_FILTER"],
                            "${{ needs.changes.outputs.nextest_filter || 'all()' }}")
                        # rdeps of a Windows-excluded crate (or pp-server) selects
                        # no tests; nextest exits 4 on that unless told otherwise.
                        filtered_runs = [step["run"] for step in jobs[job_id]["steps"]
                                         if "cargo nextest run" in step.get("run", "")
                                         and '-E "$NEXTEST_FILTER"' in step["run"]]
                        self.assertEqual(len(filtered_runs), 2)
                        for run in filtered_runs:
                            self.assertIn("--no-tests=warn", run.split())
                    self.assertEqual(jobs["rust-linux-rebuild"]["env"]["NEXTEST_FILTER"], "all()")
                    rebuild = jobs["rust-linux-rebuild"]
                    self.assertEqual(rebuild["needs"], ["rust-linux-build"])
                    aggregate = jobs["rust-linux"]
                    self.assertEqual(aggregate.get("name"), "rust (linux)")
                    self.assertEqual(aggregate["needs"], ["rust-linux-build", "rust-linux-rebuild"])
                    self.assertEqual(
                        aggregate["if"],
                        "(github.event_name != 'pull_request' || "
                        "!github.event.pull_request.draft) && (!cancelled() && "
                        "needs.rust-linux-build.result != 'skipped')",
                    )
                    require = next(
                        step for step in aggregate["steps"]
                        if step.get("name") == "Require linux build and rebuild partitions"
                    )
                    self.assertEqual(require["env"]["EVENT_NAME"], "${{ github.event_name }}")
                    self.assertIn("push|merge_group|workflow_dispatch)", require["run"])
                    self.assertIn('test "$REBUILD_RESULT" = success', require["run"])
                    self.assertIn("success|skipped)", require["run"])

    def test_native_macos_plan_respects_paths_and_event(self):
        workflow = yaml.load((ROOT / ".github/workflows/desktop-native-macos.yml").read_text(),
                             Loader=yaml.BaseLoader)
        steps = workflow["jobs"]["changes"]["steps"]
        plan = next(step["run"] for step in steps if step.get("id") == "plan")
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory, "output")
            for event, relevant, label, draft, macos, intel in (
                ("pull_request", "false", "true", "false", "false", "false"),
                ("pull_request", "true", "true", "false", "true", "false"),
                ("pull_request", "true", "false", "false", "false", "false"),
                ("pull_request", "true", "true", "true", "false", "false"),
                ("merge_group", "false", "false", "true", "true", "true"),
                ("push", "false", "false", "true", "true", "true"),
            ):
                with self.subTest(event=event, relevant=relevant, label=label, draft=draft):
                    output.write_text("")
                    subprocess.run(["bash", "-e", "-c", plan], check=True,
                                   env={**os.environ, "EVENT_NAME": event, "RELEVANT": relevant,
                                        "HAS_LABEL": label, "IS_DRAFT": draft,
                                        "GITHUB_OUTPUT": str(output),
                                        "GITHUB_STEP_SUMMARY": str(Path(directory, "summary"))})
                    self.assertEqual(output.read_text(), f"macos={macos}\nintel={intel}\n")

    def test_rust_macos_uses_the_same_selection_as_required_jobs(self):
        workflow = yaml.load((ROOT / ".github/workflows/rust-cross-os.yml").read_text(),
                             Loader=yaml.BaseLoader)
        self.assertIn("(needs.changes.result != 'success' || "
                      "needs.changes.outputs.relevant != 'false')",
                      workflow["jobs"]["rust-macos"]["if"])

    def test_merge_group_runs_filters_even_for_docs_only_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)

            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, text=True).strip()

            git("init", "-q")
            git("config", "user.name", "Merge group fixture")
            git("config", "user.email", "fixture@example.com")
            readme = directory / "README.md"
            readme.write_text("base\n")
            git("add", "README.md")
            git("commit", "-qm", "base")
            base = git("rev-parse", "HEAD")
            readme.write_text("docs-only update\n")
            git("commit", "-qam", "docs")
            head = git("rev-parse", "HEAD")
            event = {**EVENT, "merge_group": {**EVENT["merge_group"],
                                             "base_sha": base, "head_sha": head}}
            event_path = directory / "event.json"
            event_path.write_text(json.dumps(event))
            pull_request = event.get("pull_request", {})
            for kind in ("actions", "rust"):
                for event_name, base_sha, head_sha, expected in (
                    ("pull_request", base, head, "false"),
                    ("merge_group", pull_request.get("base", {}).get("sha", ""),
                     pull_request.get("head", {}).get("sha", ""), "true"),
                    ("merge_group", event["merge_group"]["base_sha"],
                     event["merge_group"]["head_sha"], "true"),
                ):
                    with self.subTest(kind=kind, event=event_name, base=base_sha):
                        output = directory / "output"
                        summary = directory / "summary"
                        output.write_text("")
                        summary.write_text("")
                        result = subprocess.run(
                            [sys.executable, str(ROOT / "scripts/ci-path-filter.py"), kind],
                            cwd=directory, text=True, capture_output=True, check=True,
                            env={**os.environ, "EVENT_NAME": event_name, "BASE_SHA": base_sha,
                                 "HEAD_SHA": head_sha, "GITHUB_EVENT_PATH": str(event_path),
                                 "GITHUB_SHA": head, "GITHUB_REF": event["merge_group"]["head_ref"],
                                 "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary)},
                        )
                        self.assertEqual(output.read_text(), f"relevant={expected}\n")
                        self.assertEqual(result.stdout, f"{kind} inputs changed: {expected}\n")
                        self.assertEqual(summary.read_text(), result.stdout)


class PullRequestEditTests(unittest.TestCase):
    """Edits to a PR must never turn a required check into a skipped one.

    GitHub counts a skipped required job as a pass, and a newer run for the head replaces an
    earlier failing one, so a job-level `if` that skips edits would let an edit clear a failure.
    """

    def workflow(self, name):
        return yaml.load((ROOT / ".github/workflows" / name).read_text(), Loader=yaml.BaseLoader)

    def test_lint_evidence_reruns_on_edits_and_skips_only_drafts(self):
        evidence = self.workflow("pr-evidence.yml")
        self.assertIn("edited", evidence["on"]["pull_request"]["types"])
        self.assertEqual(evidence["jobs"]["lint-pr-body"]["if"],
                         "github.event_name != 'pull_request' || !github.event.pull_request.draft")


if __name__ == "__main__":
    unittest.main()
