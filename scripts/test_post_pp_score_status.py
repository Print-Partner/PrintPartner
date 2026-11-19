#!/usr/bin/env python3
import unittest
import urllib.parse

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
        self.assertNotIn("gpt-6.1-sol", data["scorer_seats"])
        self.assertEqual(data["pp_score_seats"], {"gpt-6.1-sol": "openai", "gpt-6.2-sol": "openai",
                                                  "gpt-6.2-astra": "openai"})
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


if __name__ == "__main__":
    unittest.main()


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

    def write(self, name, text):
        from pathlib import Path

        Path(self.path, name).write_text(text, encoding="utf-8")

    def commit(self, name, text, message):
        self.write(name, text)
        self.git("add", name)
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def merge_main(self, main):
        """Merge main into the checked-out branch like GitHub's update-branch; parents [head, main]."""
        self.git("merge", "-q", "--no-ff", "--no-edit", main, check=False)
        return self.git("rev-parse", "HEAD")


class RepoFacts(pps.GitHubFacts):
    """GitHubFacts with the GitHub reads answered from the local repo; merges still run LocalGit."""

    def __init__(self, repo, failing=None):
        super().__init__("o/r", pps.LocalGit(repo.path))
        self.repo_ = repo
        self.failing = failing or {}

    def commit(self, sha):
        parents = self.repo_.git("rev-list", "--parents", "-n1", sha).split()[1:]
        return parents, self.repo_.git("rev-parse", f"{sha}^{{tree}}")

    def on_main(self, sha):
        import subprocess

        return subprocess.run(["git", "merge-base", "--is-ancestor", sha, "main"], cwd=self.repo_.path,
                              env=self.repo_.env).returncode == 0

    def checks_failing(self, sha):
        return self.failing.get(sha)


def record(head, seat="gpt-6.2-sol", vendor="openai", score=9, author="anthropic"):
    return pps.Record(head, seat, vendor, author, float(score), ())


