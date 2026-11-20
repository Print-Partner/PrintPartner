#!/usr/bin/env python3
"""Post a pp/score commit status from already-recorded PR score comments.

Reads trusted pp-score lines with merge_class.read_comments, the reader the sensitive merge check
also uses, and plans one pp/score status per exact head SHA they name. Never invents or re-derives
a score. A record whose seat vendor equals its author= vendor is a self-score and fails the head.
A record must end with obstacles=, the scorer's disclosure of any refusal, guard refusal or
approval stall it hit; a record without it, an edited record, a wrong seat vendor or a malformed
record line fails the head (read_comments lists every fault). author=cursor-auto or author=unknown requires trusted
anthropic and openai records on that exact head; the lower score is the bar. In --merge-group mode it checks every PR in a merge
queue entry and posts one pp/score status on the merge group SHA, failing closed.
A PR head with no records of its own can carry the records of an earlier head across clean
merges of main (Chad, 2026-11-18 11:02 PM ET); hop and plan_carry list every condition.
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

# The record reader, its rules and the carry decision are shared with the coordinator's sensitive
# merge check (agent-guard.py), so both read the same comments the same way.
from merge_class import (DUAL_AUTHOR_FAMILIES, NO_CARRY_VENDORS, PASS_AT, Reading, Record, carry_source,  # noqa: F401
                         classify, head_fault, load_classes, load_trust, read_comments)
from merge_class import (CARRY_MAX_HOPS, Carry, GitHubFacts, LocalGit,  # noqa: F401
                         check_state, decide_carry, gate_file, hop)

ROOT = Path(__file__).resolve().parent.parent
CLASSES = ROOT / "scripts" / "merge-classes.json"
DEFAULT_REPO = "Print-Partner/PrintPartner"
CONTEXT = "pp/score"
MAX_DESCRIPTION = 140
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
DUAL_REQUIRED_VENDORS = ("anthropic", "openai")
# GitHub names a merge queue entry's branch gh-readonly-queue/<base>/pr-<number>-<base sha>.
QUEUE_REF = re.compile(r"^refs/heads/gh-readonly-queue/.+/pr-([0-9]+)-([0-9a-f]{40})$")
# The queue squashes each PR into one commit whose title GitHub ends with "(#<number>)".
SQUASH_TITLE = re.compile(r"\(#([0-9]+)\)$")


@dataclass(frozen=True)
class StatusPlan:
    head: str
    state: str
    description: str
    records: tuple[Record, ...]


class GateError(Exception):
    """A merge group cannot be verified; the status fails closed with this message."""


def format_score(score: float) -> str:
    if score == int(score):
        return str(int(score))
    return str(score)


def _fit(description: str, fallback: str) -> str:
    if len(description) <= MAX_DESCRIPTION:
        return description
    return fallback[:MAX_DESCRIPTION]


def plan_head(head: str, reading: Reading) -> StatusPlan:
    group = reading.on(head)
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
    # The rules shared with the sensitive merge check: faults, wrapped records, the author's own
    # family, author=, and a record below PASS_AT.
    fault = head_fault(reading, head) or (None if unique else "no score records")
    if fault:
        return StatusPlan(head, "failure", _fit(f"rejected: {fault}", "rejected: invalid score record"), records)
    author = unique[0].author
    if author in DUAL_AUTHOR_FAMILIES:
        present = {r.vendor for r in group if r.bare}  # a wrapped record counts toward no pass
        missing = [v for v in DUAL_REQUIRED_VENDORS if v not in present]
        if missing:
            return StatusPlan(head, "failure", _fit(
                f"{author} requires anthropic and openai scores; missing {', '.join(missing)}",
                f"{author}: missing a required family"), records)
    # head_fault refused any record below PASS_AT, so every record here passes.
    minimum = min(r.score for r in unique)
    description = _fit(
        "; ".join(f"{format_score(r.score)}/10 from {r.seat}" for r in unique),
        f"{format_score(minimum)}/10 from {len(unique)} seats",
    )
    return StatusPlan(head, "success", description, records)


def plan_statuses(reading: Reading) -> list[StatusPlan]:
    """One plan per head that has records or faults of its own."""
    return [plan_head(head, reading) for head in sorted(reading.heads())]


def plan_carry(head: str, reading: Reading, strict: bool, facts,
               checks: str | None) -> tuple[StatusPlan | None, str]:
    """pp/score for a head with no records of its own, carried from an earlier head, or (None, why).

    merge_class.carry_source finds the source head from checks, the caller's one check_state
    read, exactly as the sensitive merge check does. Then the source's records must pass plan_head
    as they would on the source itself, Grok included; Grok and wrapped records are then dropped,
    and the rest must pass plan_head again on their own; and a strict (owner or sensitive) PR needs
    every record on the source above 8.
    """
    source, why = carry_source(reading, head, facts, load_classes(), checks)
    if source is None:
        return None, why
    plan = plan_head(source, reading)
    if plan.state != "success":
        return None, f"{source[:7]} did not pass: {plan.description}"
    low = [r for r in plan.records if r.score <= PASS_AT]
    if strict and low:
        return None, f"sensitive: {low[0].seat} scored {format_score(low[0].score)} on {source[:7]}"
    carried = tuple(r for r in reading.on(source) if r.vendor not in NO_CARRY_VENDORS and r.bare)
    if not carried:
        return None, f"{source[:7]} has no record that can carry"
    plan = plan_head(source, Reading(carried))
    if plan.state != "success":
        # The records that carry must pass on their own, for example a cursor-auto pair.
        return None, f"{source[:7]} did not pass without its Grok and wrapped records: {plan.description}"
    prefix = f"carried from {source[:7]}: "
    return StatusPlan(head, "success", _fit(prefix + plan.description, prefix + "scored 8+"),
                      plan.records), f"carried from {source[:7]}"


def no_carry_plan(head: str, why: str) -> StatusPlan:
    """Failure on a head with no records and no carry. It replaces a carried success posted
    earlier, for example before a newer record on the source head failed it."""
    return StatusPlan(head, "failure", _fit(f"no score and no carry: {why}", "no score and no carry"), ())


def strict_class(paths: list[str]) -> bool:
    """Sensitive or owner paths need every carried seat above 8."""
    return classify(paths, load_classes()) != "lane"


def carry_for_pr(repo: str, pr: int, head: str, reading: Reading, facts,
                 checks: str | None) -> tuple[StatusPlan | None, str]:
    """plan_carry for a live PR; any read failure means no carry."""
    try:
        files = fetch_files(repo, pr)
        return plan_carry(head, reading, strict_class(files), facts, checks)
    except Exception as err:  # noqa: BLE001 - an unreadable fact never carries a score
        return None, f"cannot verify carry: {type(err).__name__}: {err}"


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


def plan_merge_group(head_sha: str, prs: list[int], pr_heads: dict, pr_records: dict,
                     carried: dict | None = None) -> StatusPlan:
    """One pp/score for the merge group SHA: success only if every PR passes on its current head.

    pr_records maps a PR to its Reading; carried maps a PR to the plan_carry result for a head
    with no records or faults of its own.
    """
    passed = []
    for pr in prs:
        head = pr_heads.get(pr)
        if not isinstance(head, str) or not FULL_SHA.fullmatch(head):
            return StatusPlan(head_sha, "failure", f"#{pr}: could not read its head SHA", ())
        reading = pr_records.get(pr, Reading())
        plan = plan_head(head, reading) if head in reading.heads() else (carried or {}).get(pr, (None, ""))[0]
        if plan is None:
            return StatusPlan(head_sha, "failure",
                              f"#{pr}: no trusted score on its head {head[:7]}", ())
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


# The REST file list stops at 3000 entries; a list that size may be incomplete.
FILE_LIST_CAP = 3000


def changed_paths(entries, changed_files) -> list[str]:
    """Every path a PR touches, both sides of each rename, as the merge guard reads them.

    Refuses an empty, incomplete or capped list, so an unreadable class never carries.
    """
    if not isinstance(entries, list) or not entries or type(changed_files) is not int \
            or changed_files != len(entries) or changed_files >= FILE_LIST_CAP:
        raise GateError("missing, incomplete or capped PR file list")
    paths = []
    for entry in entries:
        paths.append(entry.get("filename"))
        if entry.get("status") == "renamed":
            paths.append(entry.get("previous_filename"))
    if any(not isinstance(path, str) or not path for path in paths):
        raise GateError("missing PR file path")
    return paths


def fetch_files(repo: str, pr: int) -> list[str]:
    out = subprocess.check_output(["gh", "api", "--paginate", f"repos/{repo}/pulls/{pr}/files",
                                   "--jq", ".[] | {filename, status, previous_filename}"], text=True)
    entries = [json.loads(line) for line in out.splitlines() if line.strip()]
    return changed_paths(entries, gh_json(f"repos/{repo}/pulls/{pr}").get("changed_files"))


def fetch_head(repo: str, pr: int) -> str:
    data = gh_json(f"repos/{repo}/pulls/{pr}")
    return ((data or {}).get("head") or {}).get("sha")


def apply_plan(repo: str, plan: StatusPlan) -> None:
    subprocess.check_call(gh_status_call(repo, plan), stdout=subprocess.DEVNULL)


def show(repo: str, plan: StatusPlan, post: bool) -> None:
    print(format_call(repo, plan))
    seats = ", ".join(
        f"{r.seat} {format_score(r.score)} (vendor={r.vendor} author={r.author}"
        f" obstacles={','.join(r.obstacles) or 'none'}) on {r.head[:7]}"
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
        records = {pr: read_comments(fetch_comments(repo, pr), trust) for pr in prs}
        facts = GitHubFacts(repo, gh_json)
        carried = {pr: carry_for_pr(repo, pr, heads[pr], records[pr], facts, check_state(facts, heads[pr]))
                   for pr in prs
                   if isinstance(heads[pr], str) and FULL_SHA.fullmatch(heads[pr])
                   and heads[pr] not in records[pr].heads()}
        for pr, (_, why) in carried.items():
            print(f"# #{pr} {heads[pr][:7]}: {why}")
        plan = plan_merge_group(head_sha, prs, heads, records, carried)
    except GateError as err:
        plan = StatusPlan(head_sha, "failure", _fit(f"cannot verify merge group: {err}",
                                                    "cannot verify merge group"), ())
    except Exception as err:  # noqa: BLE001 - any read failure must still fail the queue entry closed
        plan = StatusPlan(head_sha, "failure",
                          f"cannot verify merge group: {type(err).__name__} reading GitHub", ())
    show(repo, plan, post)
    return 0 if plan.state == "success" else 1


# Runs for one PR can overlap or finish out of order: a concurrency group does not keep dispatch
# order, and GitHub replaces a pending run with a newer one. So after posting, a run reads the
# facts again: the head SHA, the ids and edit times of the comments, and the head's required-check
# result. If any changed since it decided, it decides and posts again, up to SETTLE_ROUNDS times.
# If they are still changing after that, it posts failure on the head and exits 1 (fail closed).
SETTLE_ROUNDS = 3
UNSETTLED = "re-evaluation needed: the comments, head or checks kept changing"


def snapshot(comments, head, checks):
    return head, checks, tuple((c.get("id"), c.get("created_at"), c.get("updated_at")) for c in comments)


def read_snapshot(repo: str, pr: int):
    try:
        comments, head = fetch_comments(repo, pr), fetch_head(repo, pr)
        return snapshot(comments, head, check_state(GitHubFacts(repo, gh_json), head))
    except Exception:  # noqa: BLE001 - an unreadable re-read never matches, so the run decides again
        return None


def comment_pass(repo: str, pr: int, trust, carry_only: bool):
    """(plans, snapshot, head_ok) from one read of the PR's comments, head and head checks."""
    comments = fetch_comments(repo, pr)
    reading = read_comments(comments, trust)
    plans = plan_statuses(reading)
    try:
        head = fetch_head(repo, pr)
    except Exception as err:  # noqa: BLE001 - the recorded heads still post; the run then fails
        head = None
        print(f"# no carry: cannot read the head of #{pr} ({type(err).__name__})")
    head_ok = isinstance(head, str) and bool(FULL_SHA.fullmatch(head))
    facts = GitHubFacts(repo, gh_json)
    # One read of the head's checks: the carry decides from it and the snapshot records it.
    checks = check_state(facts, head) if head_ok else None
    if carry_only:
        # Dry-run aid: decide the carry for the current head as if it had no records of its own.
        reading = reading.without(head)
        plans = []
    if head_ok and head not in reading.heads():
        carried, why = carry_for_pr(repo, pr, head, reading, facts, checks)
        print(f"# carry to {head}: {why}")
        plans.append(carried or no_carry_plan(head, why))
    return plans, snapshot(comments, head, checks), head_ok


