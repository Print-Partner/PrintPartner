#!/usr/bin/env python3
import json
import sys
import unittest
import urllib.parse
from unittest import mock

import merge_class
import post_pp_score_status as pps

SHA = "d3a9756ded5a914769c8a64680b40f97f0b4a490"
OTHER = "c6cf084d12604d7a24d519c176e1574fa8829d42"


def comment(body, user="poitee", created="2026-11-13T01:51:14Z", updated=None):
    return {
        "user": {"login": user},
        "body": body,
        "created_at": created,
        "updated_at": updated or created,
    }


class PostPpScoreStatusTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def test_pp_score_lines_plan_success_at_eight(self):
        comments = [
            comment(
                "pp-score head="
                f"{SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none\n"
                "pp-score head="
                f"{SHA} seat=glm-5.3-flash-free vendor=zhipu score=9 author=anthropic obstacles=none\n"
            )
        ]
        records = pps.read_comments(comments, self.trust)
        plans = pps.plan_statuses(records)
        self.assertEqual(len(plans), 1)
        self.assertEqual(plans[0].head, SHA)
        self.assertEqual(plans[0].state, "success")
        self.assertEqual(
            plans[0].description,
            "9/10 from grok-4.7; 9/10 from glm-5.3-flash-free",
        )
        call = pps.format_call("Print-Partner/PrintPartner", plans[0])
        self.assertEqual(
            call,
            "gh api -X POST "
            f"repos/Print-Partner/PrintPartner/statuses/{SHA} "
            "-f state=success -f context=pp/score "
            "-f 'description=9/10 from grok-4.7; 9/10 from glm-5.3-flash-free'",
        )

    def test_score_below_eight_is_failure(self):
        comments = [
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=7 author=anthropic obstacles=none")
        ]
        plans = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual(plans[0].state, "failure")
        self.assertEqual(plans[0].description, "rejected: grok-4.7 scored 7")

    def test_legacy_score_line_is_ignored(self):
        # The old "Score: N/10 from <seat>" form names no author family, so it cannot pass.
        comments = [comment(f"Score: 8/10 from claude-opus-5-5 on head {SHA}")]
        self.assertEqual(pps.read_comments(comments, self.trust), pps.Reading())

    def test_untrusted_authors_are_ignored_and_an_edited_record_fails_its_head(self):
        comments = [
            comment(
                f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none",
                user="stranger",
            ),
            comment(
                f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none",
                created="2026-11-13T01:51:14Z",
                updated="2026-11-13T02:00:00Z",
            ),
        ]
        self.assertEqual(pps.read_comments(comments[:1], self.trust), pps.Reading())
        (plan,) = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual((plan.state, plan.description),
                         ("failure", "rejected: the grok-4.7 record was edited after it was posted"))
        self.assertEqual(pps.plan_statuses(pps.Reading()), [])

    def test_prose_is_ignored_and_an_unknown_seat_fails_its_head(self):
        comments = [
            comment(
                "a note about pp-score head= tokens in prose\n"
                f"pp-score head={SHA} seat=unknown vendor=xai score=9 author=anthropic obstacles=none\n"
                f"> SCORE: 9\n"
                f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none\n"
            )
        ]
        found = pps.read_comments(comments, self.trust)
        self.assertEqual([(r.head, r.seat) for r in found.records], [(SHA, "grok-4.7")])
        (plan,) = pps.plan_statuses(found)
        self.assertEqual((plan.state, plan.description),
                         ("failure", "rejected: unknown is not an allowed seat for vendor=xai"))

    def test_records_stay_on_their_own_sha(self):
        comments = [
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"),
            comment(f"pp-score head={OTHER} seat=grok-4.7 vendor=xai score=6 author=anthropic obstacles=none"),
        ]
        plans = {p.head: p for p in pps.plan_statuses(pps.read_comments(comments, self.trust))}
        self.assertEqual(plans[SHA].state, "success")
        self.assertEqual(plans[OTHER].state, "failure")

    def test_minimum_score_sets_state(self):
        comments = [
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"),
            comment(
                f"pp-score head={SHA} seat=glm-5.3-flash-free vendor=zhipu score=5 author=anthropic obstacles=none"
            ),
        ]
        plans = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual(plans[0].state, "failure")
        self.assertEqual(plans[0].description, "rejected: glm-5.3-flash-free scored 5")

    def test_comments_come_from_every_slurped_page(self):
        # The guard's fetch: `gh api --paginate --slurp` prints one JSON array of pages. Page 2 decides.
        good = f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"
        low = good.replace("seat=grok-4.7 vendor=xai score=9", "seat=glm-5.3-flash-free vendor=zhipu score=5")
        for pages, state in (([[comment("intro")], [comment(good)]], "success"),
                             ([[comment(good)], [comment(low)]], "failure"), ([[comment(good)], {"page": 2}], None)):
            with self.subTest(state=state), mock.patch.object(pps.subprocess, "check_output",
                                                             return_value=json.dumps(pages)) as gh:
                if state is None:
                    self.assertRaises(ValueError, pps.fetch_comments, "o/r", 7)
                    continue
                (plan,) = pps.plan_statuses(pps.read_comments(pps.fetch_comments("o/r", 7), self.trust))
                self.assertEqual((plan.state, gh.call_args[0][0][:4]), (state, ["gh", "api", "--paginate", "--slurp"]))

    def test_a_null_comment_still_posts_the_failure(self):
        good = f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"
        posted = []
        with mock.patch.object(pps, "fetch_comments", return_value=[None, comment(good)]), \
                mock.patch.object(pps, "fetch_head", return_value=SHA), mock.patch.object(pps, "check_state"), \
                mock.patch.object(pps, "apply_plan", side_effect=lambda repo, plan: posted.append(plan)), \
                mock.patch("sys.stdout"):
            self.assertEqual(pps.main(["--pr", "7", "--post"]), 0)
        self.assertEqual([(p.head, p.state) for p in posted], [(SHA, "failure")])

    def test_dry_run_render_leaves_unscored_prs_quiet(self):
        import io
        from contextlib import redirect_stdout

        buf = io.StringIO()
        with redirect_stdout(buf):
            code = pps.render("Print-Partner/PrintPartner", 1827, [], post=False)
        self.assertEqual(code, 0)
        self.assertIn("unscored: Print-Partner/PrintPartner#1827", buf.getvalue())


class RenderCaptureTests(unittest.TestCase):
    def test_render_prints_the_api_call_and_skips_post(self):
        plan = pps.StatusPlan(
            SHA,
            "success",
            "9/10 from grok-4.7",
            (pps.Record(SHA, "grok-4.7", "xai", "anthropic", 9.0, ()),),
        )
        posted = []

        def fake_apply(repo, item):
            posted.append((repo, item))

        original = pps.apply_plan
        pps.apply_plan = fake_apply
        try:
            import io
            from contextlib import redirect_stdout

            buf = io.StringIO()
            with redirect_stdout(buf):
                code = pps.render("Print-Partner/PrintPartner", 1765, [plan], post=False)
        finally:
            pps.apply_plan = original
        self.assertEqual(code, 0)
        self.assertEqual(posted, [])
        self.assertIn(
            f"gh api -X POST repos/Print-Partner/PrintPartner/statuses/{SHA}",
            buf.getvalue(),
        )
        self.assertIn("-f state=success -f context=pp/score", buf.getvalue())


# Real comment bodies, copied from Print-Partner/PrintPartner (trimmed prose kept verbatim).
PR1777_HEAD = "b8136d36e5eaf3a2244351b6e54fb8548a364145"
PR1777_BODY = (
    "> [!NOTE]\n"
    "> \U0001f916 **Claude Opus 5.5 responding on behalf of poitee**\n"
    "\n"
    "Grok 4.7 seat on the symlink-fix head `b8136d36e`: **10, no findings.**\n"
    "\n"
    "The second seat (Sol 6.1) is waiting for Codex to be signed in again.\n"
    "\n"
    "`pp-score head=b8136d36e5eaf3a2244351b6e54fb8548a364145 seat=grok-4.7 vendor=xai score=10 author=anthropic`"
)
PR1734_ED251_HEAD = "ed251f839d07f27f812291b2d8ffe9fdf0a2a464"
PR1734_ED251_BODY = (
    "> [!NOTE]\n"
    "> \U0001f916 **Claude Opus 5.5 responding on behalf of poitee**\n"
    "\n"
    "Grok 4.7 seat on the P1-fix and main-merge head `ed251f839`: 10, no findings.\n"
    "\n"
    "The second seat (OpenCode) is still out of quota.\n"
    "\n"
    "`pp-score head=ed251f839d07f27f812291b2d8ffe9fdf0a2a464 seat=grok-4.7 vendor=xai score=10 author=anthropic`"
)
PR1734_765_HEAD = "765984a2cdbace387421c1de92bc25427b178b29"
PR1734_765_BODY = (
    "pp-score head=765984a2cdbace387421c1de92bc25427b178b29 seat=grok-4.7 vendor=xai score=10 author=anthropic\n"
    "pp-score head=765984a2cdbace387421c1de92bc25427b178b29 seat=glm-5.3-flash-free vendor=zhipu score=9 author=anthropic\n"
)


class RealFormatTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def test_real_records_without_obstacles_fail_their_head(self):
        for body in (PR1777_BODY, PR1734_ED251_BODY, PR1734_765_BODY):
            with self.subTest(body=body[-60:]):
                found = pps.read_comments([comment(body)], self.trust)
                self.assertEqual(found.records, ())
                (plan,) = pps.plan_statuses(found)
                self.assertEqual(plan.state, "failure")
                self.assertRegex(plan.description, r"^rejected: the [a-z0-9.-]+ record has no obstacles=$")

    def test_backticked_records_from_1777_and_1734_are_read_but_never_count(self):
        # GRE-1859: the sensitive merge check never counted a wrapped record. Both evaluators now
        # read it, so a low or invalid one fails its head, and neither counts it toward a pass.
        for body, head in ((PR1777_BODY, PR1777_HEAD), (PR1734_ED251_BODY, PR1734_ED251_HEAD)):
            with self.subTest(head=head):
                body = body.replace("author=anthropic`", "author=anthropic obstacles=none`")
                found = pps.read_comments([comment(body)], self.trust)
                self.assertEqual([(r.head, r.seat, r.score, r.bare) for r in found.records],
                                 [(head, "grok-4.7", 10, False)])
                (plan,) = pps.plan_statuses(found)
                self.assertEqual((plan.state, plan.description),
                                 ("failure", "rejected: a wrapped or indented record never counts"))

    def test_bare_records_from_1734_parse_with_obstacles(self):
        body = PR1734_765_BODY.replace("author=anthropic\n", "author=anthropic obstacles=none\n")
        records = pps.read_comments([comment(body)], self.trust).records
        self.assertEqual({r.head for r in records}, {PR1734_765_HEAD})
        self.assertEqual([r.seat for r in records], ["grok-4.7", "glm-5.3-flash-free"])

    def test_fenced_and_double_backticked_records_parse(self):
        # A fenced line at column 0 is a bare record; the others are wrapped.
        line = f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"
        for body, bare in ((f"```text\n{line}\n```", True), (f"``{line}``", False), (f"  `{line}`  ", False),
                           (f"```\n  {line}\n```", False)):
            with self.subTest(body=body):
                found = pps.read_comments([comment(body)], self.trust)
                self.assertEqual([(r.seat, r.bare) for r in found.records], [("grok-4.7", bare)])
                self.assertEqual(pps.plan_statuses(found)[0].state, "success" if bare else "failure")

    def test_every_line_main_faulted_is_a_fault_here(self):
        # origin/main d01b7a735's guard (has_two_scores): a body line that starts with "pp-score " must
        # fullmatch the record pattern, or it refuses every head; it ignores any other line.
        main = merge_class.score_pattern(CONFIG["vendors"])
        cases = {"pp-score": "ignore", "pp-score ": "fault", "pp-score\t": "ignore", "pp-score  ": "fault",
                 "pp-score\r\n": "ignore", "pp-score \r\n": "fault", "pp-score head=": "fault", "pp-scorex": "ignore",
                 "pp-score-foo": "ignore", " pp-score": "ignore", "PP-SCORE ": "ignore", "pp-score\u00a0": "ignore",
                 "pp-score\u200b": "ignore", "pp-score \u00a0": "fault"}
        for line, expected in cases.items():
            for body in (line, f"Scores:\n{line}"):
                faults = [x for x in body.splitlines() if x.startswith("pp-score ") and not main.fullmatch(x)]
                with self.subTest(body=body):
                    self.assertEqual("fault" if faults else "ignore", expected)
                    fault = pps.Reading(faults=((None, "a malformed pp-score record"),))
                    self.assertEqual(pps.read_comments([comment(body)], self.trust), fault if faults else pps.Reading())

    def test_records_inside_prose_still_do_not_parse(self):
        line = f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"
        for body in (f"Recorded `{line}` today.", f"`{line}` and more", f"> {line}", f"`{line}"):
            with self.subTest(body=body):
                self.assertEqual(pps.read_comments([comment(body)], self.trust), pps.Reading())


class FamilyTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def test_same_family_score_fails_the_head(self):
        comments = [comment(
            f"pp-score head={SHA} seat=claude-opus-5-5 vendor=anthropic score=10 author=anthropic obstacles=none"
        )]
        (plan,) = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual(plan.state, "failure")
        self.assertEqual(plan.description,
                         "rejected: claude-opus-5-5 is the author's own family (anthropic)")

    def test_same_family_score_fails_even_beside_a_passing_seat(self):
        comments = [comment(
            f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=10 author=anthropic obstacles=none\n"
            f"pp-score head={SHA} seat=claude-opus-5-5 vendor=anthropic score=10 author=anthropic obstacles=none"
        )]
        (plan,) = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual(plan.state, "failure")

    def test_codex_scoring_a_codex_pr_fails(self):
        comments = [comment(f"pp-score head={SHA} seat=gpt-6.1-sol vendor=openai score=9 author=openai obstacles=none")]
        (plan,) = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual(plan.state, "failure")
        self.assertIn("(openai)", plan.description)

    def test_records_that_disagree_on_author_fail(self):
        comments = [comment(
            f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=10 author=anthropic obstacles=none\n"
            f"pp-score head={SHA} seat=glm-5.3-flash-free vendor=zhipu score=9 author=openai obstacles=none"
        )]
        (plan,) = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual((plan.state, plan.description),
                         ("failure", "rejected: records disagree on the author family"))


class CodexSeatTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def test_codex_seat_scores_a_claude_pr(self):
        comments = [comment(f"pp-score head={SHA} seat=gpt-6.1-sol vendor=openai score=9 author=anthropic obstacles=none")]
        (plan,) = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.1-sol"))

    def test_codex_seat_must_name_openai(self):
        comments = [comment(f"pp-score head={SHA} seat=gpt-6.1-sol vendor=xai score=9 author=anthropic obstacles=none")]
        self.assertEqual(pps.read_comments(comments, self.trust).faults,
                         ((SHA, "gpt-6.1-sol is not an allowed seat for vendor=xai"),))

    def test_only_sol_6_2_is_a_sensitive_merge_seat(self):
        import json

        data = json.loads(pps.CLASSES.read_text(encoding="utf-8"))
        # gpt-6.1-sol stays on the status map only. The 7:38 PM ET seats are on both maps.
        added = {"gemini-3.8-flash": "google", "qwen38-gufo": "qwen", "qwen38-flash": "qwen",
                 "longcat-2.5-free": "meituan", "hy4-flash-free": "tencent"}
        self.assertNotIn("gpt-6.1-sol", data["scorer_seats"])
        for seat, vendor in added.items():
            self.assertEqual(data["scorer_seats"][seat], vendor)
            self.assertEqual(data["pp_score_seats"][seat], vendor)
        self.assertEqual(data["pp_score_seats"], {
            "gpt-6.1-sol": "openai", "gpt-6.2-sol": "openai", **added, "gpt-6.2-astra": "openai"})
        # Owner decision 2026-11-18: gpt-6.2-sol and gpt-6.2-astra join the council scorer pool.
        for seat in ("gpt-6.2-sol", "gpt-6.2-astra"):
            self.assertEqual(data["scorer_seats"][seat], "openai")
            self.assertEqual(pps.load_trust()["seats"][seat], "openai")

    def test_astra_seat_scores_a_claude_pr(self):
        line = f"pp-score head={SHA} seat=gpt-6.2-astra vendor=openai score=9 author=anthropic obstacles=none"
        (plan,) = pps.plan_statuses(pps.read_comments([comment(line)], self.trust))
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.2-astra"))
        self.assertEqual(pps.read_comments([comment(line.replace("vendor=openai", "vendor=cursor"))],
                                           self.trust).faults,
                         ((SHA, "gpt-6.2-astra is not an allowed seat for vendor=cursor"),))

    def test_gemini_flash_and_qwen_flash_count_on_an_xai_pr(self):
        for seat, vendor in (("gemini-3.8-flash", "google"), ("qwen38-flash", "qwen")):
            with self.subTest(seat=seat):
                body = (f"pp-score head={SHA} seat={seat} vendor={vendor} "
                        "score=9 author=xai obstacles=none")
                (plan,) = pps.plan_statuses(pps.read_comments([comment(body)], self.trust))
                self.assertEqual((plan.state, plan.description), ("success", f"9/10 from {seat}"))
                wrong = body.replace(f"vendor={vendor}", "vendor=cursor")
                (plan,) = pps.plan_statuses(pps.read_comments([comment(wrong)], self.trust))
                self.assertEqual(plan.description, f"rejected: {seat} is not an allowed seat for vendor=cursor")
                own = body.replace("author=xai", f"author={vendor}")
                (plan,) = pps.plan_statuses(pps.read_comments([comment(own)], self.trust))
                self.assertEqual(plan.state, "failure")

    def test_sol_6_2_seat_scores_a_claude_pr(self):
        comments = [comment(
            f"pp-score head={SHA} seat=gpt-6.2-sol vendor=openai score=9 author=anthropic obstacles=none")]
        (plan,) = pps.plan_statuses(pps.read_comments(comments, self.trust))
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.2-sol"))
        wrong = comments[0]["body"].replace("vendor=openai", "vendor=xai")
        (plan,) = pps.plan_statuses(pps.read_comments([comment(wrong)], self.trust))
        self.assertEqual(plan.state, "failure")
        own = comments[0]["body"].replace("author=anthropic", "author=openai")
        (plan,) = pps.plan_statuses(pps.read_comments([comment(own)], self.trust))
        self.assertEqual(plan.state, "failure")

    def test_pp_score_seat_cannot_override_a_scorer_seat_vendor(self):
        import json
        import tempfile
        from pathlib import Path

        data = json.loads(pps.CLASSES.read_text(encoding="utf-8"))
        data["pp_score_seats"] = {"grok-4.7": "openai"}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "classes.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            with self.assertRaises(ValueError):
                pps.load_trust(path)


class ObstacleTests(unittest.TestCase):
    """A record discloses the refusals, guard refusals and approval stalls its scorer hit."""

    def setUp(self):
        self.trust = pps.load_trust()

    def line(self, obstacles):
        return f"pp-score head={SHA} seat=gpt-6.1-sol vendor=openai score=9 author=anthropic{obstacles}"

    def test_none_counts(self):
        found = pps.read_comments([comment(self.line(" obstacles=none"))], self.trust)
        self.assertEqual(found.records[0].obstacles, ())
        (plan,) = pps.plan_statuses(found)
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.1-sol"))

    def test_missing_field_fails_the_head(self):
        found = pps.read_comments([comment(self.line(""))], self.trust)
        self.assertEqual(found.records, ())
        (plan,) = pps.plan_statuses(found)
        self.assertEqual((plan.head, plan.state, plan.description),
                         (SHA, "failure", "rejected: the gpt-6.1-sol record has no obstacles="))

    def test_listed_obstacles_still_count_as_records(self):
        for token, expected in (("refusal", ("refusal",)), ("guard", ("guard",)),
                                ("approval-stall", ("approval-stall",)),
                                ("guard,refusal", ("guard", "refusal")),
                                ("refusal,guard,approval-stall", ("refusal", "guard", "approval-stall"))):
            with self.subTest(token=token):
                found = pps.read_comments([comment(self.line(f" obstacles={token}"))], self.trust)
                self.assertEqual(found.records[0].obstacles, expected)
                (plan,) = pps.plan_statuses(found)
                self.assertEqual(plan.state, "success")

    def test_invalid_tokens_refuse_every_head(self):
        for token in ("", "None", "NONE", "none,guard", "guard,guard", "guard,", ",guard", "stall",
                      "approval_stall", "guard refusal", "guard;refusal", "yes", "0"):
            with self.subTest(token=token):
                self.assertEqual(pps.read_comments([comment(self.line(f" obstacles={token}"))], self.trust),
                                 MALFORMED)

    def test_field_must_come_last(self):
        line = (f"pp-score head={SHA} seat=gpt-6.1-sol vendor=openai score=9 obstacles=none author=anthropic")
        self.assertEqual(pps.read_comments([comment(line)], self.trust), MALFORMED)
        self.assertEqual(pps.read_comments([comment(self.line(" obstacles=none extra"))], self.trust), MALFORMED)

    def test_whole_number_and_exact_head_rules_still_hold(self):
        for line in (self.line(" obstacles=none").replace("score=9", "score=8.5"),
                     self.line(" obstacles=none").replace(SHA, SHA[:12])):
            with self.subTest(line=line):
                self.assertEqual(pps.read_comments([comment(line)], self.trust), MALFORMED)

    def test_trial_line_is_not_a_record(self):
        line = self.line(" obstacles=none").replace("pp-score ", "pp-score-trial ", 1)
        self.assertEqual(pps.read_comments([comment(line)], self.trust), pps.Reading())


MALFORMED = pps.Reading(faults=((None, "a malformed pp-score record"),))
QUEUE_BASE = "5348fa04c2657f661d489c5dc46fcdcec2df33f0"
QUEUE_HEAD = "7acda503680cc741feb3b8c83426d0eb0bbbba20"


def squash(sha, title):
    return {"sha": sha, "commit": {"message": title + "\n\nbody"}}


def compare(*commits, status="ahead"):
    return {"status": status, "ahead_by": len(commits), "commits": list(commits)}


