#!/usr/bin/env python3
import io
import json
import re
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
STARTED = "2026-11-19T04:39:00Z"
# Arguments the workflow always passes: the Playwright step's outcome and the job's start time.
PASSED = ["--e2e-outcome", "success", "--timestamp", STARTED]
WORKFLOW = Path(__file__).resolve().parents[1] / ".github" / "workflows" / "desktop-runtime.yml"


REAL_GIT_HEAD = er.git_head


def setUpModule():
    # write reads the checkout SHA from `git rev-parse HEAD` only; tests stand in for that lookup.
    patcher = mock.patch.object(er, "git_head", lambda cwd: HEAD)
    patcher.start()
    unittest.addModuleCleanup(patcher.stop)


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

    def run(self, *extra, pr="1866", head=HEAD, pr_head=HEAD, artifact=ARTIFACT, checkout=HEAD):
        report = self.write_report()
        args = ["--report", str(report), "--out", str(self.out)]
        err = io.StringIO()
        self.asked = []

        def live_head(number):
            self.asked.append(number)
            return pr_head

        with redirect_stdout(io.StringIO()), redirect_stderr(err), mock.patch.object(er, "pr_head", live_head), \
                mock.patch.object(er, "git_head", lambda cwd: checkout):
            code = er.main(["collect", *args])
            if code == 0:
                code = er.main(["write", *args, "--repo", "Print-Partner/PrintPartner", "--pr", pr,
                                "--head", head, "--run-id", "44460000000",
                                "--suite", "web-playwright", "--runner-host", "GitHub Actions 7",
                                "--runner-environment", "github-hosted", "--artifact-url", artifact,
                                *PASSED, *extra])
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
                "--run-id", "1", "--suite", "s", "--runner-host", "h",
                "--artifact-url", "https://github.com/o/r/actions/runs/1/artifacts/2", *PASSED]
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

    def test_a_copy_changed_after_collect_refuses(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        report = f.write_report()
        with redirect_stdout(io.StringIO()):
            self.assertEqual(er.main(["collect", "--report", str(report), "--out", str(f.out)]), 0)
        # Same name, same size, non-empty, different bytes: what a swap after the upload would look like.
        (f.out / "screenshots" / "001-help-r0.png").write_bytes(PNG[:-1] + b"X")
        args = ["write", "--report", str(report), "--out", str(f.out), "--repo", "Print-Partner/PrintPartner",
                "--head", HEAD, "--run-id", "44460000000", "--suite", "s",
                "--runner-host", "h", "--artifact-url", ARTIFACT, *PASSED]
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
            self.assertEqual(er.main(args), 1)
        self.assertIn("differs from the report's attachment", err.getvalue())
        self.assertFalse((f.out / "receipt.json").exists())
        # A changed source attachment is refused the same way.
        (f.out / "screenshots" / "001-help-r0.png").write_bytes(PNG)
        f.png("help.png", PNG + b"later")
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
            self.assertEqual(er.main(args), 1)
        self.assertIn("differs from the report's attachment", err.getvalue())

    def test_collect_refuses_a_used_directory(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        self.assertEqual(f.run(), 0, f.err)
        with redirect_stderr(io.StringIO()) as err:
            self.assertEqual(er.main(["collect", "--report", str(f.dir / "report.json"), "--out", str(f.out)]), 1)
        self.assertIn("already exists", err.getvalue())

    def test_collect_refuses_an_existing_empty_directory(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        (f.out / "screenshots").mkdir(parents=True)
        self.assertEqual(f.run(), 1)
        self.assertIn("already exists", f.err)
        self.assertEqual(list((f.out / "screenshots").iterdir()), [])
        self.assertFalse((f.out / "receipt.json").exists())

    def test_a_thousand_screenshots_pass_in_report_order(self):
        specs = [spec(f"page {i}", "expected", result()) for i in range(1000)]
        f = Fixture(specs)
        for i, s in enumerate(specs):
            s["tests"][0]["results"][0] = result(shot=f.png(f"{i}.png", PNG + str(i).encode()))
        self.assertEqual(f.run(), 0, f.err)
        shots = f.receipt()["screenshots"]
        self.assertEqual(len(shots), 1000)
        self.assertEqual([s["path"] for s in shots[:2] + shots[-1:]],
                         ["screenshots/0001-page-0-r0.png", "screenshots/0002-page-1-r0.png",
                          "screenshots/1000-page-999-r0.png"])
        # Each copy in the receipt is the attachment of the test at the same report position.
        for i, shot in enumerate(shots):
            self.assertEqual((f.out / shot["path"]).read_bytes(), PNG + str(i).encode())


class OutcomeTests(unittest.TestCase):
    """A receipt never stands for a Playwright step that failed, was cancelled or was skipped."""

    def passing(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        return f

    def test_only_a_successful_step_gets_a_receipt(self):
        # A passing report with a failed step is what a missed clean-shutdown line looks like.
        for outcome in ("failure", "cancelled", "skipped", "", "Success"):
            with self.subTest(outcome=outcome):
                f = self.passing()
                self.assertEqual(f.run("--e2e-outcome", outcome), 1)
                self.assertIn("not 'success'", f.err)
                self.assertFalse((f.out / "receipt.json").exists())

    def test_workflow_writes_and_uploads_only_after_a_successful_step(self):
        text = WORKFLOW.read_text(encoding="utf-8")
        steps = {m.group(1): m.group(0) for m in
                 re.finditer(r"(?ms)^      - name: ([^\n]+)\n.*?(?=^      - |\Z)", text)}
        write = steps["Write the e2e receipt"]
        self.assertIn("if: ${{ !cancelled() && steps.playwright.outcome == 'success' }}\n", write)
        self.assertIn("E2E_OUTCOME: ${{ steps.playwright.outcome }}\n", write)
        self.assertIn('--e2e-outcome "$E2E_OUTCOME" --timestamp "$PP_JOB_STARTED"', write)
        self.assertIn("if: ${{ !cancelled() && steps.playwright.outcome == 'success' "
                      "&& steps.e2e_receipt.outcome == 'success' }}\n", steps["Upload the e2e receipt"])
        self.assertIn("id: playwright\n", steps["Run Playwright specs against pp-server"])


class InputTests(unittest.TestCase):
    """Malformed report input or arguments exit 1 and write no receipt."""

    def passing(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        return f

    def refuse(self, f, *extra, message, **kw):
        self.assertEqual(f.run(*extra, **kw), 1)
        self.assertIn(message, f.err)
        self.assertFalse((f.out / "receipt.json").exists())

    def test_stats_must_be_non_negative_integers(self):
        for key, bad in (("expected", True), ("unexpected", False), ("expected", 1.0), ("skipped", "0"),
                         ("flaky", None), ("unexpected", -1)):
            with self.subTest(key=key, value=bad):
                f = self.passing()
                f.report["stats"][key] = bad
                self.refuse(f, message=f"stats.{key} must be a non-negative integer")
        f = self.passing()
        f.report["stats"] = [1, 0, 0, 0]
        self.refuse(f, message="stats must be an object")

    def test_errors_must_be_a_present_list(self):
        for bad in (None, {}, "none", 0):
            with self.subTest(errors=bad):
                f = self.passing()
                f.report["errors"] = bad
                self.refuse(f, message="errors must be a list")
        f = self.passing()
        del f.report["errors"]
        self.refuse(f, message="errors must be a list")

    def test_run_numbers_must_be_positive_integers(self):
        for bad in ("0", "-1", "01", "1.0", "", "two"):
            with self.subTest(attempt=bad):
                self.refuse(self.passing(), "--run-attempt", bad, message="run-attempt must be a positive integer")
        for bad in ("0", "-5"):
            with self.subTest(run_id=bad):
                self.refuse(self.passing(), "--run-id", bad, message="run-id must be a positive integer")
        self.refuse(self.passing(), pr="0", message="pr must be a positive integer")
        f = self.passing()
        self.assertEqual(f.run("--run-attempt", "2"), 0, f.err)
        self.assertEqual(f.receipt()["run_attempt"], 2)

    def test_checkout_sha_must_be_forty_lowercase_hex(self):
        for bad in (HEAD.upper(), HEAD[:12], "g" * 40):
            with self.subTest(checkout=bad):
                self.refuse(self.passing(), checkout=bad, message="checkout_sha must be a 40-character")

    def test_the_cli_cannot_override_the_checkout(self):
        # Both SHAs set to another commit: the CLI has no such option, and git's answer stands.
        f = self.passing()
        report = f.write_report()
        with redirect_stdout(io.StringIO()):
            self.assertEqual(er.main(["collect", "--report", str(report), "--out", str(f.out)]), 0)
        args = ["write", "--report", str(report), "--out", str(f.out), "--repo", "Print-Partner/PrintPartner",
                "--head", OTHER, "--checkout-sha", OTHER, "--run-id", "44460000000", "--suite", "s",
                "--runner-host", "h", "--artifact-url", ARTIFACT, *PASSED]
        with self.assertRaises(SystemExit) as exit_, redirect_stderr(io.StringIO()) as err, \
                mock.patch.object(er, "pr_head", lambda _: OTHER):
            er.main(args)
        self.assertEqual(exit_.exception.code, 2)
        self.assertIn("unrecognized arguments: --checkout-sha", err.getvalue())
        self.assertFalse((f.out / "receipt.json").exists())
        self.refuse(self.passing(), head=OTHER, pr_head=OTHER, message=f"is not the tested checkout {HEAD[:12]}")

    def test_write_reads_the_checkout_from_git_in_its_working_directory(self):
        f = self.passing()
        report = f.write_report()
        with redirect_stdout(io.StringIO()):
            self.assertEqual(er.main(["collect", "--report", str(report), "--out", str(f.out)]), 0)
        seen = []

        def fake_git(cmd, **kw):
            seen.append((cmd, kw.get("cwd")))
            return OTHER + "\n"

        args = ["write", "--report", str(report), "--out", str(f.out), "--repo", "Print-Partner/PrintPartner",
                "--head", HEAD, "--run-id", "44460000000", "--suite", "s", "--runner-host", "h",
                "--artifact-url", ARTIFACT, *PASSED]
        with mock.patch.object(er, "git_head", REAL_GIT_HEAD), mock.patch.object(er.subprocess, "check_output", fake_git), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
            self.assertEqual(er.main(args), 1)
        self.assertEqual(seen, [(["git", "rev-parse", "HEAD"], Path.cwd())])
        self.assertIn(f"is not the tested checkout {OTHER[:12]}", err.getvalue())
        self.assertFalse((f.out / "receipt.json").exists())

    def test_timestamp_must_be_utc_seconds(self):
        for bad in ("2026-11-19T04:39:00.000Z", "2026-11-19 04:39:00", "2026-11-19T04:39:00+00:00",
                    "2026-02-30T04:39:00Z", ""):
            with self.subTest(timestamp=bad):
                self.refuse(self.passing(), "--timestamp", bad, message="timestamp must be")


def first_test(report):
    return report["suites"][0]["specs"][0]["tests"][0]


def first_result(report):
    return first_test(report)["results"][0]


class ReportShapeTests(unittest.TestCase):
    """Every structural field write reads must have Playwright's declared type, or nothing is written."""

    def passing(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        return f

    def refuse(self, change, message):
        f = self.passing()
        change(f.report)
        self.assertEqual(f.run(), 1, f.err)
        self.assertIn(message, f.err)
        self.assertFalse((f.out / "receipt.json").exists())

    def test_malformed_collections_refuse(self):
        cases = [
            ("specs: false", lambda r: r["suites"][0]["suites"][0].update(specs=False),
             "report.suites[0].suites[0].specs must be a list, not False"),
            ("suites: null", lambda r: r.update(suites=None), "report.suites must be a list, not None"),
            ("child suites: null", lambda r: r["suites"][0].update(suites=None),
             "report.suites[0].suites must be a list, not None"),
            ("tests: {}", lambda r: r["suites"][0]["specs"][0].update(tests={}),
             "report.suites[0].specs[0].tests must be a list, not {}"),
            ("results: 'x'", lambda r: first_test(r).update(results="x"), ".tests[0].results must be a list, not 'x'"),
            ("attachments: 0", lambda r: first_result(r).update(attachments=0),
             ".results[0].attachments must be a list, not 0"),
            ("specs missing", lambda r: r["suites"][0].pop("specs"), "report.suites[0].specs must be a list, not None"),
            ("results missing", lambda r: first_test(r).pop("results"), ".tests[0].results must be a list, not None"),
            ("attachments missing", lambda r: first_result(r).pop("attachments"),
             ".results[0].attachments must be a list, not None"),
            ("suite not an object", lambda r: r["suites"].append([]), "report.suites[1] must be an object"),
            ("spec not an object", lambda r: r["suites"][0]["specs"].append("x"), "report.suites[0].specs[1] must be an object"),
            ("attachment not an object", lambda r: first_result(r)["attachments"].append(None),
             ".results[0].attachments[2] must be an object"),
        ]
        for label, change, message in cases:
            with self.subTest(label):
                self.refuse(change, message)

    def test_malformed_fields_refuse(self):
        shot = lambda r: first_result(r)["attachments"][1]  # noqa: E731
        cases = [
            ("status missing", lambda r: first_test(r).pop("status"), ".tests[0].status must be a str, not None"),
            ("title missing", lambda r: r["suites"][0]["specs"][0].pop("title"), ".specs[0].title must be a str, not None"),
            ("retry bool", lambda r: first_result(r).update(retry=True), ".results[0].retry must be a int, not True"),
            ("retry missing", lambda r: first_result(r).pop("retry"), ".results[0].retry must be a int, not None"),
            ("contentType missing", lambda r: shot(r).pop("contentType"),
             ".attachments[1].contentType must be a str, not None"),
            ("path not a string", lambda r: shot(r).update(path=7), ".attachments[1].path must be a str, not 7"),
            ("path empty", lambda r: shot(r).update(path=""), ".attachments[1].path must not be empty"),
        ]
        for label, change, message in cases:
            with self.subTest(label):
                self.refuse(change, message)

    def test_documented_optional_fields_may_be_absent(self):
        # Playwright omits `suites` on a suite with no child suites, and `path` on an inline attachment.
        f = self.passing()
        del f.report["suites"][0]["suites"]
        first_result(f.report)["attachments"].append({"name": "inline", "contentType": "image/png", "body": "aGk="})
        self.assertEqual(f.run(), 0, f.err)
        self.assertEqual(len(f.receipt()["screenshots"]), 1)


class StableBytesTests(unittest.TestCase):
    def test_the_same_input_gives_the_same_bytes(self):
        f = Fixture([spec("help", "expected", result())])
        f.report["suites"][0]["specs"][0]["tests"][0]["results"][0] = result(shot=f.png("help.png"))
        self.assertEqual(f.run(), 0, f.err)
        first = (f.out / "receipt.json").read_bytes()
        self.assertEqual(json.loads(first)["timestamp"], STARTED)
        args = ["write", "--report", str(f.dir / "report.json"), "--out", str(f.out),
                "--repo", "Print-Partner/PrintPartner", "--pr", "1866", "--head", HEAD,
                "--run-id", "44460000000", "--suite", "web-playwright", "--runner-host", "GitHub Actions 7",
                "--runner-environment", "github-hosted", "--artifact-url", ARTIFACT, *PASSED]
        # A second write from the same input, at a later wall-clock second, gives the same bytes.
        with redirect_stdout(io.StringIO()), mock.patch.object(er, "pr_head", lambda _: HEAD), \
                mock.patch.object(er, "datetime", wraps=er.datetime) as clock:
            clock.now.return_value = er.datetime(2030, 1, 1)
            self.assertEqual(er.main(args), 0)
        self.assertEqual((f.out / "receipt.json").read_bytes(), first)


if __name__ == "__main__":
    unittest.main()
