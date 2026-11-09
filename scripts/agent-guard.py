#!/usr/bin/env python3
"""Refuse agent actions that Print Partner reserves for the maintainer.

Any agent runner, or a person, can call this before running a shell command,
editing a file, or applying a patch. Factory Droid calls it through the
adapter in .factory/hooks/guard.py and Claude Code through .claude/hooks/guard.py;
other runners must call it themselves. It
is a guard rail, not a security boundary: it reads the operation it is given
and refuses known-dangerous shapes. When it cannot classify a git or gh
invocation, it refuses rather than guessing.

    python3 -B scripts/agent-guard.py command --cwd "$PWD" 'git push origin HEAD:topic'
    python3 -B scripts/agent-guard.py file docs/guide.md
    python3 -B scripts/agent-guard.py patch change.patch
    printf '%s' '{"operation": "file", "path": "docs/guide.md"}' | python3 -B scripts/agent-guard.py json
"""

import argparse
import html
import itertools
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import sys
import time
from urllib.parse import unquote_plus, urlsplit

MERGE = ("Only the coordinator worktree merges, with `gh pr merge <number> --squash --match-head-commit <sha>`; "
         "other merges need the maintainer. Report readiness instead.")
FORCE = "Never force-push. Add a commit instead."
MAIN = "Push a branch and open a PR; main only changes by merge."
RELEASE = "Releases and tags need separate authorization."
HOOKS = "Do not bypass Git hooks."
UNCLEAR = "Cannot classify this git or gh command; run it without the wrapper option."
UNPARSABLE = "Cannot parse shell command; use a simpler command."
EXPANSION = "Cannot classify a shell expansion here; write git and gh arguments literally."
GH_OPTIONS = "Put gh options after the subcommand; only -R/--repo, --help, and --version may come before it."

ASSIGNMENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
SHELL_PREFIXES = {"!", "{", "}", "if", "then", "elif", "else", "while", "until", "do"}
# Wrapper programs and their options that take a separate value.
WRAPPERS = {
    "command": set(),
    "env": {"-u", "--unset", "-C", "--chdir"},
    "exec": {"-a"},
    "nohup": set(),
    "sudo": {"-u", "--user", "-g", "--group", "-h", "--host", "-p", "--prompt", "-C", "-D", "-R", "-T", "-U"},
    "nice": {"-n", "--adjustment"},
    "time": {"-f", "--format", "-o", "--output"},
    "timeout": {"-s", "--signal", "-k", "--kill-after"},
}
# Positional operands, such as a duration, that come before the wrapped program.
WRAPPER_OPERANDS = {"timeout": 1}
GIT_VALUE_OPTIONS = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env"}
HOOK_COMMANDS = {"am", "cherry-pick", "merge", "rebase", "revert"}
MAIN_REF = re.compile(r"^(?:(?:refs/)?heads/)?main$")
PUSH_VALUE_LONG = {"--exec", "--push-option", "--receive-pack", "--repo"}
COMMIT_VALUE_LONG = {"--author", "--date", "--file", "--message", "--reedit-message",
                     "--reuse-message", "--template", "--trailer"}
GH_SHORT_BOOLEAN = set("ih")
# gh command groups whose subcommand the guard classifies.
GH_GROUPS = {"pr", "release", "variable", "alias", "extension", "extensions", "ext"}
# gh options that may come before or between command words; none takes the next word as its value.
GH_PATH_FLAGS = {"--help", "-h", "--version"}
RELEASE_READS = {"download", "list", "verify", "verify-asset", "view"}
VARIABLE_READS = {"get", "help", "list"}
# The word in the coordinator's review record, which the review-gate check accepts in place of an
# automatic review. Outside the coordinator worktree, any command line that contains it in any case
# or decodable encoding is refused, wherever it appears.
MARKER_WORD = "pp-review-gate"
MARKER = ("Only the coordinator worktree posts pp-review-gate review markers or runs any command that names "
          "pp-review-gate, in any case or encoding; report readiness to the coordinator instead.")
# Shells whose -c script the guard classifies like a top-level command, and the long options they
# accept before it. Any other option makes the script position unknown.
SHELLS = {"sh", "bash", "zsh", "dash"}
SHELL_VALUE_LONG = {"--rcfile", "--init-file"}
SHELL_FLAG_LONG = {"--login", "--norc", "--noprofile", "--posix", "--noediting", "--restricted", "--verbose",
                   "--debugger", "--dump-strings", "--dump-po-strings", "--pretty-print"}
SHELL_OPTIONS = ("Cannot tell which argument this shell runs as its script; run git and gh directly, "
                 "or use plain bash -c without other options.")
EVAL = ("Run git and gh commands directly, not through eval or source, so the guard can classify them.")
JSON_ESCAPE = re.compile(r'\\(?:u([0-9a-fA-F]{4})|(["\\/bfnrt]))')
JSON_SIMPLE = {'"': '"', "\\": "\\", "/": "/", "b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t"}
COMMENT_COMMANDS = (["pr", "comment"], ["issue", "comment"])
# GraphQL mutations that write comment text, which could carry a marker.
COMMENT_MUTATIONS = {"addComment", "addPullRequestReviewComment", "addPullRequestReviewThreadReply",
                     "updateIssueComment", "updatePullRequestReviewComment"}
UNREAD_BODY = ("The guard must read a comment body literally to check it for a pp-review-gate marker. "
               "Write the body as a literal argument, or to a file passed with --body-file in a command "
               "of its own; no variables, substitutions, or stdin.")
VARIABLE = "GitHub Actions variables are owner-maintained; ask the maintainer to change them."
ALIAS = "A gh alias hides the command from the guard; run the full gh command instead."
EXTENSION = "gh extensions run code the guard cannot read; ask the maintainer to install one."
GH_CONFIG = "The gh config can define aliases that hide a command from the guard; ask the maintainer to change it."
SPLIT_GH = "Cannot classify a gh command built from variables; write gh and its arguments literally."
# Interpreter code (python3 -c, node -e) cannot be parsed as shell, so its text is matched after quote
# characters and backslashes are removed.
NAMED_MERGE = re.compile(r"\bpr\s*merge\b")
MERGE_WORD = re.compile(r"\bmerge\b|mergepullrequest|enablepullrequestautomerge", re.I)
# Interpreter code splits into words at whitespace, quotes, and the punctuation around string literals and calls.
CODE_WORD = re.compile(r"[^\s'\"`(),;\[\]{}+]+")
INTERPRETERS = {"python", "python3", "node", "perl", "ruby", "php", "deno", "bun"}
# gh options that take a value, before or between gh, pr, and the subcommand.
GH_VALUE_FLAGS = {"-R", "--repo"}
# Programs that run another command from their arguments in a way program() does not unwrap. A gh command
# under one of them that names a merge, or that the guard cannot read literally, is refused.
UNWRAPPED_RUNNERS = {"xargs", "find", "setsid", "flock", "watch", "script", "parallel", "su", "runuser", "doas",
                     "ssh", "unbuffer", "chronic", "ionice", "taskset", "chrt", "systemd-run", "busybox", "entr",
                     "stdbuf", "strace", "ltrace", "fakeroot", "firejail", "bwrap", "unshare", "nsenter", "chroot",
                     "expect", "tmux", "screen", "dtach", "at", "batch"}
# Runners that hand an argument to a shell as a command line (script -c, su -c, flock -c, entr -s, ssh host,
# watch, tmux).
# The others run a program with arguments, so only their program positions are commands.
STRING_RUNNERS = {"script", "su", "runuser", "flock", "expect", "ssh", "watch", "tmux", "screen", "dtach",
                  "parallel", "at", "batch", "entr"}
# Git commands that can change which branch HEAD names before a later push runs.
BRANCH_CHANGES = {"bisect", "checkout", "rebase", "switch"}
# Marks text the shell expands at run time, so its value is unknown here.
EXPANDED = "\x00"
GH_VALUE_OPTIONS = {"-f", "-F", "--field", "--raw-field", "-H", "--header", "--input", "-q", "--jq",
                    "-t", "--template", "--hostname", "-p", "--preview", "--cache", "-X", "--method"}
GH_SHORT_VALUE = {option[1] for option in GH_VALUE_OPTIONS if len(option) == 2}
# Wrapper options that run the program in another directory.
WRAPPER_CHDIR = {"-C", "--chdir", "-D"}
REPO_PATH = r"(?:^|/)repos/[^/]+/[^/]+/"
# The only REST writes agents may make through gh api: PR and issue comments.
COMMENT_WRITES = re.compile(REPO_PATH + r"(?:issues/\d+/comments|pulls/\d+/comments(?:/\d+/replies)?"
                            r"|issues/comments/\d+|pulls/comments/\d+)$")
