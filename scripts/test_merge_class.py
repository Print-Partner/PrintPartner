import json
import unittest
from pathlib import Path

from scripts import merge_class as mc

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
        self.assertEqual(set(data) - {"_about"}, {"owner", "sensitive", "scorer_authors", "scorer_seats"})
        self.assertEqual(data["scorer_authors"], ["poitee"])
        self.assertEqual(data["scorer_seats"], {
            "gpt-6.1-sol": "openai", "gpt-6-astra": "openai",
            "claude-fable-5-2": "anthropic", "claude-opus-5-5": "anthropic",
        })


class ScoreTests(unittest.TestCase):
    def setUp(self):
        self.allowed = json.loads(mc.CLASSES.read_text())["scorer_seats"]

    def line(self, seat="gpt-6.1-sol", sha=SHA, score=9, vendor="openai", author="anthropic"):
        return f"pp-score head={sha} seat={seat} vendor={vendor} score={score} author={author}"

    def check(self, comments, author="anthropic", head=SHA):
        return mc.has_two_scores(comments, head, author, self.allowed)

    def test_two_openai_seats_pass_for_anthropic(self):
        self.assertTrue(self.check([self.line(), self.line("gpt-6-astra")]))

    def test_two_anthropic_seats_pass_for_openai(self):
        self.assertTrue(self.check([
            self.line(seat, vendor="anthropic", author="openai")
            for seat in ("claude-fable-5-2", "claude-opus-5-5")
        ], author="openai"))

    def test_duplicate_seat_counts_once(self):
        self.assertFalse(self.check([self.line(), self.line(score=10)]))

    def test_stale_head_does_not_count(self):
        self.assertFalse(self.check([self.line(), self.line("gpt-6-astra", sha=OTHER)]))

    def test_missing_or_unknown_author_vendor_refused(self):
        for author in (None, "", "unknown"):
            self.assertFalse(self.check([self.line(), self.line("gpt-6-astra")], author=author))

    def test_invalid_record_refuses_even_with_two_good_scores(self):
        good = [self.line(), self.line("gpt-6-astra")]
        for bad in (
            self.line(score=8), self.line(author="openai"),
            self.line("unknown"), self.line(vendor="anthropic"),
            self.line(vendor="unknown"), self.line(author="unknown"),
            self.line().replace(" author=anthropic", ""),
            self.line().replace(" vendor=openai", ""),
            self.line().replace("score=9", "score=nine"),
            self.line().replace("score=9", "score=" + "9" * 5000),
            self.line().replace("head=" + SHA, "head=abc"),
            "prose " + self.line(), self.line() + " extra",
            self.line("claude-opus-5-5", vendor="anthropic", author="openai"),
            None,
        ):
            with self.subTest(bad=bad):
                self.assertFalse(self.check(good + [bad]))

    def test_all_unapproved_seats_refused(self):
        for seat in ("opencode/pulse-alpha-free", "cursor/grok-4.7", "grok-4.7", "pi/halo/qwen38-flash"):
            self.assertFalse(self.check([self.line(), self.line(seat)]))

    def test_head_must_be_full_sha(self):
        self.assertFalse(self.check([self.line(), self.line("gpt-6-astra")], head="abc"))

    def test_prose_and_valid_stale_scores_are_ignored(self):
        self.assertTrue(self.check(["Review complete.", self.line(sha=OTHER),
                                   self.line(), self.line("gpt-6-astra")]))


if __name__ == "__main__":
    unittest.main()
