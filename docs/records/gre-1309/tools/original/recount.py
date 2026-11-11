#!/usr/bin/env python3
"""Recount GRE-1309 without-arm sessions from recovered T3 activity logs.

usage: recount.py LOG_DIR REPO COMMIT NAVCOUNT_ORIG > sessions.csv

Rules
- Old rule (distinct_files_read): files under the worktree that a reader
  command (cat head tail sed nl less more bat awk) was given as a file
  argument, as navcount.py defines it, with three parser fixes: shell
  variables (R=...; $R/...) are expanded, every line of a multi-line
  `bash -lc` script is parsed (not only the first), and globs/brace lists are
  expanded against the commit's file list.
- New rule (distinct_files_with_content): old-rule files, plus any file whose
  content lines appear in a command's output. A search output line counts
  when its text equals that file's line at COMMIT (path-prefixed lines
  `path:N:text` / `path-N-text`, and unprefixed `N:text` lines from a search
  that named files, matched to the named file whose line N equals text).
  Lines that rg replaced with "[Omitted long line ...]" count by path.
- search_calls: rg/grep/egrep/fgrep segments that search files (not pipe
  filters, not `rg --files`). read_calls: reader segments with at least one
  existing file argument.
- total_content_bytes: UTF-8 bytes of every command output in the log
  (command line and the "Shell cwd was reset" trailer excluded).
- minutes: run startedAt to completedAt.
"""
import csv
import fnmatch
import glob
import importlib.util
import json
import os
import re
import shlex
import subprocess
import sys
from datetime import datetime

LOG_DIR, REPO, COMMIT, NAVCOUNT = sys.argv[1:5]
ROOT = "/persist/tmp/claude-1000/-persist-home-code-PrintPartner/6c7d6e89-1800-4793-8afa-7450e022e749/scratchpad/nav-without"
READERS = {"cat", "head", "tail", "sed", "nl", "less", "more", "bat", "awk"}
SEARCHERS = {"rg", "grep", "egrep", "fgrep"}
VALUE_FLAGS = {"head": {"-n", "-c"}, "tail": {"-n", "-c"}, "sed": {"-e", "-f"}, "awk": {"-F", "-v", "-f"},
               "nl": {"-b", "-w", "-s"},
               "rg": {"-g", "--glob", "-t", "--type", "-T", "--type-not", "-A", "-B", "-C", "-m", "--max-count",
                      "-e", "--regexp", "-f", "--file", "--max-columns", "-M", "-j", "--threads", "--max-depth",
                      "-E", "--encoding", "--sort", "--sortr", "--color", "--colors", "-r", "--replace"},
               "grep": {"-A", "-B", "-C", "-m", "-e", "-f", "--include", "--exclude", "--exclude-dir", "--color"}}
VALUE_FLAGS["egrep"] = VALUE_FLAGS["fgrep"] = VALUE_FLAGS["grep"]

FILES = set(subprocess.check_output(["git", "-C", REPO, "ls-tree", "-r", "--name-only", COMMIT], text=True).split("\n")) - {""}
DIRS = {os.path.dirname(f) for f in FILES}
for d in list(DIRS):
    while d:
        d = os.path.dirname(d)
        DIRS.add(d)

_blob = {}
def lines_of(path):
    if path not in _blob:
        try:
            data = subprocess.check_output(["git", "-C", REPO, "show", f"{COMMIT}:{path}"], stderr=subprocess.DEVNULL)
            _blob[path] = data.decode("utf-8", "replace").split("\n")
        except subprocess.CalledProcessError:
            _blob[path] = None
    return _blob[path]

def line_matches(path, n, text):
    ls = lines_of(path)
    if not ls or n < 1 or n > len(ls):
        return False
    want, have = ls[n - 1].rstrip(), text.rstrip()
    # A logged line can be cut short when the tool clipped a very long line.
    return have == want or (len(have) >= 40 and want.startswith(have))

# ---------------------------------------------------------------- parsing

def split_item(text):
    """Return (command, output) for a command_execution item."""
    assert text.startswith("$ "), text[:40]
    body = text[2:]
    idx = 0
    while True:
        nl = body.find("\n", idx)
        cand = body if nl < 0 else body[:nl]
        try:
            shlex.split(cand)
            cmd, out = cand, ("" if nl < 0 else body[nl + 1:])
            break
        except ValueError:
            if nl < 0:
                cmd, out = body, ""
                break
            idx = nl + 1
    out = re.sub(r"\n*Shell cwd was reset to [^\n]*\n?$", "", out)
    return cmd, out

def script_of(cmd):
    toks = shlex.split(cmd)
    if len(toks) >= 3 and os.path.basename(toks[0]) in {"bash", "sh"} and toks[1] in {"-lc", "-c"}:
        return toks[2]
    return cmd

