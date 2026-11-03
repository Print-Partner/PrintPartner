#!/usr/bin/env python3
"""Check the shared agent guide and its linked readiness documents, offline."""

from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
DOCUMENTS = (
    "AGENTS.md",
    ".factory/skills/pp-readiness/SKILL.md",
    ".factory/skills/pp-verify/SKILL.md",
    ".factory/skills/pp-mission-qa/SKILL.md",
    "docs/development/factory-missions.md",
    "docs/development/agent-workflow.md",
)
LINK = re.compile(r"\[[^\]]+\]\(([^)\s]+)\)")
HEADING = re.compile(r"^#{1,6}[ \t]+(.+?)[ \t#]*$")
FENCE = re.compile(r"^\s*(```|~~~)")


def anchors(markdown):
    """Return the heading anchors GitHub generates for a Markdown document."""
    found, seen, fenced = set(), {}, False
    for line in markdown.splitlines():
        if FENCE.match(line):
            fenced = not fenced
            continue
        heading = None if fenced else HEADING.match(line)
        if heading:
            slug = re.sub(r"[^\w\- ]", "", heading.group(1).lower()).replace(" ", "-")
            count = seen.get(slug, 0)
            seen[slug] = count + 1
            found.add(f"{slug}-{count}" if count else slug)
    return found


def check_document(root, relative):
    path = root / relative
    if not path.is_file():
        return [f"{relative}: missing document"]
    body = path.read_text(encoding="utf-8")
    errors = []
    if not body.startswith("# ") and path.name != "SKILL.md":
        errors.append(f"{relative}: missing title")
    for target in LINK.findall(body):
        url = urlsplit(target)
        if url.scheme or url.netloc:
            continue
        destination = (path.parent / unquote(url.path)).resolve() if url.path else path.resolve()
        if not destination.is_relative_to(root.resolve()) or not destination.exists():
            errors.append(f"{relative}: missing or outside-checkout link {target}")
        elif url.fragment and destination.suffix == ".md" and destination.is_file() \
                and unquote(url.fragment) not in anchors(destination.read_text(encoding="utf-8")):
            errors.append(f"{relative}: missing heading for link {target}")
    if path.name == "SKILL.md":
        parts = body.split("---\n", 2)
        if len(parts) != 3 or parts[0]:
            errors.append(f"{relative}: missing YAML frontmatter")
        else:
            # This skill intentionally uses only two plain, single-line fields.
            fields = dict(re.findall(r"^(name|description): (.+)$", parts[1], re.M))
            if fields.get("name") != path.parent.name:
                errors.append(f"{relative}: name must match skill directory")
            if not fields.get("description", "").strip():
                errors.append(f"{relative}: missing skill description")
    return errors


def check_trackability(root):
    result = subprocess.run(
        ["git", "check-ignore", "--no-index", "--", *DOCUMENTS],
        cwd=root, text=True, capture_output=True, check=False,
    )
    if result.returncode not in (0, 1):
        return ["git check-ignore failed: " + result.stderr.strip()]
    return [f"{name}: shared guidance must not be ignored"
            for name in result.stdout.splitlines()]


def main():
    errors = [error for relative in DOCUMENTS
              for error in check_document(ROOT, relative)]
    errors.extend(check_trackability(ROOT))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Agent guidance: {len(DOCUMENTS)} documents, local links, heading anchors, skill metadata, "
          "and Git trackability passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