class MergeGroupTests(unittest.TestCase):
    ref = f"refs/heads/gh-readonly-queue/main/pr-1829-{QUEUE_BASE}"

    def setUp(self):
        self.trust = pps.load_trust()

    def records(self, head, score=9, author="anthropic"):
        return pps.read_comments([comment(
            f"pp-score head={head} seat=grok-4.7 vendor=xai score={score} author={author} obstacles=none"
        )], self.trust)

    def test_queue_prs_reads_the_squash_commits(self):
        payload = compare(squash("a" * 40, "Fix a thing (#999) (#1830)"),
                          squash(QUEUE_HEAD, "Post pp/score commit statuses (#1829)"))
        self.assertEqual(pps.queue_prs(self.ref, QUEUE_BASE, payload), [1830, 1829])

    def test_queue_prs_fails_closed(self):
        good = squash(QUEUE_HEAD, "Post pp/score commit statuses (#1829)")
        cases = {
            "not a merge queue branch": ("refs/heads/main", QUEUE_BASE, compare(good)),
            "base does not match": (self.ref, OTHER, compare(good)),
            "not ahead": (self.ref, QUEUE_BASE, compare(good, status="diverged")),
            "incomplete": (self.ref, QUEUE_BASE, {"status": "ahead", "ahead_by": 2, "commits": [good]}),
            "names no PR": (self.ref, QUEUE_BASE, compare(squash(QUEUE_HEAD, "Direct push"))),
            "do not end with #1829": (self.ref, QUEUE_BASE, compare(squash(QUEUE_HEAD, "Other (#1830)"))),
        }
        for expected, (ref, base, payload) in cases.items():
            with self.subTest(expected=expected):
                with self.assertRaises(pps.GateError) as raised:
                    pps.queue_prs(ref, base, payload)
                self.assertIn(expected, str(raised.exception))

    def test_every_pr_must_pass_on_its_current_head(self):
        heads = {1830: SHA, 1829: OTHER}
        ok = {1830: self.records(SHA), 1829: self.records(OTHER, score=10)}
        plan = pps.plan_merge_group(QUEUE_HEAD, [1830, 1829], heads, ok)
        self.assertEqual((plan.head, plan.state, plan.description),
                         (QUEUE_HEAD, "success", "#1830 min 9; #1829 min 10"))

        single = pps.plan_merge_group(QUEUE_HEAD, [1829], {1829: OTHER}, {1829: self.records(OTHER)})
        self.assertEqual(single.description, "#1829: 9/10 from grok-4.7")

    def test_merge_group_failures_name_the_pr(self):
        stale = {1830: self.records(OTHER), 1829: self.records(OTHER)}
        cases = (
            ({1830: SHA, 1829: OTHER}, stale, f"#1830: no trusted score on its head {SHA[:7]}"),
            ({1830: SHA, 1829: OTHER}, {1830: self.records(SHA, score=7), 1829: self.records(OTHER)},
             f"#1830 {SHA[:7]}: rejected: grok-4.7 scored 7"),
            ({1830: SHA, 1829: OTHER},
             {1830: self.records(SHA, author="xai"), 1829: self.records(OTHER)},
             f"#1830 {SHA[:7]}: rejected: grok-4.7 is the author's own family (xai)"),
            ({1830: None, 1829: OTHER}, stale, "#1830: could not read its head SHA"),
        )
        for heads, records, description in cases:
            with self.subTest(description=description):
                plan = pps.plan_merge_group(QUEUE_HEAD, [1830, 1829], heads, records)
                self.assertEqual((plan.head, plan.state, plan.description),
                                 (QUEUE_HEAD, "failure", description))
                self.assertLessEqual(len(plan.description), 140)

    def test_run_merge_group_posts_failure_when_github_reads_fail(self):
        import io
        from contextlib import redirect_stdout

        posted = []
        original_json, original_apply = pps.gh_json, pps.apply_plan

        def broken(path, paginate=False):
            raise OSError("network down")

        pps.gh_json = broken
        pps.apply_plan = lambda repo, plan: posted.append(plan)
        try:
            with redirect_stdout(io.StringIO()):
                code = pps.run_merge_group("o/r", QUEUE_HEAD, QUEUE_BASE, self.ref, self.trust, post=True)
        finally:
            pps.gh_json, pps.apply_plan = original_json, original_apply
        self.assertEqual(code, 1)
        self.assertEqual([(p.head, p.state, p.description) for p in posted],
                         [(QUEUE_HEAD, "failure", "cannot verify merge group: OSError reading GitHub")])

    def test_run_merge_group_posts_one_status_on_the_group_sha(self):
        import io
        from contextlib import redirect_stdout

        payloads = {
            f"repos/o/r/compare/{QUEUE_BASE}...{QUEUE_HEAD}":
                compare(squash(QUEUE_HEAD, "Post pp/score commit statuses (#1829)")),
            "repos/o/r/pulls/1829": {"head": {"sha": PR1777_HEAD}},
            "repos/o/r/issues/1829/comments": [[comment(PR1777_BODY.replace("anthropic`", "anthropic obstacles=none`"))]],
        }
        posted = []
        original_json, original_apply = pps.gh_json, pps.apply_plan
        pps.gh_json = lambda path, paginate=False: payloads[path]
        pps.apply_plan = lambda repo, plan: posted.append(plan)
        try:
            with redirect_stdout(io.StringIO()):
                code = pps.run_merge_group("o/r", QUEUE_HEAD, QUEUE_BASE, self.ref, self.trust, post=True)
        finally:
            pps.gh_json, pps.apply_plan = original_json, original_apply
        # The real record is wrapped in backticks, so it never counts (GRE-1859).
        self.assertEqual(code, 1)
        self.assertEqual([(p.head, p.state, p.description) for p in posted],
                         [(QUEUE_HEAD, "failure",
                           f"#1829 {PR1777_HEAD[:7]}: rejected: a wrapped or indented record never counts")])



class CursorAutoAuthorTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def records(self, *lines):
        return pps.read_comments([comment("\n".join(lines))], self.trust)

    def line(self, head, seat, vendor, score, author="cursor-auto"):
        return f"pp-score head={head} seat={seat} vendor={vendor} score={score} author={author} obstacles=none"

    def test_cursor_auto_record_parses(self):
        records = self.records(self.line(SHA, "claude-opus-5-5", "anthropic", 9)).records
        self.assertEqual([(r.author, r.vendor, r.seat, r.score) for r in records],
                         [("cursor-auto", "anthropic", "claude-opus-5-5", 9.0)])

    def test_missing_one_family_fails(self):
        (plan,) = pps.plan_statuses(self.records(
            self.line(SHA, "claude-opus-5-5", "anthropic", 9)))
        self.assertEqual((plan.state, plan.description),
                         ("failure", "cursor-auto requires anthropic and openai scores; missing openai"))

        (plan,) = pps.plan_statuses(self.records(
            self.line(SHA, "gpt-6.1-sol", "openai", 9, author="unknown")))
        self.assertEqual((plan.state, plan.description),
                         ("failure", "unknown requires anthropic and openai scores; missing anthropic"))

    def test_both_families_pass_at_the_lower_score(self):
        (plan,) = pps.plan_statuses(self.records(
            self.line(SHA, "claude-opus-5-5", "anthropic", 9),
            self.line(SHA, "gpt-6.1-sol", "openai", 8),
        ))
        self.assertEqual(plan.state, "success")
        self.assertEqual(plan.description, "9/10 from claude-opus-5-5; 8/10 from gpt-6.1-sol")

    def test_one_family_below_eight_fails(self):
        (plan,) = pps.plan_statuses(self.records(
            self.line(SHA, "claude-opus-5-5", "anthropic", 9),
            self.line(SHA, "gpt-6.1-sol", "openai", 7),
        ))
        self.assertEqual(plan.state, "failure")
        self.assertEqual(plan.description, "rejected: gpt-6.1-sol scored 7")

    def test_stale_head_does_not_satisfy_the_current_head(self):
        records = self.records(
            self.line(OTHER, "claude-opus-5-5", "anthropic", 9),
            self.line(OTHER, "gpt-6.1-sol", "openai", 9),
            self.line(SHA, "claude-opus-5-5", "anthropic", 9),
        )
        plans = {p.head: p for p in pps.plan_statuses(records)}
        self.assertEqual(plans[OTHER].state, "success")
        self.assertEqual(plans[SHA].state, "failure")
        self.assertEqual(plans[SHA].description,
                         "cursor-auto requires anthropic and openai scores; missing openai")

    def test_known_family_still_rejects_a_same_family_score(self):
        (plan,) = pps.plan_statuses(self.records(
            self.line(SHA, "claude-opus-5-5", "anthropic", 9, author="anthropic")))
        self.assertEqual(plan.state, "failure")
        self.assertIn("author's own family (anthropic)", plan.description)


class CursorAutoMergeGroupTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def dual(self, head, anthropic=9, openai=8, author="cursor-auto", read=True):
        return (pps.read_comments if read else lambda c, _: c)([comment(
            f"pp-score head={head} seat=claude-opus-5-5 vendor=anthropic score={anthropic} author={author} obstacles=none\n"
            f"pp-score head={head} seat=gpt-6.1-sol vendor=openai score={openai} author={author} obstacles=none"
        )], self.trust)

    def one(self, head, seat="claude-opus-5-5", vendor="anthropic", score=9, author="cursor-auto", read=True):
        return (pps.read_comments if read else lambda c, _: c)([comment(
            f"pp-score head={head} seat={seat} vendor={vendor} score={score} author={author} obstacles=none"
        )], self.trust)

    def test_merge_group_passes_when_both_families_score_the_current_head(self):
        plan = pps.plan_merge_group(
            QUEUE_HEAD, [1829], {1829: SHA}, {1829: self.dual(SHA)})
        self.assertEqual((plan.state, plan.description),
                         ("success", "#1829: 9/10 from claude-opus-5-5; 8/10 from gpt-6.1-sol"))

    def test_merge_group_fails_when_one_family_is_missing_on_the_current_head(self):
        # A complete pair on a stale SHA does not count for the current head.
        records = pps.read_comments(self.dual(OTHER, read=False) + self.one(SHA, read=False), self.trust)
        plan = pps.plan_merge_group(
            QUEUE_HEAD, [1829], {1829: SHA}, {1829: records})
        self.assertEqual(plan.state, "failure")
        self.assertEqual(
            plan.description,
            f"#1829 {SHA[:7]}: cursor-auto requires anthropic and openai scores; missing openai",
        )
        self.assertLessEqual(len(plan.description), 140)

    def test_merge_group_fails_when_one_family_is_below_eight(self):
        plan = pps.plan_merge_group(
            QUEUE_HEAD, [1829], {1829: SHA}, {1829: self.dual(SHA, anthropic=9, openai=7)})
        self.assertEqual(plan.state, "failure")
        self.assertEqual(
            plan.description,
            f"#1829 {SHA[:7]}: rejected: gpt-6.1-sol scored 7",
        )


TEN = "".join(f"{i}\n" for i in range(1, 11))
PR_TEN = TEN.replace("1\n", "1 from the PR\n", 1)
MAIN_TEN = TEN.replace("10\n", "10 from main\n")


class CarryRepo:
    """A throwaway git repo holding a PR branch and main, so the carry rules run real merges."""

    def __init__(self):
        import os
        import tempfile

        # Left in place for inspection; the caller's TMPDIR owns cleanup.
        self.path = tempfile.mkdtemp(prefix="pp-score-carry-")
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
                        GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")
        self.git("init", "-q", "-b", "main")

    def git(self, *args, check=True):
        import subprocess

        done = subprocess.run(["git", *args], cwd=self.path, env=self.env, text=True, capture_output=True)
        if check and done.returncode:
            raise RuntimeError(done.stderr)
        return done.stdout.strip()

    def is_ancestor(self, sha, ref):
        import subprocess

        return subprocess.run(["git", "merge-base", "--is-ancestor", sha, ref], cwd=self.path,
                              env=self.env, capture_output=True).returncode == 0

    def write(self, name, text):
        from pathlib import Path

        Path(self.path, name).write_text(text, encoding="utf-8")

    def mkdir(self, name):
        from pathlib import Path

        Path(self.path, name).mkdir(parents=True, exist_ok=True)

    def write_path(self, name, text):
        self.mkdir(name.rpartition("/")[0])
        self.write(name, text)

    def commit_path(self, name, text, message):
        self.write_path(name, text)
        self.git("add", name)
        self.git("commit", "-q", "-m", message)
        return self.head()

    def commit(self, name, text, message):
        self.write(name, text)
        self.git("add", name)
        self.git("commit", "-q", "-m", message)
        return self.head()

    def head(self):
        return self.git("rev-parse", "HEAD")

    def branch(self, name, start):
        self.git("checkout", "-q", "-b", name, start)

    def line(self, name, start, *edits):
        """A branch from start with one commit per (path, text) edit; returns its tip."""
        self.branch(name, start)
        for path, text in edits:
            self.commit(path, text, f"{name}: {path}")
        return self.head()

    def merge(self, *others):
        """Merge into the checked-out branch like GitHub's update-branch; parents [head, *others]."""
        self.git("merge", "-q", "--no-ff", "--no-edit", *others, check=False)
        return self.head()

    def amend(self, name, text):
        self.write(name, text)
        self.git("add", name)
        self.git("commit", "-q", "--amend", "--no-edit")
        return self.head()

    def merge_commit(self, tree_of, *parents):
        args = ["commit-tree", f"{tree_of}^{{tree}}", "-m", "hand-built merge"]
        for parent in parents:
            args += ["-p", parent]
        return self.git(*args)


