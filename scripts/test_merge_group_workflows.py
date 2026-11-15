import importlib.util
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
EVENT_NAME = "merge_group"


def job_runs(condition, contexts):
    """Whether a required job's `if` lets it run on a non-PR event.

    Only the shapes the required jobs use are accepted; anything else raises.
    """
    assert contexts["github"]["event_name"] != "pull_request"
    draft = "github.event_name != 'pull_request' || !github.event.pull_request.draft"
    if condition in ("", draft):
        return True
    prefix = f"({draft}) && ("
    if not condition.startswith(prefix) or not condition.endswith(")"):
        raise ValueError(f"unsupported job condition: {condition!r}")
    rest = condition[len(prefix):-1]
    event = contexts["github"]["event_name"]
    linux_events = ("pull_request", "push", "merge_group")
    linux_clause = " || ".join(f"github.event_name == '{name}'" for name in linux_events)
    linux_guard = f"!cancelled() && ({linux_clause}) && "
    if rest.startswith(linux_guard):
        if event not in linux_events:
            return False
        rest = "!cancelled() && " + rest[len(linux_guard):]
    known = {}
    outputs = contexts["needs"]["changes"]["outputs"]
    for kind in ("relevant", "windows", "web", "ui"):
        known[f"!cancelled() && (needs.changes.result != 'success' || needs.changes.outputs.{kind} != 'false')"] = (
            contexts["needs"]["changes"]["result"] != "success"
            or outputs.get(kind, "false") != "false")
    if rest not in known:
        raise ValueError(f"unsupported job condition: {condition!r}")
    return known[rest]


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
                                ("rust-cross-os.yml", {"rust (linux)"}),
                                ("web-ci.yml", {"release-audit", "ui"}),
                                ("rust-image.yml", {"rust image"})):
            workflow = yaml.load((ROOT / ".github/workflows" / filename).read_text(),
                                 Loader=yaml.BaseLoader)
            self.assertNotIn("paths", workflow["on"]["pull_request"])
            self.assertNotIn("paths-ignore", workflow["on"]["pull_request"])
            self.assertTrue(names <= {job.get("name", job_id)
                                      for job_id, job in workflow["jobs"].items()})

    def test_required_workflows_accept_merge_group_fixture(self):
        for filename in ("actionlint.yml", "rust-cross-os.yml", "web-ci.yml",
                         "rust-image.yml"):
            with self.subTest(workflow=filename):
                workflow = yaml.load(
                    (ROOT / ".github/workflows" / filename).read_text(), Loader=yaml.BaseLoader
                )
                # No branches filter: the merge-group step checks base_ref itself, and a filter that
                # skipped a group would leave the required check pending.
                self.assertEqual(workflow["on"].get("merge_group"), {"types": [EVENT["action"]]})

    def test_release_audit_runs_without_path_selection_or_dependencies(self):
        workflow = yaml.load((ROOT / ".github/workflows/web-ci.yml").read_text(),
                             Loader=yaml.BaseLoader)
        job = workflow["jobs"]["release-audit"]
        self.assertEqual(job["if"],
                         "github.event_name != 'pull_request' || !github.event.pull_request.draft")
        self.assertNotIn("needs", job)
        self.assertNotIn("continue-on-error", job)
        self.assertEqual(workflow["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", job)
        checkout, setup, verify, audit = job["steps"]
        self.assertEqual(checkout, {
            "uses": "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
            "with": {"persist-credentials": "false"}})
        self.assertEqual(setup, {
            "uses": "actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1",
            "with": {"node-version": "22.23.3"}})
        self.assertEqual(verify, {"name": "Verify pinned Node version",
                                  "run": 'test "$(node --version)" = "v22.23.3"'})
        self.assertEqual(audit, {"name": "Audit public release inputs",
                                "run": "node scripts/audit-public-release.mjs"})
        for event in ("pull_request", "push", "merge_group"):
            self.assertNotIn("paths", workflow["on"][event])
            self.assertNotIn("paths-ignore", workflow["on"][event])

    def test_ui_job_uses_path_selection_and_fails_closed(self):
        workflow = yaml.load((ROOT / ".github/workflows/web-ci.yml").read_text(),
                             Loader=yaml.BaseLoader)
        jobs = workflow["jobs"]
        for event in ("push", "pull_request", "merge_group"):
            self.assertNotIn("paths", workflow["on"][event])
            self.assertNotIn("paths-ignore", workflow["on"][event])
        step = next(step for step in jobs["changes"]["steps"] if step.get("id") == "ui")
        self.assertEqual(step["run"], "python3 scripts/ci-path-filter.py ui")
        self.assertEqual(jobs["changes"]["outputs"]["ui"], "${{ steps.ui.outputs.relevant }}")
        self.assertEqual(step["env"], {
            "EVENT_NAME": "${{ github.event_name }}",
            "BASE_SHA": "${{ github.event.pull_request.base.sha }}",
            "HEAD_SHA": "${{ github.event.pull_request.head.sha }}"})
        checkout = jobs["changes"]["steps"][0]
        self.assertEqual(checkout["with"]["fetch-depth"], "0")
        self.assertEqual(checkout["with"]["persist-credentials"], "false")
        self.assertNotIn("web", jobs)
        self.assertNotIn("web-build", jobs)
        self.assertNotIn("sidecar", jobs)
        self.assertNotIn("docker", jobs)
        for selected, result, runs in (("false", "success", False), ("true", "success", True),
                                       ("", "failure", True)):
            contexts = {"github": {"event_name": "merge_group"}, "needs": {
                "changes": {"result": result, "outputs": {"ui": selected, "relevant": "true"}}}}
            job = jobs["ui"]
            self.assertEqual(job["needs"], ["changes"])
            self.assertEqual(job_runs(job["if"], contexts), runs)

    def test_every_required_check_reports_on_merge_groups(self):
        """Each required context comes from a job that a merge_group run starts and cannot skip."""
        required = {"actionlint": "actionlint.yml", "zizmor": "actionlint.yml",
                    "rust (linux)": "rust-cross-os.yml"}
        ruleset = json.loads((ROOT / "docs/development/main-ci-controls.ruleset.json").read_text())
        (checks,) = [rule["parameters"]["required_status_checks"] for rule in ruleset["rules"]
                     if rule["type"] == "required_status_checks"]
        self.assertEqual({check["context"] for check in checks}, set(required))
        contexts = {"github": {"event_name": EVENT_NAME}, "needs": {
            "changes": {"result": "success", "outputs": {"relevant": "true", "windows": "true",
                                                        "ui": "true", "web": "false"}},
            "rust-linux-build": {"result": "success"}}}
        for context, filename in required.items():
            with self.subTest(context=context):
                workflow = yaml.load((ROOT / ".github/workflows" / filename).read_text(),
                                     Loader=yaml.BaseLoader)
                self.assertIn("merge_group", workflow["on"])
                (job,) = [job for job_id, job in workflow["jobs"].items()
                          if job.get("name", job_id) == context]
                self.assertTrue(job_runs(job.get("if", ""), contexts), f"{context} skips merge groups")

    def test_merge_groups_run_linux_rust_on_hosted_runners(self):
        spec = importlib.util.spec_from_file_location("ci_rust_runner", ROOT / "scripts/ci-rust-runner.py")
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)

        def fetch(path, token):
            raise AssertionError(f"merge groups must not query runners: {path}")

        for mode in ("", "local", "hosted"):
            with self.subTest(mode=mode):
                runs_on, _ = runner.choose({"EVENT_NAME": EVENT_NAME, "PP_RUST_RUNNER": mode,
                                            "REPOSITORY": "o/r", "RUNNER_TOKEN": "admin"}, fetch, None)
                self.assertEqual(runs_on, runner.HOSTED)

    def test_required_lint_and_rust_jobs_use_the_filter(self):
        for filename, kind, contexts in (
            ("actionlint.yml", "actions", {"actionlint": "actionlint", "zizmor": "zizmor"}),
            ("rust-cross-os.yml", "rust", {"rust-linux": "rust (linux)"}),
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
                    selected = "needs.changes.outputs.relevant != 'false'"
                    event_guard = ""
                    if job_id == "rust-linux":
                        event_guard = ("(github.event_name == 'pull_request' || "
                                       "github.event_name == 'push' || "
                                       "github.event_name == 'merge_group') && ")
                        self.assertEqual(job["needs"], "changes")
                    self.assertEqual(job["if"], "(github.event_name != 'pull_request' || "
                                     "!github.event.pull_request.draft) && (!cancelled() && "
                                     f"{event_guard}(needs.changes.result != 'success' || {selected}))")
                    checkout = next(step for step in job["steps"]
                                    if step.get("uses", "").startswith("actions/checkout@"))
                    self.assertNotIn("ref", checkout.get("with", {}))
                if filename == "rust-cross-os.yml":
                    self.assertNotIn("windows", {step.get("id") for step in jobs["changes"]["steps"]})
                    self.assertIn("python3 scripts/ci-affected-nextest-filter.py",
                                  select["run"].splitlines())
                    self.assertEqual(jobs["changes"]["outputs"]["nextest_filter"],
                                     "${{ steps.select.outputs.nextest_filter }}")
                    linux = jobs["rust-linux"]
                    self.assertEqual(linux["needs"], "changes")
                    self.assertEqual(
                        linux["env"]["NEXTEST_FILTER"],
                        "${{ needs.changes.outputs.nextest_filter || 'all()' }}")
                    filtered_runs = [step["run"] for step in linux["steps"]
                                     if "cargo nextest run" in step.get("run", "")
                                     and '-E "$NEXTEST_FILTER"' in step["run"]]
                    self.assertEqual(len(filtered_runs), 1)
                    self.assertIn("--no-tests=warn", filtered_runs[0].split())
                    self.assertNotIn("--features", filtered_runs[0])
                    self.assertNotIn("--no-default-features", filtered_runs[0])
                    clippy = [step["run"] for step in linux["steps"]
                              if step.get("run", "").lstrip().startswith("cargo clippy")]
                    self.assertEqual(clippy, [
                        "cargo clippy --manifest-path rust/Cargo.toml --locked --workspace "
                        "--all-targets --keep-going -- -D warnings"])
                    self.assertFalse(any("check-clippy-limits.py" in step.get("run", "")
                                         or "cargo test" in step.get("run", "")
                                         for step in linux["steps"]))
                    self.assertNotIn("rust-linux-build", jobs)
                    self.assertNotIn("rust-linux-rebuild", jobs)

    def test_merge_groups_run_linux_nextest_not_other_oses(self):
        """A merge group runs the Linux fmt, clippy and nextest job, not the nightly builds."""
        jobs = yaml.load((ROOT / ".github/workflows/rust-cross-os.yml").read_text(),
                         Loader=yaml.BaseLoader)["jobs"]
        self.assertIn("github.event_name == 'merge_group'", jobs["rust-linux"]["if"])
        self.assertNotIn("merge_group", jobs["rust-nightly"]["if"])
        self.assertIn("github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'",
                      jobs["rust-nightly"]["if"])
        tests, checks = [], []
        for step in jobs["rust-linux"]["steps"]:
            run = step.get("run", "")
            if "cargo nextest" in run:
                tests.append(step)
            elif "cargo fmt" in run or "cargo clippy" in run:
                checks.append(step)
        self.assertEqual([step["name"] for step in tests], ["nextest"])
        self.assertNotIn("merge_group", tests[0].get("if", ""))
        self.assertEqual([step["name"] for step in checks], ["cargo fmt", "clippy"])

    def test_pushes_to_main_run_linux_rust_only(self):
        """A push to main runs the Linux job. Windows and macOS stay on the nightly build."""
        workflow = yaml.load((ROOT / ".github/workflows/rust-cross-os.yml").read_text(),
                             Loader=yaml.BaseLoader)
        self.assertEqual(workflow["on"]["push"], {"branches": ["main"]})
        self.assertIn("schedule", workflow["on"])
        jobs = workflow["jobs"]
        self.assertIn("github.event_name == 'push'", jobs["rust-linux"]["if"])
        self.assertNotIn("push", jobs["rust-nightly"]["if"])
        alert = jobs["alert-macos-main"]
        self.assertEqual(alert["needs"], "rust-nightly")
        self.assertTrue(alert["if"].endswith(
            "(failure() && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'))"),
            alert["if"])
        self.assertEqual(jobs["rust-nightly"]["strategy"]["matrix"]["include"], [
            {"os": "windows-latest", "name": "windows"},
            {"os": "macos-26", "name": "macos"},
        ])

    def test_native_macos_plan_respects_paths_and_event(self):
        workflow = yaml.load((ROOT / ".github/workflows/desktop-native-macos.yml").read_text(),
                             Loader=yaml.BaseLoader)
        steps = workflow["jobs"]["changes"]["steps"]
        plan = next(step["run"] for step in steps if step.get("id") == "plan")
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory, "output")
            for event, relevant, label, draft, macos, intel in (
                ("pull_request", "false", "false", "false", "false", "false"),
                ("pull_request", "true", "false", "false", "true", "false"),
                ("pull_request", "true", "false", "true", "false", "false"),
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

    def test_nightly_builds_do_not_use_the_cross_os_label(self):
        rust = (ROOT / ".github/workflows/rust-cross-os.yml").read_text()
        self.assertNotIn("ci:cross-os", rust)
        native = yaml.load((ROOT / ".github/workflows/desktop-native-macos.yml").read_text(),
                           Loader=yaml.BaseLoader)
        self.assertEqual(list(native["on"]), ["workflow_dispatch"])
        self.assertTrue(any(step.get("run") == "python3 scripts/ci-path-filter.py macos"
                            for step in native["jobs"]["changes"]["steps"]))

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


class MainPushConcurrencyTests(unittest.TestCase):
    """Main pushes never cancel a running run (GRE-1228) and queue at most one pending run per workflow."""

    EACH_PUSH = set()
    MAIN_GROUP = "github.event_name == 'push' && github.ref == 'refs/heads/main' && github.run_attempt == '1' && 'main'"

    def test_only_pull_requests_cancel_and_other_events_get_their_own_group(self):
        checked = bounded = 0
        for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
            workflow = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
            events = workflow.get("on", {})
            concurrency = workflow.get("concurrency")
            if not isinstance(events, dict) or "push" not in events or not isinstance(concurrency, dict):
                continue
            cancel = concurrency["cancel-in-progress"]
            if cancel in {"false", "true"}:
                self.assertEqual(cancel, "false", f"{path.name} cancels runs on every event")
                continue
            checked += 1
            with self.subTest(workflow=path.name):
                expected = "${{ github.event_name == 'pull_request' }}"
                if path.name in {"rust-cross-os.yml", "rust-image.yml"}:
                    expected = "${{ github.event_name == 'pull_request' || github.event_name == 'merge_group' }}"
                    self.assertIn("github.event_name == 'merge_group'", concurrency["group"])
                    self.assertIn("github.ref", concurrency["group"])
                    self.assertIn("github.event.merge_group.head_sha", concurrency["group"])
                self.assertEqual(cancel, expected)
                self.assertIn("github.run_id", concurrency["group"])
                if path.name in self.EACH_PUSH:
                    self.assertNotIn("'main'", concurrency["group"], f"{path.name} compares each push with the one before")
                    self.assertNotIn("run_attempt", concurrency["group"] + cancel)
                    continue
                bounded += 1
                self.assertIn(self.MAIN_GROUP, concurrency["group"])
        # Desktop workflows no longer run on push (GRE-1649), so fewer workflows
        # share the main-push group.
        self.assertGreaterEqual(checked, 7)
        self.assertGreaterEqual(bounded, 7)


if __name__ == "__main__":
    unittest.main()