# Pull request reviews are comment writes only when their event is COMMENT or left pending.
REVIEW_WRITES = re.compile(REPO_PATH + r"pulls/\d+/reviews(?:/\d+/events)?$")
ALLOWED_MUTATIONS = {"addComment", "addPullRequestReviewComment", "addPullRequestReviewThreadReply",
                     "resolveReviewThread", "unresolveReviewThread", "updateIssueComment",
                     "updatePullRequestReviewComment"}
# GraphQL ends comments at CR or LF, and a block string may contain an escaped \""".
GRAPHQL_TOKEN = re.compile(r'"""(?:\\"""|[^"]|"(?!""))*"""|"(?:\\.|[^"\\\n\r])*"|#[^\n\r]*|\.\.\.|[A-Za-z_]\w*|\S')
STALE_INPUT = ("Run a gh api request that reads a file as a separate command; "
               "other commands could change the file or directory before gh reads it.")
API_WRITE = ("Through gh api, agents may read and write PR or issue comments and review threads; "
             "other writes need the maintainer.")
SAFE_PUSH_LONG = {"--atomic", "--dry-run", "--force-if-includes", "--ipv4", "--ipv6", "--no-atomic",
                  "--no-dry-run", "--no-follow-tags", "--no-force-if-includes", "--no-progress", "--no-signed", "--no-thin",
                  "--porcelain", "--progress", "--push-option", "--quiet", "--repo", "--set-upstream",
                  "--signed", "--thin", "--verbose"}
SAFE_PUSH_SHORT = set("46noquv")
PUSH_OPTIONS = "Use common git push options spelled in full."
PUSH_EXPLICIT = "Name the destination branch, for example `git push origin HEAD:topic`."
FOLLOW_TAGS = "push.followTags may publish tags; add --no-follow-tags to the push."
ENV_CONFIG = "GIT_CONFIG_* variables can disable hooks or redirect pushes; run git without them."
# Environment assignments that add or replace git configuration.
GIT_CONFIG_ENV = re.compile(r"^GIT_CONFIG\w*=")
# Builtins whose arguments are assignments, such as export GIT_DIR=...
DECLARATIONS = {"declare", "export", "local", "readonly", "typeset"}
CONFIG_READ_OPTIONS = {"--get", "--get-all", "--get-color", "--get-colorbool", "--get-regexp",
                       "--get-urlmatch", "--list", "-l"}
CONFIG_READ_COMMANDS = {"get", "list"}
CONFIG_WRITE_OPTIONS = {"--add", "--edit", "-e", "--remove-section", "--rename-section", "--replace-all",
                        "--unset", "--unset-all"}
# Environment assignments that point git at another repository.
GIT_LOCATION = re.compile(r"^(?:GIT_DIR|GIT_WORK_TREE|GIT_COMMON_DIR)=")
GENERATED = {"rust/bundle-manifest.json"}
# Claude Code settings and hooks; an edit there could switch the guard off.
CLAUDE_HOOK_CONFIG = {"settings.json", "settings.local.json", "hooks"}
# The coordinator's hook config names its worktree root here. Lane worktrees live under LANE_WORKTREES.
COORDINATOR_ENV = "PP_COORDINATOR_WORKTREE"
LANE_WORKTREES = "/persist/worktrees/t3"
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
PATCH_FILE = re.compile(r"^\*\*\* (?:(?:Add|Update|Delete) File|Move to): (.+)$", re.M)


class Unclear(Exception):
    pass


def protected_path(raw, cwd=None):
    # Collapse "dir/../" so an alias such as .claude/agents/../settings.json is checked as its target.
    path = PurePosixPath(posixpath.normpath(raw.replace("\\", "/")) if raw else raw)
    name = path.name
    if ".git" in path.parts:
        return "Do not edit Git metadata."
    if (name == ".env" or name.startswith(".env.")) and not name.endswith(".example"):
        return "Keep real environment files out of agent edits; use the .example template."
    if any(path.as_posix().endswith(entry) for entry in GENERATED):
        return "This file is generated. Run its generator instead of editing it."
    if any(a == ".claude" and b in CLAUDE_HOOK_CONFIG for a, b in zip(path.parts, path.parts[1:])):
        return "Claude Code settings and hooks run the safety guard; ask the maintainer to change them."
    if gh_config_path(path, cwd):
        return GH_CONFIG
    return None


def gh_config_path(path, cwd=None):
    """Whether path lies in a gh config directory: $GH_CONFIG_DIR, $XDG_CONFIG_HOME/gh, or $HOME/.config/gh.

    Relative paths, and a relative GH_CONFIG_DIR, resolve against cwd: the cwd in the hook payload, which
    is the directory the agent's tool runs in. A caller that has no cwd falls back to the hook process's working
    directory. A folder elsewhere that only looks like .config/gh is not the config.
    """
    here = cwd if cwd and os.path.isabs(cwd) else None
    if here is None:
        try:
            here = os.getcwd()
        except OSError:
            here = "/"

    def absolute(raw):
        return posixpath.normpath(posixpath.join(here, os.path.expanduser(raw)))

    home, xdg = os.environ.get("HOME", ""), os.environ.get("XDG_CONFIG_HOME", "")
    roots = [os.environ.get("GH_CONFIG_DIR", ""), posixpath.join(xdg, "gh") if xdg else "",
             posixpath.join(home, ".config/gh") if home else ""]
    target = absolute(path.as_posix())
    for root in filter(None, roots):
        root = absolute(root)
        if target == root or target.startswith(root.rstrip("/") + "/"):
            return True
    return False


SEPARATORS = ";&|\n()"
# Words after which the next word is a command, as in "then case ...".
COMMAND_KEYWORDS = {"!", "{", "do", "elif", "else", "if", "then", "time", "until", "while"}
HEREDOC = re.compile(r"""[ \t]*((?:'[^']*'|"[^"]*"|\\.|[^\s;&|<>()'"\\])+)""")
HEREDOC_QUOTING = re.compile(r"""\\(.)|['"]""")
ANSI_C_QUOTE = re.compile(r"\$'((?:\\.|[^'\\])*)'", re.S)


def closing_double_quote(source, i):
    while i < len(source):
        tick()
        char = source[i]
        if char == "\\":
            i += 2
        elif char == '"':
            return i
        elif source.startswith("$(", i):
            i = closing_paren(source, i + 2) + 1
        elif char == "`":
            i = closing_backtick(source, i + 1) + 1
        else:
            i += 1
    raise ValueError("unterminated quote")


def heredoc_body(source, i, delimiter, tabs, expands):
    """Return the body lines of a heredoc that starts at i and the index after its delimiter."""
    body = []
    while i < len(source):
        tick()
        end = source.find("\n", i)
        end = len(source) if end < 0 else end
        line = source[i:end].rstrip("\r")
        i = end + 1
        # Bash joins backslash-newline in an unquoted heredoc before matching the delimiter.
        while expands and (len(line) - len(line.rstrip("\\"))) % 2 and i < len(source):
            end = source.find("\n", i)
            end = len(source) if end < 0 else end
            line = line[:-1] + source[i:end].rstrip("\r")
            i = end + 1
        if (line.lstrip("\t") if tabs else line) == delimiter:
            break
        body.append(line)
    return body, i


def heredoc_delimiter(raw):
    """Return the delimiter word and whether the heredoc body expands (the word is unquoted)."""
    return HEREDOC_QUOTING.sub(lambda m: m.group(1) or "", raw), not any(c in raw for c in "'\"\\")


def command_position(source, start, i):
    """Whether index i starts a command word inside a substitution that starts at start."""
    before = source[start:i].rstrip(" \t")
    return not before or before[-1] in ";&|(\n" or before.split()[-1] in COMMAND_KEYWORDS


def closing_paren(source, i):
    # cases holds the paren depth at each open case; a ")" at that depth ends a pattern.
    depth, pending, start, cases = 1, [], i, []
    while i < len(source):
        tick()
        char = source[i]
        if char in "ce":
            keyword = re.match(r"(case|esac)(?=[\s;&|)]|$)", source[i:i + 5])
            # "case a in esac" has no arms, so esac may follow "in".
            if keyword and (command_position(source, start, i)
                            or keyword.group(1) == "esac" and source[start:i].split()[-1:] == ["in"]):
                if keyword.group(1) == "case":
                    cases.append(depth)
                elif cases:
                    cases.pop()
                i += 4
                continue
        if char == ")" and cases and cases[-1] == depth:
            i += 1
            continue
        if char == "#" and (i == start or source[i - 1] in " \t\n;&|("):
            newline = source.find("\n", i)
            if newline < 0:
                raise ValueError("unterminated command substitution")
            i = newline
            continue
        if char == "\\":
            i += 2
            continue
        # Heredoc bodies inside a substitution are data; their quotes and parentheses do not count.
        if source.startswith("<<<", i):
            i += 3
            continue
        if source.startswith("<<", i):
            tabs = source.startswith("-", i + 2)
            match = HEREDOC.match(source, i + 2 + tabs)
            if match:
                delimiter, expands = heredoc_delimiter(match.group(1))
                pending.append((delimiter, tabs, expands))
                i = match.end()
                continue
        if char == "\n" and pending:
            i += 1
            for delimiter, tabs, expands in pending:
                i = heredoc_body(source, i, delimiter, tabs, expands)[1]
            pending = []
            continue
        if char == "'":
            end = source.find("'", i + 1)
            if end < 0:
                raise ValueError("unterminated quote")
            i = end + 1
            continue
        if char == '"':
            i = closing_double_quote(source, i + 1) + 1
            continue
        depth += {"(": 1, ")": -1}.get(char, 0)
        if depth == 0:
            return i
        i += 1
    raise ValueError("unterminated command substitution")


