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


TOKEN = "ghp_canaryEvidenceToken0123456789"
GOOD = "https://github.com/Print-Partner/PrintPartner"


class EvidenceCache(unittest.TestCase):
    def setUp(self):
        base = tempfile.TemporaryDirectory()
        self.addCleanup(base.cleanup)
        self.base = base.name
        patcher = mock.patch.dict(os.environ, {"XDG_CACHE_HOME": self.base})
        patcher.start()
        self.addCleanup(patcher.stop)

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.cache), *args], check=True, capture_output=True)

    def make_cache(self, url):
        self.cache = Path(self.base) / "evidence-repo-Print-Partner-PrintPartner"
        subprocess.run(["git", "init", "-q", str(self.cache)], check=True)
        self.git("remote", "add", "origin", url)

    def refused(self):
        with self.assertRaises(SystemExit) as refusal:
            run.evidence_cache()
        return str(refusal.exception)

    def test_no_cache_yet_is_allowed(self):
        self.assertEqual(run.evidence_cache(), Path(self.base) / "evidence-repo-Print-Partner-PrintPartner")

    def test_accepts_the_repository_in_its_usual_forms(self):
        self.make_cache(GOOD)
        for url in (GOOD, GOOD + ".git", GOOD + "/", GOOD + ".git/",
                    "git@github.com:Print-Partner/PrintPartner.git", "ssh://git@github.com/Print-Partner/PrintPartner"):
            with self.subTest(url=url):
                self.git("remote", "set-url", "origin", url)
                self.assertEqual(run.evidence_cache(), self.cache)

    def test_normalizing_never_matches_another_repository(self):
        for url in ("https://github.com/poitee/PrintPartner", "https://github.com/Print-Partner/PrintPartner2",
                    "https://github.com/Print-Partner/PrintPartner/extra", "https://github.com.evil/Print-Partner/PrintPartner",
                    "https://evil.com/github.com/Print-Partner/PrintPartner", "http://github.com/Print-Partner/PrintPartner",
                    "https://github.com:444/Print-Partner/PrintPartner", "https://github.com/Print-Partner/PrintPartner?x=1",
                    "git@evil.com:Print-Partner/PrintPartner", "github.com:Print-Partner/PrintPartner/x",
                    "https://github.com/Print-Partner/Print%50artner", "file:///github.com/Print-Partner/PrintPartner",
                    "/tmp/github.com/Print-Partner/PrintPartner", ""):
            with self.subTest(url=url):
                self.assertNotEqual(run.normalize_remote(url), "github.com/Print-Partner/PrintPartner")

    def test_refuses_another_origin(self):
        self.make_cache("https://github.com/poitee/PrintPartner")
        self.assertIn("evidence cache remote is not Print-Partner/PrintPartner", self.refused())

    def test_refuses_a_foreign_pushurl(self):
        self.make_cache(GOOD)
        self.git("remote", "set-url", "--push", "origin", "https://github.com/poitee/PrintPartner")
        self.refused()

    def test_refuses_when_any_push_url_is_foreign(self):
        self.make_cache(GOOD)
        self.git("remote", "set-url", "--add", "--push", "origin", GOOD)
        self.git("remote", "set-url", "--add", "--push", "origin", "https://github.com/poitee/PrintPartner")
        self.refused()

    def test_refuses_a_cache_without_origin(self):
        self.make_cache(GOOD)
        self.git("remote", "remove", "origin")
        self.refused()

    def test_a_token_in_a_foreign_remote_never_reaches_the_output(self):
        self.make_cache(f"https://x-access-token:{TOKEN}@github.com/poitee/PrintPartner")
        self.git("remote", "set-url", "--push", "origin", f"https://x-access-token:{TOKEN}@github.com/poitee/x")
        program = "import run; run.evidence_cache()"
        result = subprocess.run([sys.executable, "-c", program], cwd=Path(run.__file__).parent,
                                env=dict(os.environ, XDG_CACHE_HOME=self.base), capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("evidence cache remote is not Print-Partner/PrintPartner", result.stderr)
        self.assertIn(str(self.cache), result.stderr)
        self.assertNotIn(TOKEN, result.stdout + result.stderr)
        self.assertNotIn("x-access-token", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
