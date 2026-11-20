#!/usr/bin/env python3
"""Merge class of a PR, and the two-seat score record it needs.

classify: the most restrictive class of the changed paths. owner beats sensitive
beats lane. coverage: every CODEOWNERS entry must match an owner or sensitive
pattern, so a new owned path cannot slip past the classification unnoticed.
scores: whether two allowed seats from two different vendors, neither the PR author's vendor,
posted a score of 9 or more on the exact head.
carry: decide_carry, the score carry-forward decision behind the pp/score status. The
coordinator's sensitive merge check still needs two scores on the exact head (GRE-1859).
"""

import argparse
import json
import re
import subprocess
import sys
import urllib.parse
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLASSES = ROOT / "scripts" / "merge-classes.json"
CODEOWNERS = ROOT / ".github" / "CODEOWNERS"
VENDOR = re.compile(r"^[a-z][a-z0-9-]*$")
# What a scorer must disclose it hit while scoring (Chad, 2026-11-17 3:16 PM ET): a model refusal, an
# agent-guard refusal, or a tool approval that stalled. A record ends with obstacles=none, or with a
# comma-separated subset of these in any order, each at most once. A score that leaves out an obstacle
# it hit is invalid. No parser can see whether obstacles=none is true; the scorer brief carries that rule.
OBSTACLES = ("refusal", "guard", "approval-stall")


def parse_obstacles(token):
    """The obstacles a record discloses: () for none, None when the token is not valid."""
    if token == "none":
        return ()
    names = token.split(",")
    if any(name not in OBSTACLES for name in names) or len(set(names)) != len(names):
        return None
    return tuple(names)


def score_pattern(vendors):
    """The pp-score record pattern for the vendors listed in merge-classes.json.

    obstacles= is optional here so records written before it existed still parse; has_two_scores
    refuses a record on the current head that lacks it."""
    if not isinstance(vendors, (list, tuple)) or not vendors or any(
            not isinstance(v, str) or not VENDOR.fullmatch(v) for v in vendors):
        raise ValueError("invalid vendor list")
    names = "|".join(re.escape(v) for v in sorted(set(vendors)))
    return re.compile(rf"pp-score head=([0-9a-f]{{40}}) seat=(\S+) vendor=({names})"
                      rf" score=([0-9]+) author=({names})(?: obstacles=(\S+))?")

FULL_SHA = re.compile(r"^[0-9a-f]{40}$")


def glob_regex(pattern):
    """Compile a path pattern; ** crosses directories and * stays in one segment."""
    out, i = [], 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    # Case-insensitive: a case-insensitive checkout writes .Claude/x and .claude/x to the same file.
    return re.compile("^" + "".join(out) + "$", re.IGNORECASE)


def load_classes(path=CLASSES):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    return {
        "owner": [glob_regex(p) for p in data["owner"]],
        "sensitive": [glob_regex(p) for p in data["sensitive"]],
    }


def path_class(path, classes):
    if any(rx.match(path) for rx in classes["owner"]):
        return "owner"
    if any(rx.match(path) for rx in classes["sensitive"]):
        return "sensitive"
    return "lane"


def classify(paths, classes):
    """The class a PR gets: the most restrictive class among its paths."""
    found = {path_class(p, classes) for p in paths}
    for name in ("owner", "sensitive"):
        if name in found:
            return name
    return "lane"


def codeowners_entries(text):
    entries = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        entries.append(line.split()[0].lstrip("/"))
    return entries


def uncovered(entries, classes):
    """CODEOWNERS entries that match no owner or sensitive pattern."""
    covered = classes["owner"] + classes["sensitive"]
    return [e for e in entries if not any(rx.match(e) for rx in covered)]