class CarryTests(unittest.TestCase):
    """Chad's rule of 2026-11-18 11:02 PM ET: records carry only across clean merges of main."""

    @classmethod
    def setUpClass(cls):
        r = cls.r = CarryRepo()
        cls.base = r.commit("a.txt", "a1\n", "base")
        r.write("b.txt", "b1\n")
        cls.base = r.commit("b.txt", "b1\n", "base b")
        r.git("checkout", "-q", "-b", "pr")
        cls.h0 = r.commit("a.txt", "a2 from the PR\n", "pr change")
        r.git("checkout", "-q", "main")
        cls.m1 = r.commit("b.txt", "b2 from main\n", "main change")
        r.git("checkout", "-q", "pr")
        cls.h1 = r.merge_main("main")
        # A second clean update on top of the first.
        r.git("checkout", "-q", "main")
        cls.m2 = r.commit("c.txt", "c1\n", "main adds c")
        r.git("checkout", "-q", "pr")
        cls.h2 = r.merge_main("main")
        # The same merge with a hand edit folded in.
        r.git("checkout", "-q", "-b", "edited", cls.h0)
        r.merge_main(cls.m1)
        r.write("a.txt", "a2 from the PR, edited during the merge\n")
        r.git("commit", "-q", "-a", "--amend", "--no-edit")
        cls.edited = r.git("rev-parse", "HEAD")
        # main changes the PR's file: the merge conflicts and is resolved by hand.
        r.git("checkout", "-q", "main")
        cls.m_conflict = r.commit("a.txt", "a3 from main\n", "main touches a")
        r.git("checkout", "-q", "-b", "conflicted", cls.h0)
        r.merge_main(cls.m_conflict)
        r.write("a.txt", "resolved\n")
        r.git("add", "a.txt")
        r.git("commit", "-q", "--no-edit")
        cls.conflicted = r.git("rev-parse", "HEAD")
        # The PR rebased onto main: one parent, a new SHA.
        r.git("checkout", "-q", "-b", "rebased", cls.m1)
        r.git("cherry-pick", cls.h0)
        cls.rebased = r.git("rev-parse", "HEAD")
        # Parents in the wrong order: main merged with the PR, not the PR updated from main.
        r.git("checkout", "-q", "-b", "reversed", cls.m1)
        r.merge_main(cls.h0)
        cls.reversed = r.git("rev-parse", "HEAD")

    def plan(self, head, records, strict=False, failing=None):
        return pps.plan_carry(head, records, strict, RepoFacts(self.r, failing))

    def test_clean_update_carries(self):
        plan, why = self.plan(self.h1, [record(self.h0)])
        self.assertEqual((plan.head, plan.state), (self.h1, "success"))
        self.assertEqual(plan.description, f"carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol")
        self.assertEqual(why, f"carried from {self.h0[:7]}")

    def test_conflicted_merge_does_not_carry(self):
        plan, why = self.plan(self.conflicted, [record(self.h0)])
        self.assertIsNone(plan)
        self.assertIn("conflicts", why)

    def test_merge_with_an_extra_edit_does_not_carry(self):
        plan, why = self.plan(self.edited, [record(self.h0)])
        self.assertIsNone(plan)
        self.assertIn("differs from a clean merge", why)

    def test_rebased_head_does_not_carry(self):
        plan, why = self.plan(self.rebased, [record(self.h0)])
        self.assertIsNone(plan)
        self.assertIn("not a merge commit", why)

    def test_reversed_parents_do_not_carry(self):
        plan, why = self.plan(self.reversed, [record(self.h0)])
        self.assertIsNone(plan)

    def test_ci_not_green_on_the_new_head_does_not_carry(self):
        plan, why = self.plan(self.h1, [record(self.h0)], failing={self.h1: "rust (linux) is in_progress"})
        self.assertIsNone(plan)
        self.assertEqual(why, f"{self.h1[:7]}: rust (linux) is in_progress")

    def test_chain_of_two_clean_updates_carries(self):
        plan, _ = self.plan(self.h2, [record(self.h0)])
        self.assertEqual(plan.description, f"carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol")

    def test_chain_needs_every_hop_green(self):
        plan, why = self.plan(self.h2, [record(self.h0)], failing={self.h1: "zizmor is failure"})
        self.assertIsNone(plan)
        self.assertEqual(why, f"{self.h1[:7]}: zizmor is failure")

    def test_newer_records_stop_the_walk(self):
        # h1 was scored 7: its records win over h0's 9, so h2 does not carry.
        plan, why = self.plan(self.h2, [record(self.h0), record(self.h1, score=7)])
        self.assertIsNone(plan)
        self.assertIn(f"{self.h1[:7]} did not pass", why)

    def test_head_with_its_own_records_does_not_carry(self):
        plan, _ = self.plan(self.h1, [record(self.h0), record(self.h1, score=7)])
        self.assertIsNone(plan)

    def test_sensitive_class_needs_every_carried_seat_above_eight(self):
        records = [record(self.h0, score=9), record(self.h0, seat="composer-2.5", vendor="cursor", score=8)]
        plan, why = self.plan(self.h1, records, strict=True)
        self.assertIsNone(plan)
        self.assertEqual(why, f"sensitive: composer-2.5 scored 8 on {self.h0[:7]}")
        plan, _ = self.plan(self.h1, records, strict=False)
        self.assertEqual(plan.state, "success")
        plan, _ = self.plan(self.h1, [record(self.h0, score=9)], strict=True)
        self.assertEqual(plan.state, "success")

    def test_grok_and_author_family_records_never_carry(self):
        grok = record(self.h0, seat="grok-4.7", vendor="xai")
        plan, why = self.plan(self.h1, [grok])
        self.assertIsNone(plan)
        self.assertIn("no record that can carry", why)
        plan, _ = self.plan(self.h1, [grok, record(self.h0)])
        self.assertEqual(plan.description, f"carried from {self.h0[:7]}: 9/10 from gpt-6.2-sol")
        plan, why = self.plan(self.h1, [record(self.h0, seat="claude-opus-5-5", vendor="anthropic")])
        self.assertIsNone(plan)
        self.assertIn("author's own family", why)

    def test_failing_source_does_not_carry(self):
        plan, why = self.plan(self.h1, [record(self.h0, score=7)])
        self.assertIsNone(plan)

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
    """Two best merge bases: Git's own merge conflicts, so a hand-resolved merge must not carry."""

    def test_criss_cross_conflict_does_not_carry(self):
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
        h0 = r.git("rev-parse", "HEAD")
        r.git("checkout", "-q", "main")
        r.git("merge", "-q", "--no-edit", x1, check=False)
        r.write("f.txt", "y\n")
        r.git("commit", "-q", "-a", "--no-edit")
        m = r.git("rev-parse", "HEAD")
        self.assertEqual(len(r.git("merge-base", "--all", h0, m).split()), 2)
        # Merging from either single base is clean and gives "y"; the hand resolution matches it.
        r.git("checkout", "-q", "pr")
        r.git("merge", "-q", "--no-edit", m, check=False)
        r.write("f.txt", "y\n")
        r.git("commit", "-q", "-a", "--no-edit")
        h1 = r.git("rev-parse", "HEAD")
        self.assertEqual(r.git("rev-list", "--parents", "-n1", h1).split()[1:], [h0, m])
        # One explicit base (what GitHub's compare API returns) would certify the hand resolution.
        self.assertEqual(r.git("merge-tree", "--write-tree", f"--merge-base={x1}", h0, m),
                         r.git("rev-parse", f"{h1}^{{tree}}"))
        plan, why = pps.plan_carry(h1, [record(h0)], False, RepoFacts(r))
        self.assertIsNone(plan)
        self.assertIn("conflicts", why)


