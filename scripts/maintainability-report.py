#!/usr/bin/env python3
"""Print the maintainability measures of the 2026-11-09 plan (GRE-1306), read from one revision.

A report, never a check: it exits 0 whatever the numbers are. Every file is read from the named
revision (default `origin/main`) through `git cat-file`, never from the working tree, so a dirty
checkout cannot move a number. History windows end at that revision's commit date, so a pinned
revision pins them too.

Usage: python3 scripts/maintainability-report.py [--root DIR] [--rev REV] [--ci]
           [--required a,b,c] [PR ...]
`--ci` samples whole-workflow wall times and PR numbers add one push-to-green row per head; both
need `gh`. See docs/development/ci.md#maintainability-report.
"""

import argparse
from collections import Counter, defaultdict
import datetime as dt
import json
from pathlib import Path
import re
import subprocess

TICKET_NAME = re.compile(r"(^|/)(ticket_t?_?\d|gre[-_]?\d|slice\d)", re.I)
CFG_ATTR = re.compile(r"#\[cfg\([^\]]*desktop_rebuild[^\]]*\)\]")
PUB_FN = re.compile(r"^\s*pub (async )?fn ", re.M)
PUB_TYPE = re.compile(r"^\s*pub (struct|enum|trait) ", re.M)
GATE = '#[cfg(feature = "desktop_rebuild")]'
AVOID_WORDS = ("tenant", "profile")
RUST_OWNED_SURFACES = (
    "rust/crates/pp-domain/src", "rust/crates/pp-api-types/src",
    "rust/crates/pp-api/src", "web/apps/ui/src",
)
RETIRING_CRATES = ("pp-gateway", "pp-compat")
CI_WORKFLOWS = ("rust-cross-os.yml", "web-ci.yml")
# A workflow run shorter than this selected no job on path and says nothing about wall time.
CI_SKIPPED_UNDER_MIN = 6.0
# The required checks at 044e7706d. Pass `--required` when the set that blocks a merge changes.
REQUIRED_CHECKS = ("actionlint", "zizmor", "rust (linux)", "Lint Evidence block", "review-gate")
GREEN = ("success", "skipped", "neutral")
LIB = "rust/crates/pp-storage/src/lib.rs"
# File names too common to say which file they are; the report adds their parent directory.
GENERIC_NAMES = ("tests.rs", "mod.rs", "lib.rs", "main.rs")


def run(args, root, **kwargs):
    return subprocess.run(args, cwd=root, capture_output=True, check=True, **kwargs).stdout


class Revision:
    """Files and history of one revision, read through git only."""

    def __init__(self, root, rev):
        self.root = root
        self.rev = rev
        self.sha = self.git("rev-parse", "--verify", f"{rev}^{{commit}}").strip()
        self.date = dt.datetime.fromisoformat(self.git("show", "-s", "--format=%cI", self.sha).strip())
        self.paths = [p for p in self.git("ls-tree", "-r", "-z", "--name-only", self.sha).split("\0") if p]
        self._blobs = {}

    def git(self, *args):
        return run(["git", *args], self.root, text=True)

    def window(self, days):
        """`git log` arguments for the `days` days ending at the revision's commit date."""
        since = self.date - dt.timedelta(days=days)
        return (f"--since={since.isoformat()}", f"--until={self.date.isoformat()}")

    def log(self, days, *args):
        return self.git("log", self.sha, *self.window(days), *args)

    def tracked(self, prefix="", suffixes=None):
        return [p for p in self.paths if p.startswith(prefix) and (not suffixes or p.endswith(suffixes))]

    def load(self, paths):
        """Read many files with one `git cat-file --batch` process."""
        wanted = [p for p in dict.fromkeys(paths) if p not in self._blobs]
        if not wanted:
            return
        request = "".join(f"{self.sha}:{p}\n" for p in wanted).encode()
        out = run(["git", "cat-file", "--batch"], self.root, input=request)
        pos = 0
        for path in wanted:
            header_end = out.index(b"\n", pos)
            header = out[pos:header_end].decode()
            if header.endswith(" missing"):
                self._blobs[path] = ""
                pos = header_end + 1
                continue
            size = int(header.rsplit(" ", 1)[1])
            start = header_end + 1
            self._blobs[path] = out[start:start + size].decode("utf-8", errors="replace")
            pos = start + size + 1

    def text(self, path):
        if path not in self._blobs:
            self.load([path])
        return self._blobs[path]

    def lines(self, path):
        return self.text(path).count("\n")


