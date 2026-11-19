#!/usr/bin/env python3
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

import e2e_receipt as er

HEAD = "1d7950189896bc6fc68ac7a9d4aa5739e845593b"
OTHER = "f26fcb8fd71cc1215c55b1d6d38c8a9d3cbd68ce"
ARTIFACT = "https://github.com/Print-Partner/PrintPartner/actions/runs/44460000000/artifacts/4471"
PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32


def result(retry=0, shot=None):
    attachments = [{"name": "trace", "contentType": "application/zip", "path": "results/trace.zip"}]
    if shot:
        attachments.append({"name": "screenshot", "contentType": "image/png", "path": shot})
    return {"retry": retry, "status": "passed", "attachments": attachments}


def spec(title, status, *results):
    return {"title": title, "tests": [{"projectName": "", "status": status, "results": list(results)}]}


class Fixture:
    """A Playwright JSON report shaped like 1.64's, with real PNG files beside it."""

    def __init__(self, specs, nested=(), errors=(), stats=None):
        # Left in place for inspection; the caller's TMPDIR owns cleanup.
        self.dir = Path(tempfile.mkdtemp(prefix="e2e-receipt-"))
        (self.dir / "results").mkdir()
        tally = dict.fromkeys(er.STATUSES, 0)
        for s in list(specs) + list(nested):
            tally[s["tests"][0]["status"]] += 1
        self.report = {
            "config": {},
            "suites": [{"title": "a.spec.ts", "specs": list(specs),
                        "suites": [{"title": "group", "specs": list(nested), "suites": []}]}],
            "errors": list(errors),
            "stats": stats or {"startTime": "2026-11-19T04:39:21.521Z", "duration": 1.0, **tally},
        }
        self.out = self.dir / "receipt"

    def png(self, name, data=PNG):
        path = self.dir / "results" / name
        path.write_bytes(data)
        return str(path)

    def write_report(self):
        path = self.dir / "report.json"
        path.write_text(json.dumps(self.report), encoding="utf-8")
        return path

    def run(self, *extra, pr="1866", head=HEAD, pr_head=HEAD, artifact=ARTIFACT):
        report = self.write_report()
        args = ["--report", str(report), "--out", str(self.out)]
        err = io.StringIO()
        self.asked = []

        def live_head(number):
            self.asked.append(number)
            return pr_head

        with redirect_stdout(io.StringIO()), redirect_stderr(err), mock.patch.object(er, "pr_head", live_head):
            code = er.main(["collect", *args])
            if code == 0:
                code = er.main(["write", *args, "--repo", "Print-Partner/PrintPartner", "--pr", pr,
                                "--head", head, "--checkout-sha", HEAD, "--run-id", "44460000000",
                                "--suite", "web-playwright", "--runner-host", "GitHub Actions 7",
                                "--runner-environment", "github-hosted", "--artifact-url", artifact, *extra])
        self.err = err.getvalue()
        return code

    def receipt(self):
        return json.loads((self.out / "receipt.json").read_text(encoding="utf-8"))


