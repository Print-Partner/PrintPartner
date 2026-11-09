#!/usr/bin/env python3
"""Verify GRE-1309's exact receipt move against its fixed parent commit."""

from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
PARENT = "7908c70361dd22357704a9d9b38ead15e1b9c658"
SOURCE = "docs/desktop-rebuild.md"
SECTIONS = (
    ("D9 build graph receipt", "d9-build-graph"),
    ("D10a manifest model receipt", "d10a-manifest-model"),
    ("D10b manifest observation receipt", "d10b-manifest-observation"),
    ("D11 required units (GRE-606)", "d11-required-units"),
    ("GRE-644 manifest commands, slice 1", "manifest-commands"),
    ("D19 publication model and production links, GRE-612 PR 1", "d19-publication-model"),
    ("D19 publication transaction and job conflicts, GRE-612 PR 2", "d19-publication-transaction"),
    ("D19 publication client and writer admission, GRE-612 PR 3", "d19-publication-client"),
    ("Filament grams per send (GRE-1165, part a: GRE-1237)", "filament-grams-per-send"),
)
COPIED = (
    "docs/families.md", "scripts/check-family-index.py",
    "scripts/test_check_family_index.py", "rust/AGENTS.md",
    "rust/crates/pp-compat/AGENTS.md", "web/apps/server/AGENTS.md",
    "web/apps/web/AGENTS.md", "web/apps/ui/AGENTS.md",
)


def git_bytes(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT)


def parent_sections():
    original = git_bytes("show", f"{PARENT}:{SOURCE}")
    sections = []
    for title, slug in SECTIONS:
        heading = b"## " + title.encode() + b"\n"
        matches = list(re.finditer(b"(?m)^" + re.escape(heading), original))
        if len(matches) != 1:
            raise ValueError(f"Expected exactly one heading: {title}")
        start = matches[0].start()
        next_heading = re.search(b"(?m)^## ", original[matches[0].end():])
        end = matches[0].end() + next_heading.start() if next_heading else len(original)
        sections.append((title, slug, start, end, original[start:end]))
    return original, sections


def main():
    original, sections = parent_sections()
    expected = original
    total = 0
    for title, slug, start, end, section in sorted(sections, key=lambda item: item[2], reverse=True):
        path = ROOT / "docs" / "records" / f"{slug}.md"
        record = path.read_bytes()
        heading, separator, body = record.partition(b"\n")
        if not separator or heading != b"# " + title.encode():
            raise ValueError(f"Wrong record heading: {path}")
        if body != section.partition(b"\n")[2] or record != section[1:]:
            raise ValueError(f"Record differs from parent bytes: {path}")
        pointer = f"See [{title}](records/{slug}.md).\n".encode()
        expected = expected[:start] + pointer + expected[end:]
        total += section.count(b"\n")
    actual = (ROOT / SOURCE).read_bytes()
    if actual != expected:
        raise ValueError("Source differs beyond the nine exact pointer substitutions")
    for path in COPIED:
        if (ROOT / path).read_bytes() != git_bytes("show", f"6be50eae5:{path}"):
            raise ValueError(f"Copied file differs from 6be50eae5: {path}")
        delta = git_bytes("diff", "--numstat", PARENT, "--", path).decode().split()
        if delta and delta[1] != "0":
            raise ValueError(f"Copied file removes parent lines: {path}")
    for title, slug, _, _, section in sections:
        print(f"{title} | docs/records/{slug}.md | {section.count(b'\n')}")
    print(f"Moved lines (including headings and trailing blank lines): {total}")
    print(f"Plan expected 2189: {'MATCH' if total == 2189 else 'MISMATCH'}")
    print(f"{SOURCE}: {original.count(b'\n')} -> {actual.count(b'\n')} lines")
    print("PASS: nine record bodies match parent bytes; only heading levels changed.")
    print("PASS: nine pointer targets exist; all other source bytes are unchanged.")
    print("PASS: eight copied files match 6be50eae5 and only add parent lines.")


if __name__ == "__main__":
    main()