def rename_repo():
    """(repo, h0, h): main renames d1/old_0..5.txt to d2/new_0..5.txt with edits, the PR adds
    d1/added_by_pr.txt, and h merges main with merge.renameLimit=1, skipping the directory rename
    that makes the merge conflict under Git's defaults."""
    r = CarryRepo()
    for i in range(6):
        r.write_path(f"d1/old_{i}.txt", "".join(f"file {i} line {n}\n" for n in range(20)))
    r.git("add", "-A")
    r.git("commit", "-q", "-m", "base")
    base = r.head()
    r.branch("pr", base)
    h0 = r.commit_path("d1/added_by_pr.txt", "added by the PR\n", "pr adds a file")
    r.git("checkout", "-q", "main")
    r.mkdir("d2")
    for i in range(6):
        r.git("mv", f"d1/old_{i}.txt", f"d2/new_{i}.txt")
        r.write_path(f"d2/new_{i}.txt", "".join(f"file {i} line {n}\n" for n in range(20)) + "edited\n")
    r.git("add", "-A")
    r.git("commit", "-q", "-m", "main moves d1 to d2")
    r.git("checkout", "-q", "pr")
    r.git("-c", "merge.renameLimit=1", "-c", "diff.renameLimit=1", "merge", "-q", "--no-ff", "--no-edit", "main")
    return r, h0, r.head()


class RepoFacts(pps.GitHubFacts):
    """GitHubFacts on a local repo: GitHub's main tip is main_ref, and failing maps a SHA to its
    first failing required check."""

    def __init__(self, repo, failing=None, main_ref="main", git=None):
        super().__init__("o/r", self.branch_api, git or pps.LocalGit(repo.path))
        self.repo_, self.failing, self.main_ref = repo, failing or {}, main_ref

    def checks_failing(self, sha):
        return self.failing.get(sha)

    def branch_api(self, path):
        if path != "repos/o/r/branches/main":
            raise AssertionError(f"unexpected GitHub read {path}")
        return {"commit": {"sha": self.repo_.git("rev-parse", self.main_ref)}}


def record(head, seat="gpt-6.2-sol", vendor="openai", score=9, author="anthropic"):
    return pps.Record(head, seat, vendor, author, score, ())


def body(rec, obstacles="none"):
    return (f"pp-score head={rec.head} seat={rec.seat} vendor={rec.vendor} score={pps.format_score(rec.score)} "
            f"author={rec.author} obstacles={obstacles}")


def reading(*records):
    return pps.read_comments([comment(body(r)) for r in records], pps.load_trust())


CONFIG = json.loads(pps.CLASSES.read_text(encoding="utf-8"))
LANE_FILES, STRICT_FILES = ["docs/x.md"], ["scripts/merge-classes.json"]
# The carry_source results of the current carry_both call, in call order.
SOURCES = []


def carry_both(test, repo, head, comments, strict=False, failing=None, main_ref="main", git=None, files=None,
               guard=False):
    """The only route to a carry decision in this module besides main() (setUpModule makes a direct
    call to any carry entry point raise). comments are raw GitHub comments, or Records, each posted
    as a comment holding one bare record line. One read_comments Reading of them goes to both
    evaluators: the pp/score status (carry_for_pr, as the workflow calls it, with files as the PR's
    file list) and the coordinator's sensitive merge check (merge_class.sensitive_scores, as
    agent-guard.py calls it). Both must get the same carry_source result. A refusal there is each
    evaluator's reason, verbatim; from a source, the guard passes only when the status passes. The
    same holds with a second vendor at 9 beside each Record, which lets the guard pass. Returns the
    status result for comments, followed by the guard's (source, why) when guard is set."""
    raws = [c if isinstance(c, dict) else comment(body(c)) for c in comments]
    companions = [comment(body(pps.Record(c.head, "glm-5.3-flash-free", "zhipu", c.author, 9, ())))
                  for c in comments if not isinstance(c, dict)]
    results = []
    for group in (raws, raws + companions):
        found = pps.read_comments(group, pps.load_trust())
        facts = RepoFacts(repo, failing, main_ref, git)
        SOURCES.clear()
        with mock.patch.object(pps, "fetch_files", return_value=files or (STRICT_FILES if strict else LANE_FILES)):
            plan, why = pps.carry_for_pr("o/r", 7, head, found, facts, pps.check_state(facts, head))
        try:
            source, guard_why = merge_class.sensitive_scores(found, head, CONFIG["scorer_seats"], CONFIG["vendors"],
                                                             facts, merge_class.load_classes())
        except Exception as err:  # noqa: BLE001 - agent-guard.py refuses the merge on any exception
            source, guard_why = None, f"raised {type(err).__name__}"
        results.append((plan, why, source, guard_why) if guard else (plan, why))
        if head in found.heads():
            # No carry: the guard reads head's own records, and passes only what the status passes.
            if source is not None:
                test.assertEqual(pps.plan_head(head, found).state, "success")
            continue
        if source is not None:
            test.assertIsNotNone(plan, f"the guard passed what the status refused: {why}")
            test.assertEqual((plan.state, why, guard_why), ("success", f"carried from {source[:7]}",
                                                            f"carried from {source[:7]}"))
        if len(SOURCES) == 2:
            test.assertEqual(SOURCES[0], SOURCES[1], "the evaluators reached different carry decisions")
            if SOURCES[0][0] is None:
                test.assertEqual((why, guard_why), (SOURCES[0][1], SOURCES[0][1]))
        else:
            test.assertEqual((plan, source), (None, None), f"{len(SOURCES)} carry decisions: {why}; {guard_why}")
    return results[0]


# Every carry decision a test reaches goes through carry_both, or through main() or
# run_merge_group() as the workflow runs them.
CARRY_ENTRY_POINTS = [(pps, "plan_carry"), (pps, "carry_for_pr"), (pps, "decide_carry"), (pps, "carry_source"),
                      (merge_class, "decide_carry"), (merge_class, "carry_source"),
                      (merge_class, "sensitive_scores")]
DECIDING = {carry_both.__code__, pps.main.__code__, pps.run_merge_group.__code__}


def only_through_carry_both(real):
    def guarded(*args, **kwargs):
        frame = sys._getframe(1)
        while frame is not None and frame.f_code not in DECIDING:
            frame = frame.f_back
        if frame is None:
            raise AssertionError(f"{real.__name__} called outside carry_both or main()")
        result = real(*args, **kwargs)
        if real.__name__ == "carry_source":
            SOURCES.append(result)
        return result
    return guarded


def setUpModule():
    for module, name in CARRY_ENTRY_POINTS:
        patcher = mock.patch.object(module, name, only_through_carry_both(getattr(module, name)))
        patcher.start()
        unittest.addModuleCleanup(patcher.stop)


