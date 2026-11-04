"""Exercise the local CI command through its CLI entry point and the real workflows."""

import contextlib
import importlib.util
import io
from pathlib import Path
import re
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = Path(__file__).with_name("local-ci.py").resolve()
spec = importlib.util.spec_from_file_location("local_ci", SCRIPT)
local_ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(local_ci)

TOOLS = {
    "node": "v24.21.0", "npm": "11.0.0", "cargo": "cargo 1.99.0", "cargo-nextest": "0.9.148",
    "python3": "Python 3.12.0", "git": "git version 2.50.0", "ast-grep": "ast-grep 0.50.0",
    "actionlint": "1.7.12", "zizmor": "zizmor 1.31.1", "cargo-machete": "0.9.2",
    "ruff": f"ruff {local_ci.RUFF_VERSION}",
}


def fake_tools(directory, tools):
    directory.mkdir(parents=True, exist_ok=True)
    for name, version in tools.items():
        tool = directory / name
        tool.write_text(f"#!/bin/sh\necho '{version}'\n")
        tool.chmod(0o755)
    return directory


class LocalCi:
    """Run main() with fake tools on PATH and a runner that records each step's script."""

    def __init__(self, test, tools=TOOLS, node22="v22.23.3", fail=(), codes=None):
        scratch = Path(test.enterContext(tempfile.TemporaryDirectory()))
        self.env = {"PATH": str(fake_tools(scratch / "bin", tools)), "CARGO_TARGET_DIR": str(scratch / "target")}
        if node22:
            self.env["PP_SCHEMA_PARITY_NODE"] = str(fake_tools(scratch / "node22", {"node": node22, "npm": "10.0.0"}) / "node")
        self.fail = set(fail)
        self.codes = codes or {}
        self.ran = []
        self.envs = {}

    def runner(self, step_id, script, cwd, env):
        self.ran.append(step_id)
        self.envs[step_id] = env
        return self.codes.get(step_id, 1 if step_id in self.fail else 0)

    def __call__(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            code = local_ci.main(list(argv), env=self.env, run=self.runner)
        self.output = out.getvalue()
        self.summary = {line.split()[1]: line.split()[0]
                        for line in self.output.split("== local CI summary ==")[-1].splitlines()
                        if line.split() and line.split()[0] in {"PASS", "FAIL", "SKIP"}}
        return code


class LocalCiTests(unittest.TestCase):
    def test_full_run_executes_every_local_step_and_reports_host_only_steps_as_skipped(self):
        ci = LocalCi(self)
        self.assertEqual(ci(), 0)
        for step in ["fmt", "clippy-off", "clippy-on", "nextest-off", "nextest-on", "doc-off", "doc-on",
                     "web-assets", "workaround-ratchet", "ast-grep", "boundaries", "selector-tests",
                     "node-fixtures", "actionlint", "zizmor", "rust-deps", "duplication", "ui-package"]:
            self.assertEqual(ci.summary[step], "PASS", step)
        for step in ["rust-windows", "rust-macos", "pr-evidence"]:
            self.assertEqual(ci.summary[step], "SKIP", step)
            self.assertNotIn(step, ci.ran)
        self.assertIn("rust-windows", ci.output.split("== local CI summary ==")[-1])
        self.assertIn("Windows runner", ci.output)

    def test_historical_schema_commits_are_fetched_before_any_nextest_step(self):
        ci = LocalCi(self)
        self.assertEqual(ci(), 0)
        self.assertEqual(ci.summary["historical-schema"], "PASS")
        fetched = ci.ran.index("historical-schema")
        self.assertLess(fetched, ci.ran.index("nextest-off"))
        self.assertLess(fetched, ci.ran.index("nextest-on"))

    def test_cargo_steps_refuse_to_write_snapshots(self):
        ci = LocalCi(self)
        self.assertEqual(ci(), 0)
        for step in ["clippy-off", "clippy-on", "nextest-off", "nextest-on", "doc-off", "doc-on"]:
            self.assertEqual(ci.envs[step].get("INSTA_UPDATE"), "no", step)
        self.assertNotIn("INSTA_UPDATE", ci.envs["node-fixtures"])

    def test_a_failed_step_does_not_stop_later_steps_and_fails_the_run(self):
        ci = LocalCi(self, fail={"fmt"})
        self.assertEqual(ci(), 1)
        self.assertEqual(ci.summary["fmt"], "FAIL")
        self.assertEqual(ci.summary["doc-on"], "PASS")
        self.assertIn("1 failed", ci.output)

    @unittest.skipUnless(local_ci.sys.platform == "linux", "Clippy baseline needs Linux")
    def test_clippy_limits_runs_after_clippy_and_propagates_failure(self):
        ci = LocalCi(self, fail={"clippy-limits"})
        self.assertEqual(ci(), 1)
        self.assertEqual(ci.summary["clippy-limits"], "FAIL")
        self.assertLess(ci.ran.index("clippy-on"), ci.ran.index("clippy-limits"))
        self.assertIn("nextest-on", ci.ran)
        self.assertIn("clippy-limits", ci.output)

    def test_a_missing_tool_skips_its_step_with_the_reason_and_runs_nothing_for_it(self):
        tools = {k: v for k, v in TOOLS.items() if k != "actionlint"}
        ci = LocalCi(self, tools=tools)
        self.assertEqual(ci(), 0)
        self.assertEqual(ci.summary["actionlint"], "SKIP")
        self.assertNotIn("actionlint", ci.ran)
        self.assertIn("actionlint not on PATH", ci.output)

    def test_a_wrong_pinned_version_skips_its_step(self):
        ci = LocalCi(self, tools={**TOOLS, "ast-grep": "ast-grep 0.49.0"})
        self.assertEqual(ci(), 0)
        self.assertEqual(ci.summary["ast-grep"], "SKIP")
        self.assertIn("ast-grep 0.50.0", ci.output)

    def test_rust_deps_requires_the_pinned_scanner(self):
        for version in (None, "0.9.1"):
            tools = {k: v for k, v in TOOLS.items() if k != "cargo-machete"}
            if version:
                tools["cargo-machete"] = version
            ci = LocalCi(self, tools=tools)
            self.assertEqual(ci("--only", "rust-deps", "--fail-on-skip"), 1)
            self.assertEqual(ci.summary["rust-deps"], "SKIP")
            self.assertEqual(ci.ran, [])
            self.assertIn("cargo-machete", ci.output)

    def test_python_lint_requires_the_version_from_ruff_config(self):
        configured = re.search(
            r'^required-version = "==([0-9.]+)"$',
            (ROOT / "ruff.toml").read_text(),
            re.MULTILINE,
        )[1]
        self.assertEqual(local_ci.RUFF_VERSION, configured)

        wrong_version = "0.0.0" if configured != "0.0.0" else "0.0.1"
        for version in (None, f"ruff {wrong_version}"):
            tools = {k: v for k, v in TOOLS.items() if k != "ruff"}
            if version:
                tools["ruff"] = version
            ci = LocalCi(self, tools=tools)
            self.assertEqual(ci("--only", "python-lint", "--fail-on-skip"), 1)
            self.assertEqual(ci.summary["python-lint"], "SKIP")
            self.assertEqual(ci.ran, [])
            self.assertIn(configured, ci.output)

        ci = LocalCi(self)
        self.assertEqual(ci("--only", "python-lint", "--fail-on-skip"), 0)
        self.assertEqual(ci.ran, ["python-lint"])

    def test_fail_on_skip_rejects_missing_tools_and_wrong_versions(self):
        for tools in ({k: v for k, v in TOOLS.items() if k != "npm"},
                      {**TOOLS, "node": "v0.0.0"}):
            ci = LocalCi(self, tools=tools)
            self.assertEqual(ci("--only", "web-assets", "--fail-on-skip"), 1)
            self.assertEqual(ci.summary["web-assets"], "SKIP")
            self.assertEqual(ci.ran, [])
        ci = LocalCi(self)
        self.assertEqual(ci("--only", "web-assets", "--fail-on-skip"), 0)
        self.assertEqual(ci.ran, ["web-assets"])

    def test_node_22_steps_skip_without_the_pinned_node_22(self):
        for node22 in [None, "v22.22.0"]:
            for step in ["node-fixtures", "workaround-ratchet", "duplication"]:
                with self.subTest(node22=node22, step=step):
                    ci = LocalCi(self, node22=node22)
                    self.assertEqual(ci(), 0)
                    self.assertEqual(ci.summary[step], "SKIP")
                    self.assertNotIn(step, ci.ran)
                    self.assertIn("PP_SCHEMA_PARITY_NODE", ci.output)

    def test_only_and_skip_select_steps_in_plan_order(self):
        ci = LocalCi(self)
        self.assertEqual(ci("--only", "clippy-on,fmt"), 0)
        self.assertEqual(ci.ran, ["fmt", "clippy-on"])
        ci = LocalCi(self)
        self.assertEqual(ci("--skip", "nextest-on,node-fixtures"), 0)
        self.assertNotIn("nextest-on", ci.ran)
        self.assertNotIn("node-fixtures", ci.ran)
        self.assertIn("nextest-off", ci.ran)
        self.assertNotIn("nextest-on", ci.summary)

    def test_an_unknown_step_id_is_a_usage_error(self):
        ci = LocalCi(self)
        self.assertEqual(ci("--only", "fmt,lint"), 2)
        self.assertEqual(ci.ran, [])
        self.assertIn("unknown step: lint", ci.output)

    def test_list_prints_every_step_without_running_any(self):
        ci = LocalCi(self)
        self.assertEqual(ci("--list"), 0)
        self.assertEqual(ci.ran, [])
        for step in ["fmt", "nextest-on", "node-fixtures", "rust-windows"]:
            self.assertIn(step, ci.output)

    def test_the_ratchet_runs_between_the_two_installs_of_web_node_modules(self):
        ci = LocalCi(self)
        self.assertEqual(ci(), 0)
        self.assertLess(ci.ran.index("web-assets"), ci.ran.index("workaround-ratchet"))
        self.assertLess(ci.ran.index("workaround-ratchet"), ci.ran.index("node-fixtures"))
        self.assertLess(ci.ran.index("web-assets"), ci.ran.index("duplication"))
        self.assertLess(ci.ran.index("duplication"), ci.ran.index("node-fixtures"))

    def test_ui_package_runs_its_checks_after_web_assets_installs_node_modules(self):
        ci = LocalCi(self)
        self.assertEqual(ci(), 0)
        self.assertLess(ci.ran.index("web-assets"), ci.ran.index("ui-package"))
        self.assertLess(ci.ran.index("ui-package"), ci.ran.index("node-fixtures"))
        step = next(step for step in local_ci.STEPS if step.id == "ui-package")
        for script in ("lint", "test", "build"):
            self.assertIn(f"npm --prefix web run {script} --workspace @print-partner/ui", step.setup)
        self.assertNotIn("npm --prefix web run typecheck --workspace @print-partner/ui", step.setup)
        build = step.setup.index("npm --prefix web run build --workspace @print-partner/ui")
        browser = step.setup.index("npm --prefix web run test:browser --workspace @print-partner/ui")
        self.assertLess(build, browser)
        self.assertFalse([line for line in step.setup if "build-desktop-manifest.py" in line])

    def test_ui_package_reports_a_skipped_browser_engine_as_skip(self):
        ci = LocalCi(self, codes={"ui-package": 77})
        self.assertEqual(ci("--only", "ui-package"), 0)
        self.assertEqual(ci.summary["ui-package"], "SKIP")
        self.assertEqual(ci("--only", "ui-package", "--fail-on-skip"), 1)
        ci = LocalCi(self, codes={"ui-package": 2})
        self.assertEqual(ci("--only", "ui-package"), 1)
        self.assertEqual(ci.summary["ui-package"], "FAIL")

    def test_ui_package_skips_without_node_24(self):
        ci = LocalCi(self, tools={**TOOLS, "node": "v22.23.3"})
        self.assertEqual(ci("--only", "ui-package"), 0)
        self.assertEqual(ci.summary["ui-package"], "SKIP")
        self.assertIn("needs node v24.21.0", ci.output)

    def test_duplication_failure_fails_the_run_without_stopping_later_steps(self):
        ci = LocalCi(self, fail={"duplication"})
        self.assertEqual(ci(), 1)
        self.assertEqual(ci.summary["duplication"], "FAIL")
        self.assertIn("node-fixtures", ci.ran)

    def test_just_ci_runs_this_command_with_its_arguments_and_no_step_list_of_its_own(self):
        recipe = (ROOT / "justfile").read_text().split("\nci *args:\n", 1)[1]
        commands = []
        for line in recipe.splitlines():
            if line and not line.startswith((" ", "\t")):
                break
            if line.strip():
                commands.append(line.strip())
        self.assertEqual(commands, ['python3 scripts/local-ci.py "$@"'])
        self.assertIn("set positional-arguments", (ROOT / "justfile").read_text())

    def test_mirrored_commands_match_the_workflow_steps_they_copy(self):
        checked = 0
        for step in local_ci.STEPS:
            if not step.ci:
                continue
            lines = set()
            for source in step.ci:
                workflow_file, step_name = source.split("#", 1)
                workflow = yaml.load((ROOT / ".github/workflows" / workflow_file).read_text(), Loader=yaml.BaseLoader)
                runs = [s["run"] for job in workflow["jobs"].values() for s in job.get("steps", [])
                        if s.get("name") == step_name and "run" in s]
                self.assertTrue(runs, f"{source} has no run step")
                for run in runs:
                    run = run.replace("${{ env.CARGO_SCOPE }}", "--workspace")
                    lines.update(line.strip() for line in run.splitlines())
            for command in step.commands:
                with self.subTest(step=step.id, command=command):
                    self.assertIn(command, lines)
                    checked += 1
        self.assertGreater(checked, 20)


if __name__ == "__main__":
    unittest.main()
