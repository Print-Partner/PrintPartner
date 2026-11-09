"""Exercise the local CI command through its CLI entry point and the real workflows."""

import contextlib
import importlib.util
import io
from pathlib import Path
import re
import tempfile
import unittest
import unittest.mock

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

    def __init__(self, test, tools=TOOLS, node22="v22.23.3", fail=(), codes=None, affected="rdeps(=pp-fake)"):
        scratch = Path(test.enterContext(tempfile.TemporaryDirectory()))
        self.env = {"PATH": str(fake_tools(scratch / "bin", tools)), "CARGO_TARGET_DIR": str(scratch / "target")}
        if node22:
            self.env["PP_SCHEMA_PARITY_NODE"] = str(fake_tools(scratch / "node22", {"node": node22, "npm": "10.0.0"}) / "node")
        self.fail = set(fail)
        self.codes = codes or {}
        self.ran = []
        self.envs = {}
        self.affected = affected
        self.affected_calls = 0

    def filter_for(self, env):
        self.affected_calls += 1
        return self.affected

    def runner(self, step_id, script, cwd, env):
        self.ran.append(step_id)
        self.envs[step_id] = env
        return self.codes.get(step_id, 1 if step_id in self.fail else 0)

    def __call__(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            code = local_ci.main(list(argv), env=self.env, run=self.runner, affected=self.filter_for)
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

    def test_fast_runs_every_step_but_the_full_test_and_doc_passes_then_affected_nextest(self):
        ci = LocalCi(self)
        self.assertEqual(ci("--fast"), 0)
        full = [step.id for step in local_ci.STEPS if not step.host_only]
        left_out = ["nextest-off", "nextest-on", "doc-off", "doc-on"]
        expected = [("nextest-affected" if step == "nextest-on" else step)
                    for step in full if step == "nextest-on" or step not in left_out]
        self.assertEqual(ci.ran, expected)
        for step in left_out:
            self.assertNotIn(step, ci.summary)
        self.assertEqual(ci.summary["nextest-affected"], "PASS")
        for step in ["rust-windows", "rust-macos", "pr-evidence"]:
            self.assertEqual(ci.summary[step], "SKIP", step)
        self.assertLess(ci.ran.index("historical-schema"), ci.ran.index("nextest-affected"))

    def test_fast_nextest_runs_desktop_rebuild_on_with_the_affected_filter(self):
        ci = LocalCi(self, affected="rdeps(=pp-storage) + rdeps(=pp-gateway)")
        self.assertEqual(ci("--fast"), 0)
        self.assertEqual(ci.affected_calls, 1)
        env = ci.envs["nextest-affected"]
        self.assertEqual(env["NEXTEST_FILTER"], "rdeps(=pp-storage) + rdeps(=pp-gateway)")
        self.assertEqual(env["INSTA_UPDATE"], "no")
        step = next(step for step in local_ci.fast_steps() if step.id == "nextest-affected")
        nextest_on = next(step for step in local_ci.STEPS if step.id == "nextest-on")
        self.assertEqual(step.commands, nextest_on.commands)
        self.assertIn("--features desktop_rebuild", step.commands[0])
        self.assertIn('-E "$NEXTEST_FILTER"', step.commands[0])

    def test_fast_lists_its_own_steps_and_accepts_only_and_skip(self):
        ci = LocalCi(self)
        self.assertEqual(ci("--fast", "--list"), 0)
        self.assertIn("nextest-affected", ci.output)
        for step in ["nextest-off", "nextest-on", "doc-off", "doc-on"]:
            self.assertNotIn(step + " ", ci.output)
        ci = LocalCi(self)
        self.assertEqual(ci("--fast", "--only", "fmt,nextest-affected"), 0)
        self.assertEqual(ci.ran, ["fmt", "nextest-affected"])
        ci = LocalCi(self)
        self.assertEqual(ci("--fast", "--skip", "nextest-affected"), 0)
        self.assertEqual(ci.affected_calls, 0)
        self.assertEqual(ci("--fast", "--only", "nextest-on"), 2)
        self.assertEqual(ci("--only", "nextest-affected"), 2)

    def test_fast_skips_nextest_without_computing_a_filter_when_nextest_is_missing(self):
        tools = {k: v for k, v in TOOLS.items() if k != "cargo-nextest"}
        ci = LocalCi(self, tools=tools)
        self.assertEqual(ci("--fast", "--only", "nextest-affected", "--fail-on-skip"), 1)
        self.assertEqual(ci.summary["nextest-affected"], "SKIP")
        self.assertEqual(ci.ran, [])
        self.assertEqual(ci.affected_calls, 0)

    def test_fast_nextest_failure_fails_the_run(self):
        ci = LocalCi(self, fail={"nextest-affected"})
        self.assertEqual(ci("--fast"), 1)
        self.assertEqual(ci.summary["nextest-affected"], "FAIL")

    def test_sccache_wraps_rustc_when_installed_and_changes_nothing_otherwise(self):
        missing = Path(self.enterContext(tempfile.TemporaryDirectory())) / "no-sccache"
        self.enterContext(unittest.mock.patch.object(local_ci, "FLEET_SCCACHE", missing))
        for args in ((), ("--fast",)):
            with self.subTest(args=args):
                ci = LocalCi(self, tools={**TOOLS, "sccache": "sccache 0.10.0"})
                self.assertEqual(ci(*args), 0)
                sccache = str(Path(ci.env["PATH"]) / "sccache")
                for env in ci.envs.values():
                    self.assertEqual(env["RUSTC_WRAPPER"], sccache)
                    self.assertEqual(env["CARGO_INCREMENTAL"], "0")
                ci = LocalCi(self)
                self.assertEqual(ci(*args), 0)
                for env in ci.envs.values():
                    self.assertNotIn("RUSTC_WRAPPER", env)
                    self.assertNotIn("CARGO_INCREMENTAL", env)

    def test_sccache_falls_back_to_the_fleet_install_and_keeps_a_callers_wrapper(self):
        fleet = fake_tools(Path(self.enterContext(tempfile.TemporaryDirectory())), {"sccache": "sccache 0.10.0"})
        self.enterContext(unittest.mock.patch.object(local_ci, "FLEET_SCCACHE", fleet / "sccache"))
        ci = LocalCi(self)
        self.assertEqual(ci("--only", "fmt"), 0)
        self.assertEqual(ci.envs["fmt"]["RUSTC_WRAPPER"], str(fleet / "sccache"))
        self.assertEqual(ci.envs["fmt"]["CARGO_INCREMENTAL"], "0")
        for wrapper in ("/usr/bin/other-wrapper", ""):
            with self.subTest(wrapper=wrapper):
                ci = LocalCi(self)
                ci.env["RUSTC_WRAPPER"] = wrapper
                self.assertEqual(ci("--only", "fmt"), 0)
                self.assertEqual(ci.envs["fmt"]["RUSTC_WRAPPER"], wrapper)
                self.assertNotIn("CARGO_INCREMENTAL", ci.envs["fmt"])


class AffectedFilterTests(unittest.TestCase):
    """affected_nextest_filter() with a fake git, against the real filter script's rules."""

    BASE = "a" * 40

    def filter_for(self, outputs):
        calls = []

        def git(env, *args):
            calls.append(args)
            return outputs.get(args[0])

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            expression = local_ci.affected_nextest_filter({}, git=git)
        return expression, calls, out.getvalue()

    def test_diffs_the_working_tree_against_the_merge_base_with_origin_main(self):
        expression, calls, output = self.filter_for({
            "merge-base": self.BASE + "\n",
            "diff": "rust/crates/pp-domain/src/lib.rs\0docs/x.md\0",
            "ls-files": "rust/crates/pp-os/src/new.rs\0",
        })
        self.assertEqual(calls[0], ("merge-base", "origin/main", "HEAD"))
        self.assertEqual(calls[1], ("diff", "--name-only", "--no-renames", "-z", self.BASE))
        self.assertEqual(calls[2], ("ls-files", "--others", "--exclude-standard", "-z"))
        self.assertEqual(expression, "rdeps(=pp-domain) + rdeps(=pp-os)")
        self.assertIn(expression, output)

    def test_shared_inputs_force_all(self):
        for path in ["rust/Cargo.lock", "web/apps/web/src/main.tsx", "fixtures/http/a.json",
                     "scripts/ci-affected-nextest-filter.py"]:
            with self.subTest(path=path):
                expression, _, _ = self.filter_for({
                    "merge-base": self.BASE,
                    "diff": f"rust/crates/pp-domain/src/lib.rs\0{path}\0",
                    "ls-files": "",
                })
                self.assertEqual(expression, "all()")

    def test_no_merge_base_or_a_failed_diff_falls_back_to_all(self):
        for outputs in ({"merge-base": None}, {"merge-base": ""},
                        {"merge-base": self.BASE, "diff": None, "ls-files": ""},
                        {"merge-base": self.BASE, "diff": "", "ls-files": None}):
            with self.subTest(outputs=outputs):
                expression, _, output = self.filter_for(outputs)
                self.assertEqual(expression, "all()")
                self.assertIn("using all()", output)

    def test_a_broken_filter_script_falls_back_to_all(self):
        def broken():
            raise SyntaxError("broken filter")

        self.enterContext(unittest.mock.patch.object(local_ci, "load_filter_module", broken))
        expression, _, output = self.filter_for({
            "merge-base": self.BASE, "diff": "rust/crates/pp-domain/src/lib.rs\0", "ls-files": ""})
        self.assertEqual(expression, "all()")
        self.assertIn("using all()", output)

    def test_real_git_returns_a_filter_for_this_checkout(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            expression = local_ci.affected_nextest_filter(dict(local_ci.os.environ))
        self.assertTrue(expression == "all()" or expression.startswith("rdeps(="), expression)


if __name__ == "__main__":
    unittest.main()