class CarryTests(unittest.TestCase):
    """Chad's rule of 2026-11-18 11:02 PM ET, tightened so every hop fact is fixed by its SHAs.
    One test per condition in hop() and plan_carry() failing, plus the clean cases."""

    @classmethod
    def setUpClass(cls):
        r = cls.r = CarryRepo()
        r.write("a.txt", TEN)
        r.git("add", "a.txt")
        cls.base = r.commit("b.txt", "b1\n", "base")
        r.branch("pr", cls.base)
        cls.h0 = r.commit("a.txt", PR_TEN, "pr changes line 1")
        r.git("checkout", "-q", "main")
        cls.m1 = r.commit("b.txt", "b2 from main\n", "main change")
        r.git("checkout", "-q", "pr")
        cls.h1 = r.merge(cls.m1)
        r.git("checkout", "-q", "main")
        cls.m2 = r.commit("c.txt", "c1\n", "main adds c")
        r.git("checkout", "-q", "pr")
        cls.h2 = r.merge(cls.m2)
        # Each case below merges a branch that the facts treat as main when named as main_ref.
        r.branch("edited", cls.h0)
        r.merge(cls.m1)
        cls.edited = r.amend("a.txt", PR_TEN + "edited during the merge\n")
        r.branch("hidden", cls.h0)
        r.merge(cls.m1)
        cls.hidden = r.amend("evil.txt", "not in the PR or main\n")
        conflict = r.line("main-conflict", cls.m1, ("a.txt", TEN.replace("1\n", "1 from main\n", 1)))
        r.branch("conflicted", cls.h0)
        r.merge(conflict)
        r.write("a.txt", "resolved\n")
        r.git("add", "a.txt")
        r.git("commit", "-q", "--no-edit")
        cls.conflicted = r.head()
        r.branch("rebased", cls.m1)
        r.git("cherry-pick", cls.h0)
        cls.rebased = r.head()
        r.branch("reversed", cls.m1)
        cls.reversed = r.merge(cls.h0)
        side_o = r.line("side-o", cls.m1, ("o.txt", "o\n"))
        side_p = r.line("side-p", cls.m1, ("p.txt", "p\n"))
        r.branch("octopus", cls.h0)
        cls.octopus = r.merge(side_o, side_p)
        r.branch("feature", cls.h0)
        cls.feature = r.merge(side_o)
        cls.hunk_main = r.line("main-hunk", cls.m1, ("a.txt", MAIN_TEN))
        r.branch("hunk", cls.h0)
        cls.hunk = r.merge(cls.hunk_main)
        revert = r.line("main-revert", cls.m1, ("a.txt", MAIN_TEN), ("a.txt", TEN))
        r.branch("revert", cls.h0)
        cls.revert = r.merge(revert)
        gate = r.line("main-gate", cls.m1, ("Dockerfile", "FROM scratch\n"))
        r.branch("gate", cls.h0)
        cls.gate = r.merge(gate)
        r.branch("rewritten", cls.h0)
        r.git("commit", "-q", "--amend", "-m", "pr changes line 1, rewritten")
        cls.rewritten = r.merge(cls.m1)
        # A second update on top of the carried h1 whose main side edits the PR's file.
        r.branch("second", cls.h1)
        cls.second = r.merge(cls.hunk_main)
        cls.no_new = r.merge_commit(cls.h1, cls.h1, cls.m1)
        cls.on_main = r.merge_commit(cls.m1, cls.base, cls.m1)

    def plan(self, head, records, strict=False, failing=None, main_ref="main"):
        return carry_both(self, self.r, head, records, strict, failing, main_ref)

    def test_a_carry_entry_point_called_outside_carry_status_fails(self):
        # A new carry fixture cannot decide a carry another way: every entry point raises.
        for module, name in CARRY_ENTRY_POINTS:
            with self.subTest(name), self.assertRaisesRegex(AssertionError, "outside carry_both"):
                getattr(module, name)(self.h1, [record(self.h0)])

    def refused(self, head, records=None, main_ref="main", **kw):
        plan, why = self.plan(head, records or [record(self.h0)], main_ref=main_ref, **kw)
        self.assertIsNone(plan, why)
        return why

    # The clean cases.

    def test_clean_update_carries(self):
        plan, why = self.plan(self.h1, [record(self.h0)])
        self.assertEqual((plan.head, plan.state), (self.h1, "success"))
        self.assertEqual(plan.description, f"carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol")
        self.assertEqual(why, f"carried from {self.h0[:7]}")

    def test_a_second_clean_merge_on_top_of_a_carried_score_carries(self):
        plan, _ = self.plan(self.h2, [record(self.h0)])
        self.assertEqual(plan.description, f"carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol")

    # hop condition 1: exactly two parents [P, M].

    def test_rebased_head_does_not_carry(self):
        self.assertEqual(self.refused(self.rebased),
                         f"{self.rebased[:7]}: 1 parents, not a two-parent merge of main")

    def test_octopus_merge_does_not_carry(self):
        self.assertEqual(len(self.r.git("rev-list", "--parents", "-n1", self.octopus).split()), 4)
        self.assertEqual(self.refused(self.octopus),
                         f"{self.octopus[:7]}: 3 parents, not a two-parent merge of main")

    def test_force_pushed_or_rewritten_history_does_not_carry(self):
        # h0 rewritten with the same tree: its new SHA has no records and is not itself a merge.
        rewritten = self.r.git("rev-parse", f"{self.rewritten}^1")
        self.assertNotEqual(rewritten, self.h0)
        self.assertEqual(self.refused(self.rewritten),
                         f"{rewritten[:7]}: 1 parents, not a two-parent merge of main")

    def test_reversed_parents_do_not_carry(self):
        self.assertIn("was never main's tip", self.refused(self.reversed))

    def test_replace_refs_and_grafts_do_not_change_the_parents(self):
        # A lane shares refs/replace and info/grafts with the coordinator's worktree. Both would make
        # the rebased head look like a clean two-parent merge of main; the carry reads the real parents.
        from pathlib import Path

        git_dir = Path(self.r.path, ".git")
        real = self.r.git("rev-parse", f"{self.rebased}^")
        try:
            self.r.git("replace", "--graft", self.rebased, self.h0, self.m1)
            (git_dir / "info" / "grafts").write_text(f"{self.rebased} {self.h0} {self.m1}\n", encoding="utf-8")
            self.assertEqual(len(self.r.git("rev-list", "--parents", "-n1", self.rebased).split()), 3)
            self.assertEqual(pps.LocalGit(self.r.path).parents(self.rebased), [real])
            self.assertEqual(self.refused(self.rebased),
                             f"{self.rebased[:7]}: 1 parents, not a two-parent merge of main")
        finally:
            self.r.git("replace", "-d", self.rebased, check=False)
            (git_dir / "info" / "grafts").write_text("", encoding="utf-8")

    # hop condition 2: M is on main.

    def test_merging_a_branch_that_is_not_main_does_not_carry(self):
        side = self.r.git("rev-parse", "side-o")
        self.assertEqual(self.refused(self.feature),
                         f"{self.feature[:7]}: second parent {side[:7]} was never main's tip")

    def test_a_merged_commit_that_main_reaches_but_never_pointed_at_does_not_carry(self):
        # Both commits are ancestors of their main, but neither was ever its tip: an -s ours merge
        # left side's unseen.py out of main's tree, and inner is a side branch's first commit.
        r = self.r
        side = r.line("side-unseen", self.m1, ("unseen.py", "x\n"), ("unseen2.py", "y\n"))
        inner = r.git("rev-parse", f"{side}~1")
        r.branch("main-ours", self.m1)
        r.git("merge", "-q", "-s", "ours", "--no-edit", side)
        r.branch("main-side", self.m1)
        r.merge(side)
        for main_ref, merged in (("main-ours", side), ("main-side", inner)):
            r.branch(f"pr-{main_ref}", self.h0)
            head = r.merge(merged)
            with self.subTest(main_ref):
                self.assertTrue(r.is_ancestor(merged, main_ref))
                self.assertEqual(self.refused(head, main_ref=main_ref),
                                 f"{head[:7]}: second parent {merged[:7]} was never main's tip")
        # m1 was main's tip before m2: a true earlier tip still carries.
        self.assertEqual(self.plan(self.h1, [record(self.h0)], main_ref="main-side")[0].state, "success")

    # hop condition 3: one merge base, neither side.

    def test_no_new_main_commits_or_a_head_already_on_main_does_not_carry(self):
        self.assertEqual(self.refused(self.no_new, [record(self.h1)]),
                         f"{self.no_new[:7]}: the merge brought no new main commits")
        self.assertEqual(self.refused(self.on_main, [record(self.base)]),
                         f"{self.on_main[:7]}: the PR head is already on main")

    # hop condition 4: clean merge-tree equal to the head's tree.

    def test_conflicted_merge_does_not_carry(self):
        self.assertEqual(self.refused(self.conflicted, main_ref="main-conflict"),
                         f"{self.conflicted[:7]}: the merge of main had conflicts")

    def test_merge_with_an_extra_edit_does_not_carry(self):
        for head in (self.edited, self.hidden):
            with self.subTest(head=head):
                self.assertEqual(self.refused(head),
                                 f"{head[:7]}: the merge commit differs from a clean merge")

    # hop condition 5: the PR's own diff is byte-identical.

    def test_main_editing_another_hunk_of_a_pr_file_changes_the_diff(self):
        # git merges this cleanly, but the scorer never saw line 10 of the PR's file as main has it.
        self.assertEqual(self.r.git("merge-tree", "--write-tree", self.h0, self.hunk_main),
                         self.r.git("rev-parse", f"{self.hunk}^{{tree}}"))
        self.assertEqual(self.refused(self.hunk, main_ref="main-hunk"),
                         f"{self.hunk[:7]}: the PR's own diff changed")

    def test_a_second_merge_that_changes_the_diff_drops_the_carry(self):
        self.assertEqual(self.plan(self.h1, [record(self.h0)], main_ref="main-hunk")[0].state, "success")
        self.assertEqual(self.refused(self.second, main_ref="main-hunk"),
                         f"{self.second[:7]}: the PR's own diff changed")

    # hop condition 6: no main commit touched a PR path.

    def test_main_touching_a_pr_file_and_reverting_it_does_not_carry(self):
        self.assertEqual(self.refused(self.revert, main_ref="main-revert"),
                         f"{self.revert[:7]}: main changed a.txt, which the PR changes")

    # hop condition 7: no main commit touched a gate file.

    def test_main_changing_a_gate_file_does_not_carry(self):
        self.assertEqual(self.refused(self.gate, main_ref="main-gate"),
                         f"{self.gate[:7]}: main changed gate file Dockerfile")

    def test_gate_files_are_owner_sensitive_or_score_tooling(self):
        classes = pps.load_classes()
        for path in (".github/workflows/ci.yml", "scripts/merge-classes.json", "Dockerfile",
                     "rust/crates/pp-storage/src/schema.rs", "AGENTS.md",
                     "scripts/post_pp_score_status.py", "scripts/test_post_pp_score_status.py",
                     "docs/development/score-records.md"):
            self.assertTrue(pps.gate_file(path, classes), path)
        self.assertFalse(pps.gate_file("docs/product/daily-loop.md", classes))

    # hop condition 3, two merge bases: CrissCrossTests.

    # plan_carry: the records and the carried head's own checks.

    def test_ci_not_green_on_the_new_head_does_not_carry(self):
        self.assertEqual(self.refused(self.h1, failing={self.h1: "rust (linux) is in_progress"}),
                         f"{self.h1[:7]}: rust (linux) is in_progress")

    def test_only_the_carried_heads_own_checks_count(self):
        # h1 never merges, and a re-run of its checks would not re-run the workflow. Its hop is
        # judged on fixed git facts only; GitHub requires the checks on h2 and on the queue SHA.
        plan, _ = self.plan(self.h2, [record(self.h0)], failing={self.h1: "zizmor is failure"})
        self.assertEqual(plan.state, "success")
        self.assertEqual(self.refused(self.h2, failing={self.h2: "zizmor is failure"}),
                         f"{self.h2[:7]}: zizmor is failure")

    def test_newer_records_stop_the_walk(self):
        # h1 was scored 7: its records win over h0's 9, so h2 does not carry.
        self.assertEqual(self.refused(self.h2, [record(self.h0), record(self.h1, score=7)]),
                         f"{self.h1[:7]}: gpt-6.2-sol scored 7")

    def test_head_with_its_own_records_does_not_carry(self):
        self.assertEqual(self.refused(self.h1, [record(self.h0), record(self.h1, score=7)]),
                         f"{self.h1[:7]} has its own records")

    def test_no_records_do_not_carry(self):
        self.assertEqual(self.plan(self.h1, []), (None, "no records on this PR"))

    def test_sensitive_class_needs_every_carried_seat_above_eight(self):
        records = [record(self.h0, score=9), record(self.h0, seat="composer-2.5", vendor="cursor", score=8)]
        self.assertEqual(self.refused(self.h1, records, strict=True),
                         f"sensitive: composer-2.5 scored 8 on {self.h0[:7]}")
        plan, _ = self.plan(self.h1, records, strict=False)
        self.assertEqual(plan.state, "success")
        plan, _ = self.plan(self.h1, [record(self.h0, score=9)], strict=True)
        self.assertEqual(plan.state, "success")

    def test_sensitive_pr_class_comes_from_its_live_file_list(self):
        records = [record(self.h0, score=8)]
        for paths, state in ((["rust/crates/pp-storage/src/import.rs"], None),
                             (["scripts/merge-classes.json"], None), (["docs/x.md"], "success")):
            with self.subTest(paths=paths):
                plan, why = carry_both(self, self.r, self.h1, records, files=paths)
                self.assertEqual(plan and plan.state, state, why)

    def test_grok_records_never_carry(self):
        grok = record(self.h0, seat="grok-4.7", vendor="xai")
        self.assertIn("no record that can carry", self.refused(self.h1, [grok]))
        plan, _ = self.plan(self.h1, [grok, record(self.h0)])
        self.assertEqual(plan.description, f"carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol")
        # A failing Grok record still fails the source, so it fails the carry too.
        low_grok = record(self.h0, seat="grok-4.7", vendor="xai", score=5)
        self.assertEqual(self.refused(self.h1, [low_grok, record(self.h0)]), f"{self.h0[:7]}: grok-4.7 scored 5")

    def test_a_wrapped_record_completes_no_cursor_auto_pair(self):
        # GRE-1859 rounds 2 and 3: a wrapped record counts toward no pass, so a cursor-auto head whose
        # anthropic record is wrapped fails on itself (main passes it), and nothing carries from it.
        line = (f"pp-score head={self.h0} seat=claude-opus-5-5 vendor=anthropic score=9 author=cursor-auto "
                "obstacles=none")
        comments = [comment(f"`{line}`"), comment(line.replace("claude-opus-5-5 vendor=anthropic",
                                                                "gpt-6.2-sol vendor=openai"))]
        missing = "cursor-auto requires anthropic and openai scores; missing anthropic"
        (plan,) = pps.plan_statuses(pps.read_comments(comments, pps.load_trust()))
        self.assertEqual((plan.state, plan.description), ("failure", missing))
        self.assertEqual(self.refused(self.h1, comments), f"{self.h0[:7]} did not pass: {missing}")

    def test_a_record_with_a_wrong_author_family_never_carries(self):
        # The seat is the author's own family.
        own = record(self.h0, seat="claude-opus-5-5", vendor="anthropic")
        self.assertIn("author's own family", self.refused(self.h1, [own]))
        # The records on this PR name two author families, on any head.
        for records in ([record(self.h0), record(self.h0, seat="claude-opus-5-5", vendor="anthropic",
                                                 author="openai")],
                        [record(self.base, author="openai", seat="grok-4.7", vendor="xai"), record(self.h0)]):
            with self.subTest(records=records):
                self.assertEqual(self.refused(self.h1, records),
                                 "records on this PR disagree on the author family (anthropic, openai)")

    def test_failing_source_does_not_carry(self):
        self.assertEqual(self.refused(self.h1, [record(self.h0, score=7)]), f"{self.h0[:7]}: gpt-6.2-sol scored 7")

    def test_sensitive_and_owner_paths_are_strict(self):
        self.assertTrue(pps.strict_class(["rust/crates/pp-storage/src/import.rs"]))
        self.assertTrue(pps.strict_class(["scripts/merge-classes.json", "docs/x.md"]))
        self.assertFalse(pps.strict_class(["docs/product/daily-loop.md"]))

    def test_merge_group_accepts_a_carried_head(self):
        carried = self.plan(self.h1, [record(self.h0)])
        plan = pps.plan_merge_group(SHA, [1], {1: self.h1}, {1: reading(record(self.h0))}, {1: carried})
        self.assertEqual((plan.state, plan.description),
                         ("success", f"#1: carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol"))
        refused = self.plan(self.edited, [record(self.h0)])
        plan = pps.plan_merge_group(SHA, [1], {1: self.edited}, {1: reading(record(self.h0))}, {1: refused})
        self.assertEqual((plan.state, plan.description),
                         ("failure", f"#1: no trusted score on its head {self.edited[:7]}"))


