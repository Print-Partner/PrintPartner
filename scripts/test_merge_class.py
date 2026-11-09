import json
import unittest
from pathlib import Path

import merge_class as mc

ROOT = Path(__file__).resolve().parent.parent
SHA = "a" * 40
OTHER = "b" * 40


class ClassifyTests(unittest.TestCase):
    def setUp(self):
        self.classes = mc.load_classes()

    def test_owner_paths(self):
        for path in [
            ".github/workflows/review-gate.yml",
            "scripts/agent-guard.py",
            "docs/development/definition-of-done.md",
            "rust/deny.toml",
            "rust/scripts/check-macos-bundle.py",
            "scripts/linear/pr_status_sync.py",
        ]:
            self.assertEqual(mc.path_class(path, self.classes), "owner", path)

    def test_sensitive_paths(self):
        for path in [
            "rust/rules/runtime-process-command.yml",
            "rust/scripts/check-ast-grep.py",
            "rust/ast-grep-baseline.json",
            "rust/crates/pp-api/snapshots/route.snap",
            "scripts/duplication-baseline.json",
        ]:
            self.assertEqual(mc.path_class(path, self.classes), "sensitive", path)

    def test_lane_paths(self):
        for path in ["rust/crates/pp-storage/src/lib.rs", "web/apps/ui/src/App.tsx", "docs/families.md"]:
            self.assertEqual(mc.path_class(path, self.classes), "lane", path)

    def test_pr_takes_most_restrictive_class(self):
        self.assertEqual(mc.classify(["src/a.rs", "rust/rules/x.yml"], self.classes), "sensitive")
        self.assertEqual(mc.classify(["src/a.rs", ".github/workflows/x.yml"], self.classes), "owner")
        self.assertEqual(mc.classify(["src/a.rs"], self.classes), "lane")
        self.assertEqual(mc.classify([], self.classes), "lane")

    def test_double_star_crosses_directories(self):
        rx = mc.glob_regex(".github/**")
        self.assertTrue(rx.match(".github/CODEOWNERS"))
        self.assertTrue(rx.match(".github/workflows/a/b.yml"))
        self.assertFalse(mc.glob_regex("scripts/*.py").match("scripts/linear/x.py"))


class CoverageTests(unittest.TestCase):
    def test_every_codeowners_entry_is_classified(self):
        classes = mc.load_classes()
        entries = mc.codeowners_entries((ROOT / ".github" / "CODEOWNERS").read_text(encoding="utf-8"))
        self.assertEqual(mc.uncovered(entries, classes), [])

    def test_unlisted_owned_path_is_reported(self):
        classes = mc.load_classes()
        self.assertEqual(mc.uncovered(["brand/new-owned.py"], classes), ["brand/new-owned.py"])

    def test_codeowners_parsing_skips_comments_and_blank_lines(self):
        text = "# comment\n\n/rust/scripts/check-* @poitee\n/.github/ @poitee\n"
        self.assertEqual(mc.codeowners_entries(text), ["rust/scripts/check-*", ".github/"])

    def test_merge_classes_file_is_valid_json(self):
        data = json.loads((ROOT / "scripts" / "merge-classes.json").read_text(encoding="utf-8"))
        self.assertEqual(set(data) - {"_about"}, {"owner", "sensitive"})


class ScoreTests(unittest.TestCase):
    def line(self, sha, seat, score):
        return f"pp-score head={sha} seat={seat} score={score}"

    def test_two_seats_at_nine_on_the_head_pass(self):
        comments = [self.line(SHA, "alpha", 9), self.line(SHA, "sol", 10)]
        self.assertTrue(mc.has_two_scores(comments, SHA))

    def test_one_seat_twice_does_not_count_twice(self):
        comments = [self.line(SHA, "alpha", 9), self.line(SHA, "alpha", 10)]
        self.assertFalse(mc.has_two_scores(comments, SHA))

    def test_scores_below_nine_do_not_count(self):
        comments = [self.line(SHA, "alpha", 8), self.line(SHA, "sol", 8)]
        self.assertFalse(mc.has_two_scores(comments, SHA))

    def test_scores_on_another_head_do_not_count(self):
        comments = [self.line(OTHER, "alpha", 9), self.line(SHA, "sol", 9)]
        self.assertFalse(mc.has_two_scores(comments, SHA))

    def test_score_must_be_its_own_line(self):
        comments = ["looks good. " + self.line(SHA, "alpha", 9), self.line(SHA, "sol", 9)]
        self.assertFalse(mc.has_two_scores(comments, SHA))

    def test_head_must_be_a_full_sha(self):
        self.assertFalse(mc.has_two_scores([self.line("abc", "a", 9), self.line("abc", "b", 9)], "abc"))


if __name__ == "__main__":
    unittest.main()
