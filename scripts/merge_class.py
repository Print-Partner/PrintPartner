#!/usr/bin/env python3
"""Merge class of a PR, and the two-seat score record it needs.

classify: the most restrictive class of the changed paths. owner beats sensitive
beats lane. coverage: every CODEOWNERS entry must match an owner or sensitive
pattern, so a new owned path cannot slip past the classification unnoticed.
scores: whether two allowed seats from two different vendors, neither the PR author's vendor,
posted a score of 9 or more on the exact head.
"""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLASSES = ROOT / "scripts" / "merge-classes.json"
CODEOWNERS = ROOT / ".github" / "CODEOWNERS"
VENDOR = re.compile(r"^[a-z][a-z0-9-]*$")


def score_pattern(vendors):
    """The pp-score record pattern for the vendors listed in merge-classes.json."""
    if not isinstance(vendors, (list, tuple)) or not vendors or any(
            not isinstance(v, str) or not VENDOR.fullmatch(v) for v in vendors):
        raise ValueError("invalid vendor list")
    names = "|".join(re.escape(v) for v in sorted(set(vendors)))
    return re.compile(rf"pp-score head=([0-9a-f]{{40}}) seat=(\S+) vendor=({names})"
                      rf" score=([0-9]+) author=({names})")

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


def has_two_scores(comments, head, author_vendor, allowed_seats, vendors):
    """Require two allowed seats at 9+ from two different vendors, neither the author's vendor.

    allowed_seats maps each seat to its vendor and vendors lists every vendor a record may name
    (scorer_seats and vendors in merge-classes.json), so a seat or vendor change is data only. Malformed records, unknown seats and same-vendor-as-author records refuse.

    A score record is a line that starts with "pp-score "; every record must parse,
    including old ones. Other lines are ignored, including prose that mentions the
    token. Records for another head never count and are not checked against allowed_seats.
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
            if not line.strip().startswith("pp-score "):
                continue
            match = record_pattern.fullmatch(line.strip())
            if not match:
                return False
            record_head, seat, vendor, score, author = match.groups()
            # Older heads' records may name seats the owner has since dropped; they never count.
            if record_head != head:
                continue
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
