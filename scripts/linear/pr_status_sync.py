#!/usr/bin/env python3
"""Deterministic Linear <-> GitHub PR status sync for Print Partner (no LLM).

Rules (team GRE):
  * PR open and references GRE-NNN (branch gre-NNN-*, or Refs/Fixes/Closes GRE-NNN
    in title/body)                                   -> In Review
  * PR merged with Fixes/Closes/Resolves GRE-NNN     -> Done
    Closing directives must start the title or a line, with optional Markdown.
    The Done write is read back and retried once; a ticket still not Done fails
    the run. Then each Backlog issue it blocks whose other blockers are Done
    (umbrella blockers ignored), and that has no PROMOTE_SKIP label, -> Todo
    with one "fleet-promote: blockers done <ids>" comment.
  * PR merged with only Refs GRE-NNN (multi-slice)   -> In Progress (if another
    PR for the ticket is still open: stays In Review)
  * PR closed unmerged                               -> In Progress if it was In
    Review and no other open PR
  * Cron: In Progress/In Review with no PR activity for STALE_HOURS (48) and no
    open PR -> Backlog.
Writes ticket state, plus the fleet-promote comment. GitHub holds PR state.
Never touches Done/Canceled/Duplicate issues, or the owner-signed PROTECTED
tickets, whose skipped writes are logged. Never prints secrets.
Fork PRs are excluded from both event handling and reconciliation.

Usage:
  pr_status_sync.py event  --pr N        # one PR (Actions pull_request event)
  pr_status_sync.py reconcile [--dry-run] # cron fallback over recent PRs
Env: LINEAR_API_KEY, GH_TOKEN (read), GITHUB_REPOSITORY (default Print-Partner/PrintPartner)
"""
import argparse, datetime as dt, http.client, json, os, re, subprocess, sys, time, urllib.error, urllib.request

REPO = os.environ.get("GITHUB_REPOSITORY", "Print-Partner/PrintPartner")
TEAM = os.environ.get("LINEAR_TEAM_KEY", "GRE")
STALE_HOURS = int(os.environ.get("STALE_HOURS", "48"))
REPLAY_HOURS = 3  # reconcile --hours in linear-pr-sync.yml
DRY = False
CLOSE_RE = re.compile(r"^[ \t#>*-]*(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)[ \t]*:?[ \t]+((?:GRE-\d+\b[ \t,]*(?:and[ \t]+)?)+)", re.I | re.M)
REF_RE = re.compile(r"\b(?:refs?|part of|towards?)\s*:?\s+((?:GRE-\d+[\s,]*(?:and\s+)?)+)", re.I)
ID_RE = re.compile(r"GRE-(\d+)", re.I)
BRANCH_RE = re.compile(r"^gre-(\d+)\b", re.I)
TERMINAL = {"completed", "canceled", "duplicate"}
PROMOTE_SKIP = {"needs-chad-action", "umbrella", "post-parity", "pp:later"}
# Owner-signed list: the sync never writes these tickets' state or comments; the owner moves them.
PROTECTED = frozenset(f"GRE-{n}" for n in (
    451, 621, 1007, 1008, 1009, 1010, 1011, 1012, 1013, *range(1019, 1029), 1030,
    1056, 1060, 1061, 1062, 1063, 1064, 1065, 1066))
_END = "id identifier team{key} state{name type} labels(first:50){nodes{name}}"
LINKED_QUERY = ("query($id:String!){issue(id:$id){%s relations(first:100){nodes{type issue{%s} relatedIssue{%s}}} "
                "inverseRelations(first:100){nodes{type issue{%s} relatedIssue{%s}}}}}" % ((_END,) * 5))
_request_deadline = None


