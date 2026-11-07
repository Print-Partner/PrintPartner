"""Exercise CI selection with real Git histories and the workflow CLI."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("ci-path-filter.py").resolve()
spec = importlib.util.spec_from_file_location("ci_filter", SCRIPT)
ci_filter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci_filter)


class CiPathFilterTests(unittest.TestCase):
    def test_rust_inputs(self):
        for path in ["rust/Cargo.lock", "rust/crates/pp-storage/src/lib.rs",
                     "rust-toolchain.toml", "web/package.json", "web/package-lock.json",
                     "web/apps/web/src/App.tsx", "web/packages/contracts/src/a.ts",
                     ".github/workflows/rust-cross-os.yml", "scripts/ci-path-filter.py",
                     "scripts/ci-affected-nextest-filter.py",
                     "scripts/test_ci_affected_nextest_filter.py",
                     "web/tsconfig.base.json", "web/scripts/atomic-package-build.mjs",
                     "web/scripts/storage-fixtures/prepare.mts",
                     "fixtures/parity/jobs-model/jobs-model.json",
                     "fixtures/drivers/moonraker/moonraker-success.json",
                     "fixtures/http/routes-inventory.json", "scripts/ci-rust-runner.py",
                     "scripts/test_ci_rust_runner.py", "scripts/ci-pp-local-sccache.sh",
                     ".github/actions/linux-gui-deps/action.yml"]:
            with self.subTest(path=path):
                self.assertTrue(ci_filter.relevant(path, "rust"))
        for path in ["docs/ci.md", "README.md", "web/scripts/tool.mjs",
                     "web/package-data/file.json", ".github/workflows/web-ci.yml"]:
            with self.subTest(path=path):
                self.assertFalse(ci_filter.relevant(path, "rust"))

    def test_native_inputs(self):
        for path in ["rust/crates/pp-desktop/src/main.rs", "web/apps/web/src/App.tsx",
                     ".github/workflows/desktop-native-macos.yml",
                     "scripts/native_image_diagnostics.py",
                     "scripts/test_native_image_cleanup.py", "scripts/ci-path-filter.py",
                     "scripts/verify-rust-gateway/test_launch.py",
                     "scripts/verify-rust-gateway/verify.py",
                     "scripts/verify-rust-gateway/future_helper.py"]:
            self.assertTrue(ci_filter.relevant(path, "native"))
        for path in ["README.md", "docs/ci.md", ".github/workflows/pr-evidence.yml"]:
            self.assertFalse(ci_filter.relevant(path, "native"))

    def test_linux_setup_selects_rust(self):
        self.assertTrue(ci_filter.relevant("scripts/setup-ci-linux.sh", "rust"))

    def test_actions_inputs(self):
        for path in [".github/workflows/a.yml", ".github/actions/a/action.yml",
                     "scripts/test_ci_path_filter.py", ".github/zizmor.yml",
                     ".github/actionlint.yaml", ".github/actionlint.yml"]:
            self.assertTrue(ci_filter.relevant(path, "actions"))
        for path in ["docs/ci.md", "rust/Cargo.toml", "web/package.json"]:
            self.assertFalse(ci_filter.relevant(path, "actions"))

    def test_unknown_base_and_non_pr_events_run(self):
        for kind in ["rust", "actions", "native"]:
            self.assertTrue(ci_filter.select("pull_request", "", "", kind))
            self.assertTrue(ci_filter.select("pull_request", "0" * 40, "1" * 40, kind))
            self.assertTrue(ci_filter.select("push", "", "", kind))
            self.assertTrue(ci_filter.select("workflow_dispatch", "", "", kind))

    def test_real_docs_only_diff_and_deletions(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, text=True).strip()

            def commit(path):
                file = Path(directory, path)
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text("proof\n")
                git("add", "--", path)
                git("commit", "-qm", "proof")
                return git("rev-parse", "HEAD")

            def select(base, head, kind):
                output = Path(directory, "output")
                output.write_text("")
                result = subprocess.check_output(
                    ["python3", str(SCRIPT), kind], cwd=directory, text=True,
                    env={**os.environ, "EVENT_NAME": "pull_request", "BASE_SHA": base,
                         "HEAD_SHA": head, "GITHUB_OUTPUT": str(output),
                         "GITHUB_STEP_SUMMARY": str(Path(directory, "summary"))})
                return output.read_text(), result

            git("init", "-q")
            git("config", "user.email", "proof@example.com")
            git("config", "user.name", "Proof")
            base = commit("README.md")
            docs = commit("docs/space and\nnewline.md")
            for kind in ["rust", "actions", "native"]:
                self.assertEqual(select(base, docs, kind)[0], "relevant=false\n")
            git("checkout", "-q", "--detach", base)
            helper = commit("scripts/verify-rust-gateway/verify.py")
            self.assertEqual(select(base, helper, "native")[0], "relevant=true\n")
            self.assertEqual(select(base, helper, "rust")[0], "relevant=false\n")
            git("checkout", "-q", "--detach", base)
            diagnostics = commit("scripts/native_image_diagnostics.py")
            self.assertEqual(select(base, diagnostics, "native")[0], "relevant=true\n")
            self.assertEqual(select(base, diagnostics, "rust")[0], "relevant=false\n")
            git("checkout", "-q", "--detach", base)
            fixture = commit("fixtures/parity/jobs-model/jobs-model.json")
            self.assertEqual(select(base, fixture, "rust")[0], "relevant=true\n")
            self.assertEqual(select(base, fixture, "actions")[0], "relevant=false\n")
            git("checkout", "-q", "--detach", base)
            advanced_base = commit("rust/crates/new/src/lib.rs")
            # Base-only code must not turn a docs-only PR on.
            self.assertEqual(select(advanced_base, docs, "rust")[0], "relevant=false\n")
            git("rm", "rust/crates/new/src/lib.rs")
            git("commit", "-qm", "delete source")
            self.assertEqual(select(advanced_base, git("rev-parse", "HEAD"), "rust")[0], "relevant=true\n")


if __name__ == "__main__":
    unittest.main()