def closing_backtick(source, i):
    while i < len(source):
        tick()
        if source[i] == "\\":
            i += 2
        elif source[i] == "`":
            return i
        else:
            i += 1
    raise ValueError("unterminated command substitution")


def substitutions(text):
    """Return command substitutions in text where quotes are literal, such as a heredoc body."""
    found, i = [], 0
    while i < len(text):
        tick()
        if text[i] == "\\":
            i += 2
        elif text.startswith("$(", i):
            end = closing_paren(text, i + 2)
            found.append(text[i + 2:end])
            i = end + 1
        elif text[i] == "`":
            end = closing_backtick(text, i + 1)
            found.append(text[i + 1:end])
            i = end + 1
        else:
            i += 1
    return found


def shell_commands(source):  # noqa: C901
    """Split shell source into (arguments, in_subshell, after_and_or) for simple commands.

    Handles quotes, escapes, continuations, separators, comments, redirections,
    heredoc bodies, and command substitutions. It does not evaluate expansions.
    """
    commands, args, word, heredocs, nested = [], [], None, [], []
    drop_next = piped = after_logic = False
    depth = 0

    def finish_word():
        nonlocal word, drop_next
        if word is not None:
            if drop_next:
                drop_next = False
            else:
                args.append(word)
            word = None

    i = 0
    while i < len(source):
        tick()
        char = source[i]
        if char == "\\" and i + 1 < len(source):
            if source[i + 1] == "\n":
                i += 2
            elif source.startswith("\r\n", i + 1):
                i += 3
            else:
                word = (word or "") + source[i + 1]
                i += 2
        elif char == "'":
            end = source.find("'", i + 1)
            if end < 0:
                raise ValueError("unterminated quote")
            word = (word or "") + source[i + 1:end]
            i = end + 1
        elif char == '"':
            j, text = i + 1, []
            while True:
                tick()
                if j >= len(source):
                    raise ValueError("unterminated quote")
                c = source[j]
                if c == '"':
                    break
                if c == "\\" and source[j + 1:j + 2] == "\n":
                    j += 2
                elif c == "\\" and source[j + 1:j + 2] in ("$", "`", '"', "\\"):
                    text.append(source[j + 1])
                    j += 2
                elif source.startswith("$(", j):
                    end = closing_paren(source, j + 2)
                    nested.append(source[j + 2:end])
                    text.append(EXPANDED)
                    j = end + 1
                elif c == "`":
                    end = closing_backtick(source, j + 1)
                    nested.append(source[j + 1:end])
                    text.append(EXPANDED)
                    j = end + 1
                else:
                    text.append(EXPANDED + c if c == "$" else c)
                    j += 1
            word = (word or "") + "".join(text)
            i = j + 1
        elif source.startswith("$'", i):
            match = ANSI_C_QUOTE.match(source, i)
            if not match:
                raise ValueError("unterminated quote")
            decoded = match.group(1).encode("latin-1", "backslashreplace").decode("unicode_escape")
            # Bash ends an ANSI-C quoted string at a decoded NUL.
            word = (word or "") + decoded.split("\x00", 1)[0]
            i = match.end()
        elif source.startswith("$(", i):
            end = closing_paren(source, i + 2)
            nested.append(source[i + 2:end])
            word = (word or "") + EXPANDED
            i = end + 1
        elif char == "`":
            end = closing_backtick(source, i + 1)
            nested.append(source[i + 1:end])
            word = (word or "") + EXPANDED
            i = end + 1
        elif char == "#" and word is None:
            newline = source.find("\n", i)
            i = len(source) if newline < 0 else newline
        elif source.startswith("<<<", i):
            # The here-string word is stdin data, not an argument.
            if word is not None and word.isdigit():
                word = None
            finish_word()
            i += 3
            drop_next = True
        elif source.startswith("<<", i):
            if word is not None and word.isdigit():
                word = None
            finish_word()
            i += 2
            tabs = source.startswith("-", i)
            i += tabs
            match = HEREDOC.match(source, i)
            if match:
                heredocs.append((*heredoc_delimiter(match.group(1)), tabs))
                i = match.end()
        elif char in "<>" or source.startswith("&>", i):
            if word is not None and word.isdigit():
                word = None
            finish_word()
            i += 1
            while i < len(source) and source[i] in "<>&|":
                i += 1
            drop_next = True
        elif char in " \t\r" or char in SEPARATORS:
            finish_word()
            if char in SEPARATORS:
                drop_next = False
                # A pipeline stage or background job runs in its own subshell.
                pipe = char in "|&" and source[i + 1:i + 2] != char and source[i - 1:i] != char
                # A command after && or || may not run.
                logic = char in "|&" and not pipe
                inside = depth > 0
                depth += {"(": 1, ")": -1}.get(char, 0)
                if args:
                    commands.append((args, inside or pipe or piped, after_logic))
                    args = []
                piped, after_logic = pipe, logic
            i += 1
            if char == "\n":
                for delimiter, expands, tabs in heredocs:
                    body, i = heredoc_body(source, i, delimiter, tabs, expands)
                    if expands:
                        nested.extend(substitutions("\n".join(body)))
                heredocs = []
        else:
            word = (word or "") + (EXPANDED + char if char == "$" else char)
            i += 1
    finish_word()
    if args:
        commands.append((args, depth > 0 or piped, after_logic))
    for inner in nested:
        commands.extend((inner_args, True, conditional) for inner_args, _, conditional in shell_commands(inner))
    return commands


def program(args):
    """Return the executed program name, its arguments, whether a wrapper changed directory,
    and the environment assignments the command makes."""
    i, moved, assigned = 0, False, []
    while i < len(args):
        word = args[i]
        if ASSIGNMENT.match(word) or word in SHELL_PREFIXES:
            moved = moved or bool(GIT_LOCATION.match(word))
            assigned += [word] if ASSIGNMENT.match(word) else []
            i += 1
            continue
        name = PurePosixPath(word).name
        if name not in WRAPPERS:
            if name in DECLARATIONS:
                assigned += [arg for arg in args[i + 1:] if ASSIGNMENT.match(arg)]
            return name, args[i + 1:], moved, assigned
        i += 1
        while i < len(args) and args[i].startswith("-"):
            option = args[i]
            if option == "--":
                i += 1
                break
            if name == "env" and (option.startswith("--split-string") or re.match(r"^-[^-]*S", option)):
                raise Unclear
            moved = moved or option.split("=", 1)[0] in WRAPPER_CHDIR or bool(re.match(r"^-[^-]*[CD]", option))
            i += 2 if option in WRAPPERS[name] else 1
        i += WRAPPER_OPERANDS.get(name, 0)
    return None, [], moved, assigned


def options(args, value_flags="", stop_flags="", value_long=()):
    """Yield ("long" | "short" | "arg", text) for one command's arguments.

    A short bundle stops at a flag that takes a value; when that value is the
    next argument, it is skipped rather than read as another option.
    """
    i, ended = 0, False
    while i < len(args):
        arg = args[i]
        i += 1
        if not ended and arg.startswith(EXPANDED):
            raise Unclear(EXPANSION)
        if not ended and arg == "--":
            ended = True
        elif ended or arg == "-" or not arg.startswith("-"):
            yield "arg", arg
        elif arg.startswith("--"):
            name = arg.split("=", 1)[0]
            if EXPANDED in name:
                raise Unclear(EXPANSION)
            yield "long", name
            if arg in value_long:
                i += 1
        else:
            flags = ""
            for char in arg[1:]:
                flags += char
                if char in value_flags or char in stop_flags:
                    break
            if EXPANDED in flags:
                raise Unclear(EXPANSION)
            yield "short", flags
            if flags[-1:] in tuple(value_flags) and len(flags) == len(arg) - 1:
                i += 1


