import copy
import datetime as dt
import http.client
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import textwrap
import traceback
import unittest
import urllib.error
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("sync", Path(__file__).with_name("pr_status_sync.py"))
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)
real_gql = sync.gql


def ticket(number=1, state="In Progress", age=72):
    return {"id": str(number), "identifier": f"GRE-{number}",
            "state": {"name": state, "type": "completed" if state == "Done" else "started"},
            "updatedAt": (dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=age)).isoformat()}


def pr(number=1, state="OPEN", body="Refs GRE-1", age=0):
    timestamp = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=age)).isoformat()
    return {"number": number, "state": state, "body": body, "title": "A slice", "isCrossRepository": False,
            "headRefName": "slice", "updatedAt": timestamp,
            "mergedAt": timestamp if state == "MERGED" else None,
            "closedAt": timestamp if state != "OPEN" else None}


def github(world):
    """Fake gh api graphql over world(), in the page shapes fetch_prs reads."""
    def call(*args):
        query = next(arg for arg in args if arg.startswith("query="))
        nodes = [p for p in copy.deepcopy(world()) if "states:OPEN" not in query or p["state"] == "OPEN"]
        page = {"data": {"repository": {"pullRequests": {
            "nodes": nodes, "pageInfo": {"hasNextPage": False, "endCursor": None}}}}}
        return [page] if "--paginate" in args else page
    return call