def has_two_scores(comments, head, author_vendor, allowed_seats, vendors):
    """Require two allowed seats at 9+ from two different vendors, neither the author's vendor.

    allowed_seats maps each seat to its vendor and vendors lists every vendor a record may name
    (scorer_seats and vendors in merge-classes.json), so a seat or vendor change is data only. Malformed records, unknown seats and same-vendor-as-author records refuse.

    A score record is a line that starts with "pp-score " at column 0; every record must parse,
    including old ones. A record on the exact head must disclose obstacles=.
    Other lines are ignored, including prose that mentions the token. Records for another head never count and are not checked against allowed_seats.
    """
    try:
        record_pattern = score_pattern(vendors)
    except ValueError:
        return False
    if (not isinstance(head, str) or not FULL_SHA.fullmatch(head)
            or not isinstance(author_vendor, str) or author_vendor not in vendors
            or not isinstance(allowed_seats, dict) or not allowed_seats
            or any(not isinstance(seat, str) or not seat
                   or not isinstance(vendor, str) or vendor not in vendors
                   for seat, vendor in allowed_seats.items())
            or not isinstance(comments, (list, tuple))):
        return False
    seat_vendors = {}
    for body in comments:
        if not isinstance(body, str):
            return False
        for line in body.splitlines():
            if not line.startswith("pp-score "):
                continue
            match = record_pattern.fullmatch(line)
            if not match:
                return False
            record_head, seat, vendor, score, author, obstacles = match.groups()
            if obstacles is not None and parse_obstacles(obstacles) is None:
                return False
            # Older heads' records may name seats the owner has since dropped; they never count.
            if record_head != head:
                continue
            if obstacles is None:
                return False
            if allowed_seats.get(seat) != vendor or vendor == author:
                return False
            if author != author_vendor:
                return False
            try:
                if int(score) < 9:
                    return False
            except ValueError:
                return False
            seat_vendors[seat] = vendor
    return len(set(seat_vendors.values())) >= 2


# Score carry-forward (Chad, 2026-11-18 11:02 PM ET), used by the pp/score status
# (post_pp_score_status.plan_carry). docs/development/score-records.md lists the rule.
MAIN = "main"
STATUS_CONTEXT = "pp/score"
CARRY_MAX_HOPS = 5
# How far back main's first-parent history is searched for a merged-in commit.
MAIN_TIP_WALK = 5000
# Only success. A skipped or neutral required check does not qualify the carried head, although
# GitHub lets it satisfy the required check: the gate cannot prove why a check was skipped.
PASSING = frozenset({"success"})
# Grok records never count toward a carry; a failing one still fails it.
NO_CARRY_VENDORS = frozenset({"xai"})
# The score tooling. With every owner and sensitive path, these are the gate files: a merge of
# main that changes one never carries a score, because the gate changed under it.
SCORER_FILES = frozenset({
    "scripts/post_pp_score_status.py",
    "scripts/test_post_pp_score_status.py",
    "docs/development/score-records.md",
})


class CarryError(ValueError):
    """A fact the carry needs cannot be read; nothing carries."""