def expand_vars(tok, env):
    return re.sub(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?", lambda m: env.get(m.group(1), m.group(0)), tok)

def segments(script):
    """Yield (tokens, piped_in) per simple command; splits on newlines and ; & | operators."""
    for line in script.split("\n"):
        try:
            lex = shlex.shlex(line, posix=True, punctuation_chars=";&|<>")
            lex.whitespace_split = True
            toks = list(lex)
        except ValueError:
            continue
        seg, piped = [], False
        prev_op = None
        for t in toks:
            if t and set(t) <= set(";&|"):
                if seg:
                    yield seg, piped
                piped = (t == "|")
                seg = []
            else:
                seg.append(t)
        if seg:
            yield seg, piped

def brace_expand(s):
    m = re.search(r"\{([^{}]*,[^{}]*)\}", s)
    if not m:
        return [s]
    out = []
    for alt in m.group(1).split(","):
        out.extend(brace_expand(s[:m.start()] + alt + s[m.end():]))
    return out

def resolve(arg, cwd):
    """Repo-relative files for an argument (globs expanded); [] if none."""
    res = []
    for a in brace_expand(arg):
        p = os.path.normpath(os.path.join(cwd, a))
        if p == ROOT:
            rel = ""
        elif p.startswith(ROOT + "/"):
            rel = os.path.relpath(p, ROOT)
        else:
            res.append(("outside", p))
            continue
        if any(c in rel for c in "*?["):
            res.extend(("file", f) for f in sorted(FILES) if fnmatch.fnmatchcase(f, rel) and f.count("/") == rel.count("/"))
        elif rel in FILES:
            res.append(("file", rel))
        elif rel in DIRS:
            res.append(("dir", rel))
        else:
            res.append(("missing", rel))
    return res

def args_of(prog, args):
    """Split args into (positional, flags) using VALUE_FLAGS; handles --flag=value."""
    pos, flags, skip = [], [], False
    vf = VALUE_FLAGS.get(prog, set())
    for i, a in enumerate(args):
        if skip:
            skip = False
            flags.append((args[i - 1], a))
            continue
        if a in vf:
            skip = True
            continue
        if a.startswith("-") and a != "-":
            flags.append((a, None))
            continue
        if a.startswith(("<", ">", "2>")) or a in {"/dev/null", ">", "<", "2", "&1"}:
            continue
        pos.append(a)
    return pos, flags

def analyse(script, start_cwd, seat):
    env, cwd = {}, start_cwd
    reads, searches, outside = [], [], []
    for seg, piped in segments(script):
        seg = [expand_vars(t, env) for t in seg]
        # leading assignments
        while seg and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", seg[0]):
            k, v = seg[0].split("=", 1)
            env[k] = v
            seg = seg[1:]
        if not seg:
            continue
        prog = os.path.basename(seg[0])
        if prog == "cd" and len(seg) > 1:
            cwd = os.path.normpath(os.path.join(cwd, seg[1]))
            continue
        if prog in READERS:
            pos, flags = args_of(prog, seg[1:])
            if prog in {"sed", "awk"} and not any(f in {"-e", "-f"} for f, _ in flags):
                pos = pos[1:]  # first positional is the script
            files = []
            for a in pos:
                for kind, p in resolve(a, cwd):
                    if kind == "file":
                        files.append(p)
                    elif kind == "outside":
                        outside.append(p)
            if files:
                reads.append((prog, files))
        elif prog in SEARCHERS:
            pos, flags = args_of(prog, seg[1:])
            fl = {f for f, _ in flags}
            if prog == "rg" and ("--files" in fl or "-l" in fl or "--files-with-matches" in fl):
                if "--files" in fl:
                    continue  # listing, not a search
            has_pattern_flag = any(f in {"-e", "--regexp", "-f", "--file"} for f in fl)
            targets = pos if has_pattern_flag else pos[1:]
            recursive = prog == "rg" or any(f.startswith("-") and not f.startswith("--") and ("r" in f[1:] or "R" in f[1:]) for f in fl) or "--recursive" in fl
            if piped and not targets:
                continue  # filter on stdin
            if not targets and not recursive:
                continue
            resolved = []
            for a in (targets or ["."]):
                for kind, p in resolve(a, cwd):
                    if kind in {"file", "dir"}:
                        resolved.append((kind, p))
                    elif kind == "outside":
                        outside.append(p)
            lists_only = bool(fl & {"-l", "--files-with-matches", "-c", "--count", "-L", "--files-without-match"}) or any(
                f.startswith("-") and not f.startswith("--") and ("l" in f[1:] or "c" in f[1:] or "L" in f[1:]) and prog != "rg" for f in fl)
            searches.append({"prog": prog, "targets": resolved, "lists_only": lists_only})
    return reads, searches, outside, cwd

NUM = re.compile(r"[:-](\d+)[:-]")

def content_files(output, searches):
    """Files whose content lines appear in a search output, verified against COMMIT."""
    single = [p for s in searches if not s["lists_only"] for (k, p) in s["targets"] if k == "file"]
    found, verified, unverified = set(), 0, 0
    for raw in output.split("\n"):
        line = raw[2:] if raw.startswith("./") else raw
        if line.startswith(ROOT + "/"):
            line = line[len(ROOT) + 1:]
        hit = False
        for m in NUM.finditer(line):
            path = line[:m.start()]
            if path in FILES:
                text = line[m.end():]
                if text.startswith("[Omitted long line") or line_matches(path, int(m.group(1)), text):
                    found.add(path); verified += 1
                else:
                    unverified += 1
                hit = True
                break
        if hit:
            continue
        m = re.match(r"^(\d+)[:-](.*)$", line)
        if m and single:
            n, text = int(m.group(1)), m.group(2)
            for p in single:
                if line_matches(p, n, text) or line_matches(p, n, text.lstrip()):
                    found.add(p); verified += 1
                    break
            continue
        # rg/grep without -n across files: path:text
        if ":" in line:
            path, text = line.split(":", 1)
            if path in FILES and text and not text.strip().isdigit():
                ls = lines_of(path) or []
                if any(l.rstrip() == text.rstrip() for l in ls):
                    found.add(path); verified += 1
    return found, verified, unverified

def minutes(run):
    s = datetime.fromisoformat(run["startedAt"].replace("Z", "+00:00"))
    e = datetime.fromisoformat(run["completedAt"].replace("Z", "+00:00"))
    return round((e - s).total_seconds() / 60, 2)

# ---------------------------------------------------------- literal navcount

spec = importlib.util.spec_from_file_location("navcount_orig", NAVCOUNT)
nav = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nav)
class _P:  # os.path stand-in: isfile answers from the commit's file list
    def __getattr__(self, k):
        return getattr(os.path, k)
    def isfile(self, p):
        p = os.path.normpath(p)
        return p.startswith(ROOT + "/") and os.path.relpath(p, ROOT) in FILES