class CommentModeCarryTests(unittest.TestCase):
    """main() in comment mode, with GitHub reads answered from a local repo."""

    @classmethod
    def setUpClass(cls):
        CarryTests.setUpClass()
        cls.c = CarryTests

    def run_main(self, head, failing=None, argv=()):
        import io
        from contextlib import redirect_stdout
        from unittest import mock

        body = f"pp-score head={self.c.h0} seat=gpt-6.2-sol vendor=openai score=9 author=anthropic obstacles=none"
        facts = RepoFacts(self.c.r, failing)
        buf = io.StringIO()
        with mock.patch.object(pps, "fetch_comments", return_value=[comment(body)]), \
                mock.patch.object(pps, "fetch_head", return_value=head), \
                mock.patch.object(pps, "fetch_files", return_value=["docs/x.md"]), \
                mock.patch.object(pps, "GitHubFacts", return_value=facts), \
                redirect_stdout(buf):
            code = pps.main(["--pr", "7", *argv])
        return code, buf.getvalue()

    def test_carry_then_a_failed_hop_replaces_the_carried_success(self):
        h0, h1, h2 = self.c.h0, self.c.h1, self.c.h2
        _, out = self.run_main(h2)
        self.assertIn(f"statuses/{h2} -f state=success -f context=pp/score "
                      f"-f 'description=carried from {h0[:7]}: 9/10 from gpt-6.2-sol'", out)
        _, out = self.run_main(h2, failing={h1: "rust (linux) is failure"})
        self.assertIn(f"statuses/{h2} -f state=failure -f context=pp/score "
                      f"-f 'description=no score and no carry: {h1[:7]}: rust (linux) is failure'", out)
        self.assertNotIn(f"statuses/{h2} -f state=success", out)

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


class RequiredCheckTests(unittest.TestCase):
    RULES = [
        {"type": "deletion"},
        {"type": "required_status_checks", "parameters": {"required_status_checks": [
            {"context": "actionlint", "integration_id": 15368},
            {"context": "rust (linux)", "integration_id": 15368},
            {"context": "pp/score"},
        ]}},
    ]

    def facts(self, runs, statuses=()):
        from unittest import mock

        def fake(path, paginate=False):
            if "/rules/branches/" in path:
                return self.RULES
            if "/check-runs?" in path:
                name = urllib.parse.unquote(path.split("check_name=")[1].split("&")[0])
                return {"check_runs": [r for r in runs if r["name"] == name]}
            if path.endswith("/status"):
                return {"statuses": list(statuses)}
            raise AssertionError(path)

        patcher = mock.patch.object(pps, "gh_json", side_effect=fake)
        patcher.start()
        self.addCleanup(patcher.stop)
        return pps.GitHubFacts("o/r")

    @staticmethod
    def run_(name, conclusion, status="completed"):
        return {"name": name, "status": status, "conclusion": conclusion}

    def test_reads_required_checks_from_main_rules_without_pp_score(self):
        facts = self.facts([])
        self.assertEqual([c["context"] for c in facts.required_checks()], ["actionlint", "rust (linux)"])

    def test_only_success_passes(self):
        facts = self.facts([self.run_("actionlint", "success"), self.run_("rust (linux)", "success")])
        self.assertIsNone(facts.checks_failing(SHA))
        # Chad's rule says success: a path-filtered skip or a neutral result does not qualify a hop.
        for conclusion in ("skipped", "neutral"):
            facts = self.facts([self.run_("actionlint", conclusion), self.run_("rust (linux)", "success")])
            self.assertEqual(facts.checks_failing(SHA), f"actionlint is {conclusion}")

    def test_running_failed_or_missing_checks_block(self):
        facts = self.facts([self.run_("actionlint", "success"), self.run_("rust (linux)", None, "in_progress")])
        self.assertEqual(facts.checks_failing(SHA), "rust (linux) is in_progress")
        facts = self.facts([self.run_("actionlint", "failure"), self.run_("rust (linux)", "success")])
        self.assertEqual(facts.checks_failing(SHA), "actionlint is failure")
        facts = self.facts([self.run_("actionlint", "success")])
        self.assertEqual(facts.checks_failing(SHA), "rust (linux) has not run")

    def test_no_required_checks_fails_closed(self):
        self.RULES = [{"type": "deletion"}]
        facts = self.facts([])
        with self.assertRaises(pps.GateError):
            facts.checks_failing(SHA)
