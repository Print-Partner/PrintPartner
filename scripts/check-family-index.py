#!/usr/bin/env python3
"""Check family coverage and repository paths in docs/families.md."""

from collections import Counter
from pathlib import Path
import re
import sys
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
LINK = re.compile(r"\[[^\]]+\]\(([^)]+)\)")


def rows(text, columns):
    """Read a Markdown table, excluding its header and separator."""
    found = []
    for line in text.splitlines():
        if not line.startswith("| "):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if cells[0] == "Family" or re.fullmatch(r"[-: ]+", cells[0]):
            continue
        if len(cells) != columns:
            raise ValueError(f"expected {columns} columns: {line}")
        found.append(cells)
    return found


def check(root):
    root = Path(root).resolve()
    try:
        architecture = (root / "docs/architecture.md").read_text(encoding="utf-8")
        section = architecture.split("## Operation families and write ownership\n", 1)[1]
        section = section.split("\n## ", 1)[0]
        expected = Counter(row[0] for row in rows(section, 2))
        text = (root / "docs/families.md").read_text(encoding="utf-8")
        entries = rows(text, 6)
    except (OSError, UnicodeError, ValueError, IndexError) as error:
        return [f"Cannot read family tables: {error}"]
    errors = []
    actual = Counter(row[0] for row in entries)
    if not expected or actual != expected or any(n != 1 for n in actual.values()):
        errors.append("Family rows must match Architecture exactly, once per family")
    for row in entries:
        if any(not cell for cell in row):
            errors.append(f"{row[0]}: empty cell")
        for cell in (row[1], row[2], row[4]):
            if not LINK.search(cell) and not cell.startswith("None;"):
                errors.append(f"{row[0]}: path or doc must be a Markdown link: {cell}")
        if not LINK.search(row[4]):
            errors.append(f"{row[0]}: doc link required")
    # Check every link, including pointers outside the table. Paths are relative
    # to the index; fragments are checked by check-agent-guidance.py.
    for target in LINK.findall(text):
        url = urlsplit(target)
        if url.scheme or url.netloc:
            errors.append(f"Expected repository path, got {target}")
            continue
        path = (root / "docs" / unquote(url.path)).resolve()
        if not path.is_relative_to(root) or not path.exists():
            errors.append(f"Missing or outside-checkout path: {target}")
    return errors


def main():
    errors = check(ROOT)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("Family index: family rows and paths passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
