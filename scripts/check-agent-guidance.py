#!/usr/bin/env python3
"""Check local links and heading anchors in every tracked Markdown page, offline.

Link validation runs over every page `git ls-files '*.md'` lists. Title and skill-metadata
rules apply only to the shared agent guidance in DOCUMENTS.
"""

from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
DOCUMENTS = (
    "AGENTS.md",
    ".claude/skills/pp-readiness/SKILL.md",
    ".claude/skills/pp-verify/SKILL.md",
    ".claude/skills/pp-mission-qa/SKILL.md",
    ".claude/skills/pp-lane/SKILL.md",
    "docs/development/agent-workflow.md",
)
# An inline link or image destination: bare (one level of parentheses allowed) or in angle
# brackets, with an optional title.
LINK = re.compile(r"\[[^\]]*\]\(\s*(<[^>\n]*>|(?:[^()\s]|\([^()\s]*\))+)(?:\s+(?:\"[^\"]*\"|'[^']*'))?\s*\)")
# A reference definition, such as `[guide]: docs/guide.md#setup`.
REFERENCE = re.compile(r"^ {0,3}\[[^\]]+\]:\s*(<[^>\n]*>|\S+)")
HEADING = re.compile(r"^#{1,6}[ \t]+(.+?)[ \t#]*$")
FENCE = re.compile(r"^[ \t]*(`{3,}|~{3,})(.*)$")
INLINE_LINK = re.compile(r"\[([^\]]*)\](?:\([^)]*\)|\[[^\]]*\])")
CODE_SPAN = re.compile(r"(`+)(.+?)\1")
HTML_TAG = re.compile(r"</?[A-Za-z][^>]*>")
EMPHASIS = re.compile(r"(?<![\w\\])(\*{1,3}|_{1,3})(?=\S)(.+?)(?<=\S)\1(?!\w)")
# An explicit HTML anchor, such as `<a id="settings-for-chad"></a>`, which GitHub keeps.
HTML_ANCHOR = re.compile(r"<a\s(?:[^>\"']|\"[^\"]*\"|'[^']*')*?(?<![\w-])(?:id|name)\s*=\s*[\"']([^\"']+)[\"']",
                         re.I)


def rendered_text(source):
    """Return the heading text GitHub renders: link text without destinations, no emphasis markers."""
    # Code spans render literally, so hide them while stripping the rest.
    code = []

    def hide(span):
        code.append(span.group(2))
        return f"\0{len(code) - 1}\0"

    text = HTML_TAG.sub("", INLINE_LINK.sub(r"\1", CODE_SPAN.sub(hide, source)))
    previous = None
    while previous != text:
        previous, text = text, EMPHASIS.sub(r"\2", text)
    return re.sub(r"\0(\d+)\0", lambda match: code[int(match.group(1))], text)


def prose(markdown, keep_code=False):
    """Yield the lines of a Markdown document outside fenced code blocks, with inline code spans
    removed: links and anchors written as code are examples, not links."""
    opening = None
    for line in markdown.splitlines():
        fence = FENCE.match(line)
        if opening:
            # Only the same character, at least as long and with nothing after it, closes a fence.
            if fence and fence.group(1)[0] == opening[0] and len(fence.group(1)) >= len(opening) \
                    and not fence.group(2).strip():
                opening = None
            continue
        if fence and not (fence.group(1)[0] == "`" and "`" in fence.group(2)):
            opening = fence.group(1)
            continue
        yield line if keep_code else CODE_SPAN.sub("", line)


def anchors(markdown):
    """Return the anchors GitHub generates for a Markdown document's headings, plus explicit
    HTML `id` and `name` anchors."""
    found, seen = set(), {}
    for line in prose(markdown, keep_code=True):
        found.update(HTML_ANCHOR.findall(CODE_SPAN.sub("", line)))
        heading = HEADING.match(line)
        if heading:
            slug = re.sub(r"[^\w\- ]", "", rendered_text(heading.group(1)).lower()).replace(" ", "-")
            # GitHub skips a suffix that an earlier heading already produced.
            count = seen.get(slug, 0)
            candidate = f"{slug}-{count}" if count else slug
            while candidate in found:
                count += 1
                candidate = f"{slug}-{count}"
            seen[slug] = count + 1
            found.add(candidate)
    return found


def check_links(root, relative, cache=None):
    """Local link and anchor errors in one Markdown page. External links are not fetched.
    Links inside fenced code blocks are examples and are skipped. `cache` maps a page's
    resolved path to its anchors, so each page is parsed once per run."""
    cache = {} if cache is None else cache
    path = root / relative
    try:
        lines = list(prose(path.read_text(encoding="utf-8")))
    except UnicodeDecodeError:
        return [f"{relative}: not UTF-8"]
    targets = LINK.findall("\n".join(lines))
    targets += [match.group(1) for line in lines if (match := REFERENCE.match(line))]
    errors = []
    for target in targets:
        target = target[1:-1] if target.startswith("<") and target.endswith(">") else target
        url = urlsplit(target)
        if url.scheme or url.netloc:
            continue
        destination = (path.parent / unquote(url.path)).resolve() if url.path else path.resolve()
        if not destination.is_relative_to(root.resolve()) or not destination.exists():
            errors.append(f"{relative}: missing or outside-checkout link {target}")
            continue
        if not (url.fragment and destination.suffix == ".md" and destination.is_file()):
            continue
        if destination not in cache:
            try:
                cache[destination] = anchors(destination.read_text(encoding="utf-8"))
            except UnicodeDecodeError:
                cache[destination] = set()
        if unquote(url.fragment) not in cache[destination]:
            errors.append(f"{relative}: missing heading for link {target}")
    return errors


def check_policy(root, relative):
    """Title and skill-metadata rules, which apply only to the shared guidance in DOCUMENTS."""
    path = root / relative
    if not path.is_file():
        return [f"{relative}: missing document"]
    try:
        body = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return [f"{relative}: not UTF-8"]
    errors = []
    if not body.startswith("# ") and path.name != "SKILL.md":
        errors.append(f"{relative}: missing title")
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


def check_document(root, relative):
    """Every rule for one guidance document: its policy, then its links."""
    errors = check_policy(root, relative)
    if errors and errors[0].endswith("missing document"):
        return errors
    return errors + check_links(root, relative)


def tracked_markdown(root):
    """Every tracked Markdown page that exists in the working tree."""
    result = subprocess.run(["git", "ls-files", "-z", "--", "*.md"], cwd=root,
                            capture_output=True, text=True, check=False)
    if result.returncode:
        raise RuntimeError("git ls-files failed: " + result.stderr.strip())
    return [p for p in result.stdout.split("\0") if p and (root / p).is_file()]


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
    errors = [error for relative in DOCUMENTS for error in check_policy(ROOT, relative)]
    errors.extend(check_trackability(ROOT))
    try:
        pages = tracked_markdown(ROOT)
    except RuntimeError as error:
        pages = []
        errors.append(str(error))
    # A guidance document that is not committed yet still gets its links checked.
    pages = sorted(set(pages) | {d for d in DOCUMENTS if (ROOT / d).is_file()})
    cache = {}
    errors.extend(error for relative in pages for error in check_links(ROOT, relative, cache))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Agent guidance: {len(DOCUMENTS)} documents' titles, skill metadata and Git trackability, "
          f"and local links and heading anchors in {len(pages)} tracked Markdown pages passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