def gql(query, variables=None):
    key = os.environ["LINEAR_API_KEY"]
    req = urllib.request.Request(
        "https://api.linear.app/graphql",
        json.dumps({"query": query, "variables": variables or {}}).encode(),
        {"Content-Type": "application/json", "Authorization": key},
    )
    # A lost commentCreate response may hide a successful write. Only an explicit
    # rate-limit rejection is safe to replay; issueUpdate sets an absolute state.
    replay_safe = "commentCreate" not in query
    for attempt in range(3):
        timeout = 30 if _request_deadline is None else min(30, _request_deadline - time.monotonic())
        if timeout <= 0:
            raise RuntimeError("Linear reconciliation request deadline exhausted") from None
        try:
            try:
                response = urllib.request.urlopen(req, timeout=timeout)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                status = getattr(response, "status", None) or getattr(response, "code", 200)
                body = response.read().decode("utf-8", errors="replace")
        except (OSError, http.client.HTTPException):
            failure = "Linear API transport failed"
            retry = replay_safe
            print(failure, file=sys.stderr)
        else:
            try:
                out = json.loads(body)
            except ValueError:
                out = {}
            if not isinstance(out, dict):
                out = {}
            errors = out.get("errors") or []
            rate_limited = bool(errors) and all(
                isinstance(error, dict) and error.get("extensions", {}).get("code") == "RATELIMITED"
                for error in errors)
            if status < 400 and not errors and "data" in out:
                return out["data"]
            diagnostic = "; ".join(
                f"{error.get('message', '')} code={error.get('extensions', {}).get('code', '')}"
                for error in errors if isinstance(error, dict)) if errors else body
            diagnostic = diagnostic.replace(key, "[REDACTED]") if key else diagnostic
            print(f"Linear API HTTP {status}: {json.dumps(diagnostic, ensure_ascii=False)}", file=sys.stderr)
            failure = "Linear API returned GraphQL errors" if errors else f"Linear API HTTP {status} failed"
            rejected = (rate_limited or status == 429) and not out.get("data")
            server_error = 500 <= status < 600
            invalid_request = any(
                isinstance(error, dict) and error.get("extensions", {}).get("code") in
                {"BAD_USER_INPUT", "GRAPHQL_VALIDATION_FAILED", "GRAPHQL_PARSE_FAILED"}
                for error in errors)
            transient = rate_limited or status == 429 or server_error
            retry = transient and (replay_safe or rejected) and not invalid_request and (
                not errors or rate_limited or server_error)
        if not retry or attempt == 2:
            raise RuntimeError(failure) from None
        delay = 2 ** attempt
        if _request_deadline is not None and time.monotonic() + delay >= _request_deadline:
            raise RuntimeError("Linear reconciliation request deadline exhausted") from None
        time.sleep(delay)


def gh(*args):
    return json.loads(subprocess.check_output(["gh", *args], text=True))


_states = None
def state_id(name):
    global _states
    if _states is None:
        d = gql('query($k:String!){teams(filter:{key:{eq:$k}}){nodes{states{nodes{id name}}}}}', {"k": TEAM})
        _states = {s["name"]: s["id"] for s in d["teams"]["nodes"][0]["states"]["nodes"]}
    return _states[name]


def issue(ident):
    key, number = ident.rsplit("-", 1)
    # A filtered connection returns no nodes for an unknown number, rather than
    # the single-entity lookup's GraphQL not-found error. Other API errors propagate.
    d = gql('query($k:String!,$n:Float!){issues(first:1,includeArchived:true,filter:{team:{key:{eq:$k}},number:{eq:$n}}){nodes{id identifier state{name type} updatedAt}}}',
            {"k": key, "n": int(number)})
    return d["issues"]["nodes"][0] if d["issues"]["nodes"] else None


def ts(value):
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def protected(ident):
    """Whether ident is owner-protected. Logs the skipped write."""
    # issue() reads GRE-01008 as GRE-1008, so compare the same canonical number.
    team, _, number = ident.upper().rpartition("-")
    if number.isdigit() and f"{team}-{int(number)}" in PROTECTED:
        print(f"{ident}: skipped, owner-protected ticket; the sync does not write it", file=sys.stderr)
        return True
    return False


