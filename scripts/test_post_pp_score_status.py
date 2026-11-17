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
                f"{SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic\n"
                "pp-score head="
                f"{SHA} seat=glm-5.3-flash-free vendor=zhipu score=9 author=anthropic\n"
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
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=7 author=anthropic")
        ]
        plans = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual(plans[0].state, "failure")
        self.assertEqual(plans[0].description, "7/10 from grok-4.7")

    def test_legacy_score_comment_is_accepted(self):
        comments = [comment(f"Score: 8/10 from claude-opus-5-5 on head {SHA}")]
        plans = pps.plan_statuses(pps.trusted_records(comments, self.trust))
        self.assertEqual(plans[0].state, "success")
        self.assertEqual(plans[0].description, "8/10 from claude-opus-5-5")
        self.assertEqual(plans[0].records[0].source, "legacy")

    def test_untrusted_author_and_edited_comments_are_ignored(self):
        comments = [
            comment(
                f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic",
                user="stranger",
            ),
            comment(
                f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic",
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
                f"pp-score head={SHA} seat=unknown vendor=xai score=9 author=anthropic\n"
                f"> SCORE: 9\n"
                f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic\n"
            )
        ]
        records = pps.trusted_records(comments, self.trust)
        self.assertEqual([r.seat for r in records], ["grok-4.7"])
        self.assertEqual(records[0].head, SHA)

    def test_records_stay_on_their_own_sha(self):
        comments = [
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic"),
            comment(f"pp-score head={OTHER} seat=grok-4.7 vendor=xai score=6 author=anthropic"),
        ]
        plans = {p.head: p for p in pps.plan_statuses(pps.trusted_records(comments, self.trust))}
        self.assertEqual(plans[SHA].state, "success")
        self.assertEqual(plans[OTHER].state, "failure")

    def test_minimum_score_sets_state(self):
        comments = [
            comment(f"pp-score head={SHA} seat=grok-4.7 vendor=xai score=9 author=anthropic"),
            comment(
                f"pp-score head={SHA} seat=glm-5.3-flash-free vendor=zhipu score=5 author=anthropic"
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
            (pps.Record(SHA, "grok-4.7", 9.0, "pp-score"),),
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


if __name__ == "__main__":
    unittest.main()
