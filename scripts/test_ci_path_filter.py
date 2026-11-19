"""Exercise CI selection with real Git histories and the workflow CLI."""

import importlib.util
import json
import os
from pathlib import Path
import re
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
                    "README.md", "docs/README.md", "docs/INSTALL.md", "docs/index.html",
                    "docs/scripts/capture-screenshots.mjs", "docs/scripts/nested/README.md",
                    "docs/design-system.md", "docs/product/ui-tokens.md",
                    "web/apps/ui/src/content.md", "web/apps/web/src/content.md",
                    "web/apps/future/src/nested/content.md",
                    "docs/records/navigation/count.py", "root-helper.py",
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
            for path in ("docs/development/ci.md", "web/apps/ui/README.md",
                         "docs/design-system-extra.md", "docs/product/other-tokens.md",
                         "web/apps/ui/src-extra/content.md"):
                self.assertFalse(ci_filter.relevant(path, kind), (kind, path))

    def test_workaround_extensions_match_ratchet(self):
        source = SCRIPT.with_name("workaround-ratchet.mjs").read_text()

        def declaration(name):
            match = re.search(rf"const {name} = new Set\(\[(.*?)\]\);", source, re.S)
            self.assertIsNotNone(match, f"cannot read ratchet extension declaration: {name}")
            return match.group(1)

        javascript = json.loads("[" + declaration("JAVASCRIPT_EXTENSIONS") + "]")
        other = declaration("SOURCE_EXTENSIONS").replace("...JAVASCRIPT_EXTENSIONS,", "")
        extensions = set(javascript + json.loads("[" + other.rstrip().removesuffix(",") + "]"))
        self.assertEqual(set(ci_filter.WORKAROUND_EXTENSIONS), extensions)
        for extension in extensions:
            self.assertTrue(ci_filter.relevant(f"outside-inputs/file{extension}", "web"), extension)
        for path in ("desktop/e2e/x.ts", ".devcontainer/x.sh", "docs/examples/x.yaml",
                     "docs/kit-catalog.yaml", ".github/FUNDING.yml",
                     ".github/ISSUE_TEMPLATE/bug.yml"):
            self.assertTrue(ci_filter.relevant(path, "web"), path)
        for extension in (".json", ".md", ".txt", ".tsx-extra"):
            self.assertFalse(ci_filter.relevant(f"outside-inputs/file{extension}", "web"), extension)

    def test_web_test_inputs_do_not_select_ui(self):
        for path in ("README.md", "docs/README.md", "docs/INSTALL.md", "docs/index.html",
                     "docs/scripts/capture-screenshots.mjs", "docs/design-system.md",
                     "docs/product/ui-tokens.md", "docs/records/navigation/count.py"):
            self.assertFalse(ci_filter.relevant(path, "ui"), path)

    def test_merge_groups_select_queue_validation_only(self):
        for kind in ("rust", "actions", "windows", "macos", "ui", "web"):
            self.assertEqual(ci_filter.select("merge_group", "", "", kind), kind != "web")
            for event in ("push", "schedule", "workflow_call", "workflow_dispatch"):
                self.assertTrue(ci_filter.select(event, "", "", kind))
            self.assertEqual(ci_filter.select("pull_request", "", "", kind),
                             kind not in {"windows", "macos"})

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

    def test_windows_tests_inputs(self):
        for path in [
                # The three paths the GRE-1811 review reproduced as skipped.
                "rust/scripts/precompress-spa.py", "web/package-lock.json",
                "web/apps/server/package.json",
                # Tested crates, all files but Markdown.
                "rust/crates/pp-compat/src/ticket_t_2.rs", "rust/crates/pp-gateway/src/slice0_mount.rs",
                "rust/crates/pp-server/src/health.rs", "rust/crates/pp-gateway/tests/a.rs",
                # Libraries below them, derived from Cargo.toml path dependencies.
                "rust/crates/pp-os/src/windows.rs", "rust/crates/pp-net/src/http1.rs",
                "rust/crates/pp-core/src/lib.rs", "rust/crates/pp-storage/src/lib.rs",
                "rust/crates/pp-api-types/build.rs", "rust/crates/pp-api/Cargo.toml",
                # Outside files the compiled sources include, derived by the source scan.
                "fixtures/drivers/moonraker/moonraker-success.json",
                "rust/crates/pp-core/tests/support/release.rs",
                "rust/rules/no-writer-family-literal.yml",
                "web/apps/server/src/data/legal/LICENSE-SUMMARY.md",
                # Toolchain, workspace, runner, workflow and selector.
                "rust/Cargo.lock", "rust/Cargo.toml", "rust/.config/nextest.toml", "rust-toolchain.toml",
                ".github/workflows/rust-cross-os.yml", "scripts/ci-path-filter.py",
                "scripts/test_ci_path_filter.py",
                # Manifest script and its import.
                "rust/scripts/build-desktop-manifest.py", "rust/scripts/desktop_workspaces.py",
                # npm ci and the bundle build.
                "web/package.json", "web/tsconfig.base.json", "web/scripts/atomic-package-build.mjs",
                "web/apps/ui/package.json", "web/packages/rust-client/package.json",
                "web/apps/web/src/App.tsx", "web/apps/server/src/app.ts",
                "web/packages/contracts/src/a.ts", "web/packages/domain/src/a.ts"]:
            self.assertTrue(ci_filter.relevant(path, "windows-tests"), path)
        for path in ["docs/development/ci.md", "README.md", "docs/node-import.md",
                     "rust/crates/pp-compat/README.md", "web/apps/web/README.md",
                     "web/apps/ui/src/App.tsx", "web/packages/rust-client/src/client.ts",
                     "rust/crates/pp-desktop/src/main.rs", "rust/crates/pp-storage/tests/a.rs",
                     "rust/crates/pp-osx/src/lib.rs", "fixtures/parity/auth-login/node.json",
                     "rust/scripts/stage-desktop.py", ".github/workflows/web-ci.yml"]:
            self.assertFalse(ci_filter.relevant(path, "windows-tests"), path)
        self.assertFalse(ci_filter.select("merge_group", "", "", "windows-tests"))
        self.assertTrue(ci_filter.select("workflow_dispatch", "", "", "windows-tests"))

    def test_windows_test_inputs_follow_what_nextest_compiles(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()

            def write(path, text=""):
                Path(root, path).parent.mkdir(parents=True, exist_ok=True)
                Path(root, path).write_text(text)

            write("rust/crates/top/Cargo.toml", '[dependencies]\nlib = { path = "../lib" }\n'
                  '[dev-dependencies]\nhelper = { path = "../helper" }\n'
                  "[target.'cfg(windows)'.dependencies]\nwin = { path = \"../win\" }\n")
            write("rust/crates/top/tests/a.rs", '#[path = "../../lib/tests/support.rs"] mod s;\n')
            write("rust/crates/lib/Cargo.toml", '[dev-dependencies]\nunused = { path = "../unused" }\n')
            write("rust/crates/lib/src/lib.rs",
                  'const A: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/a.json"));\n')
            write("rust/crates/lib/src/tests.rs", 'const B: &str = include_str!("../../../../fixtures/b.json");\n')
            write("rust/crates/lib/src/x/tests/c.rs", 'const C: &str = include_str!("../../../../../fixtures/c.json");\n')
            for crate in ("helper", "win", "unused"):
                write(f"rust/crates/{crate}/Cargo.toml")
            prefixes, files = ci_filter.windows_test_inputs(root, ("top",))
            self.assertEqual(set(prefixes), {
                "rust/crates/top/", "rust/crates/lib/src/", "rust/crates/lib/build.rs",
                "rust/crates/lib/Cargo.toml", "rust/crates/helper/src/", "rust/crates/helper/build.rs",
                "rust/crates/helper/Cargo.toml", "rust/crates/win/src/", "rust/crates/win/build.rs",
                "rust/crates/win/Cargo.toml"})
            # The library's test modules and its dev-dependencies are not compiled.
            self.assertEqual(files, {"fixtures/a.json", "rust/crates/lib/tests/support.rs"})

    def test_windows_tests_writes_its_own_output_from_a_real_diff(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, text=True).strip()

            git("init", "-q")
            git("config", "user.email", "proof@example.com")
            git("config", "user.name", "Proof")
            heads = []
            for path in ("README.md", "docs/development/ci.md", "web/apps/ui/src/App.tsx",
                         "rust/scripts/precompress-spa.py", "web/package-lock.json",
                         "web/apps/server/package.json", "rust/crates/pp-server/src/health.rs"):
                Path(directory, path).parent.mkdir(parents=True, exist_ok=True)
                Path(directory, path).write_text("proof\n")
                git("add", "--", path)
                git("commit", "-qm", path)
                heads.append(git("rev-parse", "HEAD"))
            expected = ["false", "false", "true", "true", "true", "true"]
            for base, head, want in zip(heads, heads[1:], expected):
                output = Path(directory, "output")
                output.write_text("")
                subprocess.run(["python3", str(SCRIPT), "windows-tests"], cwd=directory, check=True,
                               capture_output=True,
                               env={**os.environ, "EVENT_NAME": "pull_request", "BASE_SHA": base,
                                    "HEAD_SHA": head, "GITHUB_OUTPUT": str(output),
                                    "GITHUB_STEP_SUMMARY": str(Path(directory, "summary"))})
                self.assertEqual(output.read_text(), f"windows_tests={want}\n", head)

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
        for kind in ["rust", "actions", "ui", "web"]:
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
            for kind in ["rust", "actions", "ui", "web"]:
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
            self.assertEqual(select(base, helper, "rust")[0], "relevant=false\n")
            git("checkout", "-q", "--detach", base)
            diagnostics = commit("scripts/native_image_diagnostics.py")
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


    def test_cross_os_cli_is_label_only_on_prs(self):
        for kind in ("windows", "macos"):
            for event in ("pull_request", "merge_group", "push", "schedule", "workflow_dispatch"):
                for labels in ([], ["macos", "ci:windows", "ci:full"], ["ci:cross-os"]):
                    with self.subTest(kind=kind, event=event, labels=labels), tempfile.TemporaryDirectory() as directory:
                        output, summary = Path(directory, "output"), Path(directory, "summary")
                        result = subprocess.run(
                            ["python3", "-B", str(SCRIPT), kind], check=True, capture_output=True, text=True,
                            env={**os.environ, "EVENT_NAME": event, "PR_LABELS": json.dumps(labels),
                                 "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary)})
                        expected = str(event != "pull_request" or "ci:cross-os" in labels).lower()
                        self.assertEqual(output.read_text(), f"{kind}={expected}\n")
                        self.assertEqual(result.stdout, f"{kind} inputs changed: {expected}\n")
                        self.assertEqual(summary.read_text(), result.stdout)

    def test_cross_os_does_not_read_paths_even_when_git_is_unavailable(self):
        for kind in ("windows", "macos"):
            self.assertFalse(ci_filter.select("pull_request", None, None, kind))
            self.assertTrue(ci_filter.select("pull_request", None, None, kind, ["ci:cross-os"]))
