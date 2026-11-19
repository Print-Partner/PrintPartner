#!/usr/bin/env python3
import json
import unittest
import urllib.parse

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
        records = pps.trusted_records(comments, self.trust)
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
        plans = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual(plans[0].state, "failure")
        self.assertEqual(plans[0].description, "7/10 from grok-4.7")

    def test_legacy_score_line_is_ignored(self):
        # The old "Score: N/10 from <seat>" form names no author family, so it cannot pass.
        comments = [comment(f"Score: 8/10 from claude-opus-5-5 on head {SHA}")]
        self.assertEqual(pps.trusted_records(comments, self.trust), [])

    def test_untrusted_author_and_edited_comments_are_ignored(self):
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
        self.assertEqual(pps.trusted_records(comments, self.trust), [])
        self.assertEqual(pps.plan_statuses([]), [])

    def test_prose_and_malformed_lines_are_ignored(self):
        comments = [
            comment(
                "a note about pp-score head= tokens in prose\n"
                f"pp-score head={SHA} seat=unknown vendor=xai score=9 author=anthropic obstacles=none\n"
                f"> SCORE: 9\n"
                f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none\n"
            )
        ]
        records = pps.trusted_records(comments, self.trust)
        self.assertEqual([r.seat for r in records], ["grok-4.7"])
        self.assertEqual(records[0].head, SHA)

    def test_records_stay_on_their_own_sha(self):
        comments = [
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"),
            comment(f"pp-score head={OTHER} seat=grok-4.7 vendor=xai score=6 author=anthropic obstacles=none"),
        ]
        plans = {p.head: p for p in pps.plan_statuses(pps.trusted_records(comments, self.trust))}
        self.assertEqual(plans[SHA].state, "success")
        self.assertEqual(plans[OTHER].state, "failure")

    def test_minimum_score_sets_state(self):
        comments = [
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"),
            comment(
                f"pp-score head={SHA} seat=glm-5.3-flash-free vendor=zhipu score=5 author=anthropic obstacles=none"
            ),
        ]
        plans = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual(plans[0].state, "failure")
        self.assertIn("9/10 from grok-4.7", plans[0].description)
        self.assertIn("5/10 from glm-5.3-flash-free", plans[0].description)

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

    def test_real_records_without_obstacles_no_longer_count(self):
        for body in (PR1777_BODY, PR1734_ED251_BODY, PR1734_765_BODY):
            with self.subTest(body=body[-60:]):
                self.assertEqual(pps.trusted_records([comment(body)], self.trust), [])

    def test_backticked_records_from_1777_and_1734_parse_with_obstacles(self):
        for body, head in ((PR1777_BODY, PR1777_HEAD), (PR1734_ED251_BODY, PR1734_ED251_HEAD)):
            with self.subTest(head=head):
                body = body.replace("author=anthropic`", "author=anthropic obstacles=none`")
                records = pps.trusted_records([comment(body)], self.trust)
                self.assertEqual([(r.head, r.seat, r.score) for r in records], [(head, "grok-4.7", 10.0)])
                (plan,) = pps.plan_statuses(records)
                self.assertEqual((plan.state, plan.description), ("success", "10/10 from grok-4.7"))

    def test_bare_records_from_1734_parse_with_obstacles(self):
        body = PR1734_765_BODY.replace("author=anthropic\n", "author=anthropic obstacles=none\n")
        records = pps.trusted_records([comment(body)], self.trust)
        self.assertEqual({r.head for r in records}, {PR1734_765_HEAD})
        self.assertEqual([r.seat for r in records], ["grok-4.7", "glm-5.3-flash-free"])

    def test_fenced_and_double_backticked_records_parse(self):
        line = f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"
        for body in (f"```text\n{line}\n```", f"``{line}``", f"  `{line}`  ", f"```\n  {line}\n```"):
            with self.subTest(body=body):
                records = pps.trusted_records([comment(body)], self.trust)
                self.assertEqual([r.seat for r in records], ["grok-4.7"])

    def test_records_inside_prose_still_do_not_parse(self):
        line = f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic obstacles=none"
        for body in (f"Recorded `{line}` today.", f"`{line}` and more", f"> {line}", f"`{line}"):
            with self.subTest(body=body):
                self.assertEqual(pps.trusted_records([comment(body)], self.trust), [])


class FamilyTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def test_same_family_score_fails_the_head(self):
        comments = [comment(
            f"pp-score head={SHA} seat=claude-opus-5-5 vendor=anthropic score=10 author=anthropic obstacles=none"
        )]
        (plan,) = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual(plan.state, "failure")
        self.assertEqual(plan.description,
                         "rejected: claude-opus-5-5 is the author's own family (anthropic)")

    def test_same_family_score_fails_even_beside_a_passing_seat(self):
        comments = [comment(
            f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=10 author=anthropic obstacles=none\n"
            f"pp-score head={SHA} seat=claude-opus-5-5 vendor=anthropic score=10 author=anthropic obstacles=none"
        )]
        (plan,) = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual(plan.state, "failure")

    def test_codex_scoring_a_codex_pr_fails(self):
        comments = [comment(f"pp-score head={SHA} seat=gpt-6.1-sol vendor=openai score=9 author=openai obstacles=none")]
        (plan,) = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual(plan.state, "failure")
        self.assertIn("(openai)", plan.description)

    def test_records_that_disagree_on_author_fail(self):
        comments = [comment(
            f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=10 author=anthropic obstacles=none\n"
            f"pp-score head={SHA} seat=glm-5.3-flash-free vendor=zhipu score=9 author=openai obstacles=none"
        )]
        (plan,) = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual((plan.state, plan.description),
                         ("failure", "rejected: records disagree on the author family"))


class CodexSeatTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def test_codex_seat_scores_a_claude_pr(self):
        comments = [comment(f"pp-score head={SHA} seat=gpt-6.1-sol vendor=openai score=9 author=anthropic obstacles=none")]
        (plan,) = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.1-sol"))

    def test_codex_seat_must_name_openai(self):
        comments = [comment(f"pp-score head={SHA} seat=gpt-6.1-sol vendor=xai score=9 author=anthropic obstacles=none")]
        self.assertEqual(pps.trusted_records(comments, self.trust), [])

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
        (plan,) = pps.plan_statuses(pps.trusted_records([comment(line)], self.trust))
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.2-astra"))
        self.assertEqual(pps.trusted_records([comment(line.replace("vendor=openai", "vendor=cursor"))],
                                             self.trust), [])

    def test_gemini_flash_and_qwen_flash_count_on_an_xai_pr(self):
        for seat, vendor in (("gemini-3.8-flash", "google"), ("qwen38-flash", "qwen")):
            with self.subTest(seat=seat):
                body = (f"pp-score head={SHA} seat={seat} vendor={vendor} "
                        "score=9 author=xai obstacles=none")
                (plan,) = pps.plan_statuses(pps.trusted_records([comment(body)], self.trust))
                self.assertEqual((plan.state, plan.description), ("success", f"9/10 from {seat}"))
                wrong = body.replace(f"vendor={vendor}", "vendor=cursor")
                self.assertEqual(pps.trusted_records([comment(wrong)], self.trust), [])
                own = body.replace("author=xai", f"author={vendor}")
                (plan,) = pps.plan_statuses(pps.trusted_records([comment(own)], self.trust))
                self.assertEqual(plan.state, "failure")

    def test_sol_6_2_seat_scores_a_claude_pr(self):
        comments = [comment(
            f"pp-score head={SHA} seat=gpt-6.2-sol vendor=openai score=9 author=anthropic obstacles=none")]
        (plan,) = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.2-sol"))
        wrong = comments[0]["body"].replace("vendor=openai", "vendor=xai")
        self.assertEqual(pps.trusted_records([comment(wrong)], self.trust), [])
        own = comments[0]["body"].replace("author=anthropic", "author=openai")
        (plan,) = pps.plan_statuses(pps.trusted_records([comment(own)], self.trust))
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
        (record,) = pps.trusted_records([comment(self.line(" obstacles=none"))], self.trust)
        self.assertEqual(record.obstacles, ())
        (plan,) = pps.plan_statuses([record])
        self.assertEqual((plan.state, plan.description), ("success", "9/10 from gpt-6.1-sol"))

    def test_missing_field_does_not_count(self):
        self.assertEqual(pps.trusted_records([comment(self.line(""))], self.trust), [])
        self.assertEqual(pps.plan_statuses(pps.trusted_records([comment(self.line(""))], self.trust)), [])

    def test_listed_obstacles_still_count_as_records(self):
        for token, expected in (("refusal", ("refusal",)), ("guard", ("guard",)),
                                ("approval-stall", ("approval-stall",)),
                                ("guard,refusal", ("guard", "refusal")),
                                ("refusal,guard,approval-stall", ("refusal", "guard", "approval-stall"))):
            with self.subTest(token=token):
                (record,) = pps.trusted_records([comment(self.line(f" obstacles={token}"))], self.trust)
                self.assertEqual(record.obstacles, expected)
                (plan,) = pps.plan_statuses([record])
                self.assertEqual(plan.state, "success")

    def test_invalid_tokens_do_not_count(self):
        for token in ("", "None", "NONE", "none,guard", "guard,guard", "guard,", ",guard", "stall",
                      "approval_stall", "guard refusal", "guard;refusal", "yes", "0"):
            with self.subTest(token=token):
                self.assertEqual(pps.trusted_records([comment(self.line(f" obstacles={token}"))], self.trust), [])

    def test_field_must_come_last(self):
        line = (f"pp-score head={SHA} seat=gpt-6.1-sol vendor=openai score=9 obstacles=none author=anthropic")
        self.assertEqual(pps.trusted_records([comment(line)], self.trust), [])
        self.assertEqual(pps.trusted_records([comment(self.line(" obstacles=none extra"))], self.trust), [])

    def test_whole_number_and_exact_head_rules_still_hold(self):
        for line in (self.line(" obstacles=none").replace("score=9", "score=8.5"),
                     self.line(" obstacles=none").replace(SHA, SHA[:12])):
            with self.subTest(line=line):
                self.assertEqual(pps.trusted_records([comment(line)], self.trust), [])

    def test_trial_line_is_not_a_record(self):
        line = self.line(" obstacles=none").replace("pp-score ", "pp-score-trial ", 1)
        self.assertEqual(pps.trusted_records([comment(line)], self.trust), [])


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
        return pps.trusted_records([comment(
            f"`pp-score head={head} seat=grok-4.7 vendor=xai score={score} author={author} obstacles=none`"
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
             f"#1830 {SHA[:7]}: 7/10 from grok-4.7"),
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
            "repos/o/r/issues/1829/comments": [comment(PR1777_BODY.replace("anthropic`", "anthropic obstacles=none`"))],
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
        self.assertEqual(code, 0)
        self.assertEqual([(p.head, p.state, p.description) for p in posted],
                         [(QUEUE_HEAD, "success", "#1829: 10/10 from grok-4.7")])



class CursorAutoAuthorTests(unittest.TestCase):
    def setUp(self):
        self.trust = pps.load_trust()

    def records(self, *lines):
        return pps.trusted_records([comment("\n".join(lines))], self.trust)

    def line(self, head, seat, vendor, score, author="cursor-auto"):
        return f"pp-score head={head} seat={seat} vendor={vendor} score={score} author={author} obstacles=none"

    def test_cursor_auto_record_parses(self):
        records = self.records(self.line(SHA, "claude-opus-5-5", "anthropic", 9))
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
        self.assertEqual(plan.description, "9/10 from claude-opus-5-5; 7/10 from gpt-6.1-sol")

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

    def dual(self, head, anthropic=9, openai=8, author="cursor-auto"):
        return pps.trusted_records([comment(
            f"pp-score head={head} seat=claude-opus-5-5 vendor=anthropic score={anthropic} author={author} obstacles=none\n"
            f"pp-score head={head} seat=gpt-6.1-sol vendor=openai score={openai} author={author} obstacles=none"
        )], self.trust)

    def one(self, head, seat="claude-opus-5-5", vendor="anthropic", score=9, author="cursor-auto"):
        return pps.trusted_records([comment(
            f"pp-score head={head} seat={seat} vendor={vendor} score={score} author={author} obstacles=none"
        )], self.trust)

    def test_merge_group_passes_when_both_families_score_the_current_head(self):
        plan = pps.plan_merge_group(
            QUEUE_HEAD, [1829], {1829: SHA}, {1829: self.dual(SHA)})
        self.assertEqual((plan.state, plan.description),
                         ("success", "#1829: 9/10 from claude-opus-5-5; 8/10 from gpt-6.1-sol"))

    def test_merge_group_fails_when_one_family_is_missing_on_the_current_head(self):
        # A complete pair on a stale SHA does not count for the current head.
        records = self.dual(OTHER) + self.one(SHA)
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
            f"#1829 {SHA[:7]}: 9/10 from claude-opus-5-5; 7/10 from gpt-6.1-sol",
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


class RepoFacts(pps.GitHubFacts):
    """GitHubFacts with GitHub's two answers taken from the local repo: a commit is on main when
    it is an ancestor of main_ref, and failing maps a SHA to its first failing required check."""

    def __init__(self, repo, failing=None, main_ref="main", git=None):
        super().__init__("o/r", self.no_api, git or pps.LocalGit(repo.path))
        self.repo_, self.failing, self.main_ref = repo, failing or {}, main_ref

    def on_main(self, sha):
        return self.repo_.is_ancestor(sha, self.main_ref)

    def checks_failing(self, sha):
        return self.failing.get(sha)

    @staticmethod
    def no_api(path):
        raise AssertionError(f"unexpected GitHub read {path}")


def record(head, seat="gpt-6.2-sol", vendor="openai", score=9, author="anthropic"):
    return pps.Record(head, seat, vendor, author, float(score), ())


def body(rec, obstacles="none"):
    return (f"pp-score head={rec.head} seat={rec.seat} vendor={rec.vendor} score={pps.format_score(rec.score)} "
            f"author={rec.author} obstacles={obstacles}")


CONFIG = json.loads(pps.CLASSES.read_text(encoding="utf-8"))
NO_CARRY_REASON = "no records on this head and no carry: "


def carry_both(test, repo, head, records, strict=False, failing=None, main_ref="main"):
    """The only way these tests evaluate a carry (test_every_carry_goes_through_carry_both checks
    that). Returns plan_carry's result, and asserts that the coordinator's sensitive check agrees
    with the strict status on these records and again with a second vendor at 9 added beside each
    record: the guard carries only what the status carries, from the same source, and a carry both
    refuse is refused for the same reason. One seat passing the status alone is the known case."""
    def facts():
        return RepoFacts(repo, failing, main_ref)

    result = pps.plan_carry(head, records, strict, facts())
    companions = [pps.Record(r.head, "glm-5.3-flash-free", "zhipu", r.author, 9.0, ()) for r in records]
    for group in (records, records + companions):
        status, status_why = pps.plan_carry(head, group, True, facts())
        source, guard_why = merge_class.sensitive_scores([body(r) for r in group], head, CONFIG, facts(),
                                                         merge_class.load_classes())
        if source is not None and source != head:
            test.assertIsNotNone(status, f"the guard carried what the status refused: {status_why}")
            test.assertEqual(status_why, f"carried from {source[:7]}")
        if status is None and guard_why.startswith(NO_CARRY_REASON):
            test.assertEqual(guard_why[len(NO_CARRY_REASON):], status_why)
    return result


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
        self.assertIn("is not on main", self.refused(self.reversed))

    # hop condition 2: M is on main.

    def test_merging_a_branch_that_is_not_main_does_not_carry(self):
        side = self.r.git("rev-parse", "side-o")
        self.assertEqual(self.refused(self.feature),
                         f"{self.feature[:7]}: second parent {side[:7]} is not on main")

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
        self.assertIn(f"{self.h1[:7]} did not pass", self.refused(self.h2, [record(self.h0), record(self.h1, score=7)]))

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
        from unittest import mock

        records = [record(self.h0, score=8)]
        for paths, state in ((["rust/crates/pp-storage/src/import.rs"], None),
                             (["scripts/merge-classes.json"], None), (["docs/x.md"], "success")):
            with self.subTest(paths=paths), mock.patch.object(pps, "fetch_files", return_value=paths):
                plan, why = pps.carry_for_pr("o/r", 7, self.h1, records, RepoFacts(self.r))
                self.assertEqual(plan and plan.state, state, why)

    def test_grok_records_never_carry(self):
        grok = record(self.h0, seat="grok-4.7", vendor="xai")
        self.assertIn("no record that can carry", self.refused(self.h1, [grok]))
        plan, _ = self.plan(self.h1, [grok, record(self.h0)])
        self.assertEqual(plan.description, f"carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol")
        # A failing Grok record still fails the source, so it fails the carry too.
        low_grok = record(self.h0, seat="grok-4.7", vendor="xai", score=5)
        self.assertIn(f"{self.h0[:7]} did not pass", self.refused(self.h1, [low_grok, record(self.h0)]))

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
        self.assertIn(f"{self.h0[:7]} did not pass", self.refused(self.h1, [record(self.h0, score=7)]))

    def test_sensitive_and_owner_paths_are_strict(self):
        self.assertTrue(pps.strict_class(["rust/crates/pp-storage/src/import.rs"]))
        self.assertTrue(pps.strict_class(["scripts/merge-classes.json", "docs/x.md"]))
        self.assertFalse(pps.strict_class(["docs/product/daily-loop.md"]))

    def test_merge_group_accepts_a_carried_head(self):
        carried = self.plan(self.h1, [record(self.h0)])
        plan = pps.plan_merge_group(SHA, [1], {1: self.h1}, {1: [record(self.h0)]}, {1: carried})
        self.assertEqual((plan.state, plan.description),
                         ("success", f"#1: carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol"))
        refused = self.plan(self.edited, [record(self.h0)])
        plan = pps.plan_merge_group(SHA, [1], {1: self.edited}, {1: [record(self.h0)]}, {1: refused})
        self.assertEqual((plan.state, plan.description),
                         ("failure", f"#1: no trusted score on its head {self.edited[:7]}"))


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


class GuardAgreementTests(unittest.TestCase):
    """The pp/score status (plan_carry, strict as for a sensitive PR) and the coordinator's
    sensitive merge check (merge_class.sensitive_scores, as agent-guard.py calls it) on the same
    carry fixtures. Both use merge_class.decide_carry. The guard never accepts what the status
    refuses, and when it accepts, it accepts the same source head. The guard additionally needs
    two different non-Grok vendors from scorer_seats, so one seat, one vendor or a Grok second
    seat passes the status but not the guard: those cases are listed explicitly below."""

    @classmethod
    def setUpClass(cls):
        CarryTests.setUpClass()
        cls.c = CarryTests
        cls.config = json.loads(pps.CLASSES.read_text(encoding="utf-8"))
        cls.classes = merge_class.load_classes()

    def both(self, head, records, main_ref="main", failing=None):
        status, status_why = carry_both(self, self.c.r, head, records, True, failing, main_ref)
        source, guard_why = merge_class.sensitive_scores([body(r) for r in records], head, self.config,
                                                         RepoFacts(self.c.r, failing, main_ref), self.classes)
        return status is not None, source is not None, status_why, guard_why

    def test_every_carry_goes_through_carry_both(self):
        # A new carry fixture cannot skip the guard: plan_carry is called only inside carry_both.
        import inspect

        call = "pps." + "plan_carry("
        text = inspect.getsource(inspect.getmodule(carry_both))
        self.assertEqual(text.count(call), inspect.getsource(carry_both).count(call))
        self.assertEqual(text.count(call), 2)

    def two(self, head):
        return [record(head), record(head, seat="glm-5.3-flash-free", vendor="zhipu")]

    def test_both_carry_a_clean_hop_and_a_second_clean_hop(self):
        c = self.c
        for head in (c.h1, c.h2):
            with self.subTest(head=head):
                self.assertEqual(self.both(head, self.two(c.h0))[:2], (True, True))

    def test_both_refuse_every_failing_hop(self):
        c = self.c
        cases = [(c.edited, "main"), (c.hidden, "main"), (c.conflicted, "main-conflict"), (c.rebased, "main"),
                 (c.reversed, "main"), (c.octopus, "main"), (c.feature, "main"), (c.hunk, "main-hunk"),
                 (c.revert, "main-revert"), (c.gate, "main-gate"), (c.rewritten, "main"),
                 (c.second, "main-hunk")]
        for head, main_ref in cases:
            with self.subTest(head=head, main_ref=main_ref):
                ok_status, ok_guard, status_why, guard_why = self.both(head, self.two(c.h0), main_ref)
                self.assertEqual((ok_status, ok_guard), (False, False))
                self.assertIn(status_why, guard_why)

    def test_both_refuse_failing_records_and_checks(self):
        c = self.c
        h0, h1 = c.h0, c.h1
        glm = dict(seat="glm-5.3-flash-free", vendor="zhipu")
        cases = {
            "a seat at 8": [record(h0), record(h0, score=8, **glm)],
            "a failing Grok record": self.two(h0) + [record(h0, seat="grok-4.7", vendor="xai", score=7)],
            "the author's own family": self.two(h0) + [record(h0, seat="claude-opus-5-5", vendor="anthropic")],
            "two author families": [record(h0), record(h0, author="cursor", **glm)],
            "newer failing records": self.two(h0) + [record(h1, score=7)],
        }
        for name, records in cases.items():
            head = c.h2 if name == "newer failing records" else h1
            with self.subTest(name):
                self.assertEqual(self.both(head, records)[:2], (False, False))
        with self.subTest("a failing check on the carried head"):
            self.assertEqual(self.both(h1, self.two(h0), failing={h1: "zizmor is failure"})[:2], (False, False))

    def test_the_status_accepts_one_seat_but_the_guard_needs_two_vendors(self):
        h0, h1 = self.c.h0, self.c.h1
        for name, records in {
            "one seat": [record(h0)],
            "two seats, one vendor": [record(h0), record(h0, seat="gpt-6.2-astra")],
            "Grok as the second seat": [record(h0), record(h0, seat="grok-4.7", vendor="xai")],
        }.items():
            with self.subTest(name):
                self.assertEqual(self.both(h1, records)[:2], (True, False))

    def test_the_guard_reads_the_exact_head_first(self):
        h1 = self.c.h1
        source, _ = merge_class.sensitive_scores([body(r) for r in self.two(h1)], h1, self.config,
                                                 RepoFacts(self.c.r), self.classes)
        self.assertEqual(source, h1)


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
        self.assertEqual(git._git("rev-parse", "--is-shallow-repository").stdout.strip(), "true")
        facts = RepoFacts(c.r, git=git)
        from unittest import mock

        with mock.patch.object(pps, "fetch_files", return_value=["docs/x.md"]):
            plan, why = pps.carry_for_pr("o/r", 7, c.h2, [record(c.h0)], facts)
        self.assertIsNone(plan)
        self.assertTrue(why.startswith("cannot verify carry: CalledProcessError"), why)
        with self.assertRaises(subprocess.CalledProcessError):
            merge_class.sensitive_scores([body(record(c.h0))], c.h2, json.loads(pps.CLASSES.read_text()),
                                         facts, merge_class.load_classes())


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
