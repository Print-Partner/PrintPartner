"""Exercise affected-crate nextest filter selection."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("ci-affected-nextest-filter.py").resolve()
spec = importlib.util.spec_from_file_location("ci_affected", SCRIPT)
ci_affected = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci_affected)

WORKSPACE_PACKAGES = frozenset({
    "pp-api", "pp-api-types", "pp-compat", "pp-contracts", "pp-core", "pp-desktop",
    "pp-domain", "pp-drivers", "pp-gateway", "pp-net", "pp-server", "pp-services",
    "pp-source", "pp-storage",
})


class CiAffectedNextestFilterTests(unittest.TestCase):
    def test_non_pr_events_use_all(self):
        for event in ["push", "merge_group", "workflow_dispatch", "schedule"]:
            self.assertEqual(
                ci_affected.filter_expression(event, ["rust/crates/pp-storage/src/lib.rs"],
                                              WORKSPACE_PACKAGES),
                "all()")

    def test_single_crate_uses_rdeps(self):
        self.assertEqual(
            ci_affected.filter_expression(
                "pull_request",
                ["rust/crates/pp-contracts/src/lib.rs"],
                WORKSPACE_PACKAGES),
            "rdeps(=pp-contracts)")

    def test_multiple_crates_union_sorted(self):
        self.assertEqual(
            ci_affected.filter_expression(
                "pull_request",
                ["rust/crates/pp-storage/src/lib.rs",
                 "rust/crates/pp-api/src/lib.rs",
                 "rust/crates/pp-storage/tests/a.rs"],
                WORKSPACE_PACKAGES),
            "rdeps(=pp-api) + rdeps(=pp-storage)")

    def test_workspace_or_shared_inputs_force_all(self):
        for path in [
            "rust/Cargo.toml",
            "rust/Cargo.lock",
            "rust-toolchain.toml",
            "rust/tests/lifecycle_m0.py",
            "rust/scripts/build-desktop-manifest.py",
            "rust/.config/nextest.toml",
            "web/apps/web/src/App.tsx",
            "web/package.json",
            "fixtures/parity/jobs-model/jobs-model.json",
            ".github/workflows/rust-cross-os.yml",
            "scripts/ci-path-filter.py",
            "scripts/ci-affected-nextest-filter.py",
            "scripts/setup-ci-linux.sh",
            ".github/actions/linux-gui-deps/action.yml",
        ]:
            with self.subTest(path=path):
                self.assertEqual(
                    ci_affected.filter_expression(
                        "pull_request", [path], WORKSPACE_PACKAGES),
                    "all()")

    def test_crate_plus_shared_input_forces_all(self):
        self.assertEqual(
            ci_affected.filter_expression(
                "pull_request",
                ["rust/crates/pp-storage/src/lib.rs", "web/package.json"],
                WORKSPACE_PACKAGES),
            "all()")

    def test_unknown_crate_directory_forces_all(self):
        self.assertEqual(
            ci_affected.filter_expression(
                "pull_request",
                ["rust/crates/not-a-member/src/lib.rs"],
                WORKSPACE_PACKAGES),
            "all()")

    def test_empty_diff_forces_all(self):
        self.assertEqual(
            ci_affected.filter_expression("pull_request", [], WORKSPACE_PACKAGES),
            "all()")

    def test_source_readers_join_the_selection(self):
        with tempfile.TemporaryDirectory() as directory:
            crates = Path(directory, "crates")
            for name, path, text in [
                ("pp-a", "src/lib.rs", 'include_str!("../data/schema.json");\n'),
                ("pp-b", "tests/guard.rs",
                 'include_str!("../../pp-a/src/schema.rs");\n'
                 'include_bytes!("../../../fixtures/x.json");\n'),
                ("pp-c", "src/lib.rs", 'fn f() {}\n'),
            ]:
                Path(crates, name, path).parent.mkdir(parents=True, exist_ok=True)
                Path(crates, name, path).write_text(text)
                Path(crates, name, "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\n')
            packages = ci_affected.workspace_packages(crates)
            readers = ci_affected.source_readers(crates)
            self.assertEqual(readers, {"pp-a": {"pp-b"}})
            for changed, expected in [
                ("pp-a", "rdeps(=pp-a) + rdeps(=pp-b)"),
                ("pp-b", "rdeps(=pp-b)"),
                ("pp-c", "rdeps(=pp-c)"),
            ]:
                with self.subTest(changed=changed):
                    self.assertEqual(
                        ci_affected.filter_expression(
                            "pull_request", [f"rust/crates/{changed}/src/lib.rs"],
                            packages, readers),
                        expected)

    def test_real_workspace_source_readers_cover_gateway_guards(self):
        # pp-gateway's ticket_t_7 tests read these crates' sources without a
        # Cargo dependency on them.
        crates = SCRIPT.parents[1] / "rust" / "crates"
        packages = ci_affected.workspace_packages(crates)
        readers = ci_affected.source_readers(crates)
        for changed in ["pp-storage", "pp-core", "pp-source"]:
            with self.subTest(changed=changed):
                selected = ci_affected.filter_expression(
                    "pull_request", [f"rust/crates/{changed}/src/lib.rs"],
                    packages, readers).split(" + ")
                self.assertLessEqual(
                    {f"rdeps(={changed})", "rdeps(=pp-gateway)"}, set(selected))

    def test_package_from_path(self):
        self.assertEqual(
            ci_affected.package_for_path("rust/crates/pp-gateway/src/ownership/resources.rs"),
            "pp-gateway")
        self.assertIsNone(ci_affected.package_for_path("rust/Cargo.toml"))
        self.assertIsNone(ci_affected.package_for_path("docs/development/ci.md"))

    def test_cli_writes_github_output(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(
                    ["git", *args], cwd=directory, text=True).strip()

            git("init", "-q")
            git("config", "user.email", "proof@example.com")
            git("config", "user.name", "Proof")
            Path(directory, "rust/crates/pp-contracts/src").mkdir(parents=True)
            Path(directory, "rust/crates/pp-contracts/src/lib.rs").write_text("fn a() {}\n")
            Path(directory, "rust/crates/pp-contracts/Cargo.toml").write_text(
                '[package]\nname = "pp-contracts"\nversion = "0.1.0"\n')
            git("add", "-A")
            git("commit", "-qm", "base")
            base = git("rev-parse", "HEAD")
            Path(directory, "rust/crates/pp-contracts/src/lib.rs").write_text("fn b() {}\n")
            git("add", "-A")
            git("commit", "-qm", "change")
            head = git("rev-parse", "HEAD")

            output = Path(directory, "output")
            summary = Path(directory, "summary")
            output.write_text("")
            summary.write_text("")
            # Point the script at this temp tree's crates for package discovery.
            result = subprocess.check_output(
                ["python3", str(SCRIPT)],
                cwd=directory,
                text=True,
                env={
                    **os.environ,
                    "EVENT_NAME": "pull_request",
                    "BASE_SHA": base,
                    "HEAD_SHA": head,
                    "GITHUB_OUTPUT": str(output),
                    "GITHUB_STEP_SUMMARY": str(summary),
                    "PP_AFFECTED_CRATES_ROOT": str(Path(directory, "rust/crates")),
                },
            )
            self.assertIn("rdeps(=pp-contracts)", result)
            self.assertEqual(output.read_text(), "nextest_filter=rdeps(=pp-contracts)\n")
            self.assertIn("rdeps(=pp-contracts)", summary.read_text())

            output.write_text("")
            subprocess.check_output(
                ["python3", str(SCRIPT)],
                cwd=directory,
                text=True,
                env={
                    **os.environ,
                    "EVENT_NAME": "push",
                    "BASE_SHA": base,
                    "HEAD_SHA": head,
                    "GITHUB_OUTPUT": str(output),
                    "GITHUB_STEP_SUMMARY": str(summary),
                    "PP_AFFECTED_CRATES_ROOT": str(Path(directory, "rust/crates")),
                },
            )
            self.assertEqual(output.read_text(), "nextest_filter=all()\n")


if __name__ == "__main__":
    unittest.main()
