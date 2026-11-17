#!/usr/bin/env python3
"""Post a pp/score commit status from already-recorded PR score comments.

Reads trusted pp-score lines (and the older "Score: N/10 from <model> on head <sha>"
form) and plans one pp/score status per exact head SHA they name. Never invents or
re-derives a score. Default is a dry run that prints the gh api POST that would
be sent. Pass --post to apply.
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
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
LEGACY = re.compile(
    r"^Score: ([0-9]+(?:\.[0-9])?)/10 from (\S+) on head ([0-9a-f]{40})$"
)
VENDOR = re.compile(r"^[a-z][a-z0-9-]*$")


@dataclass(frozen=True)
class Record:
    head: str
    seat: str
    score: float
    source: str


@dataclass(frozen=True)
class StatusPlan:
    head: str
    state: str
    description: str
    records: tuple[Record, ...]


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


def load_trust(path=CLASSES):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    authors = data.get("scorer_authors") or ["poitee"]
    if not isinstance(authors, list) or not authors or any(
        not isinstance(a, str) or not a for a in authors
    ):
        raise ValueError("invalid scorer_authors")
    vendors = data.get("vendors") or []
    seats = data.get("scorer_seats") or {}
    if not isinstance(seats, dict) or any(
        not isinstance(seat, str) or not seat
        or not isinstance(vendor, str) or vendor not in vendors
        for seat, vendor in seats.items()
    ):
        raise ValueError("invalid scorer_seats")
    return {
        "authors": tuple(authors),
        "seats": seats,
        "pattern": score_pattern(vendors),
    }


def format_score(score: float) -> str:
    if score == int(score):
        return str(int(score))
    return str(score)


def parse_line(line: str, pattern, seats) -> Record | None:
    if line.startswith("pp-score "):
        match = pattern.fullmatch(line)
        if not match:
            return None
        head, seat, vendor, score, _author = match.groups()
        if seats.get(seat) != vendor:
            return None
        return Record(head, seat, float(score), "pp-score")
    match = LEGACY.fullmatch(line)
    if not match:
        return None
    score, model, head = match.groups()
    if model not in seats:
        return None
    return Record(head, model, float(score), "legacy")


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


def plan_statuses(records: list[Record]) -> list[StatusPlan]:
    by_head: dict[str, list[Record]] = {}
    for record in records:
        by_head.setdefault(record.head, []).append(record)
    plans = []
    for head, group in sorted(by_head.items()):
        # One status per SHA: the minimum recorded score is the conservative bar.
        # Repeated seat+score pairs from two comments count once in the description.
        unique = []
        seen = set()
        for record in group:
            key = (record.seat, record.score)
            if key in seen:
                continue
            seen.add(key)
            unique.append(record)
        minimum = min(r.score for r in unique)
        state = "success" if minimum >= PASS_AT else "failure"
        parts = [f"{format_score(r.score)}/10 from {r.seat}" for r in unique]
        description = "; ".join(parts)
        if len(description) > 140:
            description = f"{format_score(minimum)}/10 from {len(unique)} seats"
        plans.append(StatusPlan(head, state, description, tuple(unique)))
    return plans


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


def fetch_comments(repo: str, pr: int) -> list[dict]:
    raw = subprocess.check_output(
        ["gh", "api", "--paginate", f"repos/{repo}/issues/{pr}/comments"],
        text=True,
    )
    data = json.loads(raw)
    if not isinstance(data, list):
        raise RuntimeError(f"unexpected comments payload for {repo}#{pr}")
    return data


def apply_plan(repo: str, plan: StatusPlan) -> None:
    subprocess.check_call(gh_status_call(repo, plan), stdout=subprocess.DEVNULL)


def render(repo: str, pr: int, plans: list[StatusPlan], post: bool) -> int:
    if not plans:
        print(f"unscored: {repo}#{pr} has no trusted recorded score for any head SHA")
        return 0
    for plan in plans:
        print(format_call(repo, plan))
        seats = ", ".join(f"{r.seat} {format_score(r.score)} ({r.source})" for r in plan.records)
        print(f"# {plan.state} on {plan.head}: {seats}")
        if post:
            apply_plan(repo, plan)
            print(f"posted pp/score={plan.state} on {plan.head}")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=DEFAULT_REPO, help="owner/name (default: %(default)s)")
    parser.add_argument("--pr", type=int, required=True, help="pull request number")
    parser.add_argument(
        "--post",
        action="store_true",
        help="POST the statuses; default is a dry run that prints the API calls",
    )
    parser.add_argument(
        "--classes",
        default=str(CLASSES),
        help="merge-classes.json path (scorer_authors and vendors)",
    )
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        print("repo must be owner/name", file=sys.stderr)
        return 2
    if args.pr <= 0:
        print("pr must be a positive integer", file=sys.stderr)
        return 2
    trust = load_trust(Path(args.classes))
    comments = fetch_comments(args.repo, args.pr)
    plans = plan_statuses(trusted_records(comments, trust))
    return render(args.repo, args.pr, plans, args.post)


if __name__ == "__main__":
    sys.exit(main())
