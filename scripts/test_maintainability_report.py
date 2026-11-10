import contextlib
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import unittest.mock

SCRIPT = Path(__file__).with_name("maintainability-report.py")
SPEC = importlib.util.spec_from_file_location("maintainability_report", SCRIPT)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)

GATE = " " * 20 + '#[cfg(feature = "desktop_rebuild")]'
ARM = " " * 20


class EnvelopeArmTests(unittest.TestCase):
    def test_body_lines_exclude_the_braces_of_a_braced_arm(self):
        block = "\n".join([
            GATE,
            ARM + "Envelope::OneLine { command, reply } => reply.send(one(command)),",
            GATE,
            ARM + "Envelope::OneStatement { command, reply } => {",
            ARM + "    let _ = reply.send(one(command));",
            ARM + "}",
            GATE,
            ARM + "Envelope::Wrapped { command, reply } => {",
            ARM + "    let _ = reply.send(one(",
            ARM + "        command,",
            ARM + "    ));",
            ARM + "}",
            ARM + "Envelope::Ungated { command } => {",
            ARM + "    first(command);",
            ARM + "    second();",
            ARM + "}",
        ])
        self.assertEqual(REPORT.envelope_arms(block), [
            ("OneLine", 1, True),
            ("OneStatement", 1, True),
            ("Wrapped", 3, True),
            ("Ungated", 2, False),
        ])

    def test_rows_escape_pipes_and_keep_three_cells(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            REPORT.row("a | b", "1 | 2", "grep x | wc -l")
        line = output.getvalue().strip()
        self.assertEqual(line, "| a \\| b | 1 \\| 2 | `grep x \\| wc -l` |")
        self.assertEqual(len(REPORT.UNESCAPED_PIPE.findall(line)), 4)


# No user or system git config and no inherited GIT_* variable, so a hook or alias cannot reach
# the fixture, and the fixture cannot reach an outer repository.
ISOLATED = {**{k: v for k, v in os.environ.items() if not k.startswith("GIT_")},
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}


def git(root, *args, date="2026-01-01T00:00:00Z"):
    env = {**os.environ, "GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date}
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                    "-c", "commit.gpgsign=false", *args], cwd=root, check=True, capture_output=True, env=env)


def write(root, path, text):
    target = Path(root, path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text)


class CompareTests(unittest.TestCase):
    """`--base` on a throwaway repository: one commit, then one ticket-named test file."""

    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="pp-report-")
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        patch = unittest.mock.patch.dict(os.environ, ISOLATED, clear=True)
        patch.start()
        self.addCleanup(patch.stop)
        git(self.root, "init", "-q")
        write(self.root, "rust/crates/pp-core/src/lib.rs", "pub fn a() {}\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "base")

    def report(self, *args):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            REPORT.main(["--root", self.root, *args])
        return output.getvalue()

    def test_prints_only_the_rows_a_ticket_named_file_changes(self):
        write(self.root, "rust/crates/pp-core/tests/ticket_t_9_flow.rs", "#[test]\nfn flow() {}\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "head")
        lines = self.report("--base", "HEAD~1").splitlines()
        self.assertTrue(lines[0].startswith("Compared `"), lines[0])
        self.assertIn("(`HEAD~1`) with", lines[0])
        self.assertEqual(lines[2:4], ["| Measure | Base | Head | Change |", "| --- | --- | --- | --- |"])
        self.assertEqual(lines[4:], [
            "| Rust lines, tracked | 1 | 3 | +2 |",
            "| Rust files named after tickets (in src/); in retiring crates; per crate "
            "| 0 (0); 0;  | 1 (0); 0; pp-core 1 | values differ |",
            "| Rust files named after tickets in crates that stay | 0 | 1 | +1 |",
            "| Largest Rust files | src/lib.rs 1 | ticket_t_9_flow.rs 2; src/lib.rs 1 | values differ |",
        ])

    def test_history_windows_end_at_the_head_commit_date(self):
        write(self.root, "rust/crates/pp-storage/src/lib.rs", "pub fn b() {}\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "lib", date="2026-01-02T00:00:00Z")
        write(self.root, "docs/page.md", "# Page\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "docs", date="2026-03-01T00:00:00Z")
        # Each revision's own window would count the January lib.rs commit for the base only.
        rows = [line.split(" | ")[0] for line in self.report("--base", "HEAD~1").splitlines()[4:]]
        self.assertEqual(rows, ["| Tracked Markdown pages / docs pages / docs named after tickets outside docs/records"])

    def test_an_unchanged_revision_prints_no_table(self):
        output = self.report("--rev", "HEAD", "--base", "HEAD")
        self.assertTrue(output.endswith("\n\nNo measure changed.\n"), output)
        self.assertNotIn("| Measure", output)

    def test_without_base_the_report_keeps_its_value_table(self):
        lines = self.report("--rev", "HEAD").splitlines()
        self.assertTrue(lines[0].startswith("Measured at `"), lines[0])
        self.assertEqual(lines[2], "| Measure | Value | How |")
        self.assertIn("| Rust lines, tracked | 1 | `all rust/crates/**/*.rs at the revision, newline-counted` |",
                      lines)

    def test_base_refuses_live_samples(self):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            REPORT.parse_args(["--base", "origin/main", "--ci"])
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            REPORT.parse_args(["--base", "origin/main", "1626"])
        self.assertEqual(REPORT.parse_args(["--base", "x"])[0].rev, "HEAD")
        self.assertEqual(REPORT.parse_args([])[0].rev, "origin/main")


class ChangeTests(unittest.TestCase):
    def test_numbers_in_the_same_text_get_signed_differences(self):
        self.assertEqual(REPORT.change("1,176", "1,177"), "+1")
        self.assertEqual(REPORT.change("103,098 / 41,146 / 236", "103,299 / 41,307 / 238"), "+201 / +161 / +2")
        self.assertEqual(REPORT.change("784 to 894", "785 to 893"), "+1 to -1")
        self.assertEqual(REPORT.change("2.5", "1"), "-1.5")
        self.assertEqual(REPORT.change("1.0", "2.5"), "+1.5")
        self.assertEqual(REPORT.change("2,000", "500"), "-1,500")
        self.assertEqual(REPORT.change("3.3", "1.3"), "-2")
        self.assertEqual(REPORT.change("pp-core 9, pp-drivers 9", "pp-core 10, pp-drivers 9"),
                         "pp-core +1, pp-drivers +0")

    def test_digits_inside_names_are_not_numbers(self):
        self.assertEqual(REPORT.change("gre_1309_x.rs 2", "gre_1309_x.rs 3"), "gre_1309_x.rs +1")
        self.assertEqual(REPORT.change("GRE-612 receipt 40", "GRE-612 receipt 41"), "GRE-612 receipt +1")
        self.assertEqual(REPORT.change("yes: ci-path-filter.py:30", "yes: ci-path-filter.py:31"), None)

    def test_other_cells_have_no_difference(self):
        self.assertIsNone(REPORT.change("none", "docs/a.md"))
        self.assertIsNone(REPORT.change("not found", "not found"))
        self.assertIsNone(REPORT.change("3 (a)", "4 (a, b)"))


if __name__ == "__main__":
    unittest.main()
