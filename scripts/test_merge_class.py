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
            # The merge gate keeps edits to this rule with the owner.
            "rust/rules/runtime-process-command.yml",
            # Owner decision 2026-11-11: the release signing key and the repository Git hooks.
            "cosign.pub",
            ".githooks/pre-commit",
        ]:
            self.assertEqual(mc.path_class(path, self.classes), "owner", path)

    def test_sensitive_paths(self):
        for path in [
            "rust/rules/no-unwrap-expect.yml",
            "rust/scripts/check-ast-grep.py",
            # The checkers' tests merge as sensitive PRs too (merge gate).
            "rust/scripts/test_ast_grep.py",
            "rust/scripts/test_clippy_limits.py",
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
        self.assertEqual(set(data) - {"_about"}, {"owner", "sensitive", "scorer_authors", "scorer_seats", "vendors"})
        self.assertEqual(data["vendors"], ["anthropic", "openai", "xai"])
        self.assertEqual(data["scorer_authors"], ["poitee"])
        self.assertEqual(data["scorer_seats"], {"claude-opus-5-5": "anthropic", "grok-4.7": "xai"})


class ScoreTests(unittest.TestCase):
    # Chad's policy of 2026-11-11: two allowed seats from two different vendors, neither the author's.
    def setUp(self):
        data = json.loads(mc.CLASSES.read_text())
        self.allowed, self.vendors = data["scorer_seats"], data["vendors"]

    def line(self, seat="claude-opus-5-5", sha=SHA, score=9, vendor="anthropic", author="openai"):
        return f"pp-score head={sha} seat={seat} vendor={vendor} score={score} author={author}"

    def grok(self, **kwargs):
        return self.line("grok-4.7", vendor="xai", **kwargs)

    def check(self, comments, author="openai", head=SHA, allowed=None, vendors=None):
        return mc.has_two_scores(comments, head, author, self.allowed if allowed is None else allowed,
                                 self.vendors if vendors is None else vendors)

    def test_opus_and_grok_pass_on_a_sol_pr(self):
        self.assertTrue(self.check([self.line(), self.grok()]))

    def test_sol_seat_on_a_sol_pr_refused(self):
        self.assertFalse(self.check([self.line(), self.line("gpt-6.1-sol", vendor="openai")]))
        # Even when Sol is an allowed seat, its record on an OpenAI-authored PR refuses.
        allowed = {**self.allowed, "gpt-6.1-sol": "openai"}
        self.assertFalse(self.check([self.line(), self.grok(), self.line("gpt-6.1-sol", vendor="openai")],
                                    allowed=allowed))

    def test_two_opus_seats_refused(self):
        self.assertFalse(self.check([self.line(), self.line(score=10)]))
        # Two different seats from one vendor do not make two vendors.
        allowed = {"claude-opus-5-5": "anthropic", "claude-fable-5-2": "anthropic"}
        self.assertFalse(self.check([self.line(), self.line("claude-fable-5-2")], allowed=allowed))

    def test_seat_list_is_data(self):
        # A later swap is a one-line change to scorer_seats.
        allowed = {"gpt-6.1-sol": "openai", "grok-4.7": "xai"}
        self.assertTrue(self.check([self.line("gpt-6.1-sol", vendor="openai", author="anthropic"),
                                    self.grok(author="anthropic")], author="anthropic", allowed=allowed))

    def test_new_vendor_is_data_only(self):
        allowed = {**self.allowed, "gemini-9-pro": "google"}
        records = [self.line(), self.line("gemini-9-pro", vendor="google")]
        self.assertTrue(self.check(records, allowed=allowed, vendors=self.vendors + ["google"]))
        # Without the vendor in the data, the record does not parse and refuses.
        self.assertFalse(self.check(records, allowed=allowed))

    def test_author_vendor_seat_refused(self):
        self.assertFalse(self.check([self.line(author="anthropic"), self.grok(author="anthropic")],
                                    author="anthropic"))

    def test_older_head_records_from_dropped_seats_do_not_block(self):
        # A seat-list change must not block a PR whose earlier heads were scored by a dropped seat.
        old = self.line("gpt-6.1-sol", sha=OTHER, vendor="openai", author="anthropic")
        self.assertTrue(self.check([old, self.line(), self.grok()]))
        self.assertFalse(self.check(["pp-score head=" + OTHER, self.line(), self.grok()]))

    def test_stale_head_does_not_count(self):
        self.assertFalse(self.check([self.line(), self.grok(sha=OTHER)]))

    def test_missing_or_unknown_author_vendor_refused(self):
        for author in (None, "", "unknown"):
            self.assertFalse(self.check([self.line(), self.grok()], author=author))

    def test_invalid_record_refuses_even_with_two_good_scores(self):
        good = [self.line(), self.grok()]
        for bad in (
            self.line(score=8), self.line(author="anthropic"),
            self.line("unknown"), self.line(vendor="xai"),
            self.line(vendor="unknown"), self.line(author="unknown"),
            self.line().replace(" author=openai", ""),
            self.line().replace(" vendor=anthropic", ""),
            self.line().replace("score=9", "score=nine"),
            self.line().replace("score=9", "score=" + "9" * 5000),
            self.line().replace("head=" + SHA, "head=abc"),
            self.line() + " extra", "pp-score head=" + SHA,
            self.line("gpt-6.1-sol", vendor="openai"),
            None,
        ):
            with self.subTest(bad=bad):
                self.assertFalse(self.check(good + [bad]))

    def test_all_unapproved_seats_refused(self):
        for seat in ("opencode/pulse-alpha-free", "cursor/grok-4.7", "gpt-6.1-sol", "pi/halo/qwen38-flash"):
            self.assertFalse(self.check([self.line(), self.line(seat)]))

    def test_head_must_be_full_sha(self):
        self.assertFalse(self.check([self.line(), self.grok()], head="abc"))

    def test_prose_and_valid_stale_scores_are_ignored(self):
        self.assertTrue(self.check(["Review complete.", self.line(sha=OTHER), self.line(), self.grok()]))
        # A record quoted in prose, a list bullet or backticks is not a record.
        for prose in ("prose " + self.line(), "- " + self.line(), "`" + self.line() + "`",
                      "Post the pp-score line after the run."):
            with self.subTest(prose=prose):
                self.assertTrue(self.check([prose, self.line(), self.grok()]))


if __name__ == "__main__":
    unittest.main()