UNESCAPED_PIPE = re.compile(r"(?<!\\)\|")


def cell(text):
    """One Markdown table cell. GFM splits cells on every unescaped `|`, inside code spans too."""
    return str(text).replace("|", "\\|")


def row(name, value, how):
    line = f"| {cell(name)} | {cell(value)} | `{cell(how)}` |"
    # Every row must keep the header's three cells, so a stray pipe cannot shift a column.
    if len(UNESCAPED_PIPE.findall(line)) != 4:
        raise ValueError(f"table row does not have three cells: {line}")
    print(line)


def crate(path):
    return path.split("/")[2]


def short_name(path):
    parts = path.split("/")
    return "/".join(parts[-2:]) if parts[-1] in GENERIC_NAMES else parts[-1]


def storage_rows(repo, rust, by_crate):
    row("Rust crates", len(by_crate), f"git ls-tree -r --name-only {repo.rev} rust/crates | cut -d/ -f3 | sort -u")
    row("Rust lines, tracked", f"{sum(repo.lines(p) for p in rust):,}",
        "all rust/crates/**/*.rs at the revision, newline-counted")
    storage = by_crate["pp-storage"]
    src = [p for p in storage if "/src/" in p]
    tests = [p for p in storage if "/tests/" in p]
    row("pp-storage lines (src / tests / files)",
        f"{sum(repo.lines(p) for p in src):,} / {sum(repo.lines(p) for p in tests):,} / {len(storage)}",
        "same, filtered to pp-storage and src/ or tests/")
    pub_fn = sum(len(PUB_FN.findall(repo.text(p))) for p in src)
    pub_ty = sum(len(PUB_TYPE.findall(repo.text(p))) for p in src)
    row("pp-storage `pub fn` / `pub struct|enum|trait` declared in src", f"{pub_fn} / {pub_ty}",
        "regex '^\\s*pub (async )?fn ' and '^\\s*pub (struct|enum|trait) ' over src")
    clients = set(re.findall(r"pub struct ([A-Za-z]+Client)\b", "\n".join(repo.text(p) for p in src)))
    row("pp-storage `*Client` handles", len(clients), "regex 'pub struct [A-Za-z]+Client' over src, unique")
    return src


def envelope_variants(lib):
    """Variant names of `enum Envelope` in a lib.rs text: brace and tuple variants, on one line or many."""
    env = re.search(r"\nenum Envelope \{\n(.*?)\n\}\n", lib, re.S)
    if not env:
        return []
    return re.findall(r"^    (?:#\[cfg\(feature = \"desktop_rebuild\"\)\]\n    )?([A-Z][A-Za-z]+)(?: \{|\()",
                      env.group(1), re.M)


def writer_loop_span(lib):
    """Lines from the writer thread's spawn to the `drop(connection)` that ends its loop."""
    src = lib.splitlines()
    start = next((i for i, line in enumerate(src) if "spawn(move" in line), None)
    if start is None:
        return None
    end = next((i for i in range(start, len(src)) if src[i].strip() == "drop(connection);"), None)
    return None if end is None else end - start + 1


def gated_arm_lines(lib):
    """First and last line numbers of the gate attributes inside `match envelope`."""
    first = last = None
    in_match = False
    for number, line in enumerate(lib.splitlines(), start=1):
        if line.startswith("                match envelope {"):
            in_match = True
        elif in_match and line == "                }":
            break
        elif in_match and line.startswith(" " * 20 + GATE):
            first = first or number
            last = number
    return first, last


