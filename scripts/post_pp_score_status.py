#!/usr/bin/env python3
"""Post a pp/score commit status from already-recorded PR score comments.

Reads trusted pp-score lines and plans one pp/score status per exact head SHA they name.
Never invents or re-derives a score. A record whose seat vendor equals its author= vendor
is a self-score and fails the head. A record must end with obstacles=, the scorer's disclosure
of any refusal, guard refusal or approval stall it hit; a line without it does not count.
author=cursor-auto or author=unknown requires trusted
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
import urllib.parse
from dataclasses import dataclass
from pathlib import Path

# obstacles= is required here: a record without it does not count. merge_class.py holds the vocabulary.
from merge_class import classify, load_classes, parse_obstacles, path_class

ROOT = Path(__file__).resolve().parent.parent
CLASSES = ROOT / "scripts" / "merge-classes.json"
DEFAULT_REPO = "Print-Partner/PrintPartner"
PASS_AT = 8
CONTEXT = "pp/score"
MAX_DESCRIPTION = 140
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
VENDOR = re.compile(r"^[a-z][a-z0-9-]*$")
# Not seat vendors. Cursor Auto and an unknown family need both an Anthropic
# and an OpenAI score on the exact head (Chad, 2026-11-16 11:23 PM ET).
DUAL_AUTHOR_FAMILIES = frozenset({"cursor-auto", "unknown"})
DUAL_REQUIRED_VENDORS = ("anthropic", "openai")
# GitHub names a merge queue entry's branch gh-readonly-queue/<base>/pr-<number>-<base sha>.
QUEUE_REF = re.compile(r"^refs/heads/gh-readonly-queue/.+/pr-([0-9]+)-([0-9a-f]{40})$")
# The queue squashes each PR into one commit whose title GitHub ends with "(#<number>)".
SQUASH_TITLE = re.compile(r"\(#([0-9]+)\)$")
MAIN = "main"
# A carry walks back at most this many merge-of-main hops.
CARRY_MAX_HOPS = 5
# Chad's rule says success. A skipped or neutral required check (path-filtered actionlint or
# zizmor) does not qualify the carried head, although GitHub lets it satisfy the required check.
PASSING = frozenset({"success"})
# Grok records never carry.
NO_CARRY_VENDORS = frozenset({"xai"})
# The score tooling. With every owner and sensitive path in merge-classes.json, these are the gate
# files: a merge of main that changes one never carries a score, because the gate changed under it.
SCORER_FILES = frozenset({
    "scripts/post_pp_score_status.py",
    "scripts/test_post_pp_score_status.py",
    "docs/development/score-records.md",
})


@dataclass(frozen=True)
class Record:
    head: str
    seat: str
    vendor: str
    author: str
    score: float
    obstacles: tuple[str, ...]


@dataclass(frozen=True)
class StatusPlan:
    head: str
    state: str
    description: str
    records: tuple[Record, ...]


class GateError(Exception):
    """A merge group cannot be verified; the status fails closed with this message."""


def score_pattern(vendors, author_names=None):
    if not isinstance(vendors, (list, tuple)) or not vendors or any(
        not isinstance(v, str) or not VENDOR.fullmatch(v) for v in vendors
    ):
        raise ValueError("invalid vendor list")
    extra = set(author_names or ())
    if any(not isinstance(v, str) or not VENDOR.fullmatch(v) for v in extra):
        raise ValueError("invalid author list")
    vendor_names = "|".join(re.escape(v) for v in sorted(set(vendors)))
    author_names = "|".join(re.escape(v) for v in sorted(set(vendors) | extra))
    return re.compile(
        rf"pp-score head=([0-9a-f]{{40}}) seat=(\S+) vendor=({vendor_names})"
        rf" score=([0-9]+) author=({author_names}) obstacles=(\S+)$"
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
        "pattern": score_pattern(vendors, DUAL_AUTHOR_FAMILIES),
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
    head, seat, vendor, score, author, token = match.groups()
    obstacles = parse_obstacles(token)
    if seats.get(seat) != vendor or obstacles is None:
        return None
    return Record(head, seat, vendor, author, float(score), obstacles)


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
    author = unique[0].author
    if author in DUAL_AUTHOR_FAMILIES:
        present = {r.vendor for r in unique}
        missing = [v for v in DUAL_REQUIRED_VENDORS if v not in present]
        if missing:
            return StatusPlan(head, "failure", _fit(
                f"{author} requires anthropic and openai scores; missing {', '.join(missing)}",
                f"{author}: missing a required family"), records)
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


class LocalGit:
    """The checkout the script runs in. Merges need full history so Git finds every merge base."""

    def __init__(self, cwd=ROOT, remote="origin"):
        self.cwd = Path(cwd)
        self.remote = remote

    def _git(self, *args, check=True, text=True):
        return subprocess.run(["git", *args], cwd=self.cwd, text=text, capture_output=True, check=check)

    def ensure(self, *shas: str) -> None:
        if self._git("rev-parse", "--is-shallow-repository").stdout.strip() == "true":
            # The workflow checkout is depth 1. Fetch the commits' full history without blobs
            # (Git fetches the blobs a merge or diff reads on demand), so every merge base is there.
            self._git("fetch", "--no-tags", "--quiet", "--unshallow", "--filter=blob:none", self.remote, *shas)
            return
        missing = [s for s in shas if self._git("cat-file", "-e", f"{s}^{{commit}}", check=False).returncode]
        if missing:
            self._git("fetch", "--no-tags", "--quiet", self.remote, *missing)

    def parents(self, sha: str) -> list[str]:
        return self._git("rev-list", "--parents", "-n1", sha).stdout.split()[1:]

    def tree(self, sha: str) -> str:
        return self._git("rev-parse", f"{sha}^{{tree}}").stdout.strip()

    def merge_bases(self, a: str, b: str) -> list[str]:
        done = self._git("merge-base", "--all", a, b, check=False)
        return done.stdout.split() if done.returncode == 0 else []

    def merge_tree(self, ours: str, theirs: str) -> str | None:
        """Tree of the clean merge of ours and theirs, or None when it conflicts.

        Plain `git merge-tree --write-tree`, so a criss-cross history merges from all its bases.
        """
        done = self._git("merge-tree", "--write-tree", ours, theirs, check=False)
        if done.returncode == 1:
            return None
        if done.returncode != 0:
            raise RuntimeError(f"git merge-tree failed: {done.stderr.strip()}")
        return done.stdout.splitlines()[0].strip()

    # Byte-exact and independent of local config: no external diff, textconv, rename pairing or
    # abbreviated blob IDs, so equal output means equal paths, modes and blobs on both sides.
    _DIFF = ("-c", "core.quotePath=true", "diff", "--no-ext-diff", "--no-textconv", "--no-renames",
             "--no-color", "--binary", "--full-index", "--src-prefix=a/", "--dst-prefix=b/")

    def diff(self, base: str, tip: str) -> bytes:
        return self._git(*self._DIFF, base, tip, "--", text=False).stdout

    def _paths(self, *args) -> set[str]:
        raw = self._git(*args, text=False).stdout
        return {p.decode("utf-8", "surrogateescape") for p in raw.split(b"\0") if p}

    def changed(self, base: str, tip: str) -> set[str]:
        """Paths that differ between base and tip, both sides of a rename."""
        return self._paths("diff", "--no-ext-diff", "--no-renames", "--name-only", "-z", base, tip, "--")

    def touched(self, pr_head: str, base: str, main: str) -> set[str]:
        """Paths that the main commits a merge brings in changed: every commit in pr_head..main
        (merges against each parent), plus the net diff base..main. A file that one commit
        changed and a later commit reverted still counts."""
        each = self._paths("log", "-m", "--no-renames", "--name-only", "-z", "--format=",
                           f"{pr_head}..{main}", "--")
        return each | self.changed(base, main)


class GitHubFacts:
    """What a carry needs to know. Commits, merges and diffs come from local git; GitHub answers
    whether a commit is on main (main's ruleset forbids force-pushes, so that never reverts) and
    which required checks passed on the carried head."""

    def __init__(self, repo: str, git: LocalGit | None = None):
        self.repo = repo
        self.git = git or LocalGit()
        self._required = None

    def on_main(self, sha: str) -> bool:
        data = gh_json(f"repos/{self.repo}/compare/{sha}...{MAIN}")
        return data.get("status") in ("identical", "ahead")

    def required_checks(self) -> list[dict]:
        """The required status checks of main's rulesets, except pp/score itself."""
        if self._required is None:
            rules = gh_json(f"repos/{self.repo}/rules/branches/{MAIN}")
            found = {}
            for rule in rules if isinstance(rules, list) else []:
                if rule.get("type") != "required_status_checks":
                    continue
                for check in (rule.get("parameters") or {}).get("required_status_checks") or []:
                    if check.get("context") and check["context"] != CONTEXT:
                        found[(check["context"], check.get("integration_id"))] = check
            if not found:
                raise GateError("could not read main's required checks")
            self._required = list(found.values())
        return self._required

    def checks_failing(self, sha: str) -> str | None:
        """None when every required check passed on sha; otherwise the first one that did not."""
        statuses = None
        for check in self.required_checks():
            name, app = check["context"], check.get("integration_id")
            query = f"check_name={urllib.parse.quote(name)}&filter=latest&per_page=100"
            if app:
                query += f"&app_id={app}"
            runs = gh_json(f"repos/{self.repo}/commits/{sha}/check-runs?{query}").get("check_runs") or []
            runs = [r for r in runs if r.get("name") == name]
            if runs:
                bad = [r for r in runs if r.get("status") != "completed" or r.get("conclusion") not in PASSING]
                if bad:
                    return f"{name} is {bad[0].get('conclusion') or bad[0].get('status')}"
                continue
            if app:
                return f"{name} has not run"
            if statuses is None:
                statuses = gh_json(f"repos/{self.repo}/commits/{sha}/status").get("statuses") or []
            state = next((st.get("state") for st in statuses if st.get("context") == name), None)
            if state != "success":
                return f"{name} is {state or 'missing'}"
        return None


@dataclass(frozen=True)
class Carry:
    source: str | None  # the earlier head whose records count, or None
    reason: str


def gate_file(path: str, classes) -> bool:
    return path in SCORER_FILES or path_class(path, classes) != "lane"


def hop(facts, head: str, classes) -> tuple[str | None, str]:
    """(P, why) when head is a merge of main into P that leaves the PR's code exactly as scored
    on P; (None, why) otherwise. Every check reads only commits named by SHA, so the answer for
    a hop can never change after a status is posted. All must hold:

    1. head has exactly two parents [P, M]: not a rebase, squash, force-push or octopus merge.
    2. M is on main.
    3. P and M have exactly one merge base B, and B is neither P nor M.
    4. `git merge-tree --write-tree P M` is clean and its tree is head's tree: no hand edits.
    5. The PR's diff B..P is byte-identical to its diff M..head (full blob IDs and modes).
    6. No commit in P..M changed a path that the PR changes, even if a later commit undid it.
    7. No commit in P..M changed a gate file: an owner or sensitive path, or the score tooling.
    """
    git = facts.git
    git.ensure(head)
    parents = git.parents(head)
    if len(parents) != 2:
        return None, f"{len(parents)} parents, not a two-parent merge of {MAIN}"
    previous, main = parents
    if not facts.on_main(main):
        return None, f"second parent {main[:7]} is not on {MAIN}"
    bases = git.merge_bases(previous, main)
    if len(bases) != 1:
        return None, f"{len(bases)} merge bases with {MAIN}"
    if bases[0] in (previous, main):
        return None, f"the merge brought no new {MAIN} commits" if bases[0] == main \
            else f"the PR head is already on {MAIN}"
    base = bases[0]
    merged = git.merge_tree(previous, main)
    if merged is None:
        return None, f"the merge of {MAIN} had conflicts"
    if merged != git.tree(head):
        return None, "the merge commit differs from a clean merge"
    if git.diff(base, previous) != git.diff(main, head):
        return None, "the PR's own diff changed"
    touched = git.touched(previous, base, main)
    overlap = sorted(touched & git.changed(base, previous))
    if overlap:
        return None, f"{MAIN} changed {overlap[0]}, which the PR changes"
    gate = sorted(p for p in touched if gate_file(p, classes))
    if gate:
        return None, f"{MAIN} changed gate file {gate[0]}"
    return previous, f"clean merge of {MAIN}"


def find_carry(head: str, scored: set[str], facts, max_hops: int = CARRY_MAX_HOPS) -> Carry:
    """Walk back from head through hops (see hop) to the nearest head with records.

    The first head with records stops the walk, so newer records always win, even failing ones.
    """
    classes = load_classes()
    current = head
    for _ in range(max_hops):
        previous, why = hop(facts, current, classes)
        if previous is None:
            return Carry(None, f"{current[:7]}: {why}")
        if previous in scored:
            return Carry(previous, why)
        current = previous
    return Carry(None, f"no scored head within {max_hops} merges of {MAIN}")


def plan_carry(head: str, records: list[Record], strict: bool, facts) -> tuple[StatusPlan | None, str]:
    """pp/score for a head with no records of its own, carried from an earlier head, or (None, why).

    Besides the hops: the source records must pass plan_head without Grok, every record on the
    PR must name one author family, a strict (owner or sensitive) PR needs every carried seat
    above 8, and every required check of main must have concluded success on head itself.
    """
    scored = {r.head for r in records}
    if not scored:
        return None, "no records on this PR"
    if head in scored:
        return None, f"{head[:7]} has its own records"
    families = sorted({r.author for r in records})
    if len(families) > 1:
        return None, f"records on this PR disagree on the author family ({', '.join(families)})"
    carry = find_carry(head, scored, facts)
    if carry.source is None:
        return None, carry.reason
    source = carry.source
    group = [r for r in records if r.head == source and r.vendor not in NO_CARRY_VENDORS]
    if not group:
        return None, f"{source[:7]} has no record that can carry"
    plan = plan_head(source, group)
    if plan.state != "success":
        return None, f"{source[:7]} did not pass: {plan.description}"
    low = [r for r in plan.records if r.score <= PASS_AT]
    if strict and low:
        return None, f"sensitive: {low[0].seat} scored {format_score(low[0].score)} on {source[:7]}"
    # Only the carried head's own checks count. A superseded hop never merges, and its checks
    # can be re-run after this status posts with nothing to re-run this workflow.
    failing = facts.checks_failing(head)
    if failing:
        return None, f"{head[:7]}: {failing}"
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


def carry_for_pr(repo: str, pr: int, head: str, records: list[Record], facts) -> tuple[StatusPlan | None, str]:
    """plan_carry for a live PR; any read failure means no carry."""
    try:
        files = fetch_files(repo, pr)
        return plan_carry(head, records, strict_class(files), facts)
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

    carried maps a PR to the plan_carry result for a head with no records of its own.
    """
    passed = []
    for pr in prs:
        head = pr_heads.get(pr)
        if not isinstance(head, str) or not FULL_SHA.fullmatch(head):
            return StatusPlan(head_sha, "failure", f"#{pr}: could not read its head SHA", ())
        group = [r for r in pr_records.get(pr, []) if r.head == head]
        plan = plan_head(head, group) if group else (carried or {}).get(pr, (None, ""))[0]
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
        records = {pr: trusted_records(fetch_comments(repo, pr), trust) for pr in prs}
        facts = GitHubFacts(repo)
        carried = {pr: carry_for_pr(repo, pr, heads[pr], records[pr], facts) for pr in prs
                   if isinstance(heads[pr], str) and FULL_SHA.fullmatch(heads[pr])
                   and not any(r.head == heads[pr] for r in records[pr])}
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
    records = trusted_records(fetch_comments(args.repo, args.pr), trust)
    plans = plan_statuses(records)
    try:
        head = fetch_head(args.repo, args.pr)
    except Exception as err:  # noqa: BLE001 - the recorded heads still post; only the carry is skipped
        head = None
        print(f"# no carry: cannot read the head of #{args.pr} ({type(err).__name__})")
    if args.carry_only:
        # Dry-run aid: decide the carry for the current head as if it had no records of its own.
        records = [r for r in records if r.head != head]
        plans = []
    if isinstance(head, str) and FULL_SHA.fullmatch(head) and not any(r.head == head for r in records):
        carried, why = carry_for_pr(args.repo, args.pr, head, records, GitHubFacts(args.repo))
        print(f"# carry to {head}: {why}")
        plans.append(carried or no_carry_plan(head, why))
    return render(args.repo, args.pr, plans, args.post)


if __name__ == "__main__":
    sys.exit(main())