class HttpTest(unittest.TestCase):
    def setUp(self):
        patch.dict(sync.os.environ, {"LINEAR_API_KEY": "secret-test-key"}).start()
        self.request = patch.object(sync.urllib.request, "urlopen").start()
        self.sleep = patch("time.sleep").start()
        self.logs = io.StringIO()
        patch.object(sync.sys, "stderr", self.logs).start()
        self.addCleanup(patch.stopall)

    def response(self, value):
        return io.BytesIO(json.dumps(value).encode())

    def http_error(self, status, body):
        return urllib.error.HTTPError("https://example.invalid", status, "secret-test-key", {},
                                      self.response(body) if isinstance(body, dict) else io.BytesIO(body))

    def errors(self, code):
        return {"errors": [{"message": "failure secret-test-key", "extensions": {"code": code}}]}

    def assert_safe_failure(self, query="query{viewer{id}}", message=None):
        with self.assertRaises(RuntimeError) as caught:
            real_gql(query)
        if message:
            self.assertEqual(str(caught.exception), message)
        rendered = "".join(traceback.format_exception(caught.exception)) + self.logs.getvalue()
        self.assertNotIn("secret-test-key", rendered)

    def test_http_400_ratelimited_then_success(self):
        self.request.side_effect = [self.http_error(400, self.errors("RATELIMITED")),
                                    self.response({"data": {"ok": True}})]
        self.assertEqual(real_gql("query{viewer{id}}"), {"ok": True})
        self.assertEqual(self.request.call_count, 2)
        self.assertEqual(self.sleep.call_count, 1)
        self.assertIn("400", self.logs.getvalue())
        self.assertIn("failure [REDACTED]", self.logs.getvalue())
        self.assertIn("RATELIMITED", self.logs.getvalue())
        self.assertNotIn("secret-test-key", self.logs.getvalue())

    def test_http_genuine_graphql_error_does_not_retry(self):
        self.request.side_effect = self.http_error(400, self.errors("GRAPHQL_VALIDATION_FAILED"))
        self.assert_safe_failure(message="Linear API returned GraphQL errors")
        self.assertEqual(self.request.call_count, 1)
        self.assertFalse(self.sleep.called)
        self.assertIn("GRAPHQL_VALIDATION_FAILED", self.logs.getvalue())
        self.assertIn("failure [REDACTED]", self.logs.getvalue())

    def test_transient_http_exhaustion_is_bounded_and_safe(self):
        for status in (429, 500, 502, 503):
            with self.subTest(status=status):
                self.request.reset_mock()
                self.sleep.reset_mock()
                self.request.side_effect = [self.http_error(status, b"secret-test-key unavailable")
                                            for _ in range(3)]
                self.assert_safe_failure()
                self.assertEqual(self.request.call_count, 3)
                self.assertEqual(self.sleep.call_count, 2)

    def test_timeout_then_success_and_exhaustion(self):
        self.request.side_effect = [TimeoutError("secret-test-key"), self.response({"data": {"ok": True}})]
        self.assertEqual(real_gql("query{viewer{id}}"), {"ok": True})
        self.request.reset_mock()
        self.request.side_effect = urllib.error.URLError(TimeoutError("secret-test-key"))
        self.assert_safe_failure()
        self.assertEqual(self.request.call_count, 3)

    def test_response_graphql_ratelimited_then_success(self):
        self.request.side_effect = [self.response(self.errors("RATELIMITED")),
                                    self.response({"data": {"ok": True}})]
        self.assertEqual(real_gql("query{viewer{id}}"), {"ok": True})
        self.assertEqual(self.request.call_count, 2)

    def test_http_protocol_failures_retry_without_exposing_key(self):
        for error in (http.client.IncompleteRead(b"secret-test-key"),
                      http.client.BadStatusLine("secret-test-key")):
            with self.subTest(error=type(error).__name__):
                self.request.reset_mock()
                self.request.side_effect = [error, self.response({"data": {"ok": True}})]
                self.assertEqual(real_gql("query{viewer{id}}"), {"ok": True})
                self.assertEqual(self.request.call_count, 2)
                self.request.reset_mock()
                self.request.side_effect = error
                self.assert_safe_failure()
                self.assertEqual(self.request.call_count, 3)

    def test_incomplete_response_body_retries(self):
        response = self.response({})
        with patch.object(response, "read", side_effect=http.client.IncompleteRead(b"secret-test-key")):
            self.request.side_effect = [response, self.response({"data": {"ok": True}})]
            self.assertEqual(real_gql("query{viewer{id}}"), {"ok": True})
        self.assertEqual(self.request.call_count, 2)
        self.assertNotIn("secret-test-key", self.logs.getvalue())

    def test_server_graphql_errors_retry_safe_operations(self):
        for code in ("INTERNAL_SERVER_ERROR", "UNKNOWN", None):
            for query in ("query{viewer{id}}", "mutation{issueUpdate(id:1,input:{}){success}}"):
                with self.subTest(code=code, query=query):
                    self.request.reset_mock()
                    self.request.side_effect = [self.http_error(503, self.errors(code)),
                                                self.response({"data": {"ok": True}})]
                    self.assertEqual(real_gql(query), {"ok": True})
                    self.assertEqual(self.request.call_count, 2)

    def test_invalid_graphql_requests_remain_fatal_on_server_error(self):
        for code in ("BAD_USER_INPUT", "GRAPHQL_VALIDATION_FAILED", "GRAPHQL_PARSE_FAILED"):
            with self.subTest(code=code):
                self.request.reset_mock()
                self.request.side_effect = self.http_error(500, self.errors(code))
                self.assert_safe_failure(message="Linear API returned GraphQL errors")
                self.assertEqual(self.request.call_count, 1)

    def test_reconcile_deadline_bounds_requests_and_preserves_summary(self):
        now = [0.0]
        timeouts = []
        def timeout(request, *, timeout):
            timeouts.append(timeout)
            now[0] += timeout
            raise TimeoutError("secret-test-key")
        def sleep(seconds):
            now[0] += seconds
        self.request.side_effect = timeout
        self.sleep.side_effect = sleep
        prs = [pr(number=n, body=f"Refs GRE-{n}") for n in range(1, 7)]
        with patch.object(sync, "all_prs", return_value=prs), \
                patch.object(sync.time, "monotonic", side_effect=lambda: now[0]):
            with self.assertRaises(RuntimeError) as caught:
                sync.reconcile(3)
        self.assertLessEqual(now[0], 240)
        self.assertEqual(timeouts[-1], 23)
        self.assertEqual(self.request.call_count, 8)
        for number in range(1, 7):
            self.assertIn(f"GRE-{number}", str(caught.exception))
        self.assertIn("stale issue listing", str(caught.exception))
        self.assertNotIn("secret-test-key", "".join(traceback.format_exception(caught.exception)))
        self.request.side_effect = None
        self.request.return_value = self.response({"data": {"ok": True}})
        self.assertEqual(real_gql("query{viewer{id}}"), {"ok": True})
        self.assertEqual(self.request.call_args.kwargs["timeout"], 30)

    def test_deadline_refuses_requests_and_waits_without_enough_time(self):
        for remaining, calls in ((0, 0), (0.5, 1)):
            with self.subTest(remaining=remaining), \
                    patch.object(sync, "_request_deadline", remaining), \
                    patch.object(sync.time, "monotonic", return_value=0):
                self.request.reset_mock()
                self.sleep.reset_mock()
                self.request.side_effect = TimeoutError("secret-test-key")
                self.assert_safe_failure()
                self.assertEqual(self.request.call_count, calls)
                self.assertFalse(self.sleep.called)
                if calls:
                    self.assertEqual(self.request.call_args.kwargs["timeout"], remaining)

    def test_genuine_or_mixed_graphql_errors_do_not_retry(self):
        for status in (200, 500):
            with self.subTest(status=status):
                body = self.errors("RATELIMITED")
                body["errors"] += self.errors("BAD_USER_INPUT")["errors"]
                self.request.reset_mock()
                self.request.side_effect = None
                if status == 200:
                    self.request.return_value = self.response(body)
                else:
                    self.request.side_effect = self.http_error(status, body)
                self.assert_safe_failure(message="Linear API returned GraphQL errors")
                self.assertEqual(self.request.call_count, 1)

    def test_comment_create_is_not_replayed_after_ambiguous_failure(self):
        for failure in (TimeoutError("secret-test-key"), self.http_error(503, b"unavailable"),
                        http.client.IncompleteRead(b"secret-test-key"),
                        http.client.BadStatusLine("secret-test-key"),
                        self.http_error(503, self.errors("INTERNAL_SERVER_ERROR"))):
            with self.subTest(failure=type(failure).__name__):
                self.request.reset_mock()
                self.request.side_effect = failure
                self.assert_safe_failure("mutation{commentCreate(input:{}){success}}")
                self.assertEqual(self.request.call_count, 1)

    def test_comment_create_can_retry_explicit_rate_limit_rejection(self):
        self.request.side_effect = [self.http_error(400, self.errors("RATELIMITED")),
                                    self.response({"data": {"commentCreate": {"success": True}}})]
        self.assertEqual(real_gql("mutation{commentCreate(input:{}){success}}"),
                         {"commentCreate": {"success": True}})
        self.assertEqual(self.request.call_count, 2)

    def test_partial_graphql_data_does_not_replay_comment_create(self):
        body = {**self.errors("RATELIMITED"), "data": {"commentCreate": {"success": True}}}
        self.request.return_value = self.response(body)
        self.assert_safe_failure("mutation{commentCreate(input:{}){success}}",
                                 "Linear API returned GraphQL errors")
        self.assertEqual(self.request.call_count, 1)

    def test_issue_update_can_retry_ambiguous_timeout(self):
        self.request.side_effect = [TimeoutError("secret-test-key"),
                                    self.response({"data": {"issueUpdate": {"success": True}}})]
        self.assertEqual(real_gql("mutation{issueUpdate(id:1,input:{}){success}}"),
                         {"issueUpdate": {"success": True}})
        self.assertEqual(self.request.call_count, 2)