def envelope_arms(body):
    """(variant, body lines, gated) for each arm of the writer's `match envelope` block."""
    arms, name, count, gated, pending_gate = [], None, 0, False, False
    for line in body.splitlines():
        if line.startswith(" " * 20 + GATE):
            pending_gate = True
            continue
        arm = re.match(r" {20}Envelope::([A-Za-z]+)", line)
        if arm:
            if name:
                arms.append((name, count, gated))
            name, count, gated, pending_gate = arm.group(1), 0, pending_gate, False
            continue
        count += 1
    if name:
        arms.append((name, count, gated))
    return arms


def lib_lines_per_family_commit(repo, days=30):
    """Mean lib.rs lines changed in commits of the window that also touch another storage module."""
    out = repo.log(days, "--numstat", "--format=@@%h", "--", "rust/crates/pp-storage/src")
    commits, current = [], None
    for line in out.splitlines():
        if line.startswith("@@"):
            current = {"lib": 0, "family": False}
            commits.append(current)
            continue
        parts = line.split("\t")
        if current is None or len(parts) != 3 or parts[0] == "-":
            continue
        if parts[2] == LIB:
            current["lib"] = int(parts[0]) + int(parts[1])
        elif parts[2].startswith("rust/crates/pp-storage/src/"):
            current["family"] = True
    touched = [c["lib"] for c in commits if c["family"]]
    with_lib = [n for n in touched if n]
    if not touched:
        return "no commits"
    return (f"{len(touched)} commits touch a family module; {len(with_lib)} of them also change lib.rs, "
            f"mean {sum(with_lib) / max(len(with_lib), 1):.0f} lib.rs lines each")


def writer_rows(repo, src):
    lib = repo.text(LIB)
    row("pp-storage/src/lib.rs lines", f"{lib.count(chr(10)):,}", f"git show {repo.rev}:{LIB} | wc -l")
    commits = len(repo.log(30, "--oneline", "--", LIB).splitlines())
    row("pp-storage/src/lib.rs commits in the 30 days to the revision date", commits,
        f"git log {repo.rev} --since=<rev date - 30d> --until=<rev date> --oneline -- {LIB} | wc -l")
    env = re.search(r"\nenum Envelope \{\n(.*?)\n\}\n", lib, re.S)
    env = env.group(1) if env else ""
    # Brace variants, and tuple variants such as `Publication(plan_publication::Dispatch),`.
    variants = envelope_variants(lib)
    gated = len(re.findall(r'#\[cfg\(feature = "desktop_rebuild"\)\]\n    [A-Z][A-Za-z]+(?: \{|\()', env))
    row("`Envelope` variants (gated)", f"{len(variants)} ({gated})", "parse `enum Envelope` in lib.rs")
    since = repo.window(7)[0]
    before = repo.git("rev-list", "-1", since.replace("--since", "--before"), repo.sha).strip()
    earlier = envelope_variants(repo.git("show", f"{before}:{LIB}")) if before else []
    added = sorted(set(variants) - set(earlier))
    row("`Envelope` variant headers added in the 7 days to the revision date",
        f"{len(added)} ({', '.join(added)})" if added else "0",
        f"variants of enum Envelope at {repo.rev} that the last commit before <rev date - 7d> lacks")
    span = writer_loop_span(lib)
    row("Writer thread closure lines (`thread::spawn` to `drop(connection)`)", span or "not found",
        "lines between those two anchors in lib.rs")
    first, last = gated_arm_lines(lib)
    row("Gated arms of `match envelope`, line range", f"{first} to {last}" if first else "not found",
        "first and last `#[cfg(feature = \"desktop_rebuild\")]` inside the match")
    block = re.search(r"\n {16}match envelope \{\n(.*?)\n {16}\}\n", lib, re.S)
    if not block:
        row("Writer `match envelope` block lines / gated arms longer than one line", "not found",
            "parse the `match envelope` block in lib.rs")
    else:
        arms = envelope_arms(block.group(1))
        over = sorted(((n, c) for n, c, g in arms if g and c > 1), key=lambda a: -a[1])
        longest = ", ".join(f"{n} {c}" for n, c in over[:4])
        row("Writer `match envelope` block lines / gated arms longer than one line (closing brace counted)",
            f"{block.group(1).count(chr(10)) + 1} / {len(over)} of {sum(1 for a in arms if a[2])} ({longest})",
            "parse the `match envelope` block in lib.rs; count lines between `Envelope::` arms")
    env_files = [p for p in src if p != LIB and "Envelope::" in repo.text(p)]
    row("pp-storage src files outside lib.rs that name `Envelope::`", len(env_files),
        "files under pp-storage/src containing 'Envelope::', lib.rs excluded")
    row("lib.rs lines changed per family-touching commit, 30 days to the revision date",
        lib_lines_per_family_commit(repo),
        f"git log {repo.rev} --since=<rev date - 30d> --until=<rev date> --numstat -- "
        "rust/crates/pp-storage/src, grouped by commit")


