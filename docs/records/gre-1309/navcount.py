#!/usr/bin/env python3
"""Count files opened and minutes per GRE-1309 session from dumped tool logs.

Files opened: distinct files under the arm's worktree whose contents a command printed
(cat, head, tail, sed, nl, less, more, bat, awk). Search and listing commands don't count.
"""
import json
import os
import re
import shlex
import sys
from datetime import datetime

ROOTS = {
    "without": "/persist/tmp/claude-1000/-persist-home-code-PrintPartner/6c7d6e89-1800-4793-8afa-7450e022e749/scratchpad/nav-without",
    "with": "/persist/tmp/claude-1000/-persist-home-code-PrintPartner/6c7d6e89-1800-4793-8afa-7450e022e749/scratchpad/nav-with",
}
READERS = {"cat", "head", "tail", "sed", "nl", "less", "more", "bat", "awk"}
VALUE_FLAGS = {"head": {"-n", "-c"}, "tail": {"-n", "-c"}, "sed": {"-e", "-f"}, "awk": {"-F", "-v", "-f"}, "nl": {"-b", "-w", "-s"}}


def command_line(text):
    first = text.split("\n", 1)[0]
    if first.startswith("$ "):
        first = first[2:]
    m = re.match(r"^/bin/(?:ba)?sh -l?c (.*)$", first)
    if m:
        try:
            first = shlex.split(m.group(1))[0]
        except ValueError:
            first = m.group(1).strip("'\"")
    return first


def segments(cmd):
    # Split on shell separators outside quotes.
    try:
        lexer = shlex.shlex(cmd, posix=True, punctuation_chars=";&|")
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return []
    seg = []
    for tok in tokens:
        if tok and set(tok) <= set(";&|"):
            if seg:
                yield seg
            seg = []
        else:
            seg.append(tok)
    if seg:
        yield seg


def files_in(seg):
    prog = os.path.basename(seg[0])
    args = seg[1:]
    out, skip, script_taken = [], False, False
    needs_script = prog in {"sed", "awk"}
    for i, a in enumerate(args):
        if skip:
            skip = False
            if prog in {"sed", "awk"} and args[i - 1] in {"-e", "-f"}:
                script_taken = True
            continue
        if a in VALUE_FLAGS.get(prog, set()):
            skip = True
            continue
        if a.startswith("-") and a != "-":
            continue
        if a.startswith(("<", ">", "2>")) or a in {"/dev/null"}:
            continue
        if needs_script and not script_taken:
            script_taken = True
            continue
        out.append(a)
    return out


def opened(items, root):
    cwd, files, outside = root, set(), set()
    known = {"user_message", "assistant_message", "reasoning", "checkpoint", "command_execution"}
    for it in items:
        if it["type"] not in known:
            raise ValueError(f"unknown item type {it['type']!r}: extend opened() before counting this log")
        if it["type"] != "command_execution":
            continue
        for seg in segments(command_line(it["text"])):
            if not seg:
                continue
            if seg[0] == "cd" and len(seg) > 1:
                cwd = os.path.normpath(os.path.join(cwd, seg[1]))
                continue
            if os.path.basename(seg[0]) not in READERS:
                continue
            for f in files_in(seg):
                p = os.path.normpath(os.path.join(cwd, f))
                if p.startswith(root + "/") and os.path.isfile(p):
                    files.add(os.path.relpath(p, root))
                elif os.path.isfile(p):
                    outside.add(p)
    return files, outside


def minutes(d):
    s = datetime.fromisoformat(d["startedAt"].replace("Z", "+00:00"))
    e = datetime.fromisoformat(d["completedAt"].replace("Z", "+00:00"))
    return round((e - s).total_seconds() / 60, 1)


def main():
    for path in sys.argv[1:]:
        d = json.load(open(path))
        arm = "with" if "-with-" in d["tag"] else "without"
        files, outside = opened(d["items"], ROOTS[arm])
        print(json.dumps({"tag": d["tag"], "files": len(files), "minutes": minutes(d),
                          "list": sorted(files), "outside": sorted(outside)}))


if __name__ == "__main__":
    main()