class CountTests(unittest.TestCase):
    def test_receipt_counts_match_the_report(self):
        f = Fixture([spec("help", "expected", result()), spec("printers", "flaky", result(0), result(1))],
                    nested=[spec("broken", "unexpected", result()), spec("later", "skipped")])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        self.assertEqual(f.run(), 0, f.err)
        r = f.receipt()
        self.assertEqual({k: r[k] for k in ("pass", "fail", "skipped", "flaky", "errors")},
                         {"pass": 2, "fail": 1, "skipped": 1, "flaky": 1, "errors": 0})
        self.assertEqual(r["run_url"], "https://github.com/Print-Partner/PrintPartner/actions/runs/44460000000")
        self.assertEqual((r["head"], r["pr"], r["run_id"]), (HEAD, 1866, 44460000000))
        self.assertEqual(set(r), {"repo", "pr", "head", "checkout_sha", "run_id", "run_attempt", "run_url", "suite",
                                  "pass", "fail", "skipped", "flaky", "errors", "screenshots", "artifact_url",
                                  "timestamp", "runner_host", "runner_environment"})

    def test_stats_that_disagree_with_the_tests_refuse(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["stats"]["expected"] = 2
        self.assertEqual(f.run(), 1)
        self.assertIn("stats.expected=2 but the report lists 1", f.err)
        self.assertFalse((f.out / "receipt.json").exists())

    def test_unknown_status_refuses(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["status"] = "passed"
        self.assertEqual(f.run(), 1)

    def test_a_report_that_ran_nothing_refuses(self):
        self.assertEqual(Fixture([]).run(), 1)

    def test_global_errors_count_and_allow_no_screenshots(self):
        f = Fixture([], errors=[{"message": "globalSetup failed"}])
        self.assertEqual(f.run(), 0, f.err)
        self.assertEqual((f.receipt()["errors"], f.receipt()["screenshots"]), (1, []))


class HeadTests(unittest.TestCase):
    def passing(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        return f

    def test_head_must_be_forty_lowercase_hex(self):
        for bad in (HEAD[:7], HEAD.upper(), HEAD + "0", "g" * 40, ""):
            with self.subTest(head=bad):
                f = self.passing()
                self.assertEqual(f.run(head=bad), 1)
                self.assertIn("40-character", f.err)

    def test_head_must_be_the_tested_checkout(self):
        f = self.passing()
        self.assertEqual(f.run(head=OTHER, pr_head=OTHER), 1)
        self.assertIn("is not the tested checkout", f.err)

    def test_pr_must_point_at_the_tested_head(self):
        f = self.passing()
        self.assertEqual(f.run(pr_head=OTHER), 1)
        self.assertIn("PR #1866's head is not", f.err)
        self.assertEqual(f.asked, [1866])
        f = self.passing()
        self.assertEqual(f.run(), 0, f.err)
        self.assertEqual((f.receipt()["pr"], f.asked), (1866, [1866]))

    def test_pr_is_canonical_or_null(self):
        f = self.passing()
        self.assertEqual(f.run(pr="", pr_head=OTHER), 0, f.err)
        self.assertIsNone(f.receipt()["pr"])
        self.assertEqual(f.asked, [])
        for bad in ("01866", "-1", "1866a"):
            self.assertEqual(self.passing().run(pr=bad), 1)


class ScreenshotTests(unittest.TestCase):
    def test_screenshots_are_copied_with_size_and_sha256(self):
        f = Fixture([spec("Help page", "flaky", result(0, None), result(1, None))])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"] = [result(0, f.png("a.png")),
                                                                    result(1, "results/b.png")]
        f.png("b.png", PNG + b"b")
        self.assertEqual(f.run(), 0, f.err)
        shots = f.receipt()["screenshots"]
        self.assertEqual([s["path"] for s in shots],
                         ["screenshots/001-help-page-r0.png", "screenshots/002-help-page-r1.png"])
        for shot in shots:
            data = (f.out / shot["path"]).read_bytes()
            self.assertGreater(len(data), 0)
            self.assertEqual(shot["bytes"], len(data))

    def test_an_empty_screenshot_refuses(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png", b""))
        self.assertEqual(f.run(), 1)
        self.assertIn("missing or empty", f.err)

    def test_a_missing_screenshot_refuses(self):
        f = Fixture([spec("help", "expected", result(shot="/nonexistent/help.png"))])
        self.assertEqual(f.run(), 1)

    def test_a_passing_run_needs_a_screenshot(self):
        f = Fixture([spec("help", "expected", result())])
        self.assertEqual(f.run(), 1)
        self.assertIn("at least one screenshot", f.err)

    def test_a_failing_run_may_have_none(self):
        f = Fixture([spec("help", "unexpected", result())])
        self.assertEqual(f.run(artifact=""), 0, f.err)
        self.assertIsNone(f.receipt()["artifact_url"])

    def test_screenshots_need_their_artifact_url(self):
        def passing():
            f = Fixture([spec("help", "expected", result())])
            f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
            return f
        f = passing()
        self.assertEqual(f.run(artifact=""), 1)
        self.assertIn("URL of their uploaded artifact", f.err)
        for bad in ("https://github.com/Print-Partner/PrintPartner/actions/runs/1/artifacts/4471",
                    "https://github.com/other/repo/actions/runs/44460000000/artifacts/4471",
                    "https://github.com/Print-Partner/PrintPartner/actions/runs/44460000000"):
            with self.subTest(url=bad):
                f = passing()
                self.assertEqual(f.run(artifact=bad), 1)
                self.assertIn("artifact of this run", f.err)

    def test_a_changed_or_extra_copy_refuses(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        report = f.write_report()
        with redirect_stdout(io.StringIO()):
            er.main(["collect", "--report", str(report), "--out", str(f.out)])
        (f.out / "screenshots" / "999-extra.png").write_bytes(PNG)
        args = ["write", "--report", str(report), "--out", str(f.out), "--repo", "o/r", "--head", HEAD,
                "--checkout-sha", HEAD, "--run-id", "1", "--suite", "s", "--runner-host", "h",
                "--artifact-url", "https://github.com/o/r/actions/runs/1/artifacts/2"]
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
            self.assertEqual(er.main(args), 1)
        self.assertIn("holds 2 files; the report lists 1", err.getvalue())
        receipt = {"repo": "o/r", "pr": None, "head": HEAD, "pass": 1, "fail": 0, "skipped": 0, "flaky": 0,
                   "errors": 0, "run_id": 1, "artifact_url": "https://github.com/o/r/actions/runs/1/artifacts/2",
                   "screenshots": [er.describe(f.out / "screenshots" / "001-help-r0.png", f.out)]}
        er.check(receipt, f.out)
        (f.out / "screenshots" / "001-help-r0.png").write_bytes(PNG + b"changed")
        with self.assertRaises(er.ReceiptError):
            er.check(receipt, f.out)

    def test_collect_refuses_a_used_directory(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        self.assertEqual(f.run(), 0, f.err)
        with redirect_stderr(io.StringIO()) as err:
            self.assertEqual(er.main(["collect", "--report", str(f.dir / "report.json"), "--out", str(f.out)]), 1)
        self.assertIn("is not empty", err.getvalue())


if __name__ == "__main__":
    unittest.main()