def gate_rows(repo, by_crate):
    cfg_src = Counter()
    cfg_all = 0
    for name, paths in by_crate.items():
        for path in paths:
            count = len(CFG_ATTR.findall(repo.text(path)))
            cfg_all += count
            if "/src/" in path:
                cfg_src[name] += count
    row("`#[cfg(..desktop_rebuild..)]` attributes in src (top 5)",
        ", ".join(f"{c} {n}" for c, n in cfg_src.most_common(5)),
        "regex '#\\[cfg\\([^]]*desktop_rebuild[^]]*\\)\\]' over <crate>/src")
    row("same, all src / including tests", f"{sum(cfg_src.values()):,} / {cfg_all:,}",
        "same over rust/crates/*/src and rust/crates")
    jobs = "rust/crates/pp-storage/src/jobs.rs"
    row("same, in pp-storage/src/jobs.rs", len(CFG_ATTR.findall(repo.text(jobs))), f"same over {jobs}")


def naming_rows(repo, rust):
    ticket_rs = [p for p in rust if TICKET_NAME.search(p)]
    retiring = [p for p in ticket_rs if crate(p) in RETIRING_CRATES]
    per_crate = Counter(crate(p) for p in ticket_rs)
    pattern = "'(^|/)(ticket_t?_?[0-9]|gre[-_]?[0-9]|slice[0-9])', case-insensitive"
    row("Rust files named after tickets (in src/); in retiring crates; per crate",
        f"{len(ticket_rs)} ({sum('/src/' in p for p in ticket_rs)}); {len(retiring)}; "
        + ", ".join(f"{c} {n}" for c, n in per_crate.most_common()),
        f"rust/crates/**/*.rs whose path matches {pattern}")
    row("Rust files named after tickets in crates that stay", len(ticket_rs) - len(retiring),
        f"same, crates other than {' and '.join(RETIRING_CRATES)}")
    prefixes = (".github/workflows/", "scripts/", "rust/scripts/", "rust/.config/")
    ref_paths = [p for p in repo.paths if p.startswith(prefixes) and not p.endswith((".png", ".json"))]
    repo.load(ref_paths)
    refs = [p for p in ref_paths if re.search(r"ticket_t_|slice0_mount|gre_[0-9]+", repo.text(p))]
    row("Workflow, script and nextest files that name a ticket-named test", len(refs),
        "files under .github/workflows, scripts, rust/scripts, rust/.config matching "
        "'ticket_t_|slice0_mount|gre_[0-9]+'")


def docs_rows(repo):
    md = repo.tracked(suffixes=(".md",))
    repo.load(md)
    docs = [p for p in md if p.startswith("docs/")]
    ticket_docs = [p for p in docs if TICKET_NAME.search(p) and not p.startswith("docs/records/")]
    row("Tracked Markdown pages / docs pages / docs named after tickets outside docs/records",
        f"{len(md)} / {len(docs)} / {len(ticket_docs)}",
        "*.md at the revision; docs/**/*.md; those matching the ticket pattern outside docs/records")
    page = repo.text("docs/desktop-rebuild.md")
    sections = re.split(r"^## ", page, flags=re.M)[1:]
    sizes = sorted(((s.count("\n"), s.splitlines()[0]) for s in sections), reverse=True)
    row("docs/desktop-rebuild.md lines / h2 sections", f"{page.count(chr(10)):,} / {len(sections)}",
        "wc -l; grep -c '^## '")
    row("Largest desktop-rebuild.md sections (lines from a heading to the next)",
        "; ".join(f"{title} {n:,}" for n, title in sizes[:4]), "split on '^## '")
    receipts = [n for n, title in sizes if re.search(r"receipt|GRE-612|D11|GRE-644|Filament grams", title)]
    row("desktop-rebuild.md receipt sections (lines)", f"{len(receipts)} sections, {sum(receipts):,} lines",
        "sections whose heading names a receipt, D11, GRE-612, GRE-644 or filament grams")
    big = [p for p in docs if not p.startswith("docs/records/") and repo.lines(p) > 1200]
    row("docs pages over 1,200 lines outside docs/records", ", ".join(big) or "none", "wc -l over docs")


