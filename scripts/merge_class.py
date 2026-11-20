#!/usr/bin/env python3
"""Merge class of a PR, and the two-seat score record it needs.

classify: the most restrictive class of the changed paths. owner beats sensitive
beats lane. coverage: every CODEOWNERS entry must match an owner or sensitive
pattern, so a new owned path cannot slip past the classification unnoticed.
records: read_comments, the one reader that turns raw PR comments into trusted score records,
for both evaluators: the pp/score status (post_pp_score_status.py) and the coordinator's
sensitive merge check (sensitive_scores, called from agent-guard.py).
scores: whether two allowed seats from two different vendors, neither the PR author's vendor,
posted a score of 9 or more on the exact head, or on the head a carry reaches.
carry: decide_carry and carry_source, the one score carry-forward decision both evaluators use.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
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


def score_pattern(vendors, author_names=()):
    """The pp-score record pattern for the vendors listed in merge-classes.json; author= also takes
    author_names. obstacles= is optional here so records written before it existed still parse;
    read_comments makes a record without it a fault of its head."""
    names = set(author_names)
    if not isinstance(vendors, (list, tuple)) or not vendors or any(
            not isinstance(v, str) or not VENDOR.fullmatch(v) for v in [*vendors, *names]):
        raise ValueError("invalid vendor list")
    vendor_names = "|".join(re.escape(v) for v in sorted(set(vendors)))
    authors = "|".join(re.escape(v) for v in sorted(set(vendors) | names))
    return re.compile(rf"pp-score head=([0-9a-f]{{40}}) seat=(\S+) vendor=({vendor_names})"
                      rf" score=([0-9]+) author=({authors})(?: obstacles=(\S+))?")

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


# Not seat vendors. For pp/score, Cursor Auto and an unknown family need both an Anthropic and an
# OpenAI score on the exact head (Chad, 2026-11-16 11:23 PM ET). The sensitive merge check refuses them.
DUAL_AUTHOR_FAMILIES = frozenset({"cursor-auto", "unknown"})
RECORD = "pp-score "
# A record below PASS_AT fails its head for both evaluators: the pp/score status passes at PASS_AT,
# and the sensitive merge check needs SENSITIVE_MIN.
PASS_AT = 8
SENSITIVE_MIN = 9
NEED_TWO = "need two qualifying scorer seats from allowed authors"


def _seat_map(raw, vendors, key):
    if not isinstance(raw, dict) or any(
            not isinstance(seat, str) or not seat or not isinstance(vendor, str) or vendor not in vendors
            for seat, vendor in raw.items()):
        raise ValueError(f"invalid {key}")
    return raw


def load_trust(path=CLASSES):
    """The trust both evaluators read records with: scorer_authors, every seat a record may name
    (scorer_seats plus pp_score_seats, which must agree), and the record pattern."""
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    authors = data.get("scorer_authors") or ["poitee"]
    if not isinstance(authors, list) or not authors or any(not isinstance(a, str) or not a for a in authors):
        raise ValueError("invalid scorer_authors")
    vendors = data.get("vendors") or []
    seats = dict(_seat_map(data.get("scorer_seats") or {}, vendors, "scorer_seats"))
    # pp_score_seats count for the pp/score status only; sensitive_fault reads scorer_seats.
    for seat, vendor in _seat_map(data.get("pp_score_seats") or {}, vendors, "pp_score_seats").items():
        if seats.setdefault(seat, vendor) != vendor:
            raise ValueError(f"pp_score_seats disagrees with scorer_seats on {seat}")
    return {"authors": tuple(authors), "seats": seats, "pattern": score_pattern(vendors, DUAL_AUTHOR_FAMILIES)}


@dataclass(frozen=True)
class Record:
    head: str
    seat: str
    vendor: str
    author: str
    score: int
    obstacles: tuple
    # The line is the record alone, at column 0. A record in backticks, indented or in a fence is
    # read and checked like any other, so a low or invalid one refuses, but it never counts toward a pass.
    bare: bool = True


@dataclass(frozen=True)
class Reading:
    """What read_comments found. faults are (head, reason): head None refuses every head; a fault
    with a head refuses that head and marks it as scored, so a carry walk stops there."""
    records: tuple = ()
    faults: tuple = ()
    authors: frozenset = frozenset()  # every author= that a parsed record line names

    def heads(self):
        return {r.head for r in self.records} | {head for head, _ in self.faults if head}

    def on(self, head):
        return [r for r in self.records if r.head == head]

    def without(self, head):
        return Reading(tuple(r for r in self.records if r.head != head),
                       tuple(f for f in self.faults if f[0] != head), self.authors)


def unwrap(line):
    """line without surrounding whitespace and inline-code backticks. Fence lines (```text) never
    start with "pp-score ", so the record lines inside a fence read as wrapped records."""
    text = line.strip()
    while len(text) >= 2 and text[0] == "`" and text[-1] == "`":
        text = text[1:-1].strip()
    return text


def read_comments(comments, trust):
    """The one reader of score records, for both evaluators: raw GitHub issue comments to a Reading.

    Only comments by trust["authors"] count. A record line is one that starts with "pp-score " once
    surrounding whitespace and backticks are dropped; prose that mentions the token is ignored. A
    record line at column 0 must be the record alone, and every record line must match the pattern,
    with a whole-number score and valid obstacles=, or it refuses every head. A parsed record
    refuses its own head when its comment was edited or has no readable edit times, when it has no
    obstacles=, or when its seat is not in trust["seats"] with that vendor. A comment without a
    readable author or body refuses every head."""
    if not isinstance(comments, (list, tuple)):
        return Reading(faults=((None, "the PR comments are unreadable"),))
    records, faults, authors = [], [], set()
    for comment in comments:
        user = comment.get("user") if isinstance(comment, dict) else None
        login = user.get("login") if isinstance(user, dict) else None
        body = comment.get("body") if isinstance(comment, dict) else None
        if not isinstance(login, str) or not login or not isinstance(body, str):
            faults.append((None, "a PR comment has no readable author or body"))
            continue
        if login not in trust["authors"]:
            continue
        created, updated = comment.get("created_at"), comment.get("updated_at")
        for line in body.splitlines():
            text = unwrap(line)
            if not text.startswith(RECORD):
                continue
            match = trust["pattern"].fullmatch(text) if line == text or not line.startswith(RECORD) else None
            token = match and match.group(6)
            obstacles = None if token is None else parse_obstacles(token)
            if not match or (token is not None and obstacles is None) or len(match.group(4)) > 2:
                faults.append((None, "a malformed pp-score record"))
                continue
            head, seat, vendor, score, author, _ = match.groups()
            authors.add(author)
            if not isinstance(created, str) or not created or not isinstance(updated, str) or not updated:
                faults.append((head, f"the {seat} record's edit time is unreadable"))
            elif created != updated:
                faults.append((head, f"the {seat} record was edited after it was posted"))
            elif obstacles is None:
                faults.append((head, f"the {seat} record has no obstacles="))
            elif trust["seats"].get(seat) != vendor:
                faults.append((head, f"{seat} is not an allowed seat for vendor={vendor}"))
            else:
                records.append(Record(head, seat, vendor, author, int(score), obstacles, line == text))
    return Reading(tuple(records), tuple(faults), frozenset(authors))


def head_fault(reading, head):
    """Why head's records refuse it under the rules both evaluators share, or None: a fault on the
    PR or on head, only wrapped records, a seat from the author's own family, records that
    disagree on author=, or a record below PASS_AT (bare or wrapped). Both evaluators report this
    one string, so a head both refuse is refused for the same reason."""
    fault = next((why for at, why in reading.faults if at in (None, head)), None)
    if fault:
        return fault
    group = reading.on(head)
    if group and not any(r.bare for r in group):
        return "a wrapped or indented record never counts"
    own = next((r for r in group if r.vendor == r.author), None)
    if own:
        return f"{own.seat} is the author's own family ({own.vendor})"
    if len({r.author for r in group}) > 1:
        return "records disagree on the author family"
    low = next((r for r in group if r.score < PASS_AT), None)
    if low:
        return f"{low.seat} scored {low.score}"
    return None


def sensitive_fault(reading, head, seats, vendors, uncounted=()):
    """Why head's records do not let a sensitive PR merge, or None. On top of head_fault: every
    author= on the PR is a seat vendor, every record on head is a seat in seats (scorer_seats) at
    9 or more, and bare records from two vendors outside uncounted remain."""
    fault = head_fault(reading, head)
    if fault:
        return fault
    other = sorted(reading.authors - set(vendors))
    if other:
        return f"author={other[0]} is not a seat vendor"
    group = reading.on(head)
    for r in group:
        if seats.get(r.seat) != r.vendor:
            return f"{r.seat} is not a sensitive-merge seat"
        if r.score < SENSITIVE_MIN:
            return f"{r.seat} scored {r.score}"
    if len({r.vendor for r in group if r.bare and r.vendor not in uncounted}) < 2:
        return NEED_TWO
    return None


def has_two_scores(comments, head, author_vendor, allowed_seats, vendors):
    """sensitive_fault on the exact head for trusted, unedited comment bodies, where every record on
    head must name author_vendor. allowed_seats is scorer_seats and vendors lists every vendor a
    record may name, so a seat or vendor change is data only. False for invalid arguments."""
    try:
        pattern = score_pattern(vendors)
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
    stamp = "unedited"
    reading = read_comments([{"user": {"login": "scorer"}, "body": body, "created_at": stamp, "updated_at": stamp}
                             for body in comments],
                            {"authors": ("scorer",), "seats": allowed_seats, "pattern": pattern})
    if any(r.author != author_vendor for r in reading.on(head)):
        return False
    return sensitive_fault(reading, head, allowed_seats, vendors) is None


# Score carry-forward (Chad, 2026-11-18 11:02 PM ET). One decision for the pp/score status
# (post_pp_score_status.plan_carry) and the coordinator's sensitive merge check (sensitive_scores):
# both reach it only through carry_source. docs/development/score-records.md lists the rule.
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
    """A checkout of the repository. deadline (a time.monotonic value) bounds every git call, so the
    coordinator hook refuses on its own before its timeout would let the merge run.

    Every lane worktree shares the coordinator clone's .git directory, and a lane can set HOME,
    XDG_CONFIG_HOME or GIT_* variables. So the commands a carry decides from run in a scratch git
    directory that this class creates under TMPDIR and leaves in place. Before every command it
    rewrites the scratch directory's config, HEAD and alternates file, so no earlier command can
    have changed them. The command gets no GIT_* variable but the ones set here, no system, global
    or XDG configuration, no refs, attributes, grafts, commit-graph or multi-pack-index, and no
    remote, so it fetches nothing and runs no program named by the environment or a config. It reads
    the clone's objects through objects/info/alternates, and commits() checks every commit it
    trusts against its name. Only ensure() runs in the clone, to fetch whole commits by SHA."""

    # Kept for the fetch in the clone only, so it can authenticate and reach the remote.
    TRANSPORT_ENV = ("GIT_ASKPASS", "GIT_SSH", "GIT_SSH_COMMAND", "GIT_SSH_VARIANT", "GIT_TERMINAL_PROMPT",
                     "GIT_SSL_", "GIT_HTTP_", "GIT_PROXY_", "GIT_CURL_")
    CONFIG = ("[core]\n\trepositoryformatversion = 0\n\tbare = true\n\tcommitGraph = false\n"
              f"\tmultiPackIndex = false\n\tattributesFile = {os.devnull}\n")

    def __init__(self, cwd=ROOT, remote="origin", deadline=None):
        self.cwd = Path(cwd)
        self.remote = remote
        self.deadline = deadline
        self._scratch = None

    def _run(self, args, cwd, env, keep=(), check=True, text=True, stdin=None):
        timeout = None if self.deadline is None else self.deadline - time.monotonic()
        if timeout is not None and timeout <= 0:
            raise subprocess.TimeoutExpired(["git", *args], 0)
        base = {k: v for k, v in os.environ.items() if not k.startswith("GIT_") or k.startswith(keep)}
        return subprocess.run(["git", "--no-pager", *args], cwd=cwd, text=text, capture_output=True, check=check,
                              timeout=timeout, env={**base, **env}, input=stdin)

    def _in_clone(self, *args, check=True):
        """The fetch side. It reads the clone's configuration and runs its transport, which can only
        change where whole commits come from; Git checks every fetched object against its hash."""
        return self._run(args, self.cwd, {}, keep=self.TRANSPORT_ENV, check=check)

    def _git(self, *args, check=True, text=True, stdin=None):
        """A read-only command in the scratch directory, with no configuration but its own."""
        if self._scratch is None:
            common = self._in_clone("rev-parse", "--path-format=absolute", "--git-common-dir").stdout.strip()
            self._scratch = (Path(tempfile.mkdtemp(prefix="pp-carry-git-")), f"{common}/objects\n")
            (self._scratch[0] / "objects" / "info").mkdir(parents=True)
            (self._scratch[0] / "refs").mkdir()
        scratch, alternates = self._scratch
        for name, content in (("config", self.CONFIG), ("HEAD", "ref: refs/heads/none\n"),
                              ("objects/info/alternates", alternates)):
            (scratch / name).write_text(content, encoding="utf-8")
        return self._run(["--no-replace-objects", *args], scratch, {
            "GIT_DIR": str(scratch), "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_ATTR_NOSYSTEM": "1", "GIT_GRAFT_FILE": os.devnull, "GIT_NO_LAZY_FETCH": "1",
            "HOME": str(scratch), "XDG_CONFIG_HOME": str(scratch)}, check=check, text=text, stdin=stdin)

    def ensure(self, *shas):
        if self._in_clone("rev-parse", "--is-shallow-repository").stdout.strip() == "true":
            # The workflow checkout is depth 1. Fetch the full history, blobs included, in the clone,
            # because the scratch directory never fetches.
            self._in_clone("fetch", "--no-tags", "--quiet", "--unshallow", self.remote, *shas)
            return
        missing = [s for s in shas if self._in_clone("cat-file", "-e", f"{s}^{{commit}}", check=False).returncode]
        if missing:
            self._in_clone("fetch", "--no-tags", "--quiet", self.remote, *missing)

    def commits(self, shas):
        """{sha: (tree, parents)} read with one cat-file --batch. Each commit's bytes must hash to its
        name, so a forged loose object, pack index or alternate cannot change it (trees and blobs
        are not checked: that would be a full fsck)."""
        out = self._git("cat-file", "--batch", text=False, stdin="".join(f"{s}\n" for s in shas).encode()).stdout
        found, at = {}, 0
        for sha in shas:
            end = out.index(b"\n", at)
            header = out[at:end].split()
            size = int(header[2]) if len(header) == 3 and header[1] == b"commit" else -1
            body, at = out[end + 1:end + 1 + size], end + 2 + size
            if size < 0 or hashlib.sha1(b"commit %d\0" % size + body).hexdigest() != sha:
                raise CarryError("object store failed verification")
            fields = [line.split(b" ", 1) for line in body.split(b"\n\n", 1)[0].split(b"\n")]
            found[sha] = (next(v.decode() for k, v in fields if k == b"tree"),
                          [v.decode() for k, v in fields if k == b"parent"])
        return found

    def parents(self, sha):
        return self.commits([sha])[sha][1]

    def first_parents(self, tip, limit):
        """main's first-parent chain from tip, each link checked against the verified commits."""
        chain = self._git("rev-list", "--first-parent", f"--max-count={limit}", tip).stdout.split()
        found = self.commits(chain)
        links = [found[sha][1][:1] for sha in chain]
        if not chain or chain[0] != tip or any(links[n] != [chain[n + 1]] for n in range(len(chain) - 1)) \
                or (len(chain) < limit and links[-1]):
            raise CarryError("object store failed verification")
        return chain

    def tree(self, sha):
        return self.commits([sha])[sha][0]

    def merge_bases(self, a, b):
        done = self._git("merge-base", "--all", a, b, check=False)
        bases = done.stdout.split() if done.returncode == 0 else []
        self.commits(bases)
        return bases

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

    # Every path in the repository, whatever the cwd: no external diff, textconv, rename pairing,
    # hidden submodule change or color.
    _WHOLE = ("--no-relative", "--no-ext-diff", "--no-textconv", "--no-renames", "--ignore-submodules=none",
              "--no-color")
    # Byte-exact: full blob IDs on every file, so equal output means equal paths, modes and blobs.
    _DIFF = ("diff", *_WHOLE, "--binary", "--full-index", "--src-prefix=a/", "--dst-prefix=b/")

    def diff(self, base, tip):
        return self._git(*self._DIFF, base, tip, "--", text=False).stdout

    def _paths(self, *args):
        raw = self._git(*args, text=False).stdout
        return {p.decode("utf-8", "surrogateescape") for p in raw.split(b"\0") if p}

    def changed(self, base, tip):
        """Paths that differ between base and tip, both sides of a rename."""
        return self._paths("diff", *self._WHOLE, "--name-only", "-z", base, tip, "--")

    def touched(self, pr_head, base, main):
        """Paths that the main commits a merge brings in changed: every commit in pr_head..main
        (merges against each parent), plus the net diff base..main. A file that one commit
        changed and a later commit reverted still counts."""
        each = self._paths("log", *self._WHOLE, "--no-show-signature", "-m", "--name-only", "-z", "--format=",
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
    Callers reach this only through carry_source.
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


def carry_source(reading, head, facts, classes, head_checks):
    """(source, why) for a head with no records of its own, or (None, why). A fault on the PR
    refuses; then decide_carry on the Reading's heads (faulted heads count as scored) and author
    families; then head_fault on the source. Both evaluators carry only through this, so they reach
    the same source or refuse for the same reason. Each then applies its own bar to the source."""
    fault = next((why for at, why in reading.faults if at is None), None)
    if fault:
        return None, fault
    carry = decide_carry(head, reading.heads(), reading.authors, facts, classes, head_checks)
    if carry.source is None:
        return None, carry.reason
    fault = head_fault(reading, carry.source)
    if fault:
        return None, f"{carry.source[:7]}: {fault}"
    return carry.source, carry.reason


def sensitive_scores(reading, head, seats, vendors, facts, classes):
    """The coordinator's sensitive merge check: (records_head, why), records_head None to refuse.

    When head has records or faults of its own, sensitive_fault on head, as before carries. Otherwise
    the source carry_source reaches, deciding from one check_state read of head, must pass
    sensitive_fault with Grok records checked but not counted toward the two vendors. seats is
    scorer_seats. The pp/score status carries from the same source; it also passes one seat, which
    this check never does. Raises when a fact cannot be read; the guard then refuses."""
    if head in reading.heads():
        fault = sensitive_fault(reading, head, seats, vendors)
        return (None, fault) if fault else (head, "two qualifying seats on this head")
    source, why = carry_source(reading, head, facts, classes, check_state(facts, head))
    if source is None:
        return None, why
    fault = sensitive_fault(reading, source, seats, vendors, NO_CARRY_VENDORS)
    return (None, f"{source[:7]}: {fault}") if fault else (source, f"carried from {source[:7]}")


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