class GuardAgreementTests(unittest.TestCase):
    """The pp/score status and the coordinator's sensitive merge check on raw comment bodies, through
    carry_both: one read_comments Reading, one carry_source decision. The guard needs two non-Grok
    vendors from scorer_seats where the status passes one seat, so one seat, one vendor or Grok as
    the second seat passes the status but not the guard. A sensitive carry needs every record at 9
    or more (STRICT_CARRY_MIN) in both."""

    @classmethod
    def setUpClass(cls):
        CarryTests.setUpClass()
        cls.c = CarryTests

    def line(self, head, seat="gpt-6.2-sol", vendor="openai", score=9, obstacles=" obstacles=none"):
        return f"pp-score head={head} seat={seat} vendor={vendor} score={score} author=anthropic{obstacles}"

    def two(self, head):
        return [comment(self.line(head)), comment(self.line(head, "glm-5.3-flash-free", "zhipu"))]

    def both(self, head, comments, **kw):
        """(status passes, guard passes, status why) from carry_both, which checks that they agree."""
        plan, why, source, _ = carry_both(self, self.c.r, head, comments, strict=True, guard=True, **kw)
        return plan is not None, source is not None, why

    def test_both_carry_two_vendors_across_one_and_two_clean_merges(self):
        for head in (self.c.h1, self.c.h2):
            with self.subTest(head=head):
                self.assertEqual(self.both(head, self.two(self.c.h0)), (True, True, f"carried from {self.c.h0[:7]}"))

    def test_the_four_parser_cases_get_one_decision_and_reason(self):
        # Neon check of 320de029, case 11: each of these once split the two evaluators.
        c, h0, h1 = self.c, self.c.h0, self.c.h1
        edited = comment(self.line(h0), updated="2026-11-13T02:00:00Z")
        cases = {
            "a wrapped record alone on a later head": (
                c.h2, self.two(h0) + [comment(f"`{self.line(h1)}`")],
                f"{h1[:7]}: a wrapped or indented record never counts"),
            "a wrong-vendor record beside a valid one": (
                h1, self.two(h0) + [comment(self.line(h0, "composer-2.5", "openai"))],
                f"{h0[:7]}: composer-2.5 is not an allowed seat for vendor=openai"),
            "an edited record": (
                h1, self.two(h0)[1:] + [edited], f"{h0[:7]}: the gpt-6.2-sol record was edited after it was posted"),
            "a record without obstacles on a later head": (
                c.h2, self.two(h0) + [comment(self.line(h1, obstacles=""))],
                f"{h1[:7]}: the gpt-6.2-sol record has no obstacles="),
            "a malformed record line on any head": (
                h1, self.two(h0) + [comment(self.line(c.base, obstacles=" obstacles=maybe"))],
                "a malformed pp-score record"),
        }
        for name, (head, comments, reason) in cases.items():
            with self.subTest(name):
                self.assertEqual(self.both(head, comments), (False, False, reason))

    def test_a_wrapped_record_is_checked_but_never_counts(self):
        h0 = self.c.h0
        wrapped = comment(f"`{self.line(h0, 'deepseek-v4.1-flash-free', 'deepseek')}`")
        self.assertEqual(self.both(self.c.h1, self.two(h0) + [wrapped])[:2], (True, True))
        # One bare vendor and a wrapped second vendor: the status passes one seat, the guard needs two.
        self.assertEqual(self.both(self.c.h1, self.two(h0)[:1] + [wrapped])[:2], (True, False))

    def test_a_low_record_blocks_the_carry_in_both_with_one_reason(self):
        # A 5 on h1, bare or wrapped, alone or beside two bare 9s: both evaluators refuse h2's carry and
        # h1 itself for one reason. Alone, a wrapped 5 fails first, because a wrapped record never counts.
        c, h1 = self.c, self.c.h1
        low = self.line(h1, score=5)
        for form in (low, f"`{low}`", f"  {low}"):
            alone = "gpt-6.2-sol scored 5" if form == low else "a wrapped or indented record never counts"
            for beside, reason in (([], alone), (self.two(h1), "gpt-6.2-sol scored 5")):
                comments = self.two(c.h0) + beside + [comment(form)]
                with self.subTest(form=form, beside=bool(beside)):
                    self.assertEqual(self.both(c.h2, comments), (False, False, f"{h1[:7]}: {reason}"))
                    self.assertEqual(carry_both(self, c.r, h1, comments, strict=True, guard=True)[2:], (None, reason))
                    found = pps.read_comments(comments, pps.load_trust())
                    self.assertEqual(pps.plan_head(h1, found).description, f"rejected: {reason}")

    def test_both_refuse_failing_records_and_checks(self):
        h0, h1 = self.c.h0, self.c.h1
        cases = {
            "a seat at 8": self.two(h0)[:1] + [comment(self.line(h0, "glm-5.3-flash-free", "zhipu", 8))],
            "a failing Grok record": self.two(h0) + [comment(self.line(h0, "grok-4.7", "xai", 7))],
            "the author's own family": self.two(h0) + [comment(self.line(h0, "claude-opus-5-5", "anthropic"))],
            "two author families": self.two(h0) + [
                comment(self.line(h0, "deepseek-v4.1-flash-free", "deepseek").replace("author=anthropic",
                                                                                        "author=cursor"))],
        }
        for name, comments in cases.items():
            with self.subTest(name):
                self.assertEqual(self.both(h1, comments)[:2], (False, False))
        with self.subTest("a failing check on the carried head"):
            self.assertEqual(self.both(h1, self.two(h0), failing={h1: "zizmor is failure"}),
                             (False, False, f"{h1[:7]}: zizmor is failure"))

    def test_the_status_passes_one_seat_but_the_guard_needs_two_vendors(self):
        h0 = self.c.h0
        for name, comments in {
            "one seat": self.two(h0)[:1],
            "two seats, one vendor": self.two(h0)[:1] + [comment(self.line(h0, "gpt-6.2-astra"))],
            "Grok as the second seat": self.two(h0)[:1] + [comment(self.line(h0, "grok-4.7", "xai"))],
        }.items():
            with self.subTest(name):
                self.assertEqual(self.both(self.c.h1, comments)[:2], (True, False))

    def test_both_decide_the_same_from_a_subdirectory_with_diff_relative(self):
        # GRE-1859 round 2: diff.relative in the clone's shared config and a cwd in a subdirectory
        # once hid main's edit to the PR's own file from the guard. Both now refuse, for one reason.
        import os

        c = self.c
        sub = os.path.join(c.r.path, "sub-relative")
        os.makedirs(sub, exist_ok=True)
        expected = f"{c.hunk[:7]}: the PR's own diff changed"
        c.r.git("config", "diff.relative", "true")
        try:
            for where in (c.r.path, sub):
                with self.subTest(where=where):
                    plan, why, source, guard_why = carry_both(self, c.r, c.hunk, self.two(c.h0), strict=True,
                                                              main_ref="main-hunk", git=pps.LocalGit(where),
                                                              guard=True)
                    self.assertEqual((plan, why, source, guard_why), (None, expected, None, expected))
        finally:
            c.r.git("config", "--unset", "diff.relative")

    def test_the_guard_reads_the_exact_head_before_any_carry(self):
        self.assertEqual(self.both(self.c.h1, self.two(self.c.h1))[1], True)
        self.assertEqual(self.both(self.c.h1, self.two(self.c.h0) + [comment(self.line(self.c.h1))])[1], False)


class CrissCrossTests(unittest.TestCase):
    """Two best merge bases: the PR's diff has no single base, so nothing carries."""

    def test_criss_cross_merge_does_not_carry(self):
        r = CarryRepo()
        r.commit("f.txt", "0\n", "base")
        r.git("checkout", "-q", "-b", "pr")
        x1 = r.commit("f.txt", "x\n", "pr x")
        r.git("checkout", "-q", "main")
        y1 = r.commit("f.txt", "y\n", "main y")
        r.git("checkout", "-q", "pr")
        r.git("merge", "-q", "--no-edit", y1, check=False)
        r.write("f.txt", "x\n")
        r.git("commit", "-q", "-a", "--no-edit")
        h0 = r.head()
        r.git("checkout", "-q", "main")
        r.git("merge", "-q", "--no-edit", x1, check=False)
        r.write("f.txt", "y\n")
        r.git("commit", "-q", "-a", "--no-edit")
        m = r.head()
        self.assertEqual(len(r.git("merge-base", "--all", h0, m).split()), 2)
        # Merging from either single base is clean and gives "y"; the hand resolution matches it.
        r.git("checkout", "-q", "pr")
        r.git("merge", "-q", "--no-edit", m, check=False)
        r.write("f.txt", "y\n")
        r.git("commit", "-q", "-a", "--no-edit")
        h1 = r.head()
        self.assertEqual(r.git("rev-list", "--parents", "-n1", h1).split()[1:], [h0, m])
        # One explicit base (what GitHub's compare API returns) would certify the hand resolution.
        self.assertEqual(r.git("merge-tree", "--write-tree", f"--merge-base={x1}", h0, m),
                         r.git("rev-parse", f"{h1}^{{tree}}"))
        plan, why = carry_both(self, r, h1, [record(h0)])
        self.assertEqual((plan, why), (None, f"{h1[:7]}: 2 merge bases with main"))