class SyncTest(unittest.TestCase):
    def setUp(self):
        sync.DRY = False
        sync._states = None
        sync._prs = []
        self.issues = {"GRE-1": ticket()}
        self.comments = []
        self.fail_update = False
        self.drop_done = 0
        self.blocks = []
        self.labels = {}
        self.pages = None
        self.api = patch.object(sync, "gql", side_effect=self.linear).start()
        self.gh = patch.object(sync, "gh", side_effect=github(lambda: sync._prs or [])).start()
        patch("builtins.print").start()
        self.addCleanup(patch.stopall)

    def linear(self, query, variables):
        if "issues(first:1," in query:
            iss = self.issues.get(f"{variables['k']}-{variables['n']}")
            return {"issues": {"nodes": [copy.deepcopy(iss)] if iss else []}}
        if "teams(" in query:
            return {"teams": {"nodes": [{"states": {"nodes": [
                {"id": name, "name": name} for name in ("In Review", "In Progress", "Backlog", "Todo", "Done")
            ]}}]}}
        if "commentCreate(" in query:
            self.comments.append({"body": variables["b"], "issueId": variables["id"]})
            return {"commentCreate": {"success": True}}
        if "issueUpdate(" in query:
            if variables["s"] == "Done" and self.drop_done:  # accepted but never applied
                self.drop_done -= 1
            elif not self.fail_update:
                self.issues[f"GRE-{variables['id']}"].update(updatedAt=ticket(age=0)["updatedAt"], state={
                    "name": variables["s"], "type": "completed" if variables["s"] == "Done" else "started"})
            return {"issueUpdate": {"success": not self.fail_update}}
        if "inverseRelations(" in query:
            end = lambda i: {**copy.deepcopy(self.issues[i]), "team": {"key": "GRE"},
                             "labels": {"nodes": [{"name": name} for name in self.labels.get(i, [])]}}
            rels = [{"type": "blocks", "issue": end(a), "relatedIssue": end(b)} for a, b in self.blocks]
            me = variables["id"]
            return {"issue": me in self.issues and {
                **end(me), "relations": {"nodes": [r for r in rels if r["issue"]["identifier"] == me]},
                "inverseRelations": {"nodes": [r for r in rels if r["relatedIssue"]["identifier"] == me]}} or None}
        if "issues(first:" in query:
            offset = int(variables["after"] or 0)
            pages = self.pages or [list(copy.deepcopy(self.issues).values())]
            return {"issues": {"nodes": pages[offset], "pageInfo": {
                "hasNextPage": offset + 1 < len(pages), "endCursor": str(offset + 1)}}}
        if "comments(first:" in query:
            return {"issue": {"comments": {"nodes": [c for c in self.comments if c["issueId"] == variables["id"]],
                    "pageInfo": {"hasNextPage": False, "endCursor": None}}}}
        return {"issue": copy.deepcopy(self.issues.get(variables["id"]))}

    def test_reference_parsing(self):
        self.assertEqual(sync.refs({"title": "GRE-4 work", "headRefName": "gre-5-slice",
                                   "body": "Closes GRE-1, GRE-2 and GRE-3. Refs GRE-6"}),
                         ({"GRE-1", "GRE-2", "GRE-3"}, {"GRE-1", "GRE-2", "GRE-3", "GRE-4", "GRE-5", "GRE-6"}))

    def test_negated_closing_text_from_real_slice_prs_is_not_a_directive(self):
        bodies = [
            "This does not close GRE-401. Production Node still owns application SQL.",  # #298
            "Does not close GRE-594, GRE-408, or GRE-354.",  # #305
            "- GRE-448 stays open. This does not complete the child issue and does not close GRE-334.",  # #308
            "Linear: GRE-387 stage 1 (this PR does not close GRE-387).",  # #210
            "fix\n\nGRE-528",  # #216 must not match across lines
        ]
        for body in bodies:
            with self.subTest(body=body):
                self.assertEqual(sync.refs(pr(body=body))[0], set())
        for directive in ("Fixes", "Closes", "Resolves", "- Fixes", "## Closes"):
            with self.subTest(directive=directive):
                self.assertEqual(sync.refs(pr(body=f"{directive} GRE-1, GRE-2 and GRE-3."))[0],
                                 {"GRE-1", "GRE-2", "GRE-3"})

    def test_negated_close_merge_does_not_mark_ticket_done(self):
        self.issues = {"GRE-401": ticket(401)}
        sync._prs = [pr(number=413, body="Refs GRE-401")]
        sync.apply_pr(pr(number=298, state="MERGED", body="Refs GRE-401. This does not close GRE-401."))
        self.assertEqual(self.issues["GRE-401"]["state"]["name"], "In Review")

    def test_open_then_merge_and_replay(self):
        sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        sync.apply_pr(pr(state="MERGED", body="Fixes GRE-1"))
        sync.apply_pr(pr(state="MERGED", body="Fixes GRE-1"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "Done")
        self.assertEqual(self.comments, [])

    def test_refs_slice_with_and_without_other_open_pr(self):
        self.issues["GRE-1"] = ticket(state="In Review")
        sync.apply_pr(pr(state="MERGED"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")
        sync._prs = [pr(number=2)]
        sync.apply_pr(pr(state="MERGED"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")

    def test_merged_slice_stays_in_review_without_comment(self):
        self.issues["GRE-1"] = ticket(state="In Review")
        sync._prs = [pr(number=2)]
        sync.apply_pr(pr(state="MERGED"))
        sync.apply_pr(pr(state="MERGED"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        self.assertEqual(self.comments, [])
        self.assertEqual([query for query, _ in self.api.call_args_list if "issueUpdate(" in query], [])

    def test_slice_state_follows_other_open_prs_without_comments(self):
        self.issues["GRE-1"] = ticket(state="In Review")
        for others in ([pr(number=2), pr(number=3)], [pr(number=3)], []):
            sync._prs = others
            sync.apply_pr(pr(state="MERGED"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")
        sync.apply_pr(pr(number=4, state="MERGED"))
        sync._prs = [pr(number=5)]
        sync.apply_pr(pr(number=4, state="MERGED"))
        self.assertEqual(self.comments, [])
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")

    def test_refs_slice_does_not_revive_todo_or_backlog(self):
        for state in ("Todo", "Backlog"):
            with self.subTest(state=state):
                self.issues["GRE-1"] = ticket(state=state)
                sync.apply_pr(pr(state="MERGED"))
                self.assertEqual(self.issues["GRE-1"]["state"]["name"], state)
                self.assertEqual(self.comments, [])

    def test_unmerged_close_and_terminal_protection(self):
        self.issues["GRE-1"] = ticket(state="In Review")
        sync.apply_pr(pr(state="CLOSED"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")
        self.issues["GRE-1"] = ticket(state="Done")
        sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "Done")

    def test_lookup_failure_is_not_missing_issue(self):
        self.api.side_effect = TimeoutError("temporary")
        with self.assertRaises(TimeoutError):
            sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")

    def test_missing_issue_is_skipped(self):
        sync.apply_pr(pr(body="Fixes GRE-99"))
        self.assertEqual(self.comments, [])
        sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")

    def test_missing_filtered_issue_http_response(self):
        self.api.side_effect = real_gql
        response = io.BytesIO(json.dumps({"data": {"issues": {"nodes": []}}}).encode())
        with patch.dict(sync.os.environ, {"LINEAR_API_KEY": "test-key"}), \
                patch.object(sync.urllib.request, "urlopen", return_value=response) as request:
            self.assertIsNone(sync.issue("GRE-999999"))
        sent = json.loads(request.call_args.args[0].data)
        self.assertEqual(sent["variables"], {"k": "GRE", "n": 999999})
        self.assertIn("number:{eq:$n}", sent["query"])

    def test_missing_issue_does_not_block_stale_sweep(self):
        sync._prs = [pr(body="Refs GRE-999999")]
        sync.reconcile(3)
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "Backlog")

    def test_reconcile_restores_deadline_on_success_and_unexpected_failure(self):
        with patch.object(sync, "_request_deadline", None):
            sync.reconcile(3)
            self.assertIsNone(sync._request_deadline)
            with patch.object(sync, "all_prs", side_effect=RuntimeError("unavailable")):
                with self.assertRaisesRegex(RuntimeError, "unavailable"):
                    sync.reconcile(3)
            self.assertIsNone(sync._request_deadline)

    def test_reconcile_continues_same_pr_later_pr_and_stale_issues(self):
        self.issues = {f"GRE-{n}": ticket(n) for n in range(1, 7)}
        sync._prs = [pr(body="Refs GRE-1, GRE-2"), pr(number=2, body="Refs GRE-3")]
        def failing(query, variables):
            if "issues(first:1," in query and variables["n"] == 1:
                raise RuntimeError("secret-test-key lookup failed")
            if "issueUpdate(" in query and variables["id"] == "4":
                raise RuntimeError("secret-test-key update failed")
            return self.linear(query, variables)
        self.api.side_effect = failing
        with self.assertRaises(RuntimeError) as caught:
            sync.reconcile(3)
        self.assertNotIn("secret-test-key", str(caught.exception))
        self.assertIn("GRE-1", str(caught.exception))
        self.assertIn("GRE-4", str(caught.exception))
        self.assertEqual([self.issues[f"GRE-{n}"]["state"]["name"] for n in range(1, 7)],
                         ["In Progress", "In Review", "In Review", "In Progress", "Backlog", "Backlog"])

    def test_event_still_stops_on_first_issue_failure(self):
        self.issues["GRE-2"] = ticket(2)
        self.api.side_effect = TimeoutError("temporary")
        with self.assertRaises(TimeoutError):
            sync.apply_event(pr(body="Refs GRE-1, GRE-2"), {})
        self.assertEqual(self.api.call_count, 1)
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "In Progress")

    def test_lookup_graphql_failure_remains_visible_without_echoing_key(self):
        self.api.side_effect = real_gql
        response = io.BytesIO(json.dumps({"errors": [{"message": "test-key rate limited"}]}).encode())
        with patch.dict(sync.os.environ, {"LINEAR_API_KEY": "test-key"}), \
                patch.object(sync.urllib.request, "urlopen", return_value=response):
            with self.assertRaisesRegex(RuntimeError, "^Linear API returned GraphQL errors$"):
                sync.issue("GRE-1")

    def test_fork_event_cannot_change_linear(self):
        fork = pr()
        fork["isCrossRepository"] = True
        self.issues["GRE-1"] = ticket(state="In Review")
        sync.apply_pr(fork)
        fork["body"] = ""
        sync.apply_event(fork, {"action": "edited", "changes": {"body": {"from": "Refs GRE-1"}}})
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        self.assertEqual(self.comments, [])
        self.assertEqual(self.api.call_count, 0)

    def test_fork_pr_is_excluded_from_reconcile_and_open_pr_protection(self):
        sync._prs = None
        fork = pr(body="Refs GRE-1")
        fork["isCrossRepository"] = True
        trusted = pr(number=2, body="Refs GRE-2")
        self.issues["GRE-2"] = ticket(2)
        self.gh.side_effect = github(lambda: [fork, trusted])
        sync.reconcile(3)
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "Backlog")
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "In Review")

    def test_status_moves_post_no_linear_comments(self):
        sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        sync.apply_pr(pr(state="MERGED", body="Fixes GRE-1"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "Done")
        self.assertEqual(self.comments, [])

    def test_failed_update_retries_on_the_next_run(self):
        self.fail_update = True
        with self.assertRaisesRegex(RuntimeError, "issueUpdate failed"):
            sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")
        self.fail_update = False
        sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        self.assertEqual(self.comments, [])

    def test_open_pr_on_later_github_page_prevents_demotion(self):
        sync._prs = None
        sync.REPO = "Print-Partner/PrintPartner"
        pages = [{"data": {"repository": {"pullRequests": {"nodes": nodes, "pageInfo": {"hasNextPage": more}}}}}
                 for nodes, more in (([pr(number=n, body="Refs GRE-99") for n in range(1, 201)], True),
                                     ([pr(number=201)], False))]
        self.gh.side_effect = lambda *args: pages if "--paginate" in args else pages[0]
        sync.apply_pr(pr(state="MERGED"))
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        self.assertIn("--paginate", self.gh.call_args_list[0].args)
        self.assertIn("--slurp", self.gh.call_args_list[0].args)

    def test_stall_sweep_reads_every_linear_page(self):
        self.issues = {f"GRE-{n}": ticket(n) for n in range(1, 252)}
        rows = list(copy.deepcopy(self.issues).values())
        self.pages = [rows[:250], rows[250:]]
        sync.reconcile(3)
        self.assertEqual(self.issues["GRE-251"]["state"]["name"], "Backlog")
        self.assertEqual(self.comments, [])

    def test_stall_sweep_preserves_recent_activity_and_open_prs(self):
        self.issues = {f"GRE-{n}": ticket(n) for n in range(1, 5)}
        self.issues["GRE-2"] = ticket(2, age=47)
        recent_commit = pr(number=3, state="CLOSED", body="Refs GRE-3", age=72)
        recent_commit["commits"] = {"nodes": [{"commit": {"committedDate": pr()["updatedAt"]}}]}
        sync._prs = [recent_commit, pr(number=4, body="Refs GRE-4", age=72)]
        sync.reconcile(3)
        self.assertEqual([self.issues[f"GRE-{n}"]["state"]["name"] for n in range(1, 5)],
                         ["Backlog", "In Progress", "In Progress", "In Review"])

    def test_reconcile_includes_open_prs_older_than_replay_window(self):
        self.issues["GRE-1"] = ticket(state="Todo")
        sync._prs = [pr(age=96)]
        sync.reconcile(3)
        sync.reconcile(3)
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        self.assertEqual(self.comments, [])

    def test_edit_returns_removed_ticket_to_in_progress(self):
        for field in ("title", "body"):
            with self.subTest(field=field):
                self.issues["GRE-1"] = ticket(state="In Review")
                current = pr(body="Refs GRE-2")
                self.issues["GRE-2"] = ticket(2)
                sync.apply_event(current, {"action": "edited", "changes": {field: {"from": "Refs GRE-1"}}})
                self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")
                self.assertEqual(self.issues["GRE-2"]["state"]["name"], "In Review")

    def test_edit_preserves_ticket_with_another_open_pr_or_branch_reference(self):
        self.issues["GRE-1"] = ticket(state="In Review")
        sync._prs = [pr(number=2)]
        event = {"action": "edited", "changes": {"body": {"from": "Refs GRE-1"}}}
        sync.apply_event(pr(body=""), event)
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        sync._prs = []
        current = pr(body="")
        current["headRefName"] = "gre-1-work"
        sync.apply_event(current, event)
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Review")
        self.assertEqual(self.comments, [])

    def test_stall_sweep_uses_latest_merge_close_or_update(self):
        for field in ("mergedAt", "closedAt", "updatedAt"):
            with self.subTest(field=field):
                self.issues["GRE-1"] = ticket()
                closed = pr(state="CLOSED", age=96)
                closed[field] = pr(age=24)["updatedAt"]
                sync._prs = [closed]
                sync.reconcile(3)
                self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")
                self.assertEqual(self.comments, [])

    def blocked_world(self, blocker="Done", **labels):
        """GRE-1 (In Review) and GRE-3 (blocker) both block GRE-2 (Backlog)."""
        self.issues = {"GRE-1": ticket(1, "In Review"), "GRE-2": ticket(2, "Backlog"), "GRE-3": ticket(3, blocker)}
        self.blocks = [("GRE-1", "GRE-2"), ("GRE-3", "GRE-2")]
        self.labels = {f"GRE-{n}": [v] for n, v in labels.items()}

    def calls(self, marker):
        return [c.args[1] for c in self.api.call_args_list if marker in c.args[0]]

    def merge_fixes(self):
        sync.apply_pr(pr(state="MERGED", body="Fixes GRE-1"))

    def test_merge_promotes_backlog_once_all_blockers_done(self):
        self.blocked_world()
        self.merge_fixes()
        self.merge_fixes()  # replay: already Todo, no second comment
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "Done")
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Todo")
        self.assertEqual(self.comments, [{"issueId": "2", "body": "fleet-promote: blockers done GRE-1, GRE-3"}])

    def test_open_blocker_keeps_backlog(self):
        self.blocked_world(blocker="In Progress")
        self.merge_fixes()
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Backlog")
        self.assertEqual(self.comments, [])

    def test_umbrella_blocker_is_ignored(self):
        self.blocked_world(blocker="In Progress", **{"3": "umbrella"})
        self.merge_fixes()
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Todo")
        self.assertEqual(self.comments, [{"issueId": "2", "body": "fleet-promote: blockers done GRE-1"}])

    def test_excluded_label_is_not_promoted(self):
        for label in ("needs-chad-action", "umbrella", "post-parity", "pp:later"):
            with self.subTest(label=label):
                self.blocked_world(**{"2": label})
                self.merge_fixes()
                self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Backlog")
                self.assertEqual(self.comments, [])

    def test_promotion_leaves_non_backlog_alone(self):
        for state in ("Todo", "In Progress", "Canceled"):
            with self.subTest(state=state):
                self.blocked_world()
                self.issues["GRE-2"] = ticket(2, state)
                self.merge_fixes()
                self.assertEqual(self.issues["GRE-2"]["state"]["name"], state)
                self.assertEqual(self.comments, [])

    def test_refs_merge_does_not_promote(self):
        self.blocked_world()
        sync.apply_pr(pr(state="MERGED", body="Refs GRE-1"))
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Backlog")
        self.assertTrue(self.calls("issueUpdate("))
        self.assertEqual(self.calls("inverseRelations("), [])

    def test_owner_protected_tickets_are_never_written(self):
        self.assertEqual(len(sync.PROTECTED), 28)
        for number in (451, 621, 1007, 1008, 1013, 1019, 1028, 1030, 1056, 1060, 1066):
            ident = f"GRE-{number}"
            with self.subTest(ident=ident):
                self.issues = {ident: ticket(number, "In Progress")}
                self.api.reset_mock()
                print.reset_mock()
                sync._prs = []
                sync.apply_pr(pr(body=f"Fixes {ident}"))
                sync.apply_pr(pr(state="MERGED", body=f"Fixes {ident}"))
                sync.reconcile(3)
                self.assertEqual(self.issues[ident]["state"]["name"], "In Progress")
                self.assertEqual((self.calls("issueUpdate("), self.comments), ([], []))
                self.assertIn(((f"{ident}: skipped, owner-protected ticket; the sync does not write it",),
                               {"file": sync.sys.stderr}), [(c.args, c.kwargs) for c in print.call_args_list])
        for number in (1014, 1018, 1029, 1031):
            with self.subTest(unprotected=number):
                self.issues = {f"GRE-{number}": ticket(number, "In Progress")}
                sync.apply_pr(pr(body=f"Fixes GRE-{number}"))
                self.assertEqual(self.issues[f"GRE-{number}"]["state"]["name"], "In Review")

    def test_zero_padded_reference_to_protected_ticket_is_skipped(self):
        self.issues = {"GRE-1008": ticket(1008, "In Progress")}
        sync.apply_pr(pr(body="Refs GRE-01008"))
        sync.apply_pr(pr(state="MERGED", body="Fixes GRE-001008"))
        self.assertEqual(self.issues["GRE-1008"]["state"]["name"], "In Progress")
        self.assertEqual(self.calls("issueUpdate("), [])

    def test_done_protected_ticket_still_promotes_unprotected_dependents(self):
        self.issues = {"GRE-1010": ticket(1010, "Done"), "GRE-2": ticket(2, "Backlog")}
        self.blocks = [("GRE-1010", "GRE-2")]
        sync.apply_pr(pr(state="MERGED", body="Fixes GRE-1010"))
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Todo")
        self.assertEqual([v["id"] for v in self.calls("issueUpdate(")], ["2"])
        self.assertEqual(self.comments, [{"issueId": "2", "body": "fleet-promote: blockers done GRE-1010"}])
        self.issues["GRE-1010"] = ticket(1010, "In Review")
        self.issues["GRE-2"] = ticket(2, "Backlog")
        self.comments = []
        sync.apply_pr(pr(state="MERGED", body="Fixes GRE-1010"))
        self.assertEqual((self.issues["GRE-2"]["state"]["name"], self.comments), ("Backlog", []))

    def test_owner_protected_ticket_is_not_promoted(self):
        self.blocked_world()
        self.issues["GRE-1010"] = ticket(1010, "Backlog")
        self.blocks.append(("GRE-1", "GRE-1010"))
        self.merge_fixes()
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Todo")
        self.assertEqual(self.issues["GRE-1010"]["state"]["name"], "Backlog")
        self.assertEqual([c["issueId"] for c in self.comments], ["2"])

    def test_done_write_is_verified_and_retried_once(self):
        self.blocked_world()
        self.drop_done = 1
        self.merge_fixes()
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "Done")
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Todo")
        self.assertEqual([v["s"] for v in self.calls("issueUpdate(")], ["Done", "Done", "Todo"])

    def test_done_write_that_never_lands_fails_the_run(self):
        self.blocked_world()
        self.drop_done = 2
        with self.assertRaises(RuntimeError) as caught:
            self.merge_fixes()
        self.assertIn("GRE-1: not Done", str(caught.exception))
        self.assertEqual([v["s"] for v in self.calls("issueUpdate(")], ["Done", "Done"])
        self.assertIn(("::error::GRE-1 is not Done after a Fixes merge and one retried write",),
                      [c.args for c in print.call_args_list])
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Backlog")
        self.assertEqual(self.comments, [])

    def test_reconcile_collects_a_done_write_that_never_lands(self):
        self.blocked_world()
        self.drop_done = 2
        sync._prs = [pr(state="MERGED", body="Fixes GRE-1")]
        with self.assertRaises(RuntimeError) as caught:
            sync.reconcile(3)
        self.assertIn("PR #1 GRE-1", str(caught.exception))
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Backlog")

    def test_dry_run_reports_promotion_without_writing(self):
        self.blocked_world()
        sync.DRY = True
        self.merge_fixes()
        self.assertEqual(self.issues["GRE-2"]["state"]["name"], "Backlog")
        self.assertTrue(self.calls("inverseRelations("))
        self.assertEqual(self.calls("issueUpdate(") + self.calls("commentCreate("), [])

    def test_dry_run_does_not_mutate_linear(self):
        sync.DRY = True
        sync.apply_pr(pr())
        self.assertEqual(self.issues["GRE-1"]["state"]["name"], "In Progress")
        self.assertEqual(self.comments, [])

    def test_workflow_skips_without_secret_or_script(self):
        workflow = Path(__file__).parents[2] / ".github/workflows/linear-pr-sync.yml"
        command = textwrap.dedent(workflow.read_text().split("run: |\n", 1)[1])
        command = command.replace("${{ github.event_name }}", "pull_request_target")
        result = subprocess.run(["/bin/bash", "-c", command], env={"LINEAR_API_KEY": "", "PATH": ""},
                                cwd="/tmp", capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "LINEAR_API_KEY secret not set; skipping")

    def test_workflow_bootstrap_skips_when_base_has_no_script(self):
        workflow = Path(__file__).parents[2] / ".github/workflows/linear-pr-sync.yml"
        command = textwrap.dedent(workflow.read_text().split("run: |\n", 1)[1])
        command = command.replace("${{ github.event_name }}", "pull_request_target")
        result = subprocess.run(["/bin/bash", "-c", command], env={"LINEAR_API_KEY": "test-sentinel", "PATH": ""},
                                cwd="/tmp", capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "Sync script absent at trusted base; scheduled reconciliation will retry")
        self.assertNotIn("test-sentinel", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
