"""Argument checks for run.py. The end-to-end check is `scripts/evidence/run.sh --self-test`."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import run


def parse(*argv):
    with contextlib.redirect_stderr(io.StringIO()):
        return run.parse_args(list(argv))


class ParseArgs(unittest.TestCase):
    def test_shots_and_steps_keep_their_order(self):
        args = parse("--shot", "/builds/2/history", "--step", "snapshot:2",
                     "--step", "settle-reconciliation:3", "--shot", "/farm/log")
        self.assertEqual(args.shot, ["/builds/2/history", "/farm/log"])
        self.assertEqual(args.step, [{"name": "snapshot", "arg": 2},
                                     {"name": "settle-reconciliation", "arg": 3}])
        self.assertEqual(args.seed, 1138)

    def test_refuses_paths_that_leave_the_ui_origin(self):
        for path in ("//evil.example/x", "http://evil.example/", "builds", "/a b", "/x?next=//y"):
            with self.subTest(path=path), self.assertRaises(SystemExit):
                parse("--shot", path)

    def test_refuses_unknown_steps_and_bad_build_ids(self):
        for step in ("drop-tables:1", "snapshot", "snapshot:0", "snapshot:x", "snapshot:-1"):
            with self.subTest(step=step), self.assertRaises(SystemExit):
                parse("--shot", "/builds", "--step", step)

    def test_needs_a_shot_and_self_test_supplies_one(self):
        with self.assertRaises(SystemExit):
            parse()
        self.assertEqual(parse("--self-test").shot, ["/builds"])
        with self.assertRaises(SystemExit):
            parse("--self-test", "--shot", "/farm")

    def test_modules_are_checked(self):
        self.assertEqual(parse("--shot", "/farm", "--module", "farm", "--module", "orders").module,
                         ["farm", "orders"])
        with self.assertRaises(SystemExit):
            parse("--shot", "/farm", "--module", "telemetry")

    def test_shot_names(self):
        self.assertEqual(run.shot_name("/builds/2/history"), "builds-2-history.png")
        self.assertEqual(run.shot_name("/"), "root.png")

    def test_publishing_is_not_a_capture_option(self):
        for argv in (("--publish", "7"), ("--note", "x")):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                parse("--shot", "/builds", *argv)


class Stop(Exception):
    pass


class Spawn(unittest.TestCase):
    def setUp(self):
        def raise_stop(_signum, _frame):
            raise Stop()
        self.previous = signal.signal(signal.SIGTERM, raise_stop)
        self.addCleanup(signal.signal, signal.SIGTERM, self.previous)

    def test_a_signal_during_spawn_lands_after_the_process_is_recorded(self):
        real = subprocess.Popen

        def popen_then_signal(*args, **kwargs):
            os.kill(os.getpid(), signal.SIGTERM)  # held until spawn records the process
            return real(*args, **kwargs)

        procs = {}
        with mock.patch.object(subprocess, "Popen", popen_then_signal), self.assertRaises(Stop):
            run.spawn(procs, "child", [sys.executable, "-c", "import time; time.sleep(30)"])
        self.assertIn("child", procs)
        self.assertEqual(run.stop(procs["child"], "child", 5), -signal.SIGTERM)

    def test_the_child_does_not_inherit_the_held_signals(self):
        procs = {}
        child = run.spawn(procs, "child", [sys.executable, "-c",
                                           "import signal; print(sorted(signal.pthread_sigmask(signal.SIG_BLOCK, [])))"],
                          stdout=subprocess.PIPE, text=True)
        out, _ = child.communicate(timeout=30)
        self.assertEqual(out.strip(), "[]")


class CaptureNeverPublishes(unittest.TestCase):
    def test_a_clean_capture_run_prints_the_next_step_and_never_publishes(self):
        import publisher
        signals = {signum: signal.getsignal(signum) for signum in run.STOP_SIGNALS}
        self.addCleanup(lambda: [signal.signal(n, h) for n, h in signals.items()])

        def capture(args, work, out_dir, node, binary):
            for name in ("builds.png", "farm-log.png"):
                (out_dir / name).write_bytes(b"png")
            return []

        with tempfile.TemporaryDirectory() as out, \
                mock.patch.object(run, "build", return_value=Path("pp-server")), \
                mock.patch.object(run, "session", side_effect=capture), \
                mock.patch.object(publisher, "publish") as publish, \
                mock.patch.object(publisher.subprocess, "run") as subprocess_run, \
                contextlib.redirect_stdout(io.StringIO()) as printed:
            status = run.main(["--shot", "/builds", "--shot", "/farm/log", "--out", out])
            out = Path(out).resolve()
        self.assertEqual(status, 0)
        publish.assert_not_called()
        subprocess_run.assert_not_called()
        self.assertFalse(hasattr(run, "publish"))
        self.assertIn(f"evidence-run: open each PNG, then publish with: python3 scripts/evidence/publisher.py "
                      f"<pr> {out / 'builds.png'} {out / 'farm-log.png'}\n", printed.getvalue())


class StopLabel(unittest.TestCase):
    def test_names_the_signal(self):
        self.assertEqual(run.stop_label(KeyboardInterrupt()), "SIGINT")
        self.assertEqual(run.stop_label(run.Stopped("SIGTERM")), "SIGTERM")
        self.assertEqual(run.stop_label(run.Stopped("SIGHUP")), "SIGHUP")


class MainStops(unittest.TestCase):
    def run_main_raising(self, error):
        signals = {signum: signal.getsignal(signum) for signum in run.STOP_SIGNALS}
        self.addCleanup(lambda: [signal.signal(n, h) for n, h in signals.items()])
        with tempfile.TemporaryDirectory() as out, \
                mock.patch.object(run, "build", return_value=Path("pp-server")), \
                mock.patch.object(run, "session", side_effect=error), \
                contextlib.redirect_stdout(io.StringIO()) as printed:
            status = run.main(["--shot", "/builds", "--out", out])
        return status, printed.getvalue()

    def test_ctrl_c_reports_sigint(self):
        status, printed = self.run_main_raising(KeyboardInterrupt())
        self.assertEqual(status, 1)
        self.assertIn("FAIL: stopped by SIGINT\n", printed)

    def test_a_stop_while_building_reports_it_without_a_traceback(self):
        signals = {signum: signal.getsignal(signum) for signum in run.STOP_SIGNALS}
        self.addCleanup(lambda: [signal.signal(n, h) for n, h in signals.items()])
        for error, name in ((KeyboardInterrupt(), "SIGINT"), (run.Stopped("SIGTERM"), "SIGTERM")):
            with self.subTest(name), mock.patch.object(run, "build", side_effect=error), \
                    contextlib.redirect_stdout(io.StringIO()) as printed:
                self.assertEqual(run.main(["--shot", "/builds"]), 1)
            self.assertIn(f"FAIL: stopped by {name} while building\n", printed.getvalue())

    def test_sigterm_reports_its_name(self):
        status, printed = self.run_main_raising(run.Stopped("SIGTERM"))
        self.assertEqual(status, 1)
        self.assertIn("FAIL: stopped by SIGTERM\n", printed)


def sha(data):
    return hashlib.sha256(data).hexdigest()


class ReleaseMatches(unittest.TestCase):
    """The reuse check mirrors the core's start-up release check (pp-compat verify_bundle)."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.web = Path(directory.name) / "web"
        self.node = Path(directory.name) / "node"
        self.node.write_bytes(b"node")
        manifest = {"node_sha256": sha(b"node"), "metadata": {"package.json": sha(b"{}")}}
        (self.web).mkdir()
        (self.web / "package.json").write_bytes(b"{}")
        for key, path in run.RELEASE_DIRS.items():
            if path.endswith("/current"):
                # As in the real tree, dist/current links to a versioned directory.
                versioned = self.web / path.removesuffix("current") / "v1"
                versioned.mkdir(parents=True)
                (self.web / path).symlink_to("v1")
            else:
                (self.web / path).mkdir(parents=True)
            (self.web / path / "index.js").write_bytes(key.encode())
            manifest[key] = {"index.js": sha(key.encode())}
        self.manifest = Path(directory.name) / "bundle-manifest.json"
        self.manifest.write_text(json.dumps(manifest))

    def matches(self):
        return run.release_matches(self.manifest, self.web, str(self.node))

    def test_matches_the_build_it_was_made_from(self):
        self.assertTrue(self.matches())

    def test_a_rebuilt_web_dist_does_not_match(self):
        (self.web / "apps/web/dist/index.js").write_bytes(b"a plain npm run build")
        self.assertFalse(self.matches())

    def test_an_extra_or_missing_file_does_not_match(self):
        (self.web / "apps/web/dist/extra.js").write_bytes(b"x")
        self.assertFalse(self.matches())
        (self.web / "apps/web/dist/extra.js").unlink()
        (self.web / "apps/server/dist/current/index.js").unlink()
        self.assertFalse(self.matches())

    def test_a_symlink_inside_a_build_does_not_match(self):
        (self.web / "apps/web/dist/link.js").symlink_to(self.web / "package.json")
        self.assertFalse(self.matches())

    def test_changed_metadata_or_node_does_not_match(self):
        (self.web / "package.json").write_bytes(b"{ }")
        self.assertFalse(self.matches())
        (self.web / "package.json").write_bytes(b"{}")
        self.node.write_bytes(b"another node")
        self.assertFalse(self.matches())

    def test_a_missing_or_broken_manifest_does_not_match(self):
        self.manifest.write_text("not json")
        self.assertFalse(self.matches())
        self.manifest.unlink()
        self.assertFalse(self.matches())

    def test_the_directories_are_the_manifest_generators(self):
        sys.path.insert(0, str(run.ROOT / "rust/scripts"))
        self.addCleanup(sys.path.remove, str(run.ROOT / "rust/scripts"))
        from desktop_workspaces import ARTIFACTS
        self.assertEqual(run.RELEASE_DIRS, ARTIFACTS)


