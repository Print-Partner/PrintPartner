"""Check rolling issue creation, updates, and exclusion of non-macOS Rust failures."""

import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("alert", Path(__file__).with_name("alert-macos-main.py"))
alert = importlib.util.module_from_spec(spec)
spec.loader.exec_module(alert)


class AlertTests(unittest.TestCase):
    def run_alert(self, names, issues=(), labels=(), kind="rust"):
        def respond(*args):
            if args[0] == "api":
                return json.dumps([{"jobs": [{"name": name, "conclusion": "failure",
                                             "html_url": "https://example.com/job"} for name in names]}])
            if args[:2] == ("label", "list"):
                return json.dumps(labels)
            if args[:2] == ("issue", "list"):
                return json.dumps(issues)
            return ""
        with patch.object(alert, "gh", side_effect=respond) as gh:
            alert.alert("owner/repo", "123", "2", kind, "https://github.com")
        return [call.args for call in gh.call_args_list]

    def test_linux_and_windows_failures_do_not_alert(self):
        calls = self.run_alert(["rust (linux)", "rust (windows)"])
        self.assertEqual(len(calls), 1)

    def test_create_label_and_rolling_issue(self):
        calls = self.run_alert(["rust (macos)"])
        self.assertTrue(any(call[:2] == ("label", "create") for call in calls))
        create = next(call for call in calls if call[:2] == ("issue", "create"))
        self.assertIn("ci-macos-main", create)
        self.assertIn("macOS CI failure on main: rust (macos)", create)
        self.assertIn("rust (macos)", create[-1])
        self.assertIn("/actions/runs/123/attempts/2", create[-1])

    def test_repeated_rust_failure_comments_on_existing_rolling_issue(self):
        calls = self.run_alert(["rust (macos)"],
                               [{"number": 41, "title": "macOS CI failure on main: rust (macos)"}])
        comment = next(call for call in calls if call[:2] == ("issue", "comment"))
        self.assertEqual(comment[2], "41")
        self.assertIn("/actions/runs/123/attempts/2", comment[-1])
        self.assertFalse(any(call[:2] == ("issue", "create") for call in calls))

    def test_native_and_rust_rolling_issues_stay_separate(self):
        calls = self.run_alert(["rust (macos)"],
                               [{"number": 41, "title": "macOS CI failure on main: Native macOS bundle"}])
        create = next(call for call in calls if call[:2] == ("issue", "create"))
        self.assertIn("macOS CI failure on main: rust (macos)", create)
        self.assertFalse(any(call[:2] == ("issue", "comment") for call in calls))

    def test_native_precheck_failure_identifies_workflow(self):
        calls = self.run_alert(["linux-precheck"], kind="native")
        create = next(call for call in calls if call[:2] == ("issue", "create"))
        self.assertIn("macOS CI failure on main: Native macOS bundle", create)
        self.assertIn("Native macOS bundle workflow failed on main", create[-1])
        self.assertIn("linux-precheck", create[-1])

    def test_comment_existing_issue_for_native_failure(self):
        calls = self.run_alert(["bundle (macos-26)"],
                               [{"number": 42, "title": "macOS CI failure on main: Native macOS bundle"}],
                               [{"name": "ci-macos-main"}], "native")
        comment = next(call for call in calls if call[:2] == ("issue", "comment"))
        self.assertEqual(comment[2], "42")
        self.assertFalse(any(call[:2] == ("issue", "create") for call in calls))
        self.assertTrue(any(call[:2] == ("label", "create") and "--force" in call for call in calls))


if __name__ == "__main__":
    unittest.main()