def settle(ident, decide, basis=None):
    """Write decide(issue) for ident, then correct the write if it raced another run.

    Linear's issueUpdate has no conditional input. The issue is read again just before
    the mutation, and any change since the decision repeats the decision. After the
    mutation, GitHub is read again and replayed the way reconcile would replay it. A
    run whose PR change or Linear write this one overtook acted on GitHub state from
    before that read, so the replay sees it and its state is written back. This does
    not rely on Linear history, which can merge rows from the shared API key.
    A run that writes after this one owns the state and runs the same check.
    """
    if protected(ident):
        return False
    basis = basis or issue(ident)
    version = basis and (basis["state"]["name"], basis["updatedAt"])
    for _ in range(4):
        if not basis:
            return False
        target = basis["state"]["name"]
        if basis["state"]["type"] not in TERMINAL:
            target = decide(basis) or target
        if target == basis["state"]["name"]:
            return False
        if DRY:
            print(f"{ident}: {basis['state']['name']} -> {target}")
            return True
        current = issue(ident)
        if not current or (current["state"]["name"], current["updatedAt"]) != version:
            basis, version = current, current and (current["state"]["name"], current["updatedAt"])
            continue
        write_state(current["id"], target)
        print(f"{ident}: {current['state']['name']} -> {target}")
        want = converge(ident, target)
        if want == target:
            return True
        basis = issue(ident)
        if not basis or basis["state"]["name"] != target:
            return True
        version = (basis["state"]["name"], basis["updatedAt"])
        decide = lambda _, want=want: want
    raise RuntimeError(f"{ident}: concurrent Linear writes did not settle")


def write_state(issue_id, name):
    updated = gql('mutation($id:String!,$s:String!){issueUpdate(id:$id,input:{stateId:$s}){success}}',
        {"id": issue_id, "s": state_id(name)})
    if not updated["issueUpdate"]["success"]:
        raise RuntimeError("Linear issueUpdate failed")


def labels(node):
    return {label["name"] for label in node["labels"]["nodes"]}


def blocking(ident):
    """(issue, issues it blocks, its blockers), read from both relation directions."""
    node = gql(LINKED_QUERY, {"id": ident})["issue"]
    if not node:
        return None, [], []
    blocks, blockers = [], []
    for rel in node["relations"]["nodes"] + node["inverseRelations"]["nodes"]:
        if rel["type"] != "blocks":
            continue
        if rel["issue"]["id"] == node["id"]:
            blocks.append(rel["relatedIssue"])
        elif rel["relatedIssue"]["id"] == node["id"]:
            blockers.append(rel["issue"])
    return node, blocks, blockers


def close_out(ident):
    """After a Fixes merge: confirm ident is Done (one retried write), then promote what it unblocked."""
    if protected(ident):
        # The owner moves protected tickets; once one is Done, its unprotected dependents still get promoted.
        current = issue(ident)
        if current and current["state"]["name"] == "Done":
            promote(ident)
        return
    current = issue(ident)
    if not current or current["state"]["type"] in TERMINAL - {"completed"}:
        return
    if current["state"]["name"] != "Done" and not DRY:
        print(f"{ident}: {current['state']['name']} after the Done write; retrying once", file=sys.stderr)
        write_state(current["id"], "Done")
        current = issue(ident)
        if not current or current["state"]["name"] != "Done":
            print(f"::error::{ident} is not Done after a Fixes merge and one retried write", file=sys.stderr)
            raise RuntimeError(f"{ident}: not Done after a Fixes merge")
    promote(ident)


def promote(done):
    """Backlog -> Todo for issues done blocked, once every non-umbrella blocker is Done. Never demotes."""
    _, unblocked, _ = blocking(done)
    for blocked in unblocked:
        if blocked["team"]["key"] != TEAM or protected(blocked["identifier"]):
            continue
        node, _, blockers = blocking(blocked["identifier"])
        if not node or node["state"]["name"] != "Backlog" or labels(node) & PROMOTE_SKIP:
            continue
        real = [b for b in blockers if "umbrella" not in labels(b)]
        if any(b["identifier"] != done and b["state"]["name"] != "Done" for b in real):
            continue
        print(f"{node['identifier']}: Backlog -> Todo")
        if DRY:
            continue
        write_state(node["id"], "Todo")
        ids = sorted({b["identifier"] for b in real} or {done}, key=lambda i: int(i.rsplit("-", 1)[1]))
        gql('mutation($id:String!,$b:String!){commentCreate(input:{issueId:$id,body:$b}){success}}',
            {"id": node["id"], "b": "fleet-promote: blockers done " + ", ".join(ids)})