class Reuse(unittest.TestCase):
    class Rebuilt(Exception):
        pass

    def decide(self, release_matches):
        with tempfile.TemporaryDirectory() as directory:
            stamp = Path(directory) / "stamp.json"
            stamp.write_text(json.dumps({"fingerprint": "fp", "files": {"f": "h"}}))
            with mock.patch.object(run, "STAMP", stamp), \
                    mock.patch.object(run, "fingerprint", return_value="fp"), \
                    mock.patch.object(run, "file_hashes", return_value={"f": "h"}), \
                    mock.patch.object(run, "release_matches", return_value=release_matches), \
                    mock.patch.object(run, "run", side_effect=self.Rebuilt), \
                    contextlib.redirect_stdout(io.StringIO()) as printed:
                try:
                    return run.build("node", rebuild=False), printed.getvalue()
                except self.Rebuilt:
                    return "rebuilt", printed.getvalue()

    def test_reuses_when_the_web_build_still_matches(self):
        self.assertEqual(self.decide(True)[0], run.BINARY)

    def test_rebuilds_when_the_web_build_changed(self):
        result, printed = self.decide(False)
        self.assertEqual(result, "rebuilt")
        self.assertIn("rebuilding: the web build or Node no longer matches", printed)


class CoreStartFailure(unittest.TestCase):
    def test_a_core_that_exits_at_start_suggests_rebuild(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "launcher.log"
            log.write_text("Release file inventory mismatch\n")
            exited = subprocess.Popen([sys.executable, "-c", "raise SystemExit(1)"])
            exited.wait()
            with self.assertRaises(RuntimeError) as failure:
                run.wait_for_core(log, exited)
        self.assertEqual(str(failure.exception),
                         "Rust-only core: process exited with 1; see launcher.log, and run again with --rebuild")


if __name__ == "__main__":
    unittest.main()
