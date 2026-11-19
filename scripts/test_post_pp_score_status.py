#!/usr/bin/env python3
import unittest

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
        self.assertEqual(data["pp_score_seats"], {"gpt-6.1-sol": "openai", "gpt-6.2-sol": "openai"})
        # Owner decision 2026-11-18: gpt-6.2-sol joins the council scorer pool.
        self.assertEqual(data["scorer_seats"]["gpt-6.2-sol"], "openai")
        self.assertEqual(pps.load_trust()["seats"]["gpt-6.2-sol"], "openai")

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