def pr_number(text: str) -> int:
    """A PR number in canonical form only. The workflow's concurrency group uses the raw text,
    so 01865 must not reach the same PR as 1865 from another group."""
    if not re.fullmatch(r"[1-9][0-9]{0,9}", text):
        raise argparse.ArgumentTypeError("pr must be a positive integer without leading zeros or spaces")
    return int(text)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=DEFAULT_REPO, help="owner/name (default: %(default)s)")
    parser.add_argument("--pr", type=pr_number, help="pull request number (comment mode)")
    parser.add_argument("--merge-group", action="store_true",
                        help="check every PR in a merge queue entry and post on its SHA")
    parser.add_argument("--head-sha", help="merge group head SHA (merge-group mode)")
    parser.add_argument("--base-sha", help="merge group base SHA (merge-group mode)")
    parser.add_argument("--head-ref", help="merge group head ref (merge-group mode)")
    parser.add_argument("--prs", help="dry-run aid: comma-separated PRs instead of reading the queue")
    parser.add_argument("--carry-only", action="store_true",
                        help="dry-run aid: ignore the current head's own records and decide its carry")
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
    if args.carry_only and (args.post or args.merge_group):
        print("--carry-only is for comment-mode dry runs only", file=sys.stderr)
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
    for _ in range(SETTLE_ROUNDS):
        plans, seen, head_ok = comment_pass(args.repo, args.pr, trust, args.carry_only)
        render(args.repo, args.pr, plans, args.post)
        if not args.post or not head_ok or read_snapshot(args.repo, args.pr) == seen:
            break
        print("# the comments, head or checks changed while this run posted; deciding again")
    else:
        show(args.repo, StatusPlan(seen[0], "failure", UNSETTLED, ()), args.post)
        print(f"#{args.pr}: {UNSETTLED} after {SETTLE_ROUNDS} rounds; failed closed", file=sys.stderr)
        return 1
    if not head_ok:
        print(f"cannot read the head of #{args.pr}: its pp/score is unchanged; "
              "the merge_group run decides it again", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