class LocalGit:
    """A checkout of the repository."""

    def __init__(self, cwd=ROOT, remote="origin"):
        self.cwd = Path(cwd)
        self.remote = remote

    def _git(self, *args, check=True, text=True):
        return subprocess.run(["git", *args], cwd=self.cwd, text=text, capture_output=True, check=check)

    def ensure(self, *shas):
        if self._git("rev-parse", "--is-shallow-repository").stdout.strip() == "true":
            # The workflow checkout is depth 1. Fetch the commits' full history without blobs
            # (Git fetches the blobs a merge or diff reads on demand), so every merge base is there.
            self._git("fetch", "--no-tags", "--quiet", "--unshallow", "--filter=blob:none", self.remote, *shas)
            return
        missing = [s for s in shas if self._git("cat-file", "-e", f"{s}^{{commit}}", check=False).returncode]
        if missing:
            self._git("fetch", "--no-tags", "--quiet", self.remote, *missing)

    def parents(self, sha):
        return self._git("rev-list", "--parents", "-n1", sha).stdout.split()[1:]

    def first_parents(self, tip, limit):
        return self._git("rev-list", "--first-parent", f"--max-count={limit}", tip).stdout.split()

    def tree(self, sha):
        return self._git("rev-parse", f"{sha}^{{tree}}").stdout.strip()

    def merge_bases(self, a, b):
        done = self._git("merge-base", "--all", a, b, check=False)
        return done.stdout.split() if done.returncode == 0 else []

    def merge_tree(self, ours, theirs):
        """Tree of the clean merge of ours and theirs, or None when it conflicts.

        Plain `git merge-tree --write-tree`, so a criss-cross history merges from all its bases.
        """
        done = self._git("merge-tree", "--write-tree", ours, theirs, check=False)
        if done.returncode == 1:
            return None
        if done.returncode != 0:
            raise CarryError(f"git merge-tree failed: {done.stderr.strip()}")
        return done.stdout.splitlines()[0].strip()

    # Byte-exact and independent of local config: no external diff, textconv, rename pairing or
    # abbreviated blob IDs, so equal output means equal paths, modes and blobs on both sides.
    _DIFF = ("-c", "core.quotePath=true", "diff", "--no-ext-diff", "--no-textconv", "--no-renames",
             "--no-color", "--binary", "--full-index", "--src-prefix=a/", "--dst-prefix=b/")

    def diff(self, base, tip):
        return self._git(*self._DIFF, base, tip, "--", text=False).stdout

    def _paths(self, *args):
        raw = self._git(*args, text=False).stdout
        return {p.decode("utf-8", "surrogateescape") for p in raw.split(b"\0") if p}

    def changed(self, base, tip):
        """Paths that differ between base and tip, both sides of a rename."""
        return self._paths("diff", "--no-ext-diff", "--no-renames", "--name-only", "-z", base, tip, "--")

    def touched(self, pr_head, base, main):
        """Paths that the main commits a merge brings in changed: every commit in pr_head..main
        (merges against each parent), plus the net diff base..main. A file that one commit
        changed and a later commit reverted still counts."""
        each = self._paths("log", "-m", "--no-renames", "--name-only", "-z", "--format=",
                           f"{pr_head}..{main}", "--")
        return each | self.changed(base, main)


class GitHubFacts:
    """What a carry needs to know. Commits, merges and diffs come from local git. GitHub, through
    api(path) -> parsed JSON, answers where main's tip is and which required checks passed on the
    carried head."""

    def __init__(self, repo, api, git=None):
        self.repo = repo
        self.api = api
        self.git = git or LocalGit()
        self._required = None

    def on_main(self, sha):
        """Whether main's tip was sha at some time: sha is on the first-parent history of main's
        current tip, within MAIN_TIP_WALK commits. Reachable from main is not enough: a side
        branch's inner commit, or one merged with `-s ours`, is reachable but its content need not
        be in main. Main's ruleset forbids force-pushes, so a yes turns to no only past the bound."""
        tip = ((self.api(f"repos/{self.repo}/branches/{MAIN}") or {}).get("commit") or {}).get("sha")
        if not isinstance(tip, str) or not FULL_SHA.fullmatch(tip):
            raise CarryError("could not read main's tip")
        self.git.ensure(tip)
        return sha in self.git.first_parents(tip, MAIN_TIP_WALK)

    def required_checks(self):
        """The required status checks of main's rulesets, except pp/score itself."""
        if self._required is None:
            rules = self.api(f"repos/{self.repo}/rules/branches/{MAIN}")
            found = {}
            for rule in rules if isinstance(rules, list) else []:
                if rule.get("type") != "required_status_checks":
                    continue
                for check in (rule.get("parameters") or {}).get("required_status_checks") or []:
                    if check.get("context") and check["context"] != STATUS_CONTEXT:
                        found[(check["context"], check.get("integration_id"))] = check
            if not found:
                raise CarryError("could not read main's required checks")
            self._required = list(found.values())
        return self._required

    def checks_failing(self, sha):
        """None when every required check concluded success on sha; otherwise the first that did not."""
        statuses = None
        for check in self.required_checks():
            name, app = check["context"], check.get("integration_id")
            query = f"check_name={urllib.parse.quote(name)}&filter=latest&per_page=100"
            if app:
                query += f"&app_id={app}"
            runs = self.api(f"repos/{self.repo}/commits/{sha}/check-runs?{query}").get("check_runs") or []
            runs = [r for r in runs if r.get("name") == name]
            if runs:
                bad = [r for r in runs if r.get("status") != "completed" or r.get("conclusion") not in PASSING]
                if bad:
                    return f"{name} is {bad[0].get('conclusion') or bad[0].get('status')}"
                continue
            if app:
                return f"{name} has not run"
            if statuses is None:
                statuses = self.api(f"repos/{self.repo}/commits/{sha}/status").get("statuses") or []
            state = next((st.get("state") for st in statuses if st.get("context") == name), None)
            if state != "success":
                return f"{name} is {state or 'missing'}"
        return None