def size_rows(repo, rust):
    largest = sorted(((repo.lines(p), p) for p in rust), reverse=True)[:4]
    row("Largest Rust files", "; ".join(f"{short_name(p)} {n:,}" for n, p in largest), "wc -l | sort -rn")
    ts = [p for p in repo.tracked("web", (".ts", ".tsx")) if "generated" not in p]
    repo.load(ts)
    by_package = Counter()
    for path in ts:
        parts = path.split("/")
        by_package["/".join(parts[:3]) if parts[1] in ("apps", "packages") else "web/other"] += repo.lines(path)
    row("TypeScript lines by package, generated files excluded",
        "; ".join(f"{k.split('/')[-1]} {v:,}" for k, v in by_package.most_common())
        + f"; total {sum(by_package.values()):,}",
        "web/**/*.ts and *.tsx at the revision, 'generated' paths excluded, grouped by package")


def word_rows(repo):
    surfaces = [p for s in RUST_OWNED_SURFACES for p in repo.tracked(s, (".rs", ".ts", ".tsx"))]
    repo.load(surfaces)
    for word in AVOID_WORDS:
        pattern = re.compile(rf"\b{word}\b", re.I)
        per_surface = Counter()
        for surface in RUST_OWNED_SURFACES:
            key = surface.split("/")[2] if surface.startswith("rust") else "ui"
            for path in repo.tracked(surface, (".rs", ".ts", ".tsx")):
                per_surface[key] += sum(1 for line in repo.text(path).splitlines() if pattern.search(line))
        row(f"Lines with glossary Avoid word `{word}` in Rust-owned surfaces",
            f"{sum(per_surface.values())} (" + ", ".join(f"{k} {v}" for k, v in per_surface.most_common() if v) + ")",
            f"git grep -w -i -c {word} {repo.rev} -- {' '.join(RUST_OWNED_SURFACES)}")


def include_rows(repo, rust, by_crate):
    count, files, targets = Counter(), Counter(), Counter()
    for path in rust:
        hits = re.findall(r'#\[path = "\.\./\.\./(pp-[a-z-]+/[^"]+)"', repo.text(path))
        if hits:
            count[crate(path)] += len(hits)
            files[crate(path)] += 1
            targets.update(hits)
    row("Cross-crate `#[path]` test includes; files per including crate",
        f"{sum(count.values())} in {sum(files.values())} files; "
        + ", ".join(f"{c} {n}" for c, n in files.most_common()),
        "regex '#\\[path = \"\\.\\./\\.\\./pp-' over rust/crates")
    row("same, by target", "; ".join(f"{t} {n}" for t, n in targets.most_common()),
        "same, grouped by the included path")
    rusqlite = [p for p in by_crate["pp-api"] if "/src/" in p and "rusqlite" in repo.text(p)]
    row("pp-api src files that mention rusqlite", len(rusqlite), "files under pp-api/src containing 'rusqlite'")