def rule(pr, ident, state, others):
    """Target for ident after pr, from the issue state and others(), another open PR."""
    closing, ids = refs(pr)
    if ident not in ids:
        return None
    if pr["state"] == "OPEN":
        return "In Review"
    if pr["state"] == "MERGED":
        if ident in closing:
            return "Done"
        if others():
            return "In Review"
        if state in {"In Review", "In Progress"}:  # never pull Todo/Backlog back up
            return "In Progress"
    elif pr["state"] == "CLOSED" and state == "In Review" and not others():
        return "In Progress"
    return None


def converge(ident, state):
    """The state reconcile would reach for ident from state, on GitHub read now."""
    prs = [p for p in fresh_prs() if ident in refs(p)[1]]
    opened = {p["number"] for p in prs if p["state"] == "OPEN"}
    for p in sorted(prs, key=lambda p: ts(p["updatedAt"])):  # oldest first, newest wins
        if state == "Done":
            break
        state = rule(p, ident, state, lambda: opened - {p["number"]}) or state
    return state


def refs(pr):
    """Return (closing_ids, all_ids) for a PR dict with title/body/headRefName."""
    text = f"{pr.get('title','')}\n{pr.get('body') or ''}"
    closing = {f"GRE-{n}" for m in CLOSE_RE.findall(text) for n in ID_RE.findall(m)}
    allids = set(closing)
    allids |= {f"GRE-{n}" for m in REF_RE.findall(text) for n in ID_RE.findall(m)}
    allids |= {f"GRE-{n}" for n in ID_RE.findall(pr.get("title", ""))}
    b = BRANCH_RE.match(pr.get("headRefName", ""))
    if b:
        allids.add(f"GRE-{b.group(1)}")
    return closing, allids


FIELDS = "number,state,title,body,headRefName,mergedAt,closedAt,updatedAt,isCrossRepository"
PR_QUERY = '''
  query($owner:String!, $name:String!, $endCursor:String) {
    repository(owner:$owner, name:$name) {
      pullRequests(first:100, after:$endCursor, %sorderBy:{field:UPDATED_AT,direction:DESC}) {
        nodes { number state title body headRefName mergedAt closedAt updatedAt isCrossRepository
          commits(last:1) { nodes { commit { committedDate } } }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }'''
def fetch_prs(states="", paginate=True):
    """Return (same-repo PRs, whether more pages exist), newest update first."""
    owner, name = REPO.split("/", 1)
    args = ["-f", "query=" + PR_QUERY % states, "-f", f"owner={owner}", "-f", f"name={name}"]
    pages = gh("api", "graphql", "--paginate", "--slurp", *args) if paginate else [gh("api", "graphql", *args)]
    prs = [pr for page in pages for pr in page["data"]["repository"]["pullRequests"]["nodes"]
           if not pr["isCrossRepository"]]
    return prs, pages[-1]["data"]["repository"]["pullRequests"]["pageInfo"]["hasNextPage"]


_prs = None
def all_prs():
    global _prs
    if _prs is None:
        _prs = fetch_prs()[0]
    return _prs


def fresh_prs():
    """Open PRs and PRs updated within REPLAY_HOURS, read now rather than cached."""
    since = dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=REPLAY_HOURS)
    recent, more = fetch_prs(paginate=False)
    if more and recent and ts(recent[-1]["updatedAt"]) >= since:
        recent = fetch_prs()[0]
    prs = {p["number"]: p for p in recent if ts(p["updatedAt"]) >= since}
    prs.update((p["number"], p) for p in fetch_prs("states:OPEN, ")[0])
    return list(prs.values())


