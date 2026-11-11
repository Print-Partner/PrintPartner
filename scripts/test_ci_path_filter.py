"""Exercise CI selection with real Git histories and the workflow CLI."""

import importlib.util
import json
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
    def test_ui_and_web_inputs(self):
        for kind, paths in {
            "ui": ["web/apps/ui/src/App.tsx", "web/packages/rust-client/src/client.ts"],
            "web": ["web/apps/web/src/App.tsx", "web/apps/server/src/app.ts",
                    "web/apps/ui/src/App.tsx", "web/packages/rust-client/src/client.ts",
                    "slicer-sidecar/main.py", "Dockerfile", ".dockerignore", "Caddyfile",
                    "docker-compose.hosted.yml", "pp-compose.yml", "rust/Cargo.lock",
                    "web/DEPLOY.md", "web/apps/server/src/data/legal/ATTRIBUTION.md",
                    "fixtures/parity/README.md", "LICENSE", "LICENSE-SUMMARY.md",
                    "ATTRIBUTION.md", "THIRD_PARTY_NOTICES.md",
                    "web/apps/server/src/test-fixtures/kit-workspace/assistant-domain/sources/Example-Probe/workflow.md",
                    "web/apps/server/src/test-fixtures/kit-workspace/assistant-domain/sources/Example-Probe/pitfalls.md"],
        }.items():
            for path in paths + ["web/packages/contracts/src/index.ts", "web/scripts/tool.mjs",
                                 "web/package-lock.json", "web/tsconfig.base.json",
                                 "web/eslint.config.mjs", "scripts/check-github-actions-pinned.sh",
                                 ".github/workflows/web-ci.yml", ".github/actions/example/action.yml",
                                 "manifests/server.json", "fixtures/input.json"]:
                with self.subTest(kind=kind, path=path):
                    self.assertTrue(ci_filter.relevant(path, kind))
            for path in ("docs/development/ci.md", "README.md", "web/apps/ui/README.md"):
                self.assertFalse(ci_filter.relevant(path, kind), (kind, path))

    def test_merge_groups_select_queue_validation_only(self):
        for kind in ("rust", "actions", "native", "windows", "ui", "web"):
            self.assertEqual(ci_filter.select("merge_group", "", "", kind), kind != "web")
            for event in ("push", "schedule", "workflow_call", "workflow_dispatch"):
                self.assertTrue(ci_filter.select(event, "", "", kind))
            self.assertTrue(ci_filter.select("pull_request", "", "", kind))

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
                     ".github/actions/linux-gui-deps/action.yml",
                     "web/apps/server/src/data/legal/LICENSE-SUMMARY.md",
                     "web/apps/server/src/data/legal/ATTRIBUTION.md", "fixtures/parity/README.md"]:
            with self.subTest(path=path):
                self.assertTrue(ci_filter.relevant(path, "rust"))
        for path in ["docs/ci.md", "README.md", "rust/crates/pp-api/README.md", "web/apps/web/README.md",
                     "web/packages/contracts/README.md", "web/scripts/tool.mjs",
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
        for path in ["README.md", "docs/ci.md", ".github/workflows/pr-evidence.yml",
                     "rust/crates/pp-desktop/README.md"]:
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
        for kind in ["rust", "actions", "native", "ui", "web"]:
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
            for kind in ["rust", "actions", "native", "ui", "web"]:
                self.assertEqual(select(base, docs, kind)[0], "relevant=false\n")
            for path, kind in (("web/apps/ui/src/App.tsx", "ui"),
                               ("web/packages/rust-client/src/client.ts", "ui"),
                               ("web/apps/server/src/app.ts", "web"),
                               ("web/DEPLOY.md", "web")):
                git("checkout", "-q", "--detach", base)
                changed = commit(path)
                self.assertEqual(select(base, changed, kind)[0], "relevant=true\n")
                git("rm", path)
                git("commit", "-qm", "delete input")
                self.assertEqual(select(changed, git("rev-parse", "HEAD"), kind)[0],
                                 "relevant=true\n")
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


    def test_windows_tier(self):
        for event in ["push", "merge_group", "schedule", "workflow_dispatch"]:
            self.assertTrue(ci_filter.select(event, "", "", "windows"))
        self.assertTrue(ci_filter.select("pull_request", "", "", "windows"))
        for label in ["ci:full", "ci:windows"]:
            self.assertTrue(ci_filter.select("pull_request", "0" * 40, "0" * 40, "windows", [label]))
        self.assertFalse(ci_filter.WINDOWS_LABELS.intersection(["macos", "ci:macos"]))
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, text=True).strip()

            def commit(path, text):
                file = Path(directory, path)
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(text)
                git("add", "--", path)
                git("commit", "-qm", "proof")
                return git("rev-parse", "HEAD")

            def windows(base, head, labels=()):
                output = Path(directory, "output")
                output.write_text("")
                subprocess.check_output(
                    ["python3", str(SCRIPT), "windows"], cwd=directory, text=True,
                    env={**os.environ, "EVENT_NAME": "pull_request", "BASE_SHA": base,
                         "HEAD_SHA": head, "PR_LABELS": json.dumps(list(labels)),
                         "GITHUB_OUTPUT": str(output),
                         "GITHUB_STEP_SUMMARY": str(Path(directory, "summary"))})
                return output.read_text()

            git("init", "-q")
            git("config", "user.email", "proof@example.com")
            git("config", "user.name", "Proof")
            base = commit("README.md", "base\n")
            cases = [
                ("rust/crates/pp-api/src/lib.rs", "fn a() {}\n", "windows=false\n"),
                ("web/apps/web/src/App.tsx", "export {}\n", "windows=false\n"),
                ("docs/a.md", "text\n", "windows=false\n"),
                ("rust/crates/pp-os/src/new.rs", "fn a() {}\n", "windows=true\n"),
                ("rust/crates/pp-core/src/state.rs", "#[cfg(windows)]\nmod w;\n", "windows=true\n"),
                ("rust/crates/pp-core/src/perm.rs", "use std::os::unix::fs::PermissionsExt;\n", "windows=true\n"),
                ("rust/crates/pp-core/src/unix_only.rs", "#[cfg(unix)]\nfn a() {}\n", "windows=true\n"),
                ("rust/crates/pp-core/src/grouped.rs", "use std::{fs, os::unix::fs::PermissionsExt};\n", "windows=true\n"),
                ("rust/crates/pp-core/src/any.rs", "#[cfg(any(unix, test))]\nfn a() {}\n", "windows=true\n"),
                ("rust/crates/pp-core/src/stream.rs", "use tokio::net::UnixStream;\n", "windows=true\n"),
                ("web/apps/server/src/desktop.ts", "process.platform === 'win32'\n", "windows=true\n"),
                ("web/apps/server/src/pipe.ts", "const p = '\\\\.\\pipe\\x';\n", "windows=true\n"),
                ("web/apps/server/src/plain.ts", "export const a = 1;\n", "windows=false\n"),
                ("web/apps/server/src/sep.ts", "const k = key.replace(/\\\\/g, \"/\");\n", "windows=true\n"),
                ("web/apps/server/src/plat.ts", "if (process.platform !== \"linux\") return;\n", "windows=true\n"),
                ("web/apps/server/src/proc.ts", "readFileSync(\"/proc/sys/kernel/random/boot_id\");\n", "windows=true\n"),
                ("web/apps/server/src/join.ts", "const p = parts.join(path.sep);\n", "windows=true\n"),
                ("web/apps/server/src/up.ts",
                 "import { relative, sep } from \"node:path\";\nif (p.startsWith(`..${sep}`)) {}\n", "windows=true\n"),
                ("web/apps/server/src/all.ts", "const k = key.replaceAll(\"\\\\\", \"/\");\n", "windows=true\n"),
                ("web/apps/server/src/single.ts", "const k = key.split('\\\\');\n", "windows=true\n"),
                ("web/apps/server/src/separate.ts", "const separator = \", \";\n", "windows=false\n"),
                (".gitattributes", "rust/** text=auto eol=lf\n", "windows=true\n"),
                ("web/apps/server/package.json", "{}\n", "windows=true\n"),
                ("web/packages/contracts/package.json", "{}\n", "windows=true\n"),
                ("web/apps/web/package.json", "{}\n", "windows=true\n"),
                ("web/packages/domain/src/paths.ts", "export const k = (p: string) => p.split('\\\\');\n", "windows=true\n"),
                ("web/packages/domain/src/plain.ts", "export const a = 1;\n", "windows=false\n"),
                ("rust/crates/pp-core/src/bang.rs", "fn a() { if cfg!(unix) {} }\n", "windows=true\n"),
                ("rust/crates/pp-core/src/attr.rs", "#[cfg_attr(unix, derive(Debug))]\nstruct A;\n", "windows=true\n"),
                ("rust/crates/pp-core/src/fam.rs", "#[cfg(target_family = \"unix\")]\nfn a() {}\n", "windows=true\n"),
                ("rust/crates/pp-core/Cargo.toml", "[target.'cfg(windows)'.dependencies]\n", "windows=true\n"),
                ("rust/Cargo.lock", "lock\n", "windows=true\n"),
                ("web/package.json", "{}\n", "windows=true\n"),
                ("web/package-lock.json", "{}\n", "windows=true\n"),
                ("web/scripts/atomic-package-build.mjs", "x\n", "windows=true\n"),
                ("rust/scripts/build-desktop-manifest.py", "x\n", "windows=true\n"),
                ("rust/scripts/desktop_workspaces.py", "x\n", "windows=true\n"),
                ("rust/scripts/fetch-historical-schema-commits.sh", "x\n", "windows=true\n"),
                ("rust/crates/pp-desktop/tauri.conf.json", "{}\n", "windows=true\n"),
                ("rust/crates/pp-desktop/icons/icon.ico", "ico\n", "windows=true\n"),
                (".github/workflows/web-ci.yml", "name: x\n", "windows=true\n"),
                ("scripts/ci-path-filter.py", "x\n", "windows=true\n"),
            ]
            for path, text, expected in cases:
                git("checkout", "-q", "--detach", base)
                head = commit(path, text)
                with self.subTest(path=path):
                    self.assertEqual(windows(base, head), expected)
            git("checkout", "-q", "--detach", base)
            with_marker = commit("rust/crates/pp-core/src/w.rs", "#[cfg(windows)]\nmod w;\n")
            git("checkout", "-q", with_marker)
            head = commit("rust/crates/pp-core/src/w.rs", "fn a() {}\n")
            self.assertEqual(windows(with_marker, head), "windows=true\n")
            git("checkout", "-q", "--detach", with_marker)
            head = commit("rust/crates/pp-core/src/w.rs", "fn a() {}\n")
            git("checkout", "-q", "--detach", with_marker)
            moved = commit("rust/crates/pp-core/src/w.rs", "fn b() {}\n")
            self.assertEqual(windows(moved, head), "windows=true\n")
            # A deletion is judged on the base copy: no marker skips Windows, a marker selects it.
            for path, text, expected in [
                ("web/apps/server/src/gone.ts", "export const a = 1;\n", "windows=false\n"),
                ("rust/crates/pp-core/src/gone.rs", "fn a() {}\n", "windows=false\n"),
                ("rust/crates/pp-core/src/gone_w.rs", "#[cfg(windows)]\nmod w;\n", "windows=true\n"),
            ]:
                git("checkout", "-q", "--detach", base)
                added = commit(path, text)
                git("rm", "-q", "--", path)
                git("commit", "-qm", "delete")
                with self.subTest(deleted=path):
                    self.assertEqual(windows(added, git("rev-parse", "HEAD")), expected)
            self.assertTrue(ci_filter.windows_relevant("rust/crates/pp-core/src/none.rs", base, base))
            git("checkout", "-q", "--detach", base)
            head = commit("rust/crates/pp-api/src/lib.rs", "fn a() {}\n")
            self.assertEqual(windows(base, head, ["ci:windows"]), "windows=true\n")
            self.assertEqual(windows(base, head, ["ci:full"]), "windows=true\n")
            self.assertEqual(windows(base, head, ["macos"]), "windows=false\n")


if __name__ == "__main__":
    unittest.main()