class CommentModeCarryTests(unittest.TestCase):
    """main() in comment mode, with GitHub reads answered from a local repo."""

    @classmethod
    def setUpClass(cls):
        CarryTests.setUpClass()
        cls.c = CarryTests

    def body(self, score=9):
        return (f"pp-score head={self.c.h0} seat=gpt-6.2-sol vendor=openai score={score} "
                "author=anthropic obstacles=none")

    def run_main(self, head, failing=None, argv=(), bodies=None):
        import io
        from contextlib import redirect_stdout
        from unittest import mock

        facts = RepoFacts(self.c.r, failing)
        buf = io.StringIO()
        with mock.patch.object(pps, "fetch_comments",
                               return_value=[comment(b) for b in bodies or [self.body()]]), \
                mock.patch.object(pps, "fetch_head", return_value=head), \
                mock.patch.object(pps, "fetch_files", return_value=["docs/x.md"]), \
                mock.patch.object(pps, "GitHubFacts", return_value=facts), \
                redirect_stdout(buf):
            code = pps.main(["--pr", "7", *argv])
        return code, buf.getvalue()

    def test_carry_then_a_failed_check_on_the_head_replaces_the_carried_success(self):
        h0, h2 = self.c.h0, self.c.h2
        _, out = self.run_main(h2)
        self.assertIn(f"statuses/{h2} -f state=success -f context=pp/score "
                      f"-f 'description=carried from {h0[:7]}: 9/10 from gpt-6.2-sol'", out)
        _, out = self.run_main(h2, failing={h2: "rust (linux) is failure"})
        self.assertIn(f"statuses/{h2} -f state=failure -f context=pp/score "
                      f"-f 'description=no score and no carry: {h2[:7]}: rust (linux) is failure'", out)
        self.assertNotIn(f"statuses/{h2} -f state=success", out)

    def test_a_newer_failing_record_on_the_source_withdraws_the_carry(self):
        # The new record's comment runs the workflow, which posts failure on the current head.
        _, out = self.run_main(self.c.h2, bodies=[self.body(9), self.body(7)])
        self.assertIn(f"statuses/{self.c.h2} -f state=failure", out)
        self.assertNotIn(f"statuses/{self.c.h2} -f state=success", out)

    def test_a_low_valid_record_on_a_later_head_blocks_the_carry(self):
        # h1's 5 stops the walk at h1, bare or wrapped in backticks: both evaluators read both forms.
        low = (f"pp-score head={self.c.h1} seat=gpt-6.2-sol vendor=openai score=5 author=anthropic "
               "obstacles=none")
        for line in (low, f"`{low}`"):
            with self.subTest(line=line):
                _, out = self.run_main(self.c.h2, bodies=[self.body(), line])
                self.assertIn(f"-f 'description=no score and no carry: {self.c.h1[:7]}", out)
                self.assertNotIn(f"statuses/{self.c.h2} -f state=success", out)

    def test_merge_group_decides_the_carry_again(self):
        """The merge_group run decides the carry from live facts: a failing check on the carried
        head or a failing newer record fails the queue entry. A superseded hop's checks do not."""
        import io
        from contextlib import redirect_stdout
        from unittest import mock

        h1, h2 = self.c.h1, self.c.h2
        trust = pps.load_trust()
        for failing, bodies, state in (({}, [self.body()], "success"),
                                       ({h1: "actionlint is failure"}, [self.body()], "success"),
                                       ({h2: "actionlint is failure"}, [self.body()], "failure"),
                                       ({}, [self.body(9), self.body(7)], "failure")):
            posted = []
            with self.subTest(failing=failing, bodies=len(bodies)), \
                    mock.patch.object(pps, "fetch_comments", return_value=[comment(b) for b in bodies]), \
                    mock.patch.object(pps, "fetch_head", return_value=h2), \
                    mock.patch.object(pps, "fetch_files", return_value=["docs/x.md"]), \
                    mock.patch.object(pps, "GitHubFacts", return_value=RepoFacts(self.c.r, failing)), \
                    mock.patch.object(pps, "apply_plan", side_effect=lambda repo, plan: posted.append(plan)), \
                    redirect_stdout(io.StringIO()):
                pps.run_merge_group("o/r", SHA, OTHER, None, trust, post=True, prs=[7])
                self.assertEqual([(p.head, p.state) for p in posted], [(SHA, state)])

    def test_pr_number_must_be_canonical(self):
        import io
        from contextlib import redirect_stderr

        for bad in ("01865", " 1865", "1865 ", "+1865", "0", "-1"):
            with self.subTest(pr=bad), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as exit_:
                pps.main(["--pr", bad])
            self.assertEqual(exit_.exception.code, 2)

    def test_carry_only_refuses_post_and_merge_group(self):
        import io
        from contextlib import redirect_stderr
        from unittest import mock

        for extra in (["--pr", "7", "--carry-only", "--post"],
                      ["--merge-group", "--carry-only", "--head-sha", SHA, "--base-sha", OTHER]):
            with mock.patch.object(pps, "run_merge_group", side_effect=AssertionError), \
                    mock.patch.object(pps, "fetch_comments", side_effect=AssertionError), \
                    mock.patch.object(pps, "apply_plan", side_effect=AssertionError), \
                    redirect_stderr(io.StringIO()) as err:
                self.assertEqual(pps.main(extra), 2)
            self.assertIn("comment-mode dry runs only", err.getvalue())


class CarryLimitTests(unittest.TestCase):
    def test_five_hops_carry_and_six_do_not(self):
        r = CarryRepo()
        base = r.commit("a.txt", "a\n", "base")
        r.branch("pr", base)
        h0 = r.commit("p.txt", "pr\n", "pr")
        heads = []
        for i in range(6):
            r.git("checkout", "-q", "main")
            main = r.commit(f"m{i}.txt", f"{i}\n", f"main {i}")
            r.git("checkout", "-q", "pr")
            heads.append(r.merge(main))
        plan, why = carry_both(self, r, heads[4], [record(h0)])
        self.assertEqual((plan.state, why), ("success", f"carried from {h0[:7]}"))
        self.assertEqual(carry_both(self, r, heads[5], [record(h0)]),
                         (None, "no scored head within 5 merges of main"))

    def test_a_failed_unshallow_fetch_refuses(self):
        import subprocess

        CarryTests.setUpClass()
        c = CarryTests
        clone = c.r.path + "-shallow"
        subprocess.run(["git", "clone", "-q", "--depth", "1", "--branch", "pr", f"file://{c.r.path}", clone],
                       check=True, env=c.r.env, capture_output=True)
        subprocess.run(["git", "-C", clone, "remote", "set-url", "origin", clone + "-missing"],
                       check=True, env=c.r.env, capture_output=True)
        git = pps.LocalGit(clone)
        self.assertEqual(git._in_clone("rev-parse", "--is-shallow-repository").stdout.strip(), "true")
        plan, why = carry_both(self, c.r, c.h2, [record(c.h0)], git=git)
        self.assertIsNone(plan)
        self.assertTrue(why.startswith("cannot verify carry: CalledProcessError"), why)


class ConfigIsolationTests(unittest.TestCase):
    """LocalGit reads no configuration it did not write (GRE-1859 round 3)."""

    # Settings that change what plain git prints for these commands.
    SETTINGS = {"merge.renameLimit": "1", "diff.renameLimit": "1", "diff.algorithm": "patience", "diff.relative": "true",
                "merge.conflictStyle": "zdiff3", "diff.noprefix": "true", "diff.mnemonicPrefix": "true",
                "diff.external": "true", "diff.renames": "copies", "diff.ignoreSubmodules": "all", "diff.context": "0",
                "diff.interHunkContext": "9", "diff.indentHeuristic": "false", "diff.suppressBlankEmpty": "true",
                "core.quotePath": "false", "core.abbrev": "7", "color.ui": "always", "color.diff": "always",
                "log.showSignature": "true", "log.decorate": "full", "merge.renames": "false", "core.eol": "crlf",
                "merge.directoryRenames": "true", "merge.renormalize": "true", "core.commitGraph": "true",
                "i18n.logOutputEncoding": "ISO-8859-1", "core.autocrlf": "true", "diff.dirstat": "files"}

    def outputs(self, r, h0, h, side):
        git = pps.LocalGit(f"{r.path}/d1")  # from a subdirectory, where diff.relative would apply
        main = git.parents(h)[1]
        (base,) = git.merge_bases(h0, main)
        return (git.parents(h), git.first_parents(main, 50), git.tree(h), git.merge_bases(h0, main),
                git.merge_tree(h0, main), git.merge_tree(h0, side), git.diff(base, h0), git.diff(main, h),
                git.diff(base, side), git.changed(base, main), git.touched(h0, base, main),
                merge_class.hop(RepoFacts(r, git=git), h, merge_class.load_classes()))

    def test_no_setting_anywhere_changes_what_local_git_reads(self):
        import os
        import tempfile
        from pathlib import Path

        r, h0, h = rename_repo()
        side = r.line("side", r.git("rev-parse", f"{h0}~1"), ("x.txt", "x\n"))
        r.git("checkout", "-q", "pr")
        clean = self.outputs(r, h0, h, side)
        self.assertEqual(clean[-1], (None, "the merge of main had conflicts"))
        home = Path(tempfile.mkdtemp(prefix="pp-home-"))
        (home / "git").mkdir()
        (home / "attributes").write_text("* binary merge=union -diff\n", encoding="utf-8")
        (home / "order").write_text("d2/*\n", encoding="utf-8")
        written = {**self.SETTINGS, "core.attributesFile": str(home / "attributes"), "diff.orderFile": str(home / "order")}
        self.assertEqual(len(written), 30, "the docs and PR body say 30 settings")
        text = "".join(f"[{key.rsplit('.', 1)[0]}]\n\t{key.rsplit('.', 1)[1]} = {value}\n" for key, value in written.items())
        for name in (".gitconfig", "git/config", "include.cfg"):
            (home / name).write_text(text, encoding="utf-8")
        local = Path(r.path, ".git", "config")
        local.write_text(local.read_text(encoding="utf-8") + text + f"[include]\n\tpath = {home / 'include.cfg'}\n",
                         encoding="utf-8")
        Path(r.path, ".git", "info").mkdir(exist_ok=True)
        Path(r.path, ".git", "info", "attributes").write_text("* binary merge=union -diff\n", encoding="utf-8")
        # The settings are live: plain git in the clone now merges the rename cleanly.
        self.assertEqual(r.git("merge-tree", "--write-tree", h0, "main", check=False).count("CONFLICT"), 0)
        env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home), "GIT_DIFF_OPTS": "-u0",
               "GIT_EXTERNAL_DIFF": "true", "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "merge.renameLimit",
               "GIT_CONFIG_VALUE_0": "1", "GIT_CONFIG_PARAMETERS": "'diff.relative'='true'"}
        with mock.patch.dict(os.environ, env):
            os.environ.pop("GIT_CONFIG_GLOBAL", None)
            self.assertEqual(self.outputs(r, h0, h, side), clean)


class FetchIsolationTests(unittest.TestCase):
    """gemini-4-neon, rounds 4 and 5: what the coordinator's own fetch can reach."""

    def test_a_read_never_fetches_or_runs_a_transport_program(self):
        # A GIT_SSH_COMMAND run by an on-demand fetch once poisoned the scratch config between two hops.
        import os
        import subprocess
        import tempfile

        r, h0, h = rename_repo()
        r.git("checkout", "-q", "main")
        r.commit("c.txt", "c\n", "main adds c")
        r.git("checkout", "-q", "pr")
        h2 = r.merge("main")
        r.git("config", "uploadpack.allowFilter", "true")
        r.git("config", "uploadpack.allowAnySHA1InWant", "true")
        clone = tempfile.mkdtemp(prefix="pp-partial-")
        subprocess.run(["git", "clone", "-q", "--filter=blob:none", "--no-checkout", f"file://{r.path}", clone],
                       check=True, env=r.env, capture_output=True)
        r.git("-C", clone, "remote", "set-url", "origin", f"ssh://fakehost{r.path}")
        marker, script = os.path.join(clone, "ssh-ran"), os.path.join(clone, "fake-ssh")
        with open(script, "w", encoding="utf-8") as out:
            out.write(f'#!/bin/sh\ntouch {marker}\nprintf "[merge]\\n\\trenameLimit = 1\\n" >> "$GIT_DIR/config"\n'
                      'shift\nexec sh -c "$*"\n')
        os.chmod(script, 0o755)
        with mock.patch.dict(os.environ, {"GIT_SSH_COMMAND": script}):
            plan, why = carry_both(self, r, h2, [record(h0)], git=pps.LocalGit(clone))
        self.assertIsNone(plan)
        self.assertTrue(why.startswith("cannot verify carry: CalledProcessError"), why)
        self.assertFalse(os.path.exists(marker))
        self.assertEqual(carry_both(self, r, h2, [record(h0)]), (None, f"{h[:7]}: the merge of main had conflicts"))

    def test_no_caller_git_variable_reaches_a_read(self):
        import os
        import subprocess

        CarryTests.setUpClass()
        calls, run = [], subprocess.run
        caller = dict.fromkeys(("GIT_SSH_COMMAND", "GIT_SSH", "GIT_ASKPASS", "GIT_PROXY_COMMAND", "GIT_FOO"), "x")
        with mock.patch.dict(os.environ, caller), \
                mock.patch.object(subprocess, "run", lambda *a, **kw: calls.append((a[0], kw.get("env") or {})) or run(*a, **kw)):
            self.assertEqual(merge_class.hop(RepoFacts(CarryTests.r), CarryTests.h1, merge_class.load_classes())[0],
                             CarryTests.h0)
        reads = [env for _, env in calls if "GIT_DIR" in env]
        self.assertGreater(len(reads), 5)
        common = [env for command, env in calls if "--git-common-dir" in command]
        self.assertTrue(common and not any(k.startswith("GIT_") for env in common for k in env), common)
        self.assertEqual({k for env in reads for k in env if k.startswith("GIT_")},
                         {"GIT_DIR", "GIT_CONFIG_NOSYSTEM", "GIT_CONFIG_GLOBAL", "GIT_ATTR_NOSYSTEM", "GIT_GRAFT_FILE",
                          "GIT_NO_LAZY_FETCH"})

    def test_a_fetch_never_touches_the_directory_later_reads_use(self):
        # The clone lacks main's new tip, so on_main fetches it; the fetch program plants files in every
        # scratch directory it finds, and later reads must use one it never saw.
        import os
        import subprocess
        import tempfile
        from pathlib import Path

        CarryTests.setUpClass()
        c = CarryTests
        clone = tempfile.mkdtemp(prefix="pp-behind-")
        subprocess.run(["git", "clone", "-q", "--no-checkout", c.r.path, clone], check=True, env=c.r.env)
        c.r.git("checkout", "-q", "main")
        c.r.commit("d.txt", "d\n", "main moves on")
        seen, script, private = os.path.join(clone, "seen"), os.path.join(clone, "fake-ssh"), tempfile.mkdtemp()
        with open(script, "w", encoding="utf-8") as out:
            out.write(f'#!/bin/sh\nfor d in {private}/pp-carry-git-*; do echo "$d" >> {seen}; '
                      'echo planted > "$d/objects/planted"; done\nshift\nexec sh -c "$*"\n')
        os.chmod(script, 0o755)
        c.r.git("-C", clone, "remote", "set-url", "origin", f"ssh://fakehost{c.r.path}")
        git = pps.LocalGit(clone)
        with mock.patch.dict(os.environ, {"GIT_SSH_COMMAND": script}), mock.patch.object(tempfile, "tempdir", private):
            self.assertEqual(carry_both(self, c.r, c.h2, [record(c.h0)], git=git)[1], f"carried from {c.h0[:7]}")
        planted = set(Path(seen).read_text(encoding="utf-8").split())
        self.assertTrue(planted)
        self.assertNotIn(str(git._scratch[0]), planted)


