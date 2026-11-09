import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("off_filter", ROOT / "scripts/ci-desktop-rebuild-off-filter.py")
off = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(off)


def crate(root, name, manifest_tail=""):
    (root / name / "src").mkdir(parents=True)
    (root / name / "src/lib.rs").write_text("")
    (root / name / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n{manifest_tail}')


# The changes job runs this file with Python only, so the tests that run cargo need an opt-in. The
# nextest (desktop_rebuild off) step sets it, after the toolchain and crates are in place.
CARGO_TESTS = os.environ.get("PP_FILTER_CARGO_TESTS") == "1" and shutil.which("cargo") is not None


@unittest.skipUnless(CARGO_TESTS, "set PP_FILTER_CARGO_TESTS=1 with cargo on PATH to run cargo metadata")
class FixtureWorkspaceTests(unittest.TestCase):
    def test_only_packages_that_cannot_reach_the_feature_skip_the_off_pass(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "Cargo.toml").write_text(
                '[workspace]\nresolver = "2"\nmembers = ["feature", "toggled", "dependent", "dev-dependent", '
                '"unrelated", "steady"]\n')
            # feature declares desktop_rebuild, which turns on toggled/x; toggled never declares
            # it. dependent and dev-dependent reach feature; unrelated and steady never do, and
            # steady's default feature is the same in both passes.
            crate(root, "feature", '\n[dependencies]\ntoggled = { path = "../toggled" }\n'
                                   '\n[features]\ndesktop_rebuild = ["toggled/x"]\n')
            crate(root, "toggled", "\n[features]\nx = []\n")
            crate(root, "dependent", '\n[dependencies]\nfeature = { path = "../feature" }\n')
            crate(root, "dev-dependent", '\n[dev-dependencies]\nfeature = { path = "../feature" }\n')
            crate(root, "unrelated")
            crate(root, "steady", '\n[features]\ndefault = ["y"]\ny = []\n')
            subprocess.run(["cargo", "generate-lockfile", "--offline", "--manifest-path", str(root / "Cargo.toml")],
                           check=True, capture_output=True)
            off_metadata = off.cargo_metadata(root / "Cargo.toml")
            on_metadata = off.cargo_metadata(root / "Cargo.toml", "--features", "desktop_rebuild")
            self.assertEqual(off.skipped_packages(off_metadata, on_metadata), ["steady", "unrelated"])
            # Declarations alone miss toggled; only the feature-set comparison catches it.
            self.assertIn("toggled", off.skipped_packages(off_metadata))
            self.assertEqual(off.main(root / "Cargo.toml", env={})[0], "not (package(=steady) + package(=unrelated))")

    def test_the_current_workspace(self):
        """A package that moves between the passes shows up here for review."""
        manifest = ROOT / "rust/Cargo.toml"
        on = off.cargo_metadata(manifest, "--features", "desktop_rebuild")
        self.assertEqual(off.skipped_packages(off.cargo_metadata(manifest), on),
                         ["pp-net", "pp-os", "pp-perf", "pp-testkit"])


def metadata(*packages, deps=None, external=None, features=None):
    """A minimal cargo metadata document: packages as (name, declares the feature)."""
    deps, features = deps or {}, features or {}
    nodes = [{"id": name, "features": sorted(features.get(name, ())), "deps": [{"pkg": dep} for dep in deps.get(name, ())]}
             for name, _ in packages]
    listed = [{"id": name, "name": name, "features": {"desktop_rebuild": []} if declares else {}}
              for name, declares in packages]
    for package_id, (name, features) in (external or {}).items():
        listed.append({"id": package_id, "name": name, "features": {}})
        nodes.append({"id": package_id, "features": sorted(features), "deps": []})
    return {"workspace_members": [name for name, _ in packages], "packages": listed, "resolve": {"nodes": nodes}}


class FailSafeTests(unittest.TestCase):
    def run_main(self, results, env=None):
        calls = iter(results)

        def fake(manifest, *features):
            result = next(calls)
            if isinstance(result, Exception):
                raise result
            return result

        return off.main(Path("Cargo.toml"), env=env or {}, metadata=fake)

    def test_a_metadata_error_falls_back_to_the_full_off_pass(self):
        workspace = metadata(("a", True), ("b", False))
        for results in ([off.Unsafe("cargo metadata exited 101")], [workspace, off.Unsafe("no JSON")],
                        [ValueError("anything else")], [{"workspace_members": []}, {}]):
            with self.subTest(results=results):
                filterset, message = self.run_main(results)
                self.assertEqual(filterset, "all()")
                self.assertIn("::warning::", message)

    def test_a_failed_or_garbled_cargo_metadata_raises_unsafe(self):
        for completed in (subprocess.CompletedProcess([], 101, stdout="", stderr="error: lock file needs updating"),
                          subprocess.CompletedProcess([], 0, stdout=None, stderr=""),
                          subprocess.CompletedProcess([], 0, stdout="not json", stderr="")):
            with self.subTest(completed=completed):
                original, off.subprocess.run = off.subprocess.run, lambda command, **options: completed
                try:
                    with self.assertRaises(off.Unsafe):
                        off.cargo_metadata(Path("Cargo.toml"))
                finally:
                    off.subprocess.run = original

    def test_non_ascii_metadata_is_read_as_utf_8(self):
        # rust (windows) on #1632 decoded it with cp1252 and crashed on byte 0x81.
        calls = []
        document = metadata(("café-あ", True), ("dünn", False))

        def fake_run(command, **options):
            calls.append(options)
            return subprocess.CompletedProcess(command, 0, stdout=json.dumps(document, ensure_ascii=False), stderr="")

        original, off.subprocess.run = off.subprocess.run, fake_run
        self.addCleanup(setattr, off.subprocess, "run", original)
        self.assertEqual(off.skipped_packages(off.cargo_metadata(Path("Cargo.toml"))), ["dünn"])
        self.assertEqual(calls[0]["encoding"], "utf-8")

    def test_no_declaring_package_or_an_unreviewed_feature_keeps_the_full_pass(self):
        self.assertEqual(self.run_main([metadata(("a", False)), metadata(("a", False))])[0], "all()")
        before = metadata(("a", True), ("b", False), external={"serde_json 1": ("serde_json", {"std"})})
        after = metadata(("a", True), ("b", False), external={"serde_json 1": ("serde_json", {"std", "arbitrary_precision"})})
        filterset, message = self.run_main([before, after])
        self.assertEqual(filterset, "all()")
        self.assertIn("serde_json/arbitrary_precision", message)
        reviewed = metadata(("a", True), ("b", False), external={"axum 1": ("axum", {"json", "multipart"})})
        self.assertEqual(self.run_main([metadata(("a", True), ("b", False), external={"axum 1": ("axum", {"json"})}),
                                        reviewed])[0], "not (package(=b))")

    def test_a_run_without_the_on_pass_keeps_the_full_off_pass(self):
        self.assertEqual(self.run_main([], env={"PP_NEXTEST_OFF_ALL": "true"})[0], "all()")

    def test_the_closure_follows_every_dependency_kind_transitively(self):
        workspace = metadata(("feature", True), ("middle", False), ("top", False), ("alone", False),
                             deps={"middle": ["feature"], "top": ["middle"]})
        self.assertEqual(off.skipped_packages(workspace), ["alone"])
        self.assertEqual(off.off_filter(workspace, workspace), "not (package(=alone))")
        everyone = metadata(("feature", True), ("top", False), deps={"top": ["feature"]})
        self.assertEqual(off.off_filter(everyone, everyone), "all()")


class MemberFeatureTests(unittest.TestCase):
    def test_a_member_enabled_only_through_another_members_feature_keeps_its_off_pass(self):
        packages = (("core", True), ("net", False), ("app", False), ("alone", False))
        before = metadata(*packages, deps={"core": ["net"], "app": ["net"]}, features={"net": ["default"]})
        after = metadata(*packages, deps={"core": ["net"], "app": ["net"]},
                         features={"core": ["desktop_rebuild"], "net": ["default", "x"]})
        # net never declares the feature, but core's desktop_rebuild turns on net/x, so net and
        # app, which depends on it, keep the off pass.
        self.assertEqual(off.skipped_packages(before, after), ["alone"])
        self.assertEqual(off.off_filter(before, after), "not (package(=alone))")

    def test_a_member_whose_features_match_in_both_passes_still_skips(self):
        packages = (("core", True), ("net", False))
        before = metadata(*packages, features={"net": ["default", "x"]})
        after = metadata(*packages, features={"core": ["desktop_rebuild"], "net": ["default", "x"]})
        self.assertEqual(off.skipped_packages(before, after), ["net"])

    def test_an_optional_member_edge_that_only_the_on_pass_has_counts(self):
        packages = (("core", True), ("app", False), ("alone", False))
        before = metadata(*packages)
        after = metadata(*packages, deps={"app": ["core"]})
        self.assertEqual(off.skipped_packages(before, after), ["alone"])

    def test_different_members_in_the_two_passes_keep_the_full_off_pass(self):
        before = metadata(("core", True), ("net", False))
        after = metadata(("core", True), ("net", False), ("extra", False))
        with self.assertRaises(off.Unsafe):
            off.skipped_packages(before, after)
        self.assertEqual(FailSafeTests.run_main(self, [before, after])[0], "all()")


class WorkflowTests(unittest.TestCase):
    def test_the_workflow_narrows_only_the_off_pass(self):
        # Text checks: the changes job runs this file without PyYAML.
        text = (ROOT / ".github/workflows/rust-cross-os.yml").read_text()
        off_step = text.split("- name: nextest (desktop_rebuild off)\n", 1)[1].split("- name:", 1)[0]
        self.assertIn("off_filter=$(python3 -B scripts/ci-desktop-rebuild-off-filter.py)\n", off_step)
        self.assertIn('--no-tests=warn -E "($NEXTEST_FILTER) & ($off_filter)"\n', off_step)
        for name in ("nextest (desktop_rebuild on)", "Archive nextest (desktop_rebuild on)"):
            step = text.split(f"- name: {name}\n", 1)[1].split("- name:", 1)[0]
            self.assertIn('-E "$NEXTEST_FILTER"', step)
            self.assertNotIn("off_filter", step)
        # macOS PRs skip the on pass, so they keep the whole off pass; no other job sets it.
        self.assertEqual(text.count("PP_NEXTEST_OFF_ALL:"), 1)
        # The cargo-backed tests run here, where the toolchain and crates already exist; never in changes.
        self.assertIn("PP_FILTER_CARGO_TESTS=1 python3 -B -m unittest discover -s scripts -p "
                      "test_ci_desktop_rebuild_off_filter.py\n", off_step)
        self.assertEqual(text.count("PP_FILTER_CARGO_TESTS"), 1)
        macos = text.split("\n  rust-macos:\n", 1)[1].split("\n  rust-windows:\n", 1)[0]
        self.assertIn("      PP_NEXTEST_OFF_ALL: ${{ github.event_name == 'pull_request' }}\n", macos)


if __name__ == "__main__":
    unittest.main()
