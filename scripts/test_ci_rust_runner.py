"""Exercise the Linux Rust runner choice without calling GitHub."""

from datetime import datetime, timedelta, timezone
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("ci-rust-runner.py").resolve()
spec = importlib.util.spec_from_file_location("ci_rust_runner", SCRIPT)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

NOW = datetime(2026, 10, 30, 12, 0, tzinfo=timezone.utc)
REPO = "poitee/PrintPartner"
BASE = {"EVENT_NAME": "push", "REPOSITORY": REPO, "GH_TOKEN": "actions-token", "RUN_ID": "1"}


def pp_runner(status="online", busy=False, label="pp-local"):
    return {"status": status, "busy": busy,
            "labels": [{"name": name} for name in ("self-hosted", "Linux", "X64", label)]}


def job(name="rust (linux) build", minutes=20, status="queued", labels=runner.LOCAL):
    created = (NOW - timedelta(minutes=minutes)).strftime("%Y-%m-%dT%H:%M:%SZ")
    return {"name": name, "status": status, "labels": labels, "created_at": created}


class Api:
    """Answer runner, run and job listings; raise for anything in `fail`."""

    def __init__(self, runners=None, jobs=None, fail=()):
        self.runners, self.jobs, self.fail, self.calls = runners or [], jobs or {}, fail, []

    def __call__(self, path, token):
        self.calls.append((path.split("?")[0], token))
        if any(part in path for part in self.fail):
            raise OSError("HTTP 403")
        if path.startswith(f"repos/{REPO}/actions/runners"):
            return {"runners": self.runners}
        if "/workflows/" in path:
            status = "queued" if "status=queued" in path else "in_progress"
            return {"workflow_runs": [{"id": run_id} for run_id, (run_status, _) in self.jobs.items()
                                      if run_status == status]}
        run_id = int(path.split("/runs/")[1].split("/")[0])
        return {"jobs": self.jobs[run_id][1]}