def current_branch(cwd, location):
    """Return the checked-out branch, using only the location options of the git command."""
    if cwd is None or any(EXPANDED in arg for arg in location):
        return None
    try:
        result = run_git([*location, "symbolic-ref", "--quiet", "--short", "HEAD"], cwd)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def push_config(cwd, location):
    """Return (remote push mappings exist, push.followTags is on), or None when config is unknown.

    location carries the command's -C, --git-dir, --work-tree, -c, and --config-env
    options, so git reports the configuration the push itself would use.
    """
    if cwd is None or any(EXPANDED in arg for arg in location):
        return None
    found = []
    # git config exits 1 when the key is unset; --type=bool applies Git's own boolean rules.
    for query in (["--get-regexp", r"^remote\..*\.push$"], ["--type=bool", "--get", "push.followTags"]):
        try:
            result = run_git([*location, "config", *query], cwd)
        except (OSError, subprocess.SubprocessError):
            return None
        if result.returncode == 1 and not result.stdout:
            found.append(False)
        elif result.returncode == 0:
            found.append(query[0] == "--get-regexp" or result.stdout.strip() == "true")
        else:
            return None
    return tuple(found)


def push_reason(args, cwd, location, head_moves, config_unknown=False):  # noqa: C901
    if args in (["-h"], ["--help"]):
        return None
    parsed = list(options(args, value_flags="o", value_long=PUSH_VALUE_LONG))
    dry_run = False
    for kind, arg in parsed:
        if kind == "long" and arg in ("--dry-run", "--no-dry-run"):
            dry_run = arg == "--dry-run"
        elif kind == "short" and "n" in arg:
            dry_run = True
    if dry_run:
        return None
    longs = [arg for kind, arg in parsed if kind == "long"]
    shorts = "".join(arg for kind, arg in parsed if kind == "short")
    if "--no-verify" in longs:
        return HOOKS
    if {"--force", "--force-with-lease", "--mirror"} & set(longs) or "f" in shorts:
        return FORCE
    if {"--tags", "--follow-tags"} & set(longs):
        return RELEASE
    if set(longs) - SAFE_PUSH_LONG or set(shorts) - SAFE_PUSH_SHORT:
        return PUSH_OPTIONS
    config = None if config_unknown else push_config(cwd, location)
    positional = [arg for kind, arg in parsed if kind == "arg"]
    refspecs = positional if "--repo" in longs else positional[1:]
    if not refspecs:
        return PUSH_EXPLICIT
    for refspec in refspecs:
        if refspec.startswith("+"):
            return FORCE
        if refspec == "tag" or "refs/tags/" in refspec:
            return RELEASE
        # ":" alone pushes every matching branch; braces and globs expand to other refs.
        if EXPANDED in refspec or set(refspec) & set("*?[{}"):
            return PUSH_EXPLICIT
        destination = refspec.rsplit(":", 1)[-1]
        if not destination:
            return PUSH_EXPLICIT
        if destination in ("HEAD", "@"):
            destination = None if head_moves else current_branch(cwd, location)
            if destination is None:
                return PUSH_EXPLICIT
        if MAIN_REF.match(destination):
            return MAIN
        # A remote.<name>.push mapping can send a source-only refspec somewhere else.
        if ":" not in refspec and (config is None or config[0]):
            return PUSH_EXPLICIT
    if "--no-follow-tags" not in longs and (config is None or config[1]):
        return FOLLOW_TAGS
    return None


def git_command(args):
    """Return the subcommand index, repository and config options, and any -c hook override reason."""
    i, location, reason = 0, [], None
    while i < len(args) and args[i].startswith("-"):
        option, value = args[i], args[i + 1] if i + 1 < len(args) else ""
        if option.startswith("--config-env="):
            value = option.split("=", 1)[1]
        if option in ("-c", "--config-env") or option.startswith("--config-env="):
            if EXPANDED in value:
                raise Unclear(EXPANSION)
            if value.lower().startswith("core.hookspath"):
                reason = HOOKS
        if EXPANDED in option.split("=", 1)[0]:
            raise Unclear(EXPANSION)
        if option in ("-C", "--git-dir", "--work-tree", "-c", "--config-env"):
            location += [option, value]
        elif option.startswith(("--git-dir=", "--work-tree=", "--config-env=")):
            location.append(option)
        i += 2 if option in GIT_VALUE_OPTIONS else 1
    if i < len(args) and EXPANDED in args[i]:
        raise Unclear(EXPANSION)
    return i, location, reason


def long_prefix(arg, option, shortest):
    """Git accepts any unambiguous prefix of a long option; shortest is the first unambiguous length."""
    return len(arg) >= shortest and option.startswith(arg)


def config_reads(rest):
    # Read operations are options or the first operand; a value such as "get" is not one.
    # Everything after "--" is an operand, even when it starts with "-".
    end = rest.index("--") if "--" in rest else len(rest)
    operands = [arg for arg in rest[:end] if not arg.startswith("-")] + rest[end + 1:]
    flags = {arg.split("=", 1)[0] for arg in rest[:end] if arg.startswith("-")}
    # "git config <key>" with no value or write option is the legacy read form.
    single_key = len(operands) == 1 and "." in operands[0] and not flags & CONFIG_WRITE_OPTIONS
    return bool(flags & CONFIG_READ_OPTIONS or operands[:1] and operands[0] in CONFIG_READ_COMMANDS or single_key)


def writes_config(name, args):
    """Whether a git config or git remote command may change push settings."""
    if name != "git":
        return False
    i = git_command(args)[0]
    if i >= len(args):
        return False
    command, rest = args[i], args[i + 1:]
    if command == "config":
        return not config_reads(rest)
    operands = [arg for arg in rest if not arg.startswith("-")]
    return command == "remote" and bool(operands) and operands[0] not in ("show", "get-url")


def moves_head(name, args):
    if name == "git":
        i = git_command(args)[0]
        if i >= len(args):
            return False
        command, rest = args[i], args[i + 1:]
        if command == "branch":
            # Only a rename of the current branch changes the name HEAD points to.
            return any(long_prefix(arg, "--move", 4) or re.match(r"^-[^-]*[mM]", arg) for arg in rest)
        if command == "symbolic-ref":
            operands = [arg for arg in rest if not arg.startswith("-")]
            return len(operands) > 1 or any(arg == "-d" or long_prefix(arg, "--delete", 3) for arg in rest)
        return command in BRANCH_CHANGES
    return name == "gh" and gh_command(args)[:2] == ["pr", "checkout"]


def git_reason(args, cwd, head_moves=False, config_unknown=False, env_config=False):
    i, location, reason = git_command(args)
    if reason or i >= len(args):
        return reason
    command, rest = args[i], args[i + 1:]
    if env_config and command in {"commit", "push", *HOOK_COMMANDS}:
        return ENV_CONFIG
    if command == "push":
        return push_reason(rest, cwd, location, head_moves, config_unknown)
    if command == "config":
        if any(arg.lower() == "core.hookspath" for arg in rest) and not config_reads(rest):
            return HOOKS
    if command == "commit":
        parsed = options(rest, value_flags="mFcCt", stop_flags="uS", value_long=COMMIT_VALUE_LONG)
        if any(kind == "long" and long_prefix(arg, "--no-verify", 3) or kind == "short" and "n" in arg
               for kind, arg in parsed):
            return HOOKS
    elif command in HOOK_COMMANDS:
        if any(kind == "long" and long_prefix(arg, "--no-verify", 3) for kind, arg in options(rest)):
            return HOOKS
    return None

