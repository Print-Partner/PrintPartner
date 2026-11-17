#!/usr/bin/env python3
"""Post a pp/score commit status from already-recorded PR score comments.

Reads trusted pp-score lines and plans one pp/score status per exact head SHA they name.
Never invents or re-derives a score. A record whose seat vendor equals its author= vendor
is a self-score and fails the head. In --merge-group mode it checks every PR in a merge
queue entry and posts one pp/score status on the merge group SHA, failing closed.
Default is a dry run that prints the gh api POST that would be sent. Pass --post to apply.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLASSES = ROOT / "scripts" / "merge-classes.json"
DEFAULT_REPO = "Print-Partner/PrintPartner"
PASS_AT = 8
CONTEXT = "pp/score"
MAX_DESCRIPTION = 140
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
VENDOR = re.compile(r"^[a-z][a-z0-9-]*$")
# GitHub names a merge queue entry's branch gh-readonly-queue/<base>/pr-<number>-<base sha>.
QUEUE_REF = re.compile(r"^refs/heads/gh-readonly-queue/.+/pr-([0-9]+)-([0-9a-f]{40})$")
# The queue squashes each PR into one commit whose title GitHub ends with "(#<number>)".
SQUASH_TITLE = re.compile(r"\(#([0-9]+)\)$")


@dataclass(frozen=True)
class Record:
    head: str
    seat: str
    vendor: str
    author: str
    score: float


@dataclass(frozen=True)
class StatusPlan:
    head: str
    state: str
    description: str
    records: tuple[Record, ...]


class GateError(Exception):
    """A merge group cannot be verified; the status fails closed with this message."""


def score_pattern(vendors):
    if not isinstance(vendors, (list, tuple)) or not vendors or any(
        not isinstance(v, str) or not VENDOR.fullmatch(v) for v in vendors
    ):
        raise ValueError("invalid vendor list")
    names = "|".join(re.escape(v) for v in sorted(set(vendors)))
    return re.compile(
        rf"pp-score head=([0-9a-f]{{40}}) seat=(\S+) vendor=({names})"
        rf" score=([0-9]+) author=({names})$"
    )


def _seat_map(raw, vendors, key):
    if not isinstance(raw, dict) or any(
        not isinstance(seat, str) or not seat
        or not isinstance(vendor, str) or vendor not in vendors
        for seat, vendor in raw.items()
    ):
        raise ValueError(f"invalid {key}")
    return raw


def load_trust(path=CLASSES):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    authors = data.get("scorer_authors") or ["poitee"]
    if not isinstance(authors, list) or not authors or any(
        not isinstance(a, str) or not a for a in authors
    ):
        raise ValueError("invalid scorer_authors")
    vendors = data.get("vendors") or []
    seats = dict(_seat_map(data.get("scorer_seats") or {}, vendors, "scorer_seats"))
    # pp_score_seats are accepted by the pp/score status only, not by the sensitive merge guard.
    for seat, vendor in _seat_map(data.get("pp_score_seats") or {}, vendors, "pp_score_seats").items():
        if seats.setdefault(seat, vendor) != vendor:
            raise ValueError(f"pp_score_seats disagrees with scorer_seats on {seat}")
    return {
        "authors": tuple(authors),
        "seats": seats,
        "pattern": score_pattern(vendors),
    }


def format_score(score: float) -> str:
    if score == int(score):
        return str(int(score))
    return str(score)


def unwrap(line: str) -> str:
    """Drop surrounding whitespace and inline-code backticks around a whole line.

    `pp-score ...` and ``pp-score ...`` read the same as a bare line. Fence lines
    (```text) never start with "pp-score ", so the record lines inside a fence read as bare.
    """
    text = line.strip()
    while len(text) >= 2 and text[0] == "`" and text[-1] == "`":
        text = text[1:-1].strip()
    return text


def parse_line(line: str, pattern, seats) -> Record | None:
    text = unwrap(line)
    if not text.startswith("pp-score "):
        return None
    match = pattern.fullmatch(text)
    if not match:
        return None
    head, seat, vendor, score, author = match.groups()
    if seats.get(seat) != vendor:
        return None
    return Record(head, seat, vendor, author, float(score))


def trusted_records(comments, trust) -> list[Record]:
    """Extract parseable records from trusted, unedited comments."""
    out: list[Record] = []
    pattern = trust["pattern"]
    authors = set(trust["authors"])
    for comment in comments:
        user = (comment.get("user") or {}).get("login")
        if user not in authors:
            continue
        created = comment.get("created_at")
        updated = comment.get("updated_at")
        if created and updated and created != updated:
            continue
        body = comment.get("body") or ""
        if not isinstance(body, str):
            continue
        for line in body.splitlines():
            record = parse_line(line, pattern, trust["seats"])
            if record is not None:
                out.append(record)
    return out


def _fit(description: str, fallback: str) -> str:
    if len(description) <= MAX_DESCRIPTION:
        return description
    return fallback[:MAX_DESCRIPTION]


def plan_head(head: str, group: list[Record]) -> StatusPlan:
    # Repeated seat+score pairs from two comments count once in the description.
    unique = []
    seen = set()
    for record in group:
        key = (record.seat, record.vendor, record.author, record.score)
        if key in seen:
            continue
        seen.add(key)
        unique.append(record)
    records = tuple(unique)
    self_scored = [r for r in unique if r.vendor == r.author]
    if self_scored:
        r = self_scored[0]
        return StatusPlan(head, "failure", _fit(
            f"rejected: {r.seat} is the author's own family ({r.vendor})",
            f"rejected: same-family score ({r.vendor})"), records)
    if len({r.author for r in unique}) > 1:
        return StatusPlan(head, "failure", "rejected: records disagree on the author family", records)
    # One status per SHA: the minimum recorded score is the conservative bar.
    minimum = min(r.score for r in unique)
    state = "success" if minimum >= PASS_AT else "failure"
    description = _fit(
        "; ".join(f"{format_score(r.score)}/10 from {r.seat}" for r in unique),
        f"{format_score(minimum)}/10 from {len(unique)} seats",
    )
    return StatusPlan(head, state, description, records)


def plan_statuses(records: list[Record]) -> list[StatusPlan]:
    by_head: dict[str, list[Record]] = {}
    for record in records:
        by_head.setdefault(record.head, []).append(record)
    return [plan_head(head, group) for head, group in sorted(by_head.items())]


def queue_prs(head_ref: str, base_sha: str, compare: dict) -> list[int]:
    """PR numbers in a merge group, oldest first, from its branch name and its commits.

    compare is the GitHub compare payload for base_sha...head_sha. Every commit must be a
    queue squash commit ending in (#N), and the last one must be the entry's own PR.
    """
    match = QUEUE_REF.fullmatch(head_ref or "")
    if not match:
        raise GateError("not a merge queue branch")
    entry_pr, ref_base = int(match.group(1)), match.group(2)
    if ref_base != base_sha:
        raise GateError("queue branch base does not match base_sha")
    if not isinstance(compare, dict) or compare.get("status") != "ahead":
        raise GateError("merge group is not ahead of its base")
    commits = compare.get("commits")
    if not isinstance(commits, list) or not commits or compare.get("ahead_by") != len(commits):
        raise GateError("merge group commit list is incomplete")
    prs = []
    for commit in commits:
        message = ((commit or {}).get("commit") or {}).get("message")
        title = message.splitlines()[0] if isinstance(message, str) and message else ""
        found = SQUASH_TITLE.search(title)
        if not found:
            raise GateError(f"commit {str((commit or {}).get('sha'))[:7]} names no PR")
        prs.append(int(found.group(1)))
    if prs[-1] != entry_pr or len(set(prs)) != len(prs):
        raise GateError(f"merge group commits do not end with #{entry_pr}")
    return prs


def plan_merge_group(head_sha: str, prs: list[int], pr_heads: dict, pr_records: dict) -> StatusPlan:
    """One pp/score for the merge group SHA: success only if every PR passes on its current head."""
    passed = []
    for pr in prs:
        head = pr_heads.get(pr)
        if not isinstance(head, str) or not FULL_SHA.fullmatch(head):
            return StatusPlan(head_sha, "failure", f"#{pr}: could not read its head SHA", ())
        group = [r for r in pr_records.get(pr, []) if r.head == head]
        if not group:
            return StatusPlan(head_sha, "failure",
                              f"#{pr}: no trusted score on its head {head[:7]}", ())
        plan = plan_head(head, group)
        if plan.state != "success":
            return StatusPlan(head_sha, "failure",
                              _fit(f"#{pr} {head[:7]}: {plan.description}", f"#{pr}: below 8 or rejected"),
                              plan.records)
        passed.append((pr, plan))
    if len(passed) == 1:
        pr, plan = passed[0]
        description = _fit(f"#{pr}: {plan.description}", f"#{pr}: scored 8+ on its head")
    else:
        description = _fit("; ".join(f"#{pr} min {format_score(min(r.score for r in plan.records))}"
                                     for pr, plan in passed),
                           f"{len(passed)} PRs scored 8+ on their heads")
    records = tuple(r for _, plan in passed for r in plan.records)
    return StatusPlan(head_sha, "success", description, records)


def gh_status_call(repo: str, plan: StatusPlan) -> list[str]:
    return [
        "gh",
        "api",
        "-X",
        "POST",
        f"repos/{repo}/statuses/{plan.head}",
        "-f",
        f"state={plan.state}",
        "-f",
        f"context={CONTEXT}",
        "-f",
        f"description={plan.description}",
    ]


def format_call(repo: str, plan: StatusPlan) -> str:
    args = gh_status_call(repo, plan)
    return " ".join(_quote(a) for a in args)


def _quote(arg: str) -> str:
    if re.fullmatch(r"[A-Za-z0-9._/=+-]+", arg):
        return arg
    return "'" + arg.replace("'", "'\\''") + "'"


def gh_json(path: str, paginate: bool = False):
    args = ["gh", "api"] + (["--paginate"] if paginate else []) + [path]
    return json.loads(subprocess.check_output(args, text=True))


def fetch_comments(repo: str, pr: int) -> list[dict]:
    data = gh_json(f"repos/{repo}/issues/{pr}/comments", paginate=True)
    if not isinstance(data, list):
        raise RuntimeError(f"unexpected comments payload for {repo}#{pr}")
    return data


def fetch_head(repo: str, pr: int) -> str:
    data = gh_json(f"repos/{repo}/pulls/{pr}")
    return ((data or {}).get("head") or {}).get("sha")


def apply_plan(repo: str, plan: StatusPlan) -> None:
    subprocess.check_call(gh_status_call(repo, plan), stdout=subprocess.DEVNULL)


def show(repo: str, plan: StatusPlan, post: bool) -> None:
    print(format_call(repo, plan))
    seats = ", ".join(
        f"{r.seat} {format_score(r.score)} (vendor={r.vendor} author={r.author}) on {r.head[:7]}"
        for r in plan.records
    )
    print(f"# {plan.state} on {plan.head}: {seats or plan.description}")
    if post:
        apply_plan(repo, plan)
        print(f"posted pp/score={plan.state} on {plan.head}")


def render(repo: str, pr: int, plans: list[StatusPlan], post: bool) -> int:
    if not plans:
        print(f"unscored: {repo}#{pr} has no trusted recorded score for any head SHA")
        return 0
    for plan in plans:
        show(repo, plan, post)
    return 0


def run_merge_group(repo, head_sha, base_sha, head_ref, trust, post, prs=None) -> int:
    if not FULL_SHA.fullmatch(head_sha or ""):
        print("head-sha must be a full lowercase SHA", file=sys.stderr)
        return 2
    try:
        if not FULL_SHA.fullmatch(base_sha or ""):
            raise GateError("base_sha is not a full SHA")
        if prs is None:
            prs = queue_prs(head_ref, base_sha, gh_json(f"repos/{repo}/compare/{base_sha}...{head_sha}"))
        print(f"# merge group {head_sha} holds PRs {', '.join(f'#{p}' for p in prs)}")
        heads = {pr: fetch_head(repo, pr) for pr in prs}
        records = {pr: trusted_records(fetch_comments(repo, pr), trust) for pr in prs}
        plan = plan_merge_group(head_sha, prs, heads, records)
    except GateError as err:
        plan = StatusPlan(head_sha, "failure", _fit(f"cannot verify merge group: {err}",
                                                    "cannot verify merge group"), ())
    except Exception as err:  # noqa: BLE001 - any read failure must still fail the queue entry closed
        plan = StatusPlan(head_sha, "failure",
                          f"cannot verify merge group: {type(err).__name__} reading GitHub", ())
    show(repo, plan, post)
    return 0 if plan.state == "success" else 1


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=DEFAULT_REPO, help="owner/name (default: %(default)s)")
    parser.add_argument("--pr", type=int, help="pull request number (comment mode)")
    parser.add_argument("--merge-group", action="store_true",
                        help="check every PR in a merge queue entry and post on its SHA")
    parser.add_argument("--head-sha", help="merge group head SHA (merge-group mode)")
    parser.add_argument("--base-sha", help="merge group base SHA (merge-group mode)")
    parser.add_argument("--head-ref", help="merge group head ref (merge-group mode)")
    parser.add_argument("--prs", help="dry-run aid: comma-separated PRs instead of reading the queue")
    parser.add_argument(
        "--post",
        action="store_true",
        help="POST the statuses; default is a dry run that prints the API calls",
    )
    parser.add_argument(
        "--classes",
        default=str(CLASSES),
        help="merge-classes.json path (scorer_authors, seats and vendors)",
    )
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        print("repo must be owner/name", file=sys.stderr)
        return 2
    trust = load_trust(Path(args.classes))
    if args.merge_group:
        prs = None
        if args.prs:
            if args.post:
                print("--prs is for dry runs only", file=sys.stderr)
                return 2
            prs = [int(p) for p in args.prs.split(",")]
        return run_merge_group(args.repo, args.head_sha, args.base_sha, args.head_ref,
                               trust, args.post, prs)
    if args.pr is None or args.pr <= 0:
        print("pr must be a positive integer", file=sys.stderr)
        return 2
    comments = fetch_comments(args.repo, args.pr)
    plans = plan_statuses(trusted_records(comments, trust))
    return render(args.repo, args.pr, plans, args.post)


if __name__ == "__main__":
    sys.exit(main())