class RustRunnerTests(unittest.TestCase):
    def choose(self, api=None, **env):
        return runner.choose({**BASE, **env}, api or Api(), NOW)

    def test_pull_requests_and_other_events_that_stay_hosted(self):
        for env in ({"EVENT_NAME": "pull_request"}, {"EVENT_NAME": "pull_request_target"},
                    {"EVENT_NAME": "schedule"}, {"EVENT_NAME": ""},
                    {"PP_RUST_RUNNER": "hosted"}, {"PP_RUST_RUNNER": " Hosted "},
                    {"PP_RUST_RUNNER": "maybe"}):
            with self.subTest(env=env):
                api = Api(runners=[pp_runner()])
                self.assertEqual(self.choose(api, RUNNER_TOKEN="admin", **env)[0], runner.HOSTED)
                self.assertEqual(api.calls, [])

    def test_local_events_use_pp_local_by_default(self):
        for event in ("push", "merge_group", "workflow_dispatch"):
            for mode in ("", "local", "LOCAL"):
                with self.subTest(event=event, mode=mode):
                    self.assertEqual(self.choose(EVENT_NAME=event, PP_RUST_RUNNER=mode)[0],
                                     runner.LOCAL)

    def test_runner_token_needs_an_online_idle_pp_local_runner(self):
        for runners, expected in (
            ([pp_runner(), pp_runner(status="offline")], runner.LOCAL),
            ([pp_runner(busy=True), pp_runner(status="offline")], runner.HOSTED),
            ([pp_runner(status="offline", busy=False)], runner.HOSTED),
            ([pp_runner(label="fleet")], runner.HOSTED),
            ([], runner.HOSTED),
        ):
            with self.subTest(runners=runners):
                api = Api(runners=runners)
                self.assertEqual(self.choose(api, RUNNER_TOKEN="admin")[0], expected)
                self.assertEqual(api.calls, [(f"repos/{REPO}/actions/runners", "admin")])

    def test_unreadable_runner_list_falls_back_to_the_queue_check(self):
        api = Api(runners=[pp_runner()], jobs={7: ("in_progress", [job()])}, fail=("/runners",))
        runs_on, reason = self.choose(api, RUNNER_TOKEN="admin")
        self.assertEqual(runs_on, runner.HOSTED)
        self.assertIn("run 7", reason)
        self.assertIn(("repos/poitee/PrintPartner/actions/runs/7/jobs", "actions-token"), api.calls)

    def test_a_stale_pp_local_rust_job_routes_new_runs_to_hosted(self):
        for jobs, expected in (
            ([job()], runner.HOSTED),
            ([job(name="rust (linux) rebuild 2/3")], runner.HOSTED),
            ([job(name="rust (linux)", minutes=15)], runner.HOSTED),
            ([job(minutes=14)], runner.LOCAL),
            ([job(status="in_progress")], runner.LOCAL),
            ([job(labels=["ubuntu-latest"])], runner.LOCAL),
            ([job(name="rust (windows)", labels=["self-hosted", "pp-local"])], runner.LOCAL),
        ):
            for status in ("queued", "in_progress"):
                with self.subTest(jobs=jobs, status=status):
                    self.assertEqual(self.choose(Api(jobs={7: (status, jobs)}))[0], expected)

    def test_the_current_run_is_ignored(self):
        self.assertEqual(self.choose(Api(jobs={1: ("in_progress", [job()])}))[0], runner.LOCAL)

    def test_unreadable_queue_keeps_the_variable_default(self):
        runs_on, reason = self.choose(Api(fail=("/workflows/",)))
        self.assertEqual(runs_on, runner.LOCAL)
        self.assertIn("unreadable", reason)

    def test_pull_requests_follow_pp_pr_runner_and_default_to_hosted(self):
        for mode, linux in (("", runner.HOSTED), ("hosted", runner.HOSTED), (" Hosted ", runner.HOSTED),
                            ("maybe", runner.HOSTED), ("pp-local", runner.HOSTED),
                            ("blacksmith", runner.BLACKSMITH["linux"]), (" Blacksmith ", runner.BLACKSMITH["linux"]),
                            ("pp-iso", runner.ISOLATED)):
            with self.subTest(mode=mode):
                api = Api(runners=[pp_runner()])
                env = {"EVENT_NAME": "pull_request", "PP_PR_RUNNER": mode, "PP_RUST_RUNNER": "local"}
                self.assertEqual(self.choose(api, RUNNER_TOKEN="admin", **env)[0], linux)
                self.assertEqual(api.calls, [])

    def test_pp_pr_runner_does_not_move_other_events(self):
        for event in ("push", "merge_group", "workflow_dispatch"):
            self.assertEqual(self.choose(EVENT_NAME=event, PP_PR_RUNNER="blacksmith")[0], runner.LOCAL)
        for event in ("schedule", "pull_request_target", ""):
            self.assertEqual(self.choose(EVENT_NAME=event, PP_PR_RUNNER="blacksmith")[0], runner.HOSTED)

    def test_runner_kind_names_the_pool(self):
        self.assertEqual(runner.runner_kind(runner.LOCAL), "pp-local")
        self.assertEqual(runner.runner_kind(runner.ISOLATED), "pp-iso")
        self.assertEqual(runner.runner_kind(runner.BLACKSMITH["linux"]), "blacksmith")
        self.assertEqual(runner.runner_kind(runner.HOSTED), "hosted")
        self.assertNotIn("pp-local", runner.ISOLATED)

    def test_cli_routes_windows_and_smoke_only_for_blacksmith_pull_requests(self):
        for event, mode, windows, smoke, kind in (
            ("pull_request", "blacksmith", "blacksmith-8vcpu-windows-2025", "blacksmith-4vcpu-ubuntu-2404", "blacksmith"),
            ("pull_request", "pp-iso", "windows-latest", "ubuntu-24.04", "pp-iso"),
            ("pull_request", "", "windows-latest", "ubuntu-24.04", "hosted"),
            ("schedule", "blacksmith", "windows-latest", "ubuntu-24.04", "hosted"),
        ):
            with self.subTest(event=event, mode=mode), tempfile.TemporaryDirectory() as directory:
                output, summary = Path(directory, "output"), Path(directory, "summary")
                env = {"PATH": os.environ["PATH"], "EVENT_NAME": event, "PP_PR_RUNNER": mode,
                       "REPOSITORY": REPO, "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary)}
                subprocess.run(["python3", "-B", str(SCRIPT)], env=env, check=True, capture_output=True)
                values = dict(line.partition("=")[::2] for line in output.read_text().splitlines())
                self.assertEqual(json.loads(values["windows_runs_on"]), windows)
                self.assertEqual(json.loads(values["smoke_runs_on"]), smoke)
                self.assertEqual(values["linux_kind"], kind)

    def test_cli_writes_a_json_runs_on_output(self):
        with tempfile.TemporaryDirectory() as directory:
            output, summary = Path(directory, "output"), Path(directory, "summary")
            env = {"PATH": os.environ["PATH"], "EVENT_NAME": "schedule", "REPOSITORY": REPO,
                   "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary)}
            subprocess.run(["python3", "-B", str(SCRIPT)], env=env, check=True, capture_output=True)
            values = dict(line.partition("=")[::2] for line in output.read_text().splitlines())
            self.assertEqual(json.loads(values["linux_runs_on"]), runner.HOSTED)
            self.assertEqual(values["linux_kind"], "hosted")
            self.assertEqual(json.loads(values["windows_runs_on"]), "windows-latest")
            self.assertEqual(json.loads(values["smoke_runs_on"]), "ubuntu-24.04")
            self.assertIn("schedule events stay hosted", summary.read_text())
        self.assertEqual(json.loads(json.dumps(runner.LOCAL, separators=(",", ":"))), runner.LOCAL)


if __name__ == "__main__":
    unittest.main()