def open_prs():
    return [pr for pr in all_prs() if pr["state"] == "OPEN"]


def other_open(ident, exclude):
    return [p["number"] for p in open_prs() if p["number"] != exclude and ident in refs(p)[1]]


def apply_pr(pr, errors=None):
    if pr["isCrossRepository"]:
        return
    n = pr["number"]
    closing, ids = refs(pr)
    for ident in sorted(ids):
        try:
            settle(ident, lambda iss, ident=ident: rule(pr, ident, iss["state"]["name"],
                                                        lambda: other_open(ident, n)))
            if pr["state"] == "MERGED" and ident in closing:
                close_out(ident)
        except Exception:
            if errors is None:
                raise
            errors.append(f"PR #{n} {ident}")


def apply_event(pr, event):
    if pr["isCrossRepository"]:
        return
    if event.get("action") == "edited":
        previous = dict(pr)
        for field in ("title", "body"):
            if field in event.get("changes", {}):
                previous[field] = event["changes"][field]["from"]
        removed = refs(previous)[1] - refs(pr)[1]
        for ident in sorted(removed):
            settle(ident, lambda iss, ident=ident: "In Progress" if iss["state"]["name"] == "In Review"
                   and not other_open(ident, pr["number"]) else None)
    apply_pr(pr)


def reconcile(hours):
    global _request_deadline
    previous_deadline = _request_deadline
    _request_deadline = time.monotonic() + 240
    try:
        _reconcile(hours)
    finally:
        _request_deadline = previous_deadline


def _reconcile(hours):
    since = dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=hours)
    prs = all_prs()
    errors = []
    recent = [p for p in prs if p["state"] == "OPEN" or ts(p["updatedAt"]) >= since]
    for p in sorted(recent, key=lambda p: ts(p["updatedAt"])):  # oldest first, newest wins
        apply_pr(p, errors)
    # stall sweep
    last = {}
    for p in prs:
        activity = [p[key] for key in ("mergedAt", "closedAt", "updatedAt") if p.get(key)]
        activity += [node["commit"]["committedDate"] for node in p.get("commits", {}).get("nodes", [])]
        t = max(ts(value) for value in activity)
        for ident in refs(p)[1]:
            last[ident] = max(last.get(ident, t), t)
    issues = []
    cursor = None
    while True:
        try:
            d = gql('query($k:String!,$after:String){issues(first:250,after:$after,filter:{team:{key:{eq:$k}},state:{name:{in:["In Progress","In Review"]}}}){nodes{id identifier updatedAt state{name type}} pageInfo{hasNextPage endCursor}}}', {"k": TEAM, "after": cursor})
        except Exception:
            errors.append("stale issue listing")
            break
        issues.extend(d["issues"]["nodes"])
        if not d["issues"]["pageInfo"]["hasNextPage"]:
            break
        cursor = d["issues"]["pageInfo"]["endCursor"]
    cutoff = dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=STALE_HOURS)
    for iss in issues:
        ident = iss["identifier"]
        try:
            def stale(fresh, ident=ident):
                t = max([ts(fresh["updatedAt"])] + ([last[ident]] if ident in last else []))
                return "Backlog" if not other_open(ident, -1) and t < cutoff else None
            settle(ident, stale, iss)
        except Exception:
            errors.append(f"stale {ident}")
    if errors:
        raise RuntimeError("Linear reconciliation failed for: " + ", ".join(errors)) from None


def main():
    global DRY
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["event", "reconcile"])
    ap.add_argument("--pr", type=int)
    ap.add_argument("--hours", type=int, default=3)
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()
    DRY = a.dry_run
    if a.mode == "event":
        event = {}
        if os.environ.get("GITHUB_EVENT_PATH"):
            with open(os.environ["GITHUB_EVENT_PATH"]) as stream:
                event = json.load(stream)
        apply_event(gh("pr", "view", str(a.pr), "-R", REPO, "--json", FIELDS), event)
    else:
        reconcile(a.hours)


if __name__ == "__main__":
    sys.exit(main())
