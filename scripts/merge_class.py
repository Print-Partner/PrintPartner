#!/usr/bin/env python3
"""Merge class of a PR, and the two-seat score record it needs.

classify: the most restrictive class of the changed paths. owner beats sensitive
beats lane. coverage: every CODEOWNERS entry must match an owner or sensitive
pattern, so a new owned path cannot slip past the classification unnoticed.
scores: whether two distinct seats posted a score of 9 or more on the exact head.
"""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLASSES = ROOT / "scripts" / "merge-classes.json"
CODEOWNERS = ROOT / ".github" / "CODEOWNERS"
SCORE = re.compile(r"^pp-score head=([0-9a-f]{40}) seat=(\S+) score=(\d+)$")
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
    return re.compile("^" + "".join(out) + "$")


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


def has_two_scores(comments, head):
    """True when two distinct seats posted score 9 or more on the exact head."""
    if not FULL_SHA.match(head):
        return False
    seats = set()
    for body in comments:
        for line in body.splitlines():
            m = SCORE.match(line.strip())
            if m and m.group(1) == head and int(m.group(3)) >= 9:
                seats.add(m.group(2))
    return len(seats) >= 2


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
