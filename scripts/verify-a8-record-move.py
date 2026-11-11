#!/usr/bin/env python3
"""Verify the A8 receipt move (GRE-1317) and its exact link repairs against the main it was cut from."""

import importlib.util
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
# The main commit merged into this branch. The nine sections are byte-identical to the GRE-1309
# freeze (7908c7036); only text outside them changed.
PARENT = "4b359b5218a8af9baace940b29bf98f71dfeaf8e"
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
COPIED = ("docs/families.md",)


# Only these destinations may differ from the fixed snapshots. Labels, titles,
# whitespace and every byte outside the destination remain part of the comparison.
LINK_REWRITES = {
    "docs/records/d9-build-graph.md": {
        b"desktop-rebuild-d9-audit.md": b"../desktop-rebuild-d9-audit.md",
    },
    "docs/records/d11-required-units.md": {
        b"#d24-drafts-http-gre-619": b"d19-publication-client.md#d24-drafts-http-gre-619",
    },
    "docs/records/d19-publication-client.md": {
        b"domain-events.md": b"../domain-events.md",
        b"domain-events.md#http-stream": b"../domain-events.md#http-stream",
        b"draft-headers.md": b"../draft-headers.md",
        b"draft-headers.md#accept": b"../draft-headers.md#accept",
        b"draft-headers.md#order-authority-gre-1095": b"../draft-headers.md#order-authority-gre-1095",
        b"plan-autosave.md": b"../plan-autosave.md",
        b"problem-responses.md": b"../problem-responses.md",
    },
    "docs/records/filament-grams-per-send.md": {
        b"send-queue.md": b"../send-queue.md",
    },
    "docs/families.md": {
        b"desktop-rebuild.md#production-setup-in-rust-gre-1209":
            b"records/d19-publication-client.md#production-setup-in-rust-gre-1209",
    },
}
# Sentences that said "described below" of a section that now has its own record, and the one
# link label that named the moved anchor. Each exact replacement is applied once to the parent
# bytes before the link comparison.
TEXT_REWRITES = {
    "docs/families.md": ((b"[desktop-rebuild.md#production-setup-in-rust-gre-1209](",
        b"[records/d19-publication-client.md#production-setup-in-rust-gre-1209]("),),
    "docs/records/d9-build-graph.md": ((b"GRE-604 D10b, described below.",
        b"GRE-604 D10b, described in [D10b manifest observation receipt](d10b-manifest-observation.md)."),),
    "docs/records/d10b-manifest-observation.md": ((b"GRE-644 slice 1 wires its build graph manifest commands, described below.",
        b"GRE-644 slice 1 wires its build graph manifest commands, described in [GRE-644 manifest commands, slice 1](manifest-commands.md)."),),
    "docs/records/d19-publication-model.md": ((b"PR 2 adds the transaction entry described below.",
        b"PR 2 adds the transaction entry described in [D19 publication transaction and job conflicts, GRE-612 PR 2](d19-publication-transaction.md)."),
        (b"The transaction cases below also import",
         b"The transaction cases in [D19 publication transaction and job conflicts, GRE-612 PR 2](d19-publication-transaction.md) also import"),),
}
# The D19 client record holds the route sections desktop-rebuild.md used to carry (Production
# setup, Accepted Plate routes, D24 drafts HTTP and others), so the source lists its ### sections
# under that pointer.
INDEXED = ("d19-publication-client",)
# The repaired links all use bare destinations without titles or parentheses.
LINK_TARGET = re.compile(rb"(\[[^\]\n]*\]\()([^\s()]+)(\))")


def verify_bytes(path, original, actual):
    """Require the fixed original bytes with exactly the listed destination repairs."""
    rewrites = LINK_REWRITES.get(path, {})
    for before, after in TEXT_REWRITES.get(path, ()):
        if original.count(before) != 1:
            raise ValueError(f"Listed text rewrite not found exactly once: {path}")
        original = original.replace(before, after)
    expected = LINK_TARGET.sub(
        lambda match: match[1] + rewrites.get(match[2], match[2]) + match[3], original)
    if actual != expected:
        raise ValueError(f"Bytes differ beyond the allowed link-target repairs: {path}")


def heading_anchors(markdown):
    """Return (heading line, anchor) in order, by check-agent-guidance.py's anchor rules."""
    spec = importlib.util.spec_from_file_location("agent_guidance", ROOT / "scripts" / "check-agent-guidance.py")
    guidance = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(guidance)
    found, seen, ordered = set(), {}, []
    for line in guidance.prose(markdown, keep_code=True):
        found.update(guidance.HTML_ANCHOR.findall(guidance.CODE_SPAN.sub("", line)))
        heading = guidance.HEADING.match(line)
        if heading:
            slug = re.sub(r"[^\w\- ]", "", guidance.rendered_text(heading.group(1)).lower()).replace(" ", "-")
            count = seen.get(slug, 0)
            candidate = f"{slug}-{count}" if count else slug
            while candidate in found:
                count += 1
                candidate = f"{slug}-{count}"
            seen[slug] = count + 1
            found.add(candidate)
            ordered.append((line, candidate))
    return ordered


def section_index(slug, record):
    """The list of a record's ### sections that the source keeps under its pointer."""
    items = [f"- [{line[4:].strip()}](records/{slug}.md#{anchor})\n"
             for line, anchor in heading_anchors(record.decode()) if line.startswith("### ")]
    return ("\nIts sections:\n\n" + "".join(items) + "\n").encode()


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
        heading, separator, _ = record.partition(b"\n")
        if not separator or heading != b"# " + title.encode():
            raise ValueError(f"Wrong record heading: {path}")
        verify_bytes(path.relative_to(ROOT).as_posix(), section[1:], record)
        pointer = f"See [{title}](records/{slug}.md).\n".encode()
        if slug in INDEXED:
            pointer += section_index(slug, record)
        expected = expected[:start] + pointer + expected[end:]
        total += section.count(b"\n")
    actual = (ROOT / SOURCE).read_bytes()
    if actual != expected:
        raise ValueError("Source differs beyond the nine exact pointer substitutions")
    for path in COPIED:
        verify_bytes(path, git_bytes("show", f"{PARENT}:{path}"), (ROOT / path).read_bytes())
    newline = b"\n"
    for title, slug, _, _, section in sections:
        print(f"{title} | docs/records/{slug}.md | {section.count(newline)}")
    print(f"Moved lines (including headings and trailing blank lines): {total}")
    if total != 2258:
        raise ValueError(f"Moved lines {total} differ from the plan's 2258")
    print("Plan expected 2258: MATCH")
    print(f"{SOURCE}: {original.count(newline)} -> {actual.count(newline)} lines")
    print("PASS: nine records match parent bytes except heading levels and the listed text and link-target repairs.")
    print("PASS: nine pointer targets exist; the D19 client pointer lists its sections; all other source bytes are unchanged.")
    print("PASS: docs/families.md matches the parent except the listed label and link-target repair.")


if __name__ == "__main__":
    main()