class _OS:
    path = _P()
    def __getattr__(self, k):
        return getattr(os, k)
nav.os = _OS()

# ---------------------------------------------------------------- main

rows, detail = [], {}
for f in sorted(glob.glob(os.path.join(LOG_DIR, "session-*.json"))):
    d = json.load(open(f))
    th = d["thread"]
    m = re.match(r"nav without (q\d) (claude|codex) (r\d)", th["title"])
    q, seat, run = m.groups()
    items = d["items"]
    # Codex runs with no cd; navcount assumed the worktree root for both seats.
    start_cwd = ROOT if seat == "codex" else "/persist/home/code/PrintPartner"
    old, new, outside = set(), set(), set()
    small_new = set()  # content files seen in outputs of at most LARGE bytes
    LARGE = 50_000
    n_search = n_read = 0
    nbytes = 0
    ver = unver = 0
    notes = []
    for it in items:
        if it["type"] != "command_execution":
            continue
        if it.get("textTruncated"):
            notes.append(f"pos {it['position']} text truncated")
        cmd, out = split_item(it["text"])
        nbytes += len(out.encode("utf-8"))
        reads, searches, outs, _ = analyse(script_of(cmd), start_cwd, seat)
        outside.update(outs)
        n_read += len(reads)
        n_search += len(searches)
        for _, fs in reads:
            old.update(fs)
        cf, v, u = content_files(out, searches)
        new.update(cf)
        if len(out.encode("utf-8")) <= LARGE:
            small_new.update(cf)
        ver += v; unver += u
    new |= old
    large_only = new - old - small_new
    finals = [it for it in items if it["type"] == "assistant_message"]
    answer = finals[-1]["text"] if finals else ""
    lit_files, lit_outside = nav.opened(items, ROOT)
    complete = d.get("complete", False)
    if not complete:
        notes.append("log incomplete: " + "; ".join(d.get("notes", [])))
    if outside:
        notes.append("paths outside worktree: " + ", ".join(sorted(outside))[:300])
    if large_only:
        notes.append(f"{len(large_only)} new-rule files appear only in command outputs over 50 KB; the model may have received a truncated copy")
    if unver:
        notes.append(f"{unver} path-prefixed output lines did not match {COMMIT[:8]}")
    slug = f"{q}-{seat}-{run}"
    rows.append({
        "session": slug, "question": q[1:], "seat": seat, "run": run[1:],
        "distinct_files_read": len(old) if complete else "",
        "distinct_files_with_content": len(new) if complete else "",
        "search_calls": n_search, "read_calls": n_read,
        "total_content_bytes": nbytes if complete else "",
        "minutes": minutes(d["recentRuns"][0]),
        "final_answer_excerpt": answer[:200].replace("\n", " "),
        "navcount_py_literal": len(lit_files),
        "verified_search_lines": ver,
        "files_only_in_outputs_over_50kb": len(large_only) if complete else "",
        "thread_id": th["threadId"],
        "notes": " | ".join(notes),
    })
    detail[slug] = {"old": sorted(old), "new_only": sorted(new - old), "navcount_literal": sorted(lit_files)}

cols = ["session", "question", "seat", "run", "distinct_files_read", "distinct_files_with_content", "search_calls",
        "read_calls", "total_content_bytes", "minutes", "final_answer_excerpt", "navcount_py_literal",
        "verified_search_lines", "files_only_in_outputs_over_50kb", "thread_id", "notes"]
w = csv.DictWriter(sys.stdout, fieldnames=cols)
w.writeheader()
for r in rows:
    w.writerow(r)
json.dump(detail, open(os.path.join(LOG_DIR, "file-lists.json"), "w"), indent=1)