def tooling_rows(repo):
    try:
        base = json.loads(repo.text("rust/clippy-limits-baseline.json"))["on"]
        row("Clippy baseline, feature on (too_many_lines / cognitive_complexity)",
            f"{base['clippy::too_many_lines']} / {base['clippy::cognitive_complexity']}",
            "rust/clippy-limits-baseline.json")
    except (KeyError, json.JSONDecodeError):
        row("Clippy baseline, feature on", "not found", "rust/clippy-limits-baseline.json")
    row("Entries in scripts/ / rust/scripts/ / .github/workflows/",
        f"{len({p.split('/')[1] for p in repo.tracked('scripts/')})} / "
        f"{len({p.split('/')[2] for p in repo.tracked('rust/scripts/')})} / "
        f"{len(repo.tracked('.github/workflows/'))}", "first path component under each directory")
    py = [p for p in repo.tracked(suffixes=(".py",)) if p.startswith(("scripts/", "rust/scripts/"))]
    repo.load(py)
    row("Python lines under scripts/ and rust/scripts/", f"{sum(repo.lines(p) for p in py):,}",
        "*.py under those directories")
    workflows = repo.tracked(".github/workflows/")
    repo.load(workflows)
    row("Hosted workflows that name web/apps/ui", len([p for p in workflows if "apps/ui" in repo.text(p)]),
        "workflows containing 'apps/ui'")
    path_filter = "scripts/ci-path-filter.py"
    hits = [n for n, line in enumerate(repo.text(path_filter).splitlines(), start=1) if '"web/apps/"' in line]
    if path_filter not in repo.paths:
        selected = "not found"
    else:
        selected = f"yes: {path_filter}:{hits[0]} matches the web/apps/ prefix" if hits else "no"
    row("Rust job selected for a UI-only PR", selected,
        f"git show {repo.rev}:{path_filter} | grep -n '\"web/apps/\"'")


def gh(root, *args):
    return run(["gh", *args], root, text=True)


def minutes(start, end):
    def parse(stamp):
        return dt.datetime.fromisoformat(stamp.replace("Z", "+00:00"))
    return round((parse(end) - parse(start)).total_seconds() / 60, 1)


