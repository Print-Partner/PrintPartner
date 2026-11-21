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
        for kind in ("rust", "rust-image", "actions", "windows", "macos", "ui", "web"):
            self.assertEqual(ci_filter.select("merge_group", "", "", kind), kind != "web")
            for event in ("push", "schedule", "workflow_call", "workflow_dispatch"):
                self.assertTrue(ci_filter.select(event, "", "", kind))
            self.assertEqual(ci_filter.select("pull_request", "", "", kind),
                             kind not in {"windows", "macos"})

    def test_rust_image_and_governance_script_filters(self):
        for path in ("Dockerfile.rust", ".dockerignore", "rust/crates/pp-server/src/main.rs",
                     "web/apps/ui/src/App.tsx", "web/packages/rust-client/src/client.ts",
                     "web/apps/web/src/App.tsx", "web/apps/web/vite.config.ts",
                     "web/apps/web/package.json", "web/apps/server/src/app.ts",
                     "web/apps/server/src/config.ts", "web/apps/server/package.json",
                     "web/apps/server/tsconfig.build.json", "web/DEPLOY.md",
                     "fixtures/parity/jobs-model/jobs-model.json",
                     "scripts/docker-rust-smoke.py", "scripts/unknown-build-helper.py",
                     ".github/workflows/rust-image.yml", "web/package-lock.json",
                     "rust-toolchain.toml", "LICENSE", "LICENSE-SUMMARY.md",
                     "ATTRIBUTION.md", "THIRD_PARTY_NOTICES.md", "justfile",
                     ".gitattributes", "new-top-level/file.txt", "docs.md/x.rs"):
            with self.subTest(kind="rust-image", selected=True, path=path):
                self.assertTrue(ci_filter.relevant(path, "rust-image"), path)
        for path in ("docs/development/ci.md", "docs/index.html", "README.md", "AGENTS.md",
                     "scripts/merge_class.py", "scripts/test_merge_class.py",
                     "scripts/agent-guard.py", "scripts/test_agent_guard.py",
                     "scripts/post_pp_score_status.py", "scripts/test_post_pp_score_status.py",
                     "scripts/linear/pr_status_sync.py", "scripts/evidence/publisher.py",
                     "web/apps/ui/src/farm/AnswerClearsNotice.test.tsx",
                     "web/apps/ui/src/App.spec.ts", "web/apps/ui/test/browser/all.mjs",
                     "web/test/daily-loop/daily-loop.spec.ts",
                     ".github/workflows/actionlint.yml", ".github/workflows/web-ci.yml",
                     ".github/CODEOWNERS", ".github/zizmor.yml"):
            with self.subTest(kind="rust-image", selected=False, path=path):
                self.assertFalse(ci_filter.relevant(path, "rust-image"), path)
        for path in ("scripts/merge_class.py", "scripts/test_merge_class.py",
                     "scripts/agent-guard.py", "scripts/test_agent_guard.py",
                     "scripts/post_pp_score_status.py", "scripts/test_post_pp_score_status.py",
                     "scripts/linear/pr_status_sync.py", "scripts/evidence/publisher.py"):
            self.assertFalse(ci_filter.relevant(path, "windows-tests"), path)
            self.assertFalse(ci_filter.relevant(path, "ui"), path)
        for path in ("web/apps/ui/src/farm/AnswerClearsNotice.test.tsx",
                     "web/apps/ui/src/App.spec.ts", "web/apps/ui/src/App.spec.tsx",
                     "web/apps/ui/test/browser/all.mjs",
                     "web/test/daily-loop/daily-loop.spec.ts"):
            with self.subTest(web_test_only=path):
                self.assertFalse(ci_filter.relevant(path, "rust"), path)
                self.assertFalse(ci_filter.relevant(path, "rust-image"), path)
                self.assertFalse(ci_filter.relevant(path, "windows-tests"), path)
                if path.startswith("web/apps/ui/"):
                    self.assertTrue(ci_filter.relevant(path, "ui"), path)
        for path in ("web/apps/ui/src/App.tsx", "web/apps/ui/src/testSetup.ts",
                     "web/apps/ui/src/foo/testHelper.ts"):
            with self.subTest(non_test_ui=path):
                self.assertTrue(ci_filter.relevant(path, "rust"), path)
                self.assertTrue(ci_filter.relevant(path, "rust-image"), path)
                self.assertTrue(ci_filter.relevant(path, "windows-tests"), path)
                self.assertTrue(ci_filter.relevant(path, "ui"), path)

    def test_rust_image_single_path_diffs_and_empty_diff_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, text=True).strip()

            def select(base, head):
                output = Path(directory, "output")
                output.write_text("")
                subprocess.run(["python3", str(SCRIPT), "rust-image"], cwd=directory, check=True,
                               capture_output=True,
                               env={**os.environ, "EVENT_NAME": "pull_request", "BASE_SHA": base,
                                    "HEAD_SHA": head, "GITHUB_OUTPUT": str(output),
                                    "GITHUB_STEP_SUMMARY": str(Path(directory, "summary"))})
                return output.read_text()

            git("init", "-q")
            git("config", "user.email", "proof@example.com")
            git("config", "user.name", "Proof")
            git("commit", "-q", "--allow-empty", "-m", "base")
            base = git("rev-parse", "HEAD")
            for paths, expected in (
                    (["docs/development/ci.md"], "false"),
                    (["README.md"], "false"),
                    (["scripts/merge_class.py"], "false"),
                    (["scripts/linear/pr_status_sync.py"], "false"),
                    ([".github/workflows/web-ci.yml"], "false"),
                    (["docs/x.md", "README.md", "scripts/agent-guard.py",
                      ".github/workflows/actionlint.yml"], "false"),
                    (["web/apps/web/src/App.tsx"], "true"),
                    (["web/apps/web/vite.config.ts"], "true"),
                    (["web/apps/server/src/app.ts"], "true"),
                    (["web/apps/server/src/config.ts"], "true"),
                    (["new-top-level/file.txt"], "true"),
                    (["LICENSE-SUMMARY.md"], "true"),
                    ([".github/workflows/rust-image.yml"], "true"),
                    (["docs/x.md", "web/apps/server/src/config.ts"], "true")):
                with self.subTest(paths=paths):
                    git("checkout", "-q", "--detach", base)
                    for path in paths:
                        Path(directory, path).parent.mkdir(parents=True, exist_ok=True)
                        Path(directory, path).write_text("proof\n")
                        git("add", "--", path)
                    git("commit", "-qm", "change")
                    self.assertEqual(select(base, git("rev-parse", "HEAD")), f"relevant={expected}\n")
            # An empty diff fails closed and runs the image jobs.
            self.assertEqual(select(base, base), "relevant=true\n")

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

    def test_windows_tests_runs_unless_every_path_is_skipped(self):
        for path in [
                # Round 1: the three inputs the first selector missed.
                "rust/scripts/precompress-spa.py", "web/package-lock.json", "web/apps/server/package.json",
                # Round 2: nested Markdown, workspace, cargo, toolchain, npm and git files.
                "web/apps/web/AGENTS.md", "web/packages/contracts/README.md", "web/apps/web/public/new.md",
                "rust/crates/pp-desktop/Cargo.toml", "web/.npmrc", "rust/.cargo/config.toml",
                "rust/rust-toolchain", "fixtures/parity/yaml-flow-properties/node.json",
                # Round 3: the skipped workspaces, now covered whole.
                "web/apps/ui/dist", "web/apps/ui/dist/index.html", "web/apps/ui/.gitattributes",
                "web/packages/rust-client/.gitattributes", "web/apps/ui/src/App.tsx",
                "web/packages/rust-client/src/client.ts",
                # Round 3: root files and scripts the changes job runs.
                "npm-shrinkwrap.json", "scripts/test_alert_macos_main.py", "scripts/alert-macos-main.py",
                "scripts/workaround-ratchet.mjs", "scripts/test_ci_rust_runner.py",
                "scripts/ci-rust-runner.py",
                # Any other root file, a root dotfile, and a new top-level directory.
                "justfile", "Cargo.lock", "rust-toolchain.toml", ".gitattributes", ".editorconfig",
                "new-top-level/file.txt", "docs.md/x.rs",
                # The job's own workflow and the zizmor config.
                ".github/workflows/rust-cross-os.yml", ".github/zizmor.yml"]:
            self.assertTrue(ci_filter.relevant(path, "windows-tests"), path)
        for path in ["docs/development/ci.md", "docs/index.html", "README.md", "AGENTS.md",
                     ".github/workflows/web-ci.yml", ".github/CODEOWNERS",
                     ".github/actions/linux-gui-deps/action.yml"]:
            self.assertFalse(ci_filter.relevant(path, "windows-tests"), path)

    def test_windows_tests_fails_towards_running(self):
        self.assertTrue(ci_filter.select("workflow_dispatch", "", "", "windows-tests"))
        self.assertTrue(ci_filter.select("pull_request", "", "", "windows-tests"))
        self.assertTrue(ci_filter.select("pull_request", "not-a-sha", "1" * 40, "windows-tests"))
        self.assertTrue(ci_filter.select("pull_request", "0" * 40, "1" * 40, "windows-tests"))
        self.assertFalse(ci_filter.select("merge_group", "", "", "windows-tests"))

    def test_windows_tests_writes_its_own_output_from_a_real_diff(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, text=True).strip()

            def select(base, head):
                output = Path(directory, "output")
                output.write_text("")
                subprocess.run(["python3", str(SCRIPT), "windows-tests"], cwd=directory, check=True,
                               capture_output=True,
                               env={**os.environ, "EVENT_NAME": "pull_request", "BASE_SHA": base,
                                    "HEAD_SHA": head, "GITHUB_OUTPUT": str(output),
                                    "GITHUB_STEP_SUMMARY": str(Path(directory, "summary"))})
                return output.read_text()

            git("init", "-q")
            git("config", "user.email", "proof@example.com")
            git("config", "user.name", "Proof")
            git("commit", "-q", "--allow-empty", "-m", "base")
            base = git("rev-parse", "HEAD")
            for paths, expected in (
                    (["docs/development/ci.md"], "false"),
                    (["README.md"], "false"),
                    ([".github/workflows/web-ci.yml"], "false"),
                    (["docs/x.md", "README.md", ".github/workflows/web-ci.yml"], "false"),
                    (["docs/x.md", "rust/y.rs"], "true"),
                    (["web/apps/ui/dist"], "true"),
                    (["web/apps/ui/src/App.tsx"], "true"),
                    (["npm-shrinkwrap.json"], "true"),
                    ([".nvmrc"], "true"),
                    (["new-top-level/file.txt"], "true"),
                    (["web/apps/web/AGENTS.md"], "true"),
                    ([".github/workflows/rust-cross-os.yml"], "true")):
                with self.subTest(paths=paths):
                    git("checkout", "-q", "--detach", base)
                    for path in paths:
                        Path(directory, path).parent.mkdir(parents=True, exist_ok=True)
                        Path(directory, path).write_text("proof\n")
                        git("add", "--", path)
                    git("commit", "-qm", "change")
                    self.assertEqual(select(base, git("rev-parse", "HEAD")), f"windows_tests={expected}\n")
            # A diff with no paths, as from a head equal to its base, runs the job.
            self.assertEqual(select(base, base), "windows_tests=true\n")

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
        for kind in ["rust", "rust-image", "actions", "ui", "web"]:
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
