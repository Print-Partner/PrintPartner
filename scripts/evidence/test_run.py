"""Argument checks for run.py. The end-to-end check is `scripts/evidence/run.sh --self-test`."""
import contextlib
import io
import unittest

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


if __name__ == "__main__":
    unittest.main()