def sampled(what):
    row(f"{what} sample taken", dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%MZ"), "date -u")


def ci_rows(root):
    """Whole-workflow wall times: samples of workflow runs, not push-to-green on a head."""
    sampled("CI")
    for selector, label in ((("--event", "pull_request"), "PR runs"), (("--branch", "main"), "main runs")):
        for workflow in CI_WORKFLOWS:
            try:
                out = gh(root, "run", "list", "--workflow", workflow, *selector, "--status", "completed",
                         "--limit", "10", "--json", "conclusion,createdAt,updatedAt")
            except (subprocess.CalledProcessError, FileNotFoundError) as error:
                row(f"CI {label}, {workflow}", f"unavailable ({type(error).__name__})", "gh run list")
                continue
            durations = [minutes(r["createdAt"], r["updatedAt"]) for r in json.loads(out)
                         if r["conclusion"] == "success"]
            kept = [d for d in durations if d >= CI_SKIPPED_UNDER_MIN]
            row(f"CI {label}, {workflow}, successful, workflow minutes",
                f"{sorted(kept)} ({len(durations) - len(kept)} runs under {CI_SKIPPED_UNDER_MIN:g} min "
                "excluded by duration alone, as too short to have run a job's suites)",
                f"gh run list --workflow {workflow} {' '.join(selector)} --status completed "
                "--json createdAt,updatedAt")


def push_to_green(runs, required):
    """The time-to-green text for one head's check runs, and the checks not green."""
    first = min(r["started_at"] for r in runs if r.get("started_at"))
    latest = {}
    def started(run_):  # a queued run has no start yet and is the newest of its name
        return run_.get("started_at") or "9999"

    for run_ in runs:  # the newest run of each check name decides
        name = run_["name"]
        if name not in latest or started(run_) > started(latest[name]):
            latest[name] = run_
    timed = [latest[n] for n in required if n in latest]
    green = [r for r in timed if r.get("status") == "completed" and r.get("conclusion") in GREEN]
    not_green = [r["name"] for r in timed if r not in green] + [n for n in required if n not in latest]
    if timed and not not_green:
        text = f"{minutes(first, max(r['completed_at'] for r in green))} min to all required checks green"
    else:
        text = "required checks not all green: " + ", ".join(not_green)
    done = [r for r in runs if r.get("status") == "completed" and r.get("completed_at")]
    span = minutes(first, max(r["completed_at"] for r in done)) if done else None
    failing = sorted({r["name"] for r in latest.values()
                      if r.get("status") != "completed" or r.get("conclusion") not in (*GREEN, None)})
    return f"{text}; completed check-run span {span} min; not green or still running: {', '.join(failing) or 'none'}"


def pr_row(root, repo, number, required):
    pr = json.loads(gh(root, "pr", "view", number, "--json", "number,headRefOid,files"))
    paths = [f["path"] for f in pr["files"]]
    ui_only = bool(paths) and all(p.startswith("web/apps/ui/") for p in paths)
    # filter=all keeps re-run jobs, so the first start is the head's first check run.
    pages = json.loads(gh(root, "api", "--paginate", "--slurp",
                          f"repos/{repo}/commits/{pr['headRefOid']}/check-runs?per_page=100&filter=all"))
    runs = [r for page in pages for r in page["check_runs"]]
    head = f"PR #{number} head {pr['headRefOid'][:9]}"
    if not any(r.get("started_at") for r in runs):
        row(head, "no check runs", "gh api .../check-runs")
        return
    shown = ", ".join(paths[:6]) + (f", +{len(paths) - 6} more" if len(paths) > 6 else "")
    row(f"{head}: UI-only {ui_only}, {len(paths)} files ({shown})", push_to_green(runs, required),
        f"gh pr view {number} --json files,headRefOid; gh api repos/{repo}/commits/<sha>/check-runs?filter=all; "
        f"required = {', '.join(required)}")


def pr_rows(root, numbers, required):
    """Per PR head: changed paths, whether the diff is UI-only, and when the required checks went green."""
    sampled("PR")
    try:
        repo = json.loads(gh(root, "repo", "view", "--json", "nameWithOwner"))["nameWithOwner"]
    except (subprocess.CalledProcessError, FileNotFoundError) as error:
        row("PR rows", f"unavailable ({type(error).__name__})", "gh repo view")
        return
    for number in numbers:
        try:
            pr_row(root, repo, number, required)
        except (subprocess.CalledProcessError, FileNotFoundError, KeyError) as error:
            row(f"PR #{number}", f"unavailable ({type(error).__name__})", f"gh pr view {number}")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="Print the maintainability measures of one revision. A report, never a check.")
    parser.add_argument("prs", nargs="*", metavar="PR", help="pull request numbers to time (needs gh)")
    parser.add_argument("--root", default=".", help="repository root (default: the current directory)")
    parser.add_argument("--rev", default="origin/main", help="revision to measure (default: origin/main)")
    parser.add_argument("--ci", action="store_true", help="sample workflow wall times (needs gh)")
    parser.add_argument("--required", default=",".join(REQUIRED_CHECKS),
                        help="comma-separated checks the push-to-green row times")
    args = parser.parse_intermixed_args(argv)
    bad = [p for p in args.prs if not p.isdigit()]
    if bad:
        parser.error(f"not a PR number: {', '.join(bad)}")
    args.required = tuple(name.strip() for name in args.required.split(",") if name.strip())
    if not args.required:
        parser.error("--required needs at least one check name")
    return args, parser


def main(argv=None):
    args, parser = parse_args(argv)
    root = Path(args.root).resolve()
    try:
        repo = Revision(root, args.rev)
    except subprocess.CalledProcessError as error:
        parser.error(f"cannot read revision {args.rev}: {(error.stderr or '').strip() or error}")
    rust = repo.tracked("rust/crates", (".rs",))
    repo.load(rust)
    by_crate = defaultdict(list)
    for path in rust:
        by_crate[crate(path)].append(path)
    print(f"Measured at `{repo.sha[:9]}` (`{args.rev}`, committed {repo.date.isoformat()}), "
          "reading that revision, not the working tree.\n")
    print("| Measure | Value | How |")
    print("| --- | --- | --- |")
    src = storage_rows(repo, rust, by_crate)
    writer_rows(repo, src)
    gate_rows(repo, by_crate)
    naming_rows(repo, rust)
    docs_rows(repo)
    size_rows(repo, rust)
    word_rows(repo)
    include_rows(repo, rust, by_crate)
    tooling_rows(repo)
    if args.ci:
        ci_rows(root)
    if args.prs:
        pr_rows(root, args.prs, args.required)


if __name__ == "__main__":
    main()