def read_input(name, cwd):
    """Read a file gh will send, or refuse when the hook cannot see it."""
    path = Path(name)
    # Device paths such as /dev/stdin read data the hook cannot see, like "-" does.
    device = re.match(r"^/(?:dev|proc)/", os.path.normpath(str(Path(cwd or "/") / path)))
    if name == "-" or EXPANDED in name or device or not path.is_absolute() and cwd is None:
        raise Unclear
    try:
        return (Path(cwd or "/") / path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        raise Unclear from None


def graphql_query(flag, value, cwd):
    key, _, text = value.partition("=")
    if key != "query":
        return ""
    if flag in ("-F", "--field") and text.startswith("@"):
        return read_input(text[1:], cwd)
    return text


def mutation_fields(document):
    """Return the root fields of every mutation in a GraphQL document.

    Returns None when a mutation's root selection uses a fragment, because the
    fields it adds are not visible here.
    """
    tokens = [token for token in GRAPHQL_TOKEN.findall(document)
              if token != "," and not token.startswith(("#", '"'))]
    fields, i, parens = [], 0, 0
    while i < len(tokens):
        # Variable definitions and directive arguments may hold "{" or a $mutation name.
        parens += {"(": 1, ")": -1}.get(tokens[i], 0)
        if not parens and tokens[i] in ("query", "subscription", "fragment"):
            # Skip the operation or fragment name, which may itself be "mutation".
            i += 2 if re.match(r"[A-Za-z_]", tokens[i + 1] if i + 1 < len(tokens) else "") else 1
            continue
        if parens or tokens[i] not in ("mutation", "{"):
            i += 1
            continue
        mutation = tokens[i] == "mutation"
        while i < len(tokens) and (tokens[i] != "{" or parens):
            parens += {"(": 1, ")": -1}.get(tokens[i], 0)
            i += 1
        depth = parens = 0
        while i < len(tokens):
            token = tokens[i]
            i += 1
            if token == "{":
                depth += 1
            elif token == "}":
                depth -= 1
                if depth == 0:
                    break
            elif token == "(":
                parens += 1
            elif token == ")":
                parens -= 1
            elif mutation and depth == 1 and parens == 0:
                if token == "...":
                    return None
                if token == "@":
                    i += 1
                elif re.match(r"[A-Za-z_]", token) and tokens[i:i + 1] != [":"]:
                    fields.append(token)
    return fields


def graphql_documents(fields, body, cwd):
    documents = [graphql_query(flag, value, cwd) for flag, value in fields]
    if any(EXPANDED in document for document in documents):
        raise Unclear(EXPANSION)
    if body is not None:
        text = read_input(body, cwd)
        try:
            decoded = json.loads(text)
        except ValueError:
            decoded = None
        if isinstance(decoded, (dict, list)):
            items = decoded if isinstance(decoded, list) else [decoded]
            text = "\n".join(str(item.get("query", "")) for item in items if isinstance(item, dict))
        documents.append(text)
    return documents

def review_events(fields, body, cwd):
    """Return the review events a request sets; an empty string means none, which leaves it pending."""
    events = set()
    for flag, value in fields:
        key, _, text = value.partition("=")
        if key == "event":
            if flag in ("-F", "--field") and text.startswith("@"):
                raise Unclear
            events.add(text.upper())
    if body is not None:
        try:
            decoded = json.loads(read_input(body, cwd))
        except ValueError:
            raise Unclear from None
        if not isinstance(decoded, dict):
            raise Unclear
        events.add(str(decoded.get("event", "")).upper())
    return events or {""}


def json_unescape(text):
    """Decode JSON and GraphQL string escapes such as \\u003c and \\n, leaving other text alone."""
    return JSON_ESCAPE.sub(lambda m: chr(int(m.group(1), 16)) if m.group(1) else JSON_SIMPLE[m.group(2)], text)


def names_marker(text):
    """Whether text, or any form GitHub or a browser could decode it to, contains MARKER_WORD.

    The decoders (JSON and GraphQL escapes, URL encoding, HTML entities) are applied repeatedly in
    every order, up to four layers deep, and the match ignores case.
    """
    forms, frontier = {text}, [text]
    for _ in range(4):
        frontier = [decoded for value in frontier for decode in (json_unescape, unquote_plus, html.unescape)
                    if (decoded := decode(value)) not in forms and not forms.add(decoded)]
    return any(MARKER_WORD in form.casefold() for form in forms)


def marker_reason(texts, files, cwd, alone, ansi_c=False):
    """Return why a comment write is refused, or None. texts are the literal arguments gh sends and
    files the files it reads. Outside the coordinator worktree, a body the guard cannot read literally
    is refused like a marker: an expansion, a $'...' string (ansi_c says the command line has one),
    stdin, an unreadable file, or a file another command in the same line could change first."""
    if coordinator_worktree(cwd):
        return None
    if ansi_c or any(EXPANDED in text for text in texts) or files and not alone:
        return UNREAD_BODY
    try:
        bodies = [read_input(name, cwd) for name in files]
    except Unclear:
        return UNREAD_BODY
    return MARKER if any(names_marker(text) for text in texts + bodies) else None


def body_files(args):
    """Return the files that gh pr comment or gh issue comment reads with -F or --body-file."""
    files = []
    for i, arg in enumerate(args):
        if arg in ("-F", "--body-file") and i + 1 < len(args):
            files.append(args[i + 1])
        elif arg.startswith("--body-file="):
            files.append(arg.split("=", 1)[1])
        elif arg.startswith("-F") and len(arg) > 2:
            files.append(arg[2:])
    return files


def without_repo_flags(args):
    kept, i = [], 0
    while i < len(args):
        arg = args[i]
        if arg in ("-R", "--repo"):
            i += 2
            continue
        if not (arg.startswith("--repo=") or arg.startswith("-R") and len(arg) > 2):
            kept.append(arg)
        i += 1
    return kept


def gh_command(args):
    """Return gh arguments with the command path first and the options inside it removed.

    gh accepts options before and between command words, so `gh --help=false variable set`
    still runs `variable set`. The path is one word, or two for a group in GH_GROUPS. Inside
    it, only -R/--repo and the GH_PATH_FLAGS, with an optional =value, are skipped. Any other
    option could take the next word as its value, so the path is unknown and gh_command refuses.
    An expansion anywhere before the path ends, including a skipped option's value, is refused:
    unquoted, it splits into more words, so `gh --help=$x` can supply the whole path.
    """
    path, i = [], 0
    while i < len(args) and len(path) < (2 if path[:1] and path[0] in GH_GROUPS else 1):
        arg = args[i]
        i += 1
        if EXPANDED in arg:
            raise Unclear(EXPANSION)
        if not arg.startswith("-"):
            path.append(arg)
        elif arg in ("-R", "--repo"):
            if EXPANDED in "".join(args[i:i + 1]):
                raise Unclear(EXPANSION)
            i += 1
        elif arg.startswith(("--repo=", "-R")):
            continue
        elif arg.split("=", 1)[0] not in GH_PATH_FLAGS:
            raise Unclear(GH_OPTIONS)
    return path + without_repo_flags(args[i:])


def gh_reason(args, cwd, alone=True, marker=False, ansi_c=False):  # noqa: C901
    """Return why a gh command is refused. marker says the command text holds a review marker."""
    args = gh_command(args)
    if any(EXPANDED in arg for arg in args[:2]):
        raise Unclear(EXPANSION)
    if args[:1] == ["help"]:
        return None
    release_write = (args[:1] == ["release"] and args[1:2] and not args[1].startswith("-")
                     and args[1] not in RELEASE_READS)
    if args[:2] == ["pr", "merge"] or release_write:
        if args[2:] and all(arg in ("--help", "-h") for arg in args[2:]):
            return None
        return MERGE if args[0] == "pr" else RELEASE
    if args[:1] in (["extension"], ["extensions"], ["ext"]) and args[1:2] and args[1] in ("install", "upgrade"):
        return None if args[2:] and all(arg in ("--help", "-h") for arg in args[2:]) else EXTENSION
    # An alias could expand to any command, including a merge the guard refuses.
    if args[:1] == ["alias"] and args[1:2] and args[1] in ("set", "import"):
        return None if args[2:] and all(arg in ("--help", "-h") for arg in args[2:]) else ALIAS
    if args[:1] == ["variable"]:
        # Only reads and help are allowed; any other form, including options before the subcommand, may write.
        if not args[1:] or args[1] in VARIABLE_READS or all(arg in ("--help", "-h") for arg in args[1:]):
            return None
        return VARIABLE
    if args[:2] in COMMENT_COMMANDS:
        if marker and not coordinator_worktree(cwd):
            return MARKER
        return marker_reason(args[2:], body_files(args[2:]), cwd, alone, ansi_c)
    if args[:1] != ["api"]:
        return None
    method, endpoint, fields, body, i = None, None, [], None, 1
    while i < len(args):
        arg = args[i]
        i += 1
        if arg.startswith(EXPANDED):
            raise Unclear(EXPANSION)
        if arg.startswith("--"):
            name, attached, value = arg.partition("=")
            if EXPANDED in name:
                raise Unclear(EXPANSION)
        elif arg.startswith("-") and arg != "-":
            name = None
            for j, flag in enumerate(arg[1:], 2):
                if flag in GH_SHORT_VALUE:
                    name, value = "-" + flag, arg[j:]
                    break
                if flag not in GH_SHORT_BOOLEAN:
                    raise Unclear
            if name is None:
                continue
            attached = value
        else:
            endpoint = arg if endpoint is None else endpoint
            continue
        if not attached and name in GH_VALUE_OPTIONS:
            value = args[i] if i < len(args) else ""
            i += 1
        if name in ("-X", "--method"):
            method = value
        elif name in ("-f", "-F", "--field", "--raw-field"):
            if EXPANDED in value.partition("=")[0]:
                raise Unclear(EXPANSION)
            fields.append((name, value))
        elif name == "--input":
            body = value
    if endpoint is None:
        return None
    # gh sends POST when the request has fields or a body.
    method = (method or ("POST" if fields or body is not None else "GET")).upper()
    if method in ("GET", "HEAD"):
        return None
    if marker and not coordinator_worktree(cwd):
        return MARKER
    # The hook reads request files before the command runs. Any other command, such as a
    # checkout, redirect, or directory change, could replace what gh reads.
    reads_file = body is not None or any(flag in ("-F", "--field") and value.partition("=")[2].startswith("@")
                                         for flag, value in fields)
    if reads_file and not alone:
        return STALE_INPUT
    path = urlsplit(endpoint).path.strip("/")
    graphql = path == "graphql" or path.endswith("/graphql")
    if graphql:
        names = [mutation_fields(document) for document in graphql_documents(fields, body, cwd)]
        comment_write = any(found is None or COMMENT_MUTATIONS & set(found) for found in names)
    else:
        comment_write = bool(COMMENT_WRITES.search(path) or REVIEW_WRITES.search(path))
    if comment_write:
        sent_files = [value.partition("=")[2][1:] for flag, value in fields
                      if flag in ("-F", "--field") and value.partition("=")[2].startswith("@")]
        reason = marker_reason([value for _, value in fields], sent_files + ([] if body is None else [body]),
                               cwd, alone, ansi_c)
        if reason:
            return reason
        # GitHub parses a REST request body as JSON; one the guard cannot parse is not read literally.
        if body is not None and not graphql and not coordinator_worktree(cwd):
            try:
                json.loads(read_input(body, cwd))
            except (Unclear, ValueError):
                return UNREAD_BODY
    if graphql:
        for document in graphql_documents(fields, body, cwd):
            names = mutation_fields(document)
            if names is None or set(names) - ALLOWED_MUTATIONS:
                return API_WRITE
        return None
    if method == "POST" and REVIEW_WRITES.search(path):
        return None if review_events(fields, body, cwd) <= {"", "COMMENT"} else API_WRITE
    if method in ("POST", "PATCH") and COMMENT_WRITES.search(path):
        return None
    return API_WRITE


def coordinator_merge(args, cwd):
    """Whether args is the one merge the coordinator may run.

    The form is gh pr merge [<number>] --squash --match-head-commit <full sha>, with no other
    option, run from the coordinator worktree (see coordinator_worktree).
    """
    if args[:3] != ["gh", "pr", "merge"] or cwd is None or os.environ.get("GIT_DIR") or os.environ.get("GIT_WORK_TREE"):
        return False
    number, squash, sha, rest = None, False, None, args[3:]
    while rest:
        arg = rest.pop(0)
        if arg == "--squash" and not squash:
            squash = True
        elif arg == "--match-head-commit" and sha is None and rest:
            sha = rest.pop(0)
        elif arg.startswith("--match-head-commit=") and sha is None:
            sha = arg.split("=", 1)[1]
        elif re.fullmatch(r"[1-9][0-9]*", arg) and number is None:
            number = arg
        else:
            return False
    return bool(squash and sha and FULL_SHA.match(sha)) and coordinator_worktree(cwd)


def coordinator_worktree(cwd):
    """Whether cwd sits inside the worktree that PP_COORDINATOR_WORKTREE names.

    That root must be the Git top level of cwd and lie outside the lane worktrees.
    Lane hook configs never set the variable.
    """
    root = os.environ.get(COORDINATOR_ENV, "")
    if cwd is None or not os.path.isabs(root):
        return False
    try:
        top = run_git(["rev-parse", "--show-toplevel"], cwd, check=True).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return False
    root, lanes = os.path.realpath(root), os.path.realpath(LANE_WORKTREES)
    return bool(top) and os.path.realpath(top) == root and os.path.commonpath([root, lanes]) != lanes


def shell_script(args):
    """Return (script, unclear) for a sh, bash, zsh, or dash invocation.

    script is what the shell runs with -c, or None. unclear is True when an option before the
    script is not one the guard knows, so it cannot tell which argument is the script.
    """
    reads_script, i = False, 0
    while i < len(args):
        arg = args[i]
        if arg in SHELL_VALUE_LONG:
            i += 2
        elif arg in SHELL_FLAG_LONG:
            i += 1
        elif arg == "--":
            i += 1
            break
        elif arg.startswith("--"):
            return None, True
        elif re.fullmatch(r"[-+][A-Za-z]+", arg):
            flags = arg[1:]
            # o and O take the next argument; anywhere but last in a bundle, the guard cannot place it.
            if any(flag in "oO" for flag in flags[:-1]):
                return None, True
            reads_script = reads_script or (arg[0] == "-" and "c" in flags)
            i += 2 if flags[-1] in "oO" else 1
        elif arg.startswith(("-", "+")) and arg not in ("-", "+"):
            return None, True
        else:
            break
    return (args[i] if reads_script and i < len(args) else None), False


def sourced_reason(name, rest, cwd):
    """Return why an eval or source command is refused, or None. Both run text the guard cannot
    classify as commands, so any git or gh in it, or text it cannot read, is refused."""
    if name == "eval":
        text = " ".join(rest)
    else:
        try:
            text = read_input(rest[0], cwd) if rest else ""
        except Unclear:
            return EVAL
    return EVAL if EXPANDED in text or re.search(r"\b(?:gh|git)\b", text) else None


def unquoted(text):
    """Return text without quote characters, backslashes, or the expansion marker, for word matching."""
    return re.sub(r"['\"\\]", "", text.replace(EXPANDED, " "))


def names_merge(text):
    """Whether text names gh and a PR merge once quotes and backslashes are removed."""
    text = unquoted(text)
    return bool(re.search(r"\bgh\b", text) and NAMED_MERGE.search(text))


def skip_gh_options(args, j):
    """Return the index of the first word at or after j that is not a gh option or an option's value."""
    while j < len(args) and args[j].startswith("-"):
        j += 2 if args[j] in GH_VALUE_FLAGS else 1
    return j


def merge_tokens(args, api=False, first_only=False):
    """Whether the words hold gh, then pr and merge, with only gh options between them.

    With api, a gh api call that names a merge endpoint or mutation counts too. With first_only, gh must be the
    first word. A word of data that only contains the text, such as a comment body or a commit message, is one
    word and never matches.
    """
    for i, word in enumerate(args[:1] if first_only else args):
        if PurePosixPath(word).name != "gh":
            continue
        j = skip_gh_options(args, i + 1)
        if j < len(args) and args[j] == "pr":
            k = skip_gh_options(args, j + 1)
            if k < len(args) and args[k] == "merge":
                return True
        if api and j < len(args) and args[j] == "api" and any(MERGE_WORD.search(arg) for arg in args[j + 1:]):
            return True
    return False


def command_strings(name, rest):
    """Return the arguments of this command that another shell runs as a command line."""
    if name == "alias":
        return [arg.split("=", 1)[1] for arg in rest if "=" in arg]
    if name == "git":
        values = [arg.split("=", 1)[1] for arg in rest if re.match(r"^alias\.[^=\s]+=", arg)]
        values += [rest[i + 1] for i, arg in enumerate(rest[:-1]) if re.fullmatch(r"alias\.\S+", arg)]
        return [value[1:] if value.startswith("!") else value for value in values]
    if name == "entr" and not any(re.fullmatch(r"-[A-Za-z]*s[A-Za-z]*", arg) for arg in rest):
        return []
    if name in STRING_RUNNERS:
        return [arg for arg in rest if re.search(r"\s", arg)]
    return []


def pr_merge_after(args):
    """Whether args, the words after an unknown program, read as gh's pr merge or a gh api merge."""
    j = skip_gh_options(args, 0)
    if j < len(args) and args[j] == "pr":
        k = skip_gh_options(args, j + 1)
        return k < len(args) and args[k] == "merge"
    return j < len(args) and args[j] == "api" and any(MERGE_WORD.search(arg) for arg in args[j + 1:])


def code_merge(code):
    """Whether interpreter code holds gh, pr and merge as words in that order, with only gh options between."""
    return merge_tokens(CODE_WORD.findall(code.replace(EXPANDED, " ")), api=True)


# How deep hidden_merge follows nested shells, runners, and alias values before it stops reading.
NESTING_LIMIT = 6
# How many hidden_merge visits, and how many seconds, one command line may take. Past either the guard refuses
# the line as too complex, so a long line cannot keep the hook running past its 10-second limit, where the hook
# runner would let the command through.
STEP_BUDGET = 5000
CHECK_SECONDS = 2.0
TOO_COMPLEX = ("This command line is too complex for the guard to check in time; split it into simpler "
               "commands.")


# Longer command lines are refused before any parsing; agents never need one this long.
MAX_COMMAND_BYTES = 64 * 1024
TOO_LONG = "This command line is too long for the guard to check; split it into shorter commands."
# The deadline of the command line being checked, read by the parser's loops; None outside check_command.
LINE_DEADLINE = None
# git results for the command line being checked, keyed by (cwd, git arguments, environment override); None
# outside check_command. A git config write or a branch change on the line already makes the guard treat config
# and HEAD as unknown, so a cached answer is never used after the line could have changed it.
LINE_GIT = None
GIT_SECONDS = 5


class TooComplex(Exception):
    pass


def merge_state():
    """Return the per-line state every check of one command line shares: the memo, the visit count, and the
    deadline. check_command makes it once for the top-level line and passes it to every nested script."""
    return {"memo": {}, "lists": {}, "steps": 0, "deadline": time.monotonic() + CHECK_SECONDS}


PARSER_TICKS = 0


def tick():
    """Count one parser loop step and check the line's deadline every 256 steps."""
    global PARSER_TICKS
    PARSER_TICKS += 1
    if PARSER_TICKS % 256 == 0:
        check_deadline()


def run_git(args, cwd, env=None, check=False):
    """Run git with args in cwd like subprocess.run, cached for the line and bounded by its deadline.

    The timeout is GIT_SECONDS or the time left on the line, whichever is shorter. Past the deadline, or when git
    runs out the line's time, it raises TooComplex.
    """
    key = (cwd, tuple(args), None if env is None else tuple(sorted(env.items())))
    if LINE_GIT is not None and key in LINE_GIT:
        result = LINE_GIT[key]
    else:
        timeout = GIT_SECONDS
        if LINE_DEADLINE is not None:
            timeout = min(timeout, LINE_DEADLINE - time.monotonic())
            if timeout <= 0:
                raise TooComplex(TOO_COMPLEX)
        try:
            result = subprocess.run(["git", *args], cwd=cwd, env=env, capture_output=True, text=True,
                                    timeout=timeout, check=False)
        except subprocess.TimeoutExpired:
            if LINE_DEADLINE is not None and time.monotonic() >= LINE_DEADLINE:
                raise TooComplex(TOO_COMPLEX) from None
            raise
        if LINE_GIT is not None:
            LINE_GIT[key] = result
    if check and result.returncode != 0:
        raise subprocess.CalledProcessError(result.returncode, ["git", *args], result.stdout, result.stderr)
    return result


def check_deadline():
    """Raise TooComplex once the deadline of the command line being checked has passed."""
    if LINE_DEADLINE is not None and time.monotonic() > LINE_DEADLINE:
        raise TooComplex(TOO_COMPLEX)


def hidden_merge(words, state, depth=0, strict=False, scan=True, start=0):
    """Whether args runs a PR merge: directly, through a runner the guard does not unwrap, as interpreter code,
    as a shell's -c script, or as the command line inside an alias value or a runner's command string.

    An expansion or substitution in program position followed by pr merge, as in $(echo gh) pr merge, counts:
    the guard cannot tell which program it names. strict is set inside a runner or an alias value, where the
    guard's own gh checks never run, so a gh api merge counts there too.

    The guard looks for gh pr merge only where a program runs: in program position after the wrappers
    program() unwraps, in the arguments of a runner, in -c or -e code, and in alias values. The arguments of an
    ordinary program, as in echo gh pr merge 12, are data. Help is decided for each simple command, as main's
    gh_reason does: gh pr merge followed only by --help or -h is help.

    scan is False when a runner's own suffix loop reached these words: the loop already tries every later
    word as a program, so a runner found here does not scan its words again. Raises TooComplex past the step
    budget or the deadline.

    The words checked are words[start:]. state is the line's merge_state(); every caller passes the one its
    command line started with, so no call starts a fresh budget.
    """
    # Every visit counts, memo hits and depth-limit visits included, before any other work.
    state["steps"] += 1
    if state["steps"] > STEP_BUDGET or time.monotonic() > state["deadline"]:
        raise TooComplex(TOO_COMPLEX)
    if depth >= NESTING_LIMIT:
        # Past the limit the guard stops reading, so it fails closed: text that still names gh, after quotes and
        # backslashes are removed, or holds an expansion counts as a merge.
        raw = " ".join(words[start:])
        return EXPANDED in raw or bool(re.search(r"\bgh\b", unquoted(raw)))
    # One result per position (the word list, the start index, depth, strict, scan). The key holds no copy of the
    # words, so the memo stays small however long the line is; "lists" keeps each list alive so its id stays
    # unique while the line is checked.
    key = (id(words), start, depth, strict, scan)
    if key in state["memo"]:
        return state["memo"][key]
    state["lists"][id(words)] = words
    state["memo"][key] = result = merge_in(words[start:], depth, strict, state, scan)
    return result


def merge_in(args, depth, strict, state, scan):
    """The body of hidden_merge for one (words, depth, strict, scan); hidden_merge adds the memo and the limits."""
    name, rest, *_ = program(args)
    if name is None:
        return False
    if EXPANDED in name:
        return pr_merge_after(rest)
    if name == "gh":
        return not merge_help(["gh", *rest]) and merge_tokens(["gh", *rest], api=strict, first_only=True)
    if name in INTERPRETERS:
        return any(names_merge(arg) or code_merge(arg) for arg in rest)
    strings = command_strings(name, rest)
    # Alias and git alias values and runner strings are commands no gh check sees.
    inner_strict = strict or bool(strings)
    if name in SHELLS:
        script, unclear = shell_script(rest)
        if unclear:
            return names_merge(" ".join(rest))
        strings = [script] if script is not None else []
    if name in UNWRAPPED_RUNNERS:
        raw = " ".join(rest)
        # The parser marks every expansion and substitution; a literal $ or backtick in single quotes is data.
        if EXPANDED in raw and re.search(r"\bgh\b", unquoted(raw)):
            return True
        # Each word could be where the runner's program starts (after xargs -n 1, flock FILE, find -exec), so
        # each is tried in program position: gh itself, a wrapper, a shell's -c, or interpreter code. The words
        # after an ordinary program, such as a grep pattern, stay data. One pass covers every later word, so a
        # runner met in it does not scan again (scan=False); its strings and expansions are still checked.
        if scan and any(hidden_merge(rest, state, depth + 1, True, scan=False, start=i) for i in range(len(rest))
                        if not rest[i].startswith("-")):
            return True
    for text in strings:
        # The same command string often repeats, as in several identical sh -c scripts; its result is kept by
        # its text, which the key refers to without copying.
        key = ("text", text, depth, inner_strict)
        if key not in state["memo"]:
            try:
                inner = shell_commands(text)
            except (Unclear, ValueError):
                state["memo"][key] = names_merge(text)
            else:
                state["memo"][key] = any(hidden_merge(words, state, depth + 1, inner_strict) for words, *_ in inner)
        if state["memo"][key]:
            return True
    return False


def expanded_program(args):
    """Whether the program this command runs comes from an expansion, or a runner gets an expanded argument.

    A runner such as flock FILE, watch -n 1, xargs -n 1, or ssh host takes options and operands before the
    program it runs, so any expanded argument of a runner counts.
    """
    name, rest, *_ = program(args)
    if name is None:
        return False
    if EXPANDED in name:
        return True
    return name in UNWRAPPED_RUNNERS and any(EXPANDED in arg for arg in rest)


def merge_help(args):
    """Whether args is gh pr merge asking only for help, after gh_command removes the options in its path."""
    if args[:1] != ["gh"]:
        return False
    try:
        words = gh_command(args[1:])
    except Unclear:
        return False
    return words[:2] == ["pr", "merge"] and bool(words[2:]) and all(arg in ("--help", "-h") for arg in words[2:])


def assigns_gh(words):
    """Whether one of the assignment words gives a variable a value whose first word is gh."""
    for word in words:
        value = unquoted(word.split("=", 1)[1]).split() if "=" in word else []
        if value and PurePosixPath(value[0]).name == "gh":
            return True
    return False


def check_command(command, cwd, outer=None, state=None):
    """Return why a shell command run in cwd is refused, or None to allow it.

    outer is (head_moves, config_unknown, env_config) from the line that runs this command as a
    sh -c script. The script inherits them: an assignment, env option, or branch change on the
    outer line applies to every git command inside it. state is the top-level line's merge_state(),
    so the step budget and the deadline cover the whole line, nested scripts included.
    """
    global LINE_DEADLINE, LINE_GIT
    if len(command.encode("utf-8", "surrogatepass")) > MAX_COMMAND_BYTES:
        return TOO_LONG
    if state is not None:
        return check_line(command, cwd, outer, state)
    state = merge_state()
    LINE_DEADLINE, LINE_GIT = state["deadline"], {}
    try:
        return check_line(command, cwd, outer, state)
    except RecursionError:
        # Thousands of nested substitutions exhaust the parser's recursion; refuse with the same reason.
        return TOO_COMPLEX
    finally:
        LINE_DEADLINE = LINE_GIT = None


def check_line(command, cwd, outer, state):
    """The body of check_command for one line or nested script, sharing the top-level line's state."""
    # The kernel follows a symlink before applying the ".." after it, so a lexical cd below would
    # collapse it wrongly. Other paths stay as given, matching the shell's logical $PWD.
    if cwd and ".." in PurePosixPath(cwd).parts:
        cwd = os.path.realpath(cwd)
    # Defence in depth: a marker can reach GitHub through a wrapper, an interpreter, or a file
    # written now and posted later, so any mention of the word is refused outside the coordinator.
    if names_marker(command) and not coordinator_worktree(cwd):
        return MARKER
    try:
        parsed = shell_commands(command)
        # Only a lone command with nothing around it qualifies; a wrapper, cd, or substitution could change the target.
        # A script run by a wrapper never qualifies, since the wrapper's line could change the target.
        if outer is None and len(parsed) == 1 and not any(parsed[0][1:]) and coordinator_merge(parsed[0][0], cwd):
            return None
        commands = [(*program(args), scoped, conditional) for args, scoped, conditional in parsed]
        assigned = [word for _, _, _, words, *_ in commands for word in words]
        # Substitutions run out of source order, so any branch change makes HEAD unknown.
        # An exported GIT_DIR or similar can also point a later push at another repository.
        head_moves = (any(moves_head(name, rest) for name, rest, *_ in commands)
                      or any(GIT_LOCATION.match(word) for word in assigned))
        env_config = any(GIT_CONFIG_ENV.match(word) for word in assigned)
        marker = names_marker(command)
        # The guard reads push settings before any of these commands run.
        config_unknown = head_moves or any(writes_config(name, rest) for name, rest, *_ in commands)
        if outer is not None:
            head_moves, config_unknown, env_config = (head_moves or outer[0], config_unknown or outer[1],
                                                      env_config or outer[2])
        control = 0
        for (args, *_), (name, rest, moved, _, scoped, conditional) in zip(parsed, commands):
            check_deadline()
            # Prefixes such as "{" or "!" can come before a control keyword.
            leading = list(itertools.takewhile(lambda word: word in SHELL_PREFIXES | {"for", "case", "select"}, args))
            control += sum(word in ("if", "while", "until", "for", "case", "select") for word in leading)
            control -= args[0] in ("fi", "done", "esac")
            if name in ("cd", "pushd", "popd"):
                targets = [arg for arg in rest if not arg.startswith("-")]
                # A directory change that may be skipped leaves no single directory after it.
                # cd -P resolves symlinks, so only a bare cd has a lexical result.
                plain = (name != "popd" and rest == targets and len(targets) == 1
                         and not (scoped or moved or conditional or control)
                         and not re.search(r"[$~`\x00]", targets[0]))
                # A directory change inside a subshell has no single directory after it.
                cwd = os.path.normpath(os.path.join(cwd, targets[0])) if plain and cwd else None
                continue
            here = None if moved else cwd
            if name in SHELLS:
                script, unclear = shell_script(rest)
                if unclear and re.search(r"\b(?:gh|git)\b", " ".join(rest)) and not coordinator_worktree(here):
                    return SHELL_OPTIONS
                if script is not None:
                    # The script's own substitutions were already classified as nested commands.
                    # Identical scripts on one line, as in several sh -c '<same text>', are checked once.
                    inherited = (head_moves, config_unknown, env_config)
                    key = ("script", script, here, inherited)
                    if key not in state["memo"]:
                        state["memo"][key] = check_command(script.replace(EXPANDED, ""), here, inherited, state)
                    reason = state["memo"][key]
                    if reason:
                        return reason
                    continue
            # program() names "." as "", like any path ending in a slash; both are treated as source.
            if name in ("eval", "source", ""):
                reason = sourced_reason(name, rest, here)
                if reason:
                    return reason
                continue
            reason = git_reason(rest, here, head_moves, config_unknown, env_config) if name == "git" else gh_reason(rest, here, len(parsed) == 1, marker, "$'" in command) if name == "gh" else None
            if reason:
                return reason
        # G=gh; P=pr; M=merge; $G $P $M builds a command the guard cannot read. A line that sets a variable to gh
        # and runs a program named by an expansion is refused; an expansion used as data, as in echo $HOME, is not.
        if assigns_gh(assigned) and any(expanded_program(args) for args, *_ in parsed):
            return SPLIT_GH
        # A PR merge is matched on each command's words after shell unquoting, so me''rge and $'m\\x65rge' still
        # count, while a quoted argument that only mentions the command (a comment body, a commit message, a
        # grep pattern) does not. Nested sh -c scripts reach here through check_command. gh pr merge --help
        # alone stays allowed.
        if any(hidden_merge(args, state) for args, *_ in parsed):
            return MERGE
    except TooComplex:
        return TOO_COMPLEX
    except Unclear as error:
        return str(error) or UNCLEAR
    except ValueError:
        return UNPARSABLE
    return None


def check_paths(paths, cwd=None):
    """Return why editing one of paths is refused, or None to allow every edit. cwd is the hook payload's."""
    for path in paths:
        reason = protected_path(path.strip(), cwd)
        if reason:
            return f"{path}: {reason}"
    return None


def patch_paths(text):
    """Return every file an ApplyPatch-format patch adds, updates, deletes, or moves to."""
    return PATCH_FILE.findall(text)


class InvalidRequest(Exception):
    pass


# Fields each neutral operation accepts; every one is required.
REQUEST_FIELDS = {"command": {"command", "cwd"}, "file": {"path"}, "patch": {"patch"}}


def directory(raw):
    if not os.path.isabs(raw) or not os.path.isdir(raw):
        raise InvalidRequest(f"cwd must be an existing absolute directory: {raw!r}")
    return raw


def check_patch(text):
    paths = patch_paths(text)
    # A diff in another format would name no files here and slip past the path rules.
    if not paths:
        raise InvalidRequest("patch has no '*** Add File:', '*** Update File:', '*** Delete File:', or '*** Move to:' lines")
    return check_paths(paths)


def check_request(request):
    """Check one neutral request object and return the refusal reason, or None to allow it.

    Raises InvalidRequest for anything other than exactly one supported operation
    with string fields, so an adapter that sends the wrong shape is refused.
    """
    if not isinstance(request, dict):
        raise InvalidRequest("expected a JSON object")
    operation = request.get("operation")
    if operation not in REQUEST_FIELDS:
        raise InvalidRequest(f"unsupported operation {operation!r}; use command, file, or patch")
    fields = set(request) - {"operation"}
    if fields != REQUEST_FIELDS[operation]:
        raise InvalidRequest(f"{operation} needs exactly these fields: {', '.join(sorted(REQUEST_FIELDS[operation]))}")
    if not all(isinstance(request[field], str) for field in fields):
        raise InvalidRequest("every field must be a string")
    if operation == "command":
        return check_command(request["command"], directory(request["cwd"]))
    if operation == "file":
        if not request["path"].strip():
            raise InvalidRequest("path is empty")
        return check_paths([request["path"]])
    return check_patch(request["patch"])


def read_text(source):
    if source == "-":
        return sys.stdin.read()
    try:
        return Path(source).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise InvalidRequest(f"cannot read {source}: {error}") from None


def parse_arguments(argv):
    parser = argparse.ArgumentParser(
        prog="agent-guard.py",
        description="Check a shell command, file edit, or patch against Print Partner's agent rules "
                    "before running it. Exit 0 allows it; exit 2 refuses it or rejects the input.")
    operations = parser.add_subparsers(dest="operation", required=True)
    command = operations.add_parser("command", help="a shell command, as one argument or - for stdin")
    command.add_argument("--cwd", default=os.getcwd(), help="directory the command runs in (default: current)")
    command.add_argument("command")
    file = operations.add_parser("file", help="files to create or edit")
    file.add_argument("paths", nargs="+")
    patch = operations.add_parser("patch", help="an ApplyPatch-format patch file, or - for stdin")
    patch.add_argument("source")
    operations.add_parser("json", help='one JSON request on stdin, such as {"operation": "file", "path": "a.md"}')
    return parser.parse_args(argv)


def run(argv):
    args = parse_arguments(argv)
    if args.operation == "command":
        text = read_text(args.command) if args.command == "-" else args.command
        return check_command(text, directory(os.path.join(os.getcwd(), args.cwd)))
    if args.operation == "file":
        if not all(path.strip() for path in args.paths):
            raise InvalidRequest("path is empty")
        return check_paths(args.paths)
    if args.operation == "patch":
        return check_patch(read_text(args.source))
    try:
        request = json.loads(sys.stdin.read())
    except ValueError:
        raise InvalidRequest("stdin is not JSON") from None
    return check_request(request)


def main():
    try:
        reason = run(sys.argv[1:])
    except InvalidRequest as error:
        print(f"pp guard: invalid input: {error}", file=sys.stderr)
        return 2
    # An internal failure must still refuse; a runner may treat a crash as permission.
    except Exception as error:
        print(f"pp guard: internal error: {error!r}", file=sys.stderr)
        return 2
    if reason:
        print(f"pp guard: {reason}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
