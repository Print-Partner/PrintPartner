"""Negative controls for the Clippy limits and Cargo JSON reader."""

import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("check-clippy-limits.py")
SPEC = importlib.util.spec_from_file_location("clippy_limits", SCRIPT)
limits = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(limits)


def baseline():
    return {mode: {lint: 0 for lint in limits.LINTS} for mode in limits.MODES}


def diagnostic(lint=limits.LINTS[0], package="workspace", start=1):
    return {
        "reason": "compiler-message",
        "package_id": package,
        "message": {
            "level": "warning",
            "code": {"code": lint},
            "message": "finding",
            "spans": [
                {
                    "file_name": "crates/a/src/lib.rs",
                    "byte_start": start,
                    "byte_end": start + 1,
                    "is_primary": True,
                }
            ],
        },
    }


FINISHED = {"reason": "build-finished", "success": True}


def run_main(*args):
    with patch.object(limits.sys, "argv", ["check-clippy-limits.py", *args]):
        return limits.main()


def main_baseline_at(root):
    with patch.object(limits, "ROOT", root):
        return limits.main_baseline()


def scan_at(root, mode, members):
    with patch.object(limits, "ROOT", root):
        return limits.scan(mode, members)


class ClippyLimitsTests(unittest.TestCase):
    def test_parser_requires_every_mode_and_lint_and_integer_counts(self):
        self.assertEqual(limits.parse_baseline(json.dumps(baseline())), baseline())
        for invalid in (
            {},
            [],
            {"off": {}},
            {**baseline(), "extra": {}},
            {**baseline(), "on": {}},
            *(
                {**baseline(), "on": {**baseline()["on"], limits.LINTS[0]: n}}
                for n in (-1, True, 1.5, "1")
            ),
        ):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                limits.parse_baseline(json.dumps(invalid))

    def test_deduplicates_lib_and_test_diagnostics_but_keeps_distinct_spans(self):
        hit = diagnostic()
        records = [hit, copy.deepcopy(hit), diagnostic(start=3), FINISHED]
        counts, hits = limits.read_diagnostics(map(json.dumps, records), {"workspace"})
        self.assertEqual(counts[limits.LINTS[0]], 2)
        self.assertEqual(len(hits), 2)

    def test_ignores_dependencies_and_unselected_lints(self):
        records = [
            diagnostic(package="dependency"),
            diagnostic(lint="unused"),
            {"reason": "compiler-artifact"},
            FINISHED,
        ]
        counts, hits = limits.read_diagnostics(map(json.dumps, records), {"workspace"})
        self.assertEqual(counts, baseline()["off"])
        self.assertEqual(hits, {})

    def test_empty_truncated_failed_and_malformed_output_never_passes(self):
        bad_span = diagnostic()
        bad_span["message"]["spans"] = []
        error = diagnostic()
        error["message"]["level"] = "error"
        for records in (
            [],
            [diagnostic()],
            [{"reason": "build-finished", "success": False}],
            [bad_span, FINISHED],
            [error, FINISHED],
            [diagnostic(lint="unknown_lints"), FINISHED],
            [diagnostic(lint="renamed_and_removed_lints"), FINISHED],
        ):
            with self.subTest(records=records), self.assertRaises(ValueError):
                limits.read_diagnostics(map(json.dumps, records), {"workspace"})
        with self.assertRaises(ValueError):
            limits.read_diagnostics(["not JSON"], {"workspace"})

    def test_regression_stale_baseline_and_baseline_raise_fail(self):
        for kind in ("regression", "stale", "raise"):
            actual, allowed, previous = baseline(), baseline(), baseline()
            if kind == "regression":
                actual["on"][limits.LINTS[0]] = 1
            elif kind == "stale":
                allowed["off"][limits.LINTS[1]] = 1
            else:
                actual["off"][limits.LINTS[0]] = allowed["off"][limits.LINTS[0]] = 1
            with self.subTest(kind=kind), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(limits.compare(actual, allowed, previous), 1)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(limits.compare(baseline(), baseline(), baseline()), 0)

    def test_commands_cover_workspace_targets_and_both_feature_modes(self):
        for mode in limits.MODES:
            command = limits.clippy_command(mode)
            for flag in (
                "--locked",
                "--workspace",
                "--all-targets",
                "--keep-going",
                "--message-format=json",
            ):
                self.assertIn(flag, command)
            self.assertEqual("--features" in command, mode == "on")
            self.assertEqual("--no-default-features" in command, mode == "off")
            self.assertEqual(
                command[command.index("--") + 1 :],
                [arg for lint in limits.LINTS for arg in ("-W", lint)],
            )

    def test_git_requires_a_commit_base_ref(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", directory], check=True)
            with self.assertRaises(subprocess.CalledProcessError):
                main_baseline_at(root)
            # A tree object needs no author identity.
            tree = subprocess.run(
                ["git", "mktree"], cwd=root, input="", text=True, capture_output=True, check=True
            ).stdout.strip()
            # A tree is not a commit, so it must not bypass the base-ref guard.
            subprocess.run(
                ["git", "update-ref", "refs/remotes/origin/main", tree], cwd=root, check=True
            )
            with self.assertRaises(subprocess.CalledProcessError):
                main_baseline_at(root)

    def test_first_landing_and_existing_main_baseline(self):
        ok = subprocess.CompletedProcess([], 0, "")
        listed = subprocess.CompletedProcess([], 0, limits.BASELINE + "\n")
        shown = subprocess.CompletedProcess([], 0, json.dumps(baseline()))
        with patch.object(limits.subprocess, "run", side_effect=[ok, ok]):
            self.assertIsNone(limits.main_baseline())
        with patch.object(limits.subprocess, "run", side_effect=[ok, listed, shown]):
            self.assertEqual(limits.main_baseline(), baseline())

    @unittest.skipUnless(shutil.which("cargo"), "cargo is required for the compile control")
    def test_real_clippy_finds_a_long_function_in_both_modes_and_replays_cached_hits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "Cargo.toml").write_text(
                '[package]\nname="clippy-control"\nversion="0.1.0"\nedition="2024"\n'
                "[features]\ndesktop_rebuild=[]\n"
            )
            shutil.copyfile(SCRIPT.parents[2] / "rust-toolchain.toml", root / "rust-toolchain.toml")
            (root / "src/lib.rs").write_text(
                "pub fn long_function(mut n: u64) -> u64 {\n"
                + "n = n.wrapping_add(1);\n" * 110
                + "n\n}\n"
            )
            env = {**os.environ, "CARGO_TARGET_DIR": str(root / "target")}
            with patch.dict(os.environ, env, clear=True):
                subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True)
                metadata = subprocess.run(
                    ["cargo", "metadata", "--offline", "--no-deps", "--format-version=1"],
                    cwd=root,
                    capture_output=True,
                    text=True,
                    check=True,
                )
                members = set(json.loads(metadata.stdout)["workspace_members"])
                results = {}
                with contextlib.redirect_stdout(io.StringIO()):
                    for mode in limits.MODES:
                        results[mode], _ = scan_at(root, mode, members)
                        replay, _ = scan_at(root, mode, members)
                        self.assertEqual(replay, results[mode])
                        self.assertEqual(results[mode]["clippy::too_many_lines"], 1)
                    self.assertEqual(limits.compare(results, baseline(), None), 1)

    def test_main_passes_and_update_refuses_to_write_an_increase(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / limits.BASELINE
            target.write_text(json.dumps(baseline()))
            metadata = subprocess.CompletedProcess([], 0, json.dumps({"workspace_members": ["w"]}))
            with (
                patch.object(limits, "ROOT", root),
                patch.object(limits.sys, "platform", "linux"),
                patch.object(limits, "main_baseline", return_value=baseline()),
                patch.object(limits.subprocess, "run", return_value=metadata),
                patch.object(limits, "scan", return_value=(baseline()["off"], {})),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(run_main(), 0)
                higher = {**baseline()["off"], limits.LINTS[0]: 1}
                before = target.read_bytes()
                with patch.object(limits, "scan", return_value=(higher, {})):
                    self.assertEqual(run_main("--update"), 1)
                self.assertEqual(target.read_bytes(), before)
                with patch.object(limits, "main_baseline", return_value=None):
                    self.assertEqual(run_main("--update"), 0)
                    self.assertEqual(limits.parse_baseline(target.read_text()), baseline())

    def test_failed_cargo_and_unsupported_host_are_errors(self):
        with (
            patch.object(limits.sys, "platform", "linux"),
            patch.object(limits, "main_baseline", return_value=None),
            patch.object(
                limits.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "cargo")
            ),
        ):
            with self.assertRaises(subprocess.CalledProcessError):
                run_main("--update")
        with patch.object(limits.sys, "platform", "darwin"), self.assertRaises(ValueError):
            run_main()


if __name__ == "__main__":
    unittest.main()