@dataclass(frozen=True)
class Carry:
    source: object  # the earlier head whose records count, or None
    reason: str


def gate_file(path, classes):
    return path in SCORER_FILES or path_class(path, classes) != "lane"


def hop(facts, head, classes):
    """(P, why) when head is a merge of main into P that leaves the PR's code exactly as scored
    on P; (None, why) otherwise. Every check reads only commits named by SHA, so the answer for
    a hop can never change after a status is posted. All must hold:

    1. head has exactly two parents [P, M]: not a rebase, squash, force-push or octopus merge.
    2. M was main's tip at some time (on main's first-parent history), not merely reachable.
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
        return None, f"second parent {main[:7]} was never {MAIN}'s tip"
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


def check_state(facts, head):
    """One read of head's required checks: None when every one concluded success, otherwise why
    not ("unreadable: <error>" when it cannot be read). A carry decides from this one value."""
    try:
        return facts.checks_failing(head)
    except Exception as err:  # noqa: BLE001 - an unreadable check never passes
        return f"unreadable: {type(err).__name__}"


def decide_carry(head, scored_heads, author_families, facts, classes, head_checks, max_hops=CARRY_MAX_HOPS):
    """Carry(source, why): the earlier head whose records count for head, or Carry(None, why).

    head must have no records of its own, every record on the PR must name one author family,
    each hop back to source must pass hop(), the walk stops at the first head with records (so
    newer records win, even failing ones) and goes back at most max_hops, and every required
    check of main must have concluded success on head itself. head_checks is the caller's one
    check_state read of them; decide_carry never reads them again, so the status run records
    exactly the value it decided from. Superseded hops' checks are not read: those heads never
    merge, and their checks can change with nothing to re-run this.
    Whether source's records pass is the caller's record rule (post_pp_score_status.plan_carry).
    """
    scored = set(scored_heads)
    if not scored:
        return Carry(None, "no records on this PR")
    if head in scored:
        return Carry(None, f"{head[:7]} has its own records")
    families = sorted(set(author_families))
    if len(families) != 1:
        return Carry(None, f"records on this PR disagree on the author family ({', '.join(families)})")
    current = head
    for _ in range(max_hops):
        previous, why = hop(facts, current, classes)
        if previous is None:
            return Carry(None, f"{current[:7]}: {why}")
        if previous in scored:
            if head_checks:
                return Carry(None, f"{head[:7]}: {head_checks}")
            return Carry(previous, why)
        current = previous
    return Carry(None, f"no scored head within {max_hops} merges of {MAIN}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("classify", help="print the class of the paths on stdin, one per line")
    c.set_defaults(func=lambda a: print(classify([l.strip() for l in sys.stdin if l.strip()], load_classes())))
    v = sub.add_parser("coverage", help="fail when a CODEOWNERS entry is unclassified")

    def run_coverage(_args):
        missing = uncovered(codeowners_entries(CODEOWNERS.read_text(encoding="utf-8")), load_classes())
        for entry in missing:
            print(f"unclassified CODEOWNERS entry: {entry}")
        return 1 if missing else 0

    v.set_defaults(func=run_coverage)
    args = parser.parse_args(argv)
    result = args.func(args)
    return result if isinstance(result, int) else 0


if __name__ == "__main__":
    sys.exit(main())
