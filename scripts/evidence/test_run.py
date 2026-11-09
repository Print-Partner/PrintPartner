"""Argument checks for run.py. The end-to-end check is `scripts/evidence/run.sh --self-test`."""
import contextlib
import io
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

    def test_note_needs_publish(self):
        with self.assertRaises(SystemExit):
            parse("--shot", "/builds", "--note", "x")
        self.assertEqual(parse("--shot", "/builds", "--publish", "7", "--note", "x").publish, 7)


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

    def test_sigterm_reports_its_name(self):
        status, printed = self.run_main_raising(run.Stopped("SIGTERM"))
        self.assertEqual(status, 1)
        self.assertIn("FAIL: stopped by SIGTERM\n", printed)


class EvidenceCache(unittest.TestCase):
    def test_uses_a_cache_for_this_repository_and_refuses_another_origin(self):
        with tempfile.TemporaryDirectory() as base, mock.patch.dict(os.environ, {"XDG_CACHE_HOME": base}):
            cache = run.evidence_cache()
            self.assertEqual(cache, Path(base) / "evidence-repo-Print-Partner-PrintPartner")
            subprocess.run(["git", "init", "-q", str(cache)], check=True)
            subprocess.run(["git", "-C", str(cache), "remote", "add", "origin",
                            "https://github.com/poitee/PrintPartner"], check=True)
            with self.assertRaises(SystemExit) as refused:
                run.evidence_cache()
            self.assertIn("refusing to publish", str(refused.exception))
            subprocess.run(["git", "-C", str(cache), "remote", "set-url", "origin", run.EVIDENCE_REMOTE], check=True)
            self.assertEqual(run.evidence_cache(), cache)


if __name__ == "__main__":
    unittest.main()