class CommentModeOrderTests(unittest.TestCase):
    """Runs for one PR can finish out of order. Each run re-reads the comments, head and head
    checks after posting, up to SETTLE_ROUNDS times, and fails closed if they are still changing."""

    @classmethod
    def setUpClass(cls):
        CarryTests.setUpClass()
        cls.c = CarryTests

    def run_posting(self, comment_lists, head, facts=None, on_post=None):
        import io
        from contextlib import redirect_stderr, redirect_stdout
        from unittest import mock

        reads = iter(comment_lists)
        last = []

        def comments(repo, pr):
            last[:] = [next(reads, last[0] if last else [])]
            return last[0]

        posted = []

        def post(repo, plan):
            posted.append(plan)
            if on_post:
                on_post(plan)

        with mock.patch.object(pps, "fetch_comments", side_effect=comments), \
                mock.patch.object(pps, "fetch_head", side_effect=head), \
                mock.patch.object(pps, "fetch_files", return_value=["docs/x.md"]), \
                mock.patch.object(pps, "GitHubFacts", return_value=facts or RepoFacts(self.c.r)), \
                mock.patch.object(pps, "apply_plan", side_effect=post), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
            code = pps.main(["--pr", "7", "--post"])
        return code, posted, err.getvalue()

    def test_facts_still_changing_after_three_rounds_fail_closed(self):
        import itertools

        # Every read sees one more comment, so the run never confirms what it posted.
        reads = ([self.comment(9, i) for i in range(n)] for n in itertools.count(1))
        code, posted, err = self.run_posting(reads, lambda repo, pr: self.c.h2)
        on_h2 = [(p.state, p.description) for p in posted if p.head == self.c.h2]
        self.assertEqual(code, 1)
        self.assertEqual(len(on_h2), pps.SETTLE_ROUNDS + 1)
        self.assertEqual(on_h2[-1], ("failure", pps.UNSETTLED))
        self.assertIn("failed closed", err)

    def test_a_check_failing_after_the_decision_flips_the_carried_success(self):
        h2 = self.c.h2
        facts = RepoFacts(self.c.r)

        def check_fails_once_posted(plan):
            if plan.head == h2 and plan.state == "success":
                facts.failing[h2] = "zizmor is failure"

        code, posted, _ = self.run_posting([[self.comment(9, 1)]], lambda repo, pr: h2, facts,
                                           check_fails_once_posted)
        self.assertEqual((code, [p.state for p in posted if p.head == h2]), (0, ["success", "failure"]))
        self.assertIn("zizmor is failure", posted[-1].description)

    def comment(self, score, id_):
        line = body(record(self.c.h0, score=score))
        return dict(comment(line), id=id_)

    def test_a_run_that_decided_before_a_new_record_posts_again(self):
        # This run read only the 9. A newer run posted failure for the 7 before this run posted
        # its carried success; the re-read after posting sees the 7 and posts failure again.
        old = [self.comment(9, 1)]
        new = [self.comment(9, 1), self.comment(7, 2)]
        code, posted, _ = self.run_posting([old, new], lambda repo, pr: self.c.h2)
        on_h2 = [p.state for p in posted if p.head == self.c.h2]
        self.assertEqual((code, on_h2), (0, ["success", "failure"]))

    def test_checks_that_change_mid_decision_never_post_a_stale_success(self):
        # The checks read in_progress, then success, then in_progress again. When the snapshot and
        # decide_carry read them separately, the carry decided from success and the run settled on
        # in_progress. Now one read decides and is recorded, and the next read is compared to it.
        h2, facts, reads = self.c.h2, RepoFacts(self.c.r), iter(["zizmor is in_progress", None])
        facts.checks_failing = lambda sha: next(reads, "zizmor is in_progress")
        code, posted, _ = self.run_posting([[self.comment(9, 1)]], lambda repo, pr: h2, facts)
        self.assertEqual((code, [p.state for p in posted if p.head == h2]), (0, ["failure", "failure"]))

    def test_a_settled_run_posts_once(self):
        code, posted, _ = self.run_posting([[self.comment(9, 1)]], lambda repo, pr: self.c.h2)
        self.assertEqual((code, [(p.head, p.state) for p in posted]),
                         (0, [(self.c.h0, "success"), (self.c.h2, "success")]))

    def test_an_unreadable_head_fails_the_run_and_posts_nothing_for_it(self):
        def no_head(repo, pr):
            raise RuntimeError("GitHub is down")

        code, posted, err = self.run_posting([[self.comment(9, 1)]], no_head)
        self.assertEqual((code, [(p.head, p.state) for p in posted]), (1, [(self.c.h0, "success")]))
        self.assertIn("its pp/score is unchanged; the merge_group run decides it again", err)


class ChangedPathTests(unittest.TestCase):
    @staticmethod
    def entry(name, status="modified", previous=None):
        return {"filename": name, "status": status, "previous_filename": previous}

    def test_a_rename_out_of_a_sensitive_path_is_strict(self):
        moved = self.entry("rust/crates/pp-storage/src/schema_old.rs", "renamed",
                           "rust/crates/pp-storage/src/schema.rs")
        paths = pps.changed_paths([moved], 1)
        self.assertEqual(paths, ["rust/crates/pp-storage/src/schema_old.rs", "rust/crates/pp-storage/src/schema.rs"])
        self.assertTrue(pps.strict_class(paths))
        self.assertFalse(pps.strict_class(paths[:1]))

    def test_incomplete_or_capped_lists_refuse(self):
        one = [self.entry("docs/x.md")]
        for entries, count in (([], 0), (one, 2), (one, None), (one * 3000, 3000),
                               ([self.entry("a", "renamed", None)], 1)):
            with self.assertRaises(pps.GateError):
                pps.changed_paths(entries, count)

    def test_workflow_runs_for_one_pr_share_a_concurrency_group(self):
        from pathlib import Path

        text = Path(pps.ROOT, ".github/workflows/post-pp-score-status.yml").read_text(encoding="utf-8")
        block = text.split("\nconcurrency:\n", 1)[1].split("\n\n", 1)[0]
        self.assertIn("github.event.issue.number || inputs.pr", block)
        self.assertIn("github.event.merge_group.head_sha", block)
        self.assertIn("cancel-in-progress: false", block)

    def test_every_run_checks_out_the_default_branch_or_the_queue_base(self):
        # A dispatch from another ref must not run that ref's copy of the script or merge-classes.json.
        from pathlib import Path

        text = Path(pps.ROOT, ".github/workflows/post-pp-score-status.yml").read_text(encoding="utf-8")
        self.assertIn("ref: ${{ github.event_name == 'merge_group' && github.event.merge_group.base_sha"
                      " || github.event.repository.default_branch }}", text)


class RequiredCheckTests(unittest.TestCase):
    RULES = [
        {"type": "deletion"},
        {"type": "required_status_checks", "parameters": {"required_status_checks": [
            {"context": "actionlint", "integration_id": 15368},
            {"context": "rust (linux)", "integration_id": 15368},
            {"context": "legacy status"},
            {"context": "pp/score"},
        ]}},
    ]

    def facts(self, runs, statuses=({"context": "legacy status", "state": "success"},)):
        def fake(path):
            if "/rules/branches/" in path:
                return self.RULES
            if "/check-runs?" in path:
                name = urllib.parse.unquote(path.split("check_name=")[1].split("&")[0])
                return {"check_runs": [r for r in runs if r["name"] == name]}
            if path.endswith("/status"):
                return {"statuses": list(statuses)}
            raise AssertionError(path)

        return pps.GitHubFacts("o/r", fake)

    @staticmethod
    def run_(name, conclusion, status="completed"):
        return {"name": name, "status": status, "conclusion": conclusion}

    def green(self, **override):
        runs = {"actionlint": self.run_("actionlint", "success"), "rust (linux)": self.run_("rust (linux)", "success")}
        runs.update(override)
        return [r for r in runs.values() if r is not None]

    def test_reads_required_checks_from_main_rules_without_pp_score(self):
        facts = self.facts([])
        self.assertEqual([c["context"] for c in facts.required_checks()],
                         ["actionlint", "rust (linux)", "legacy status"])

    def test_only_success_passes(self):
        self.assertIsNone(self.facts(self.green()).checks_failing(SHA))
        # Fail closed: the gate cannot prove that a skip came from the path filter rather than a
        # draft, a failed needs job or a cancelled run, so no outcome but success qualifies.
        for conclusion in ("skipped", "neutral", "cancelled", "timed_out", "action_required", "stale",
                           "failure", "startup_failure", None):
            with self.subTest(conclusion=conclusion):
                facts = self.facts(self.green(actionlint=self.run_("actionlint", conclusion)))
                self.assertEqual(facts.checks_failing(SHA), f"actionlint is {conclusion or 'completed'}")

    def test_running_failed_or_missing_checks_block(self):
        for status in ("queued", "in_progress", "pending", "waiting"):
            with self.subTest(status=status):
                facts = self.facts(self.green(**{"rust (linux)": self.run_("rust (linux)", None, status)}))
                self.assertEqual(facts.checks_failing(SHA), f"rust (linux) is {status}")
        # One success among the latest runs does not hide a failing one.
        facts = self.facts(self.green() + [self.run_("rust (linux)", "failure")])
        self.assertEqual(facts.checks_failing(SHA), "rust (linux) is failure")
        facts = self.facts(self.green(**{"rust (linux)": None}))
        self.assertEqual(facts.checks_failing(SHA), "rust (linux) has not run")

    def test_a_required_commit_status_must_be_success(self):
        for state, why in (("pending", "pending"), ("failure", "failure"), ("error", "error"), (None, "missing")):
            with self.subTest(state=state):
                statuses = [{"context": "legacy status", "state": state}] if state else []
                self.assertEqual(self.facts(self.green(), statuses).checks_failing(SHA), f"legacy status is {why}")

    def test_no_required_checks_fails_closed(self):
        self.RULES = [{"type": "deletion"}]
        with self.assertRaises(ValueError):
            self.facts([]).checks_failing(SHA)


if __name__ == "__main__":
    unittest.main()
