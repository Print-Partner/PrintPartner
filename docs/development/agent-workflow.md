# Agent and contributor workflow

This is the shared procedure for any coding agent or person working on Print Partner. It needs a shell, Git, Python 3, and the toolchains in [AGENTS.md](../../AGENTS.md). The repo skills in `.claude/skills/` link to the sections below.

The rules in AGENTS.md apply everywhere in this page.

## Start from a task brief

Notion holds goals and decisions and stays authoritative. Linear tracks execution. [Contributing](contributing.md#sources-of-truth) names both. You don't need Notion or Linear credentials when the task owner gives you an exported brief: a reviewed copy of the ticket and its linked plan, pasted into the prompt, the PR description, or a file outside the checkout.

A brief needs these fields:

| Field | Contents |
| --- | --- |
| Source | Notion page URL, Linear ID if any, and the export date |
| Base | The commit SHA the brief was written against |
| Goal | What the change must achieve, in one or two sentences |
| Acceptance criteria | Checkable conditions for done |
| Invariants | Operation owner, flag modes, and the SQLite, ID, and recovery rules that apply |
| Tests and evidence | Commands to run and screenshots or recordings to attach |
| Status | What is done and the next action |
| Open decisions | Unresolved questions and who decides each one |

The owner copies only what the task needs. Tokens, cookies, pairing secrets, customer files, personal data, and unrelated private pages stay out of the brief. Exports are made by hand and reviewed; nothing syncs Notion into the repository.

A brief is a snapshot. Stop and ask the owner before you continue when:

- a field is missing or says "TBD";
- files the task touches changed on `main` after the base SHA;
- the brief disagrees with the code, [Architecture](../architecture.md), an ADR, or AGENTS.md;
- you can read the Notion page and it differs from the export.

Report the conflict and the facts on each side. The owner decides which wins and updates the source.

When you stop with work in flight or hand it off, post a Handoff comment on the ticket as [Handoffs](#handoffs) describes.

## Handoffs

Any agent or person can take over Print Partner Desktop work: Claude Code, Codex, or you. Everything needed to continue lives in four places, never only in one agent's chat or machine:

| What | Where |
| --- | --- |
| Plan | Notion "Print Partner: execution plan v17.1". Phases P1 to P7, in order. |
| Live status | Notion "Execution status (live)". One row per lane with its PRs, tickets, and next step, plus a Landed list with shas and a "Needs Chad" list. |
| Execution record | Linear, team GRE, project "Print Partner Desktop (Rust core)". The newest **Handoff** comment on a ticket says where its work stands. |
| Code | GitHub `Print-Partner/PrintPartner`. All work in progress is on a pushed branch or an open PR. |

The Notion page [Agent handoff: how to take over any lane](https://app.notion.com/p/4116dd30b698814c9107f5a22b58e6b5) holds the operating rules and Chad's decisions, newest first. Read it before you take over. [Sources of truth](contributing.md#sources-of-truth) says which page wins when they disagree. If it disagrees with this page or the code, stop and ask, as for a [task brief](#start-from-a-task-brief).

### Pick up a ticket

1. Open the ticket in Linear and read its newest comment that starts with **Handoff**.
2. Check out the branch it names at the head sha it names. If the PR has moved past that sha, the PR is the truth.
3. Comment "Claimed by [your agent or thread name]" on the ticket before you change anything.
4. Follow AGENTS.md and this page for setup, checks, and evidence.

### Post a Handoff

Post a Handoff comment on the ticket whenever you stop with work in flight: you pause, end a turn with work still open, or hand the work over. Copy this template word for word and fill in each bracket:

```text
**Handoff** [date] by [agent or thread]
Branch: [name] · PR: #[n] · Head: [full sha] · Base: main
Done: [what is true at this head]
Next: [the next concrete step]
Verification: CI [green, red with the failing check, or pending] · cross-family review [clean at this sha, open findings, or not run] · open threads [n]
Decisions and constraints: [anything a newcomer must not undo, such as a schema step number, or a decision of Chad's with its date]
Blockers: [none, or who must do what]
```

Never leave work only on a machine. Push it to a branch before you stop. If you can't push, describe what is unpushed in the Handoff. Put every decision or constraint you rely on in the Handoff or the PR description; session notes, chat history, and `/tmp` files are gone for the next reader.

Without Linear access, you still follow these steps through the task owner. The owner puts the newest Handoff in your [task brief](#start-from-a-task-brief) and posts your claim on the ticket. When you stop, write your Handoff in the PR description, or give it to the owner if there is no PR, and the owner posts it on the ticket.

## Set up and run checks

AGENTS.md lists the setup and check commands. `just` is a convenience; `python3 scripts/local-ci.py` takes the same arguments as `just ci`. On Linux, the [development container](devcontainer.md) provides the pinned toolchains and native libraries. Don't install system packages on your own machine just to run a check.

Python checks need the pinned packages in `requirements-dev.txt`: Ruff, and PyYAML for the tests that read workflow and fleet YAML. Install them in a disposable virtual environment outside the checkout, never with `sudo` or into the system Python:

```sh
python3 -m venv "${TMPDIR:-/tmp}/pp-dev-venv"
"${TMPDIR:-/tmp}/pp-dev-venv/bin/pip" install -r requirements-dev.txt
PATH="${TMPDIR:-/tmp}/pp-dev-venv/bin:$PATH" python3 scripts/local-ci.py --only local-ci-self-test,python-lint --fail-on-skip
```

Before you push, a normal PR runs `just ci --fast` and lists that command and its summary under Evidence. A sensitive PR, as the [merge gate](definition-of-done.md#merge-gate) defines it, runs the full `just ci`. [Run the required checks with one command](ci.md#run-the-required-checks-with-one-command) says what `--fast` leaves out.

A full local CI run exits 0 even when steps print SKIP, because a tool or platform is missing. A SKIP is not a pass. List each one under Evidence. For steps that must run on your machine, add `--fail-on-skip`:

```sh
python3 scripts/local-ci.py --list
python3 scripts/local-ci.py --only <step-ids> --fail-on-skip
```

`just guidance` checks local links and heading anchors in every tracked Markdown page; checks titles, skill metadata and Git trackability in AGENTS.md, this page and the repo skills; and runs the safety guard tests. The [CI guide](ci.md) maps each hosted check to a local command.

## Safety guard

`scripts/agent-guard.py` checks one shell command, file edit, or patch before it runs. It refuses actions reserved for the maintainer. Exit 0 allows the operation. Exit 2 refuses it, or rejects input it does not understand, with the reason on stderr.

Claude Code runs it through `.claude/hooks/guard.py` after you add the hook settings in [Claude Code](#claude-code). No other runner does. Configure your runner's pre-tool hook to call the guard, or run it yourself before any `git` or `gh` command that pushes, merges, publishes, or changes settings, and before editing environment, Git, or generated files.

```sh
python3 -B scripts/agent-guard.py command --cwd "$PWD" 'git push -u origin HEAD:chore/topic'
python3 -B scripts/agent-guard.py command - < command.sh
python3 -B scripts/agent-guard.py file web/.env.local docs/guide.md
python3 -B scripts/agent-guard.py patch change.patch
```

A hook adapter can send one JSON object on stdin to `python3 -B scripts/agent-guard.py json`. Each operation needs exactly these fields, all strings:

```json
{"operation": "command", "command": "git push origin HEAD:topic", "cwd": "/abs/checkout"}
{"operation": "file", "path": "/abs/checkout/docs/guide.md"}
{"operation": "patch", "patch": "*** Update File: /abs/checkout/a.py\n..."}
```

The guard rejects other operations, missing or extra fields, a relative or missing `cwd`, an empty path, and a patch with no `*** Add File:`, `*** Update File:`, `*** Delete File:`, or `*** Move to:` line. A unified diff has none of those lines, so the guard rejects it rather than leave its paths unchecked. A `cwd` that contains `..` is resolved through symlinks first, as the kernel does when it enters the directory; any other `cwd` is used as given, like the shell's logical `$PWD`. Map your runner's shell tool to `command`, its file writes to `file`, and its patch tool to `patch`; never pass a tool through unchecked.

The guard refuses merges (except the [coordinator's](#coordinator-merges)), force pushes, pushes to `main`, release publishing or editing, tag pushes, and hook bypasses. It also refuses edits to real `.env` files, Git metadata, generated manifests, and the Claude Code settings and hooks that run the guard: `.claude/settings.json`, `.claude/settings.local.json`, and `.claude/hooks/`.

- `git push` needs an explicit branch refspec and common options spelled in full. Write push destinations literally. The guard refuses a destination built from a shell expansion, the `:` matching refspec, and `HEAD` when the same command also switches branches.
- When `push.followTags` is on, or a `remote.<name>.push` mapping exists, add `--no-follow-tags` or write `source:destination` refspecs. The guard treats both settings as unknown after a `git config` write or a `git remote` change in the same command.
- It refuses commits and pushes that run with `GIT_CONFIG_*` environment variables. Git and gh subcommands and option names must be literal.
- Run a `gh api` write that reads a request file (`-F key=@file` or `--input`) as its own command, after writing the file. `cd -P`, directory changes inside conditionals or groups, and a `cd` in a subshell, a pipeline, or under `env -C` leave the directory unknown, so relative `gh api` input files are refused.
- Through `gh api`, agents may read, write PR or issue comments, submit comment-only or pending PR reviews, and reply to or resolve review threads. Other API writes need the maintainer, including writes to Actions variables.
- `gh variable` may only `list`, `get`, or show help. Setting or deleting a variable needs the maintainer.
- Outside the [coordinator worktree](#coordinator-merges), any command line that names `pp-review-gate`, in any case, raw or behind JSON, GraphQL, URL, or HTML escapes, is refused wherever the word appears, including a `grep` for it or an `echo` into a file. So is a `gh pr comment`, `gh issue comment`, or `gh api` comment body the guard cannot read literally, including a REST `--input` body that is not JSON: a variable (`$VAR`, `${...}`), a command substitution (`$(...)` or backticks), an ANSI-C `$'...'` string, stdin (`-F -`, `--input -`, a pipe, or a heredoc), a missing file, or a body file in a line with other commands. Write the body as a literal argument, or to a file passed with `--body-file` in a command of its own.
- The `-c` scripts of `sh`, `bash`, `zsh`, `dash`, `ksh`, and `fish` are classified like top-level commands, and they inherit the outer line's context: a `GIT_CONFIG_*` or `GIT_DIR` assignment, a config write, or a branch change before the shell applies to every `git` command in the script, and a wrapped merge never counts as the coordinator's merge. The guard knows the options that take a value (`--rcfile`, `--init-file`, `-o`, `-O`); any other unrecognised option before the script makes it unclassifiable, and such a command that mentions `git` or `gh` is refused outside the coordinator worktree. `eval` or `source` of anything that runs `git` or `gh`, or of text the guard cannot read, is refused.
- Put `gh` options after the subcommand. Before it, and between `pr`, `release`, `variable`, `alias`, or `extension` and their subcommand, the guard accepts only `-R`/`--repo`, `--help`/`-h`, and `--version`, with or without `=value`, and refuses any other option there. It still classifies the command, so `gh --help=false variable set` is refused as a variable write. It also refuses a shell expansion there, including in an option's value, as in `gh --help=$opts` or `gh -R "$repo" pr view`, because an unquoted expansion can split into the subcommand itself. Write `gh pr view -R "$repo"` instead.
- Outside the [coordinator merge](#coordinator-merges), a PR merge is refused in the command positions, shells, runners, and interpreters the guard recognizes. The guard matches each command's words after shell unquoting: `gh`, then `pr` and `merge` with only `gh` options between them. So `me''rge`, `m\erge`, `"mer"ge`, and `$'m\x65rge'` still count, and so do the same words in `xargs` and `find -exec` arguments, `sh -c` scripts (also inside an alias value, a git alias value, or a runner), a runner's command string, a shell `alias` value, a `git -c alias.*` or `git config alias.*` value, and `python3 -c` or `node -e` code, which is split into words at quotes and call punctuation so `gh pr --repo=o/r merge` inside a string literal still counts. Inside an alias value or a runner, a `gh api` merge endpoint, `mergePullRequest`, or `enablePullRequestAutoMerge` counts too. An expansion or command substitution in program position followed by `pr merge`, as in `$(echo gh) pr merge`, counts too. A quoted argument that only mentions the command is data and stays allowed: a comment body, a commit message, a `grep` pattern, or `echo` text. The guard looks only where a program runs: program position after the wrappers it unwraps (`command`, `env`, `nice`, `timeout`), the program a runner such as `xargs` or `find -exec` runs, `-c` or `-e` code, alias values, and the command string of a runner that hands one to a shell (`script -c`, `su -c`, `flock -c`, `entr -s`, `ssh host`, `watch`, `tmux`, `parallel`). The arguments of the program a runner runs are data, so `find docs -exec grep -l 'gh pr merge' {} +` stays allowed. The arguments of an ordinary program are data, so `echo gh pr merge 12` and a top-level `grep -e gh -e pr -e merge` stay allowed (the same `grep` under `setsid` or another runner is refused; see the known limits below). Help is decided for each simple command: `gh pr merge` followed only by `--help` or `-h` stays allowed, with any `gh` options in its path and inside a compound line, as in `gh pr --repo=o/r merge --help | less`.
- A `gh` command run through a program the guard does not unwrap (`xargs`, `find`, `setsid`, `flock`, `watch`, `script`, `parallel`, `tmux`, `ssh`, `su`, and the others in `UNWRAPPED_RUNNERS`) is also refused when it is a `gh api` call naming a merge endpoint or `mergePullRequest`, or when its arguments hold a shell expansion or command substitution. A literal `$` inside single quotes, such as a GraphQL `$id`, is data.
- A line that sets a variable to `gh` (or a path ending in `gh`) and runs a program named by an expansion, or passes an expansion to a runner such as `flock FILE`, `watch -n 1`, `xargs -n 1`, or `ssh host`, is refused, as in `G=gh; P=pr; M=merge; $G $P $M` or `G=gh; watch -n 1 $G pr view`. An expansion used as data, as in `G=gh; echo $HOME`, is not.
- A `gh` subcommand that is not one of gh's built-in commands, help topics, or gh's own command aliases is refused. gh's aliases are mapped to their commands before any check: `at` to `attestation`, `rs` to `ruleset`, `cs` to `codespace`, `agent`, `agents`, and `agent-tasks` to `agent-task`, `ext` and `extensions` to `extension`, and `skills` to `skill`; within a group, `ls` to `list` (every group that has a `list`, such as `gh variable ls` and `gh release ls`) and, for `skill`, `show` to `preview`. Any other subcommand is refused because only an alias or an extension can answer to it; `gh co` counts, since it is a default alias (use `gh pr checkout`). `gh help`, with or without arguments, is refused too, because a gh alias named `help` could answer to it; use `gh <command> --help`.
- A command that sets or clears `GH_CONFIG_DIR`, `XDG_CONFIG_HOME`, or `HOME` and runs `gh` is refused as "gh config redirected". A prefix assignment, an `env NAME=…` operand, `env -u` of one of them, or `env -i` counts for its own command and for the nested `sh -c` scripts and runners it starts. An `export`, `declare`, `typeset`, `readonly`, or `unset`, or a bare assignment such as `HOME=/tmp;`, also counts for the later commands of the same shell: the line's own shell, or the same `( )` subshell and the subshells inside it. A pipeline stage is its own process, so nothing carries out of one. `(export HOME=/tmp); (gh pr view 12)` and `export HOME=/tmp | gh pr view 12` are allowed; `(export HOME=/tmp; gh pr view 12)` is refused. A runner counts only when its program is `gh`: after any `find -exec`, `-execdir`, `-ok`, or `-okdir`, the word after `xargs`'s options read as getopt reads them, or for other runners one of the first two words that are not options. Only a `gh` program counts, so `HOME=/tmp echo gh` and `HOME=/tmp npm test; gh pr view 12` stay allowed, and so does data that only looks like an assignment, as in `gh pr comment 12 --body HOME=/tmp`.
- Every option of a wrapper the guard unwraps (`env`, `timeout`, `nice`, `time`, `sudo`, `exec`, `command`, `nohup`) must be one it knows. A bundle is read letter by letter, as the wrapper reads it (`timeout -k1s`, `env -vu HOME`), and a long option must match in full; an unknown option or an abbreviation such as `env --un` or `timeout --kill-a` makes the line unclassifiable, so it is refused.
- A `gh` command found below the top level, as the program a runner runs, inside a `sh -c` script, or in an alias value, gets the same checks as a top-level one, so `xargs gh m` or `find -exec gh ext exec x` is refused like `gh m`. Interpreter code that names `gh` followed by words those checks refuse is refused too; like the rest of the interpreter rule this matches text, so `python3 -c 'print("gh is great")'` is refused.
- Built-in `gh` commands that run or change code the guard cannot see are refused too: every `gh extension` (or `ext`, `extensions`) subcommand except `list` and `search`, so `exec`, `browse`, `create`, `remove`, `install`, and `upgrade`; `gh skill install`, `update`, and `publish`; `gh agent-task` except `list` and `view`, since `create` starts a remote agent that acts on the repo with the user's GitHub identity; every `gh codespace` subcommand; and `gh copilot`. Help forms stay allowed. `gh alias list` and `delete` and `gh attestation` stay allowed.
- `gh alias set`, `gh alias import`, `gh extension install`, and `gh extension upgrade` are refused, and so are file edits under the `gh` config directory (`$GH_CONFIG_DIR`, `$XDG_CONFIG_HOME/gh`, `~/.config/gh/`), because an alias or extension runs commands the guard never sees. Relative paths, and a relative `GH_CONFIG_DIR`, resolve against the hook payload's `cwd` (the Claude Code adapter passes it for every file tool); a folder elsewhere that only looks like `.config/gh` is not the config. The CLI’s `file` and `patch` operations resolve relative paths against the guard process’s cwd; use absolute paths for these operations.
- It refuses a git or gh command it cannot classify.

The guard catches mistakes. Its threat model is a lane that runs a wrong or careless command, not one that hand-crafts gh config or shell tricks to get past it (design call on GRE-1280). It is not a shell sandbox or a security boundary, and an agent can run commands without it. Repository rules and required GitHub checks stay authoritative. Changes to the guard need the same review as any other security control, and the owner merges them.

Known limits of the merge refusals, accepted rather than fixed:

- Interpreter code (`python3 -c`, `node -e`) is matched as text, not run. The matcher removes quote characters and backslashes first, so `m\erge` and `me''rge` inside the code are caught. String concatenation, such as `"me" + "rge"`, is not detected; main allows it too.
- `find -exec` and `xargs` have a known place for the program they run, so their arguments after it are data. `xargs` options are read as getopt reads them, including attached values (`-I{}`, `-rI {}`) and `--replace`; an option the guard does not know means the program's place is unknown, so every word is tried. Under any other runner, such as `setsid` or `flock`, each word after the runner's own options is tried as the program, because the guard has no table of those runners' option values. So `setsid grep -e gh -e pr -e merge docs/x.md` is refused, while the same `grep` run directly, or under `find -exec`, is allowed.
- The same matcher refuses interpreter code whose text only contains `gh pr merge`, such as `python3 -c 'print("gh pr merge 12")'`. That conservative side of it is accepted.
- The guard follows nested shells, runners, and alias values six levels deep. Past that it fails closed: any leftover text that names `gh`, even `gh pr view`, is refused with the merge reason.
- A command line longer than 64 KiB is refused as too long, before any parsing. Shell quoting roughly triples with each nested `sh -c` level, so about nine levels already reach the cap.
- Checking one command line may take at most 5,000 steps (one per command it considers, repeats included) and 2 seconds, counted once for the whole line, nested `sh -c` scripts, shell parsing, and the guard's own `git` lookups included. Each distinct `git` lookup runs once per line, and its timeout is the time left on the line. Past either limit the line is refused as too complex to check, so the hook finishes well inside its 10-second limit. A runner reads its words in one pass, so ordinary long lines stay fast, but a runner with thousands of arguments, such as `xargs echo` followed by about 5,000 file names, can be refused as too complex. That is accepted; split the line.
- A config redirect set inside a runner or alias boundary is not seen: `xargs sh -c 'HOME=/tmp gh pr view 12'`, `xargs env -i gh pr view 12`, `alias read='env -i gh pr view 12'`.
- Config redirects set inside a command substitution, as in `url=$(export GH_CONFIG_DIR=/tmp/x; gh pr view 12)`, or under `shopt -s lastpipe`, as in `bash -c 'shopt -s lastpipe; true | export GH_CONFIG_DIR=/tmp/x; gh pr view 12'`, are not tracked, and an export in a brace-group pipeline stage, as in `{ export HOME=/tmp; } | cat; gh pr view 12`, is refused though it does not carry. Aliases and extensions are refused wherever `gh` runs, so a redirected config cannot add a command lanes can run.
- A hand-edited gh config whose alias shadows a built-in command, and config set by sourcing a file, as in `. ./env.sh; gh pr view`, are not seen. gh's documentation says an extension cannot override a core command.
- gh aliases and extensions are refused where they run, as an unknown `gh` subcommand, whatever config defines them and wherever it lives. A new gh release's built-in commands need adding to `GH_COMMANDS` in `scripts/agent-guard.py` before lanes can use them.
- The runner list includes `xvfb-run`, `pp-netns`, `direnv`, `npx`, and `sg`; the shell list includes `ksh` and `fish`, and the interpreter check recognizes versioned `python3.N` executables. Unlisted runners and interpreters, including `awk 'BEGIN{system("gh pr merge 12")}'`, can still hide a merge.
- The guard cannot see a merge run from a script file or `make`, a command an interpreter builds at run time, an alias, `gh` config, or extension that already exists, a git alias such as `git -c alias.p='push origin HEAD:main' p`, or a shell redirect into a hook or config file. Merges stay with the coordinator, under the merge gate.

### Claude Code

`.claude/hooks/guard.py` maps Claude Code's PreToolUse payload onto the guard. `Bash` becomes `check_command(command, cwd)`. `Write`, `Edit`, and `MultiEdit` check `file_path`, and `NotebookEdit` checks `notebook_path`. The adapter exits 2, which blocks the call, on a refusal and on any payload it cannot read: a non-object, a missing or empty field, a relative or missing `cwd`, an unknown tool, or an internal error.

The repository doesn't commit Claude Code settings. Add this to `.claude/settings.local.json` in your checkout (Git ignores that file). The guard then refuses later edits to it:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash|Write|Edit|MultiEdit|NotebookEdit",
        "hooks": [
          {"type": "command", "command": "python3 -B \"$CLAUDE_PROJECT_DIR\"/.claude/hooks/guard.py", "timeout": 10}
        ]
      }
    ]
  }
}
```

Claude Code reads hook settings when a session starts, so restart the session after changing them. Run `/hooks` to confirm the entry.

### Coordinator merges

The coordinator merges from one dedicated worktree, which must lie outside the lane worktrees in `/persist/worktrees/t3`. Its hook command sets `PP_COORDINATOR_WORKTREE` to that worktree's root. Lane hook configs never set it. The variable lives in the hook command, not in the agent's shell, so a command the agent runs cannot set it for the guard.

The guard allows exactly one merge form, and only when `PP_COORDINATOR_WORKTREE` equals the Git top level of the command's directory and lies outside `/persist/worktrees/t3`:

```sh
gh pr merge <number> --squash --match-head-commit <40-character sha>
```

The command must stand alone: no other options such as `--admin`, `--auto`, or `-R`; no wrapper, environment assignment, `cd`, pipeline, or substitution; no `GIT_DIR` or `GIT_WORK_TREE` in the guard's environment; and a literal lowercase SHA. Every other merge, including the same command from a lane worktree, is refused. The coordinator runs it only after the [merge gate](definition-of-done.md#merge-gate) passes. In the coordinator worktree, the Claude Code hook settings become:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash|Write|Edit|MultiEdit|NotebookEdit",
        "hooks": [
          {"type": "command", "command": "PP_COORDINATOR_WORKTREE=/persist/coordinator/PrintPartner python3 -B \"$CLAUDE_PROJECT_DIR\"/.claude/hooks/guard.py", "timeout": 10}
        ]
      }
    ]
  }
}
```

Replace `/persist/coordinator/PrintPartner` with the coordinator worktree's absolute path.

## Writing and plans

Write docs, comments, and PR text plainly. State what the code or command does, with the number, path, or command where there is one. Use active voice. Cut filler, puffery, and abstractions the change doesn't need.

Before large or risky work, write the plan down and challenge it. Risky work includes moving operation ownership, persistence formats, printer sends, CI or security controls, and changes over about 1,500 lines. List how the plan could fail, check each risk against the code, and get open decisions answered by the owner before you build.

## Review and merge

Every ready PR needs the required `review-gate` check. It passes on the PR's head when Claude's code review together with its security review succeeded on that head, or when the coordinator recorded its own review of that head. The Claude reviews run only on the owner's own PRs from this repository. [Code review](definition-of-done.md#code-review) describes each path. If you work in another tool, the checks still run on your PR. Fix each finding, or reply with why it doesn't apply, then resolve the thread. Merging needs green required checks and no open threads. The coordinator merges from its own worktree once the [merge gate](definition-of-done.md#merge-gate) passes; lanes and other agents report readiness and never merge. A score that a lane posts with `post-score.sh` is a record only.

Every PR says `Fixes GRE-xxx`. Put unmet acceptance items in a child ticket that quotes them verbatim; see [Fixes only](definition-of-done.md#fixes-only). `linear-pr-sync.yml` sets the ticket states that follow a PR, so don't move them yourself.

Only the coordinator records a review. It runs an independent model review on a checkout pinned to the PR's head SHA, waits until every finding is answered and its thread resolved, and then posts a comment with a `pp-review-gate` marker naming that head, the reviewer model, and the finding count. The steps are in [Coordinator review record](definition-of-done.md#coordinator-review-record). The safety guard refuses marker comments from any other worktree; lanes report readiness instead.

### Design calls

Design decisions that stay inside the approved plan, the AGENTS.md rules, and the data invariants (SQLite formats, immutable IDs, autosave, restart recovery, operation ownership) are pre-approved: the coordinator makes them and logs each one on the ticket with its reason. Lanes ask the coordinator instead of stopping for the owner. Anything else goes to the owner in one daily list: scope, cutover, defaults, data formats, and security posture. Work that depends on such a decision waits for the answer; pick other work meanwhile. See [Design calls](definition-of-done.md#design-calls).

## Verify in a running app

Run the real app at the PR head and look at the change. Mocked tests passing does not show that a screen works.

### Choose the runtime

- `web/apps/server` owns Node compatibility behavior.
- `rust/crates/pp-server` owns only the operations already migrated to Rust.
- `web/apps/web` is shared. Pick the server that owns the operation under test.
- `rust/crates/pp-desktop` needs native execution for window, tray, protocol, and OS behavior. Browser evidence is not native acceptance.
- `slicer-sidecar` uses a synthetic slicer in tests. Never send a real print.

Before launching, write down the exact HEAD, operation owner, fixture, expected result, feature modes, time budget, and cleanup. Use fresh data and free loopback ports. Keep `desktop_rebuild` off unless the task tests its on mode.

### Web UI

The helper drives the Node server and the shared React UI. Give each run its own state, evidence, and data folders and two free loopback ports:

```sh
(
  set -eu
  : "${API_PORT:?pick a free loopback port}" "${UI_PORT:?pick a different free port}"
  ROOT="$(mktemp -d)"
  export PP_VERIFY_STATE_DIR="$ROOT/state" PP_VERIFY_EVIDENCE_DIR="$ROOT/evidence"
  trap 'node scripts/verify-print-partner/control-print-partner.mjs cleanup' 0
  node scripts/verify-print-partner/control-print-partner.mjs launch \
    --mode npm --data-dir "$ROOT/data" --api-port "$API_PORT" --ui-port "$UI_PORT"
  node scripts/verify-print-partner/control-print-partner.mjs doctor
  node scripts/verify-print-partner/control-print-partner.mjs screenshot \
    --path /builds --out "$ROOT/evidence/builds.png" --wait 1500
)
```

Screenshot each screen the change affects, in both themes (`--theme dark`) if styling changed. The launcher waits for readiness. Its state is `$PP_VERIFY_STATE_DIR/state.json`, and the service log is `$PP_VERIFY_STATE_DIR/<project>/npm-dev.log`, where `project` comes from that state. Keep those variables unchanged through cleanup, and read only the log lines you need.

### Dialogs and other client-only state

The helper opens a fresh browser for each command, so chaining its `click` and `screenshot` commands loses an open dialog, menu, or dropdown. Interact and capture in one Playwright program instead. Save it outside the checkout, then run it inside the launch block above, after the `doctor` line, as `ROOT="$ROOT" UI_PORT="$UI_PORT" node /path/to/capture.mjs`. It loads Playwright from `web/node_modules`, so run it from the repository root after `npm --prefix web ci`:

```js
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const { chromium } = createRequire(resolve("web/package.json"))("playwright-core");
const resolver = pathToFileURL(resolve("web/apps/web/test/browser/browserExecutable.mjs")).href;
const { browserExecutable } = await import(resolver);
const browser = await chromium.launch({ headless: true, executablePath: browserExecutable() });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(`http://127.0.0.1:${process.env.UI_PORT}/library`);
  // Replace these two steps with the interaction your change needs.
  // An empty Library has a second "Add source" button, so target the menu trigger.
  await page.locator('button[aria-haspopup="menu"]').filter({ hasText: "Add source" }).click();
  await page.getByRole("menu").waitFor();
  await page.screenshot({ path: `${process.env.ROOT}/evidence/add-source-menu.png` });
} finally {
  await browser.close();
}
```

`playwright-core` does not download a browser. Both programs call `browserExecutable()` from `web/apps/web/test/browser/browserExecutable.mjs`, the resolver the browser tests use. It tries `PLAYWRIGHT_CHROMIUM_EXECUTABLE`, then a Chromium that Playwright downloaded, then the usual Chrome and Chromium install paths, and throws if it finds none. The development container sets the variable. On other machines, set it to your Chrome or Chromium binary, for example on Windows, or run `npx --prefix web playwright install chromium` first. Don't reload between the interaction and the capture.

### Rust gateway

The Rust gateway requires a desktop session. A bare request returns `401 Desktop session required`. Use the existing launcher and its one-use bootstrap credential. The credential lives in one private directory, which is always removed. The launcher log lives in another, which stays for diagnosis after the block ends:

```sh
(
  set -eu
  : "${CAPTURE:?path to capture-rust.mjs, saved outside the checkout}"
  PRIVATE_DIR="$(mktemp -d)"
  LOG_DIR="$(mktemp -d)"
  EVIDENCE_DIR="$(mktemp -d)"
  LOG="$LOG_DIR/launcher.log"
  LAUNCHER=""
  trap '[ -z "$LAUNCHER" ] || { kill -TERM "$LAUNCHER" 2>/dev/null || true; wait "$LAUNCHER" || echo "launcher status $?; see $LOG" >&2; }; rm -rf "$PRIVATE_DIR"' 0
  echo "launcher log: $LOG"
  echo "evidence: $EVIDENCE_DIR"
  python3 scripts/verify-rust-gateway/verify.py launch \
    --credential-file "$PRIVATE_DIR/credential.json" >"$LOG" 2>&1 &
  LAUNCHER=$!
  until grep -q '^PP_SERVER_URL=' "$LOG"; do
    kill -0 "$LAUNCHER" 2>/dev/null || { echo "launcher exited before readiness" >&2; exit 1; }
    sleep 2
  done
  CREDENTIAL_FILE="$PRIVATE_DIR/credential.json" EVIDENCE_DIR="$EVIDENCE_DIR" node "$CAPTURE"
  kill -TERM "$LAUNCHER"
  STATUS=0
  wait "$LAUNCHER" || STATUS=$?
  LAUNCHER=""
  [ "$STATUS" -eq 0 ] && grep -q '^STOP: exit=0' "$LOG" || { echo "launcher status $STATUS; shutdown checks failed or missing; see $LOG" >&2; exit 1; }
  echo "launcher stopped cleanly"
)
```

`mktemp -d` creates a directory only you can read. The credential path must not exist before launch. The first run builds `pp-server`, so readiness can take minutes. The block stops the launcher itself and fails unless the launcher exits 0 and the log shows `STOP: exit=0`, which means `pp-server` shut down and its cleanup checks passed. On any earlier failure, the trap stops the launcher, prints a nonzero launcher status, and removes the credential directory. The log is not in that directory, so the shutdown result stays readable. The log holds server stderr but not the bootstrap URL. Read only the lines you need, never paste it whole, and remove `LOG_DIR` and `EVIDENCE_DIR` when you are done.

The capture program, `capture-rust.mjs`, reads the bootstrap URL from the file and removes the file. It opens the URL so the redirect sets the session cookie, then works in that same browser context. It waits for the Builds heading before it interacts, because the page first shows "Loading...":

```js
import { readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const { chromium } = createRequire(resolve("web/package.json"))("playwright-core");
const resolver = pathToFileURL(resolve("web/apps/web/test/browser/browserExecutable.mjs")).href;
const { browserExecutable } = await import(resolver);
const { bootstrap_url: bootstrapUrl } = JSON.parse(readFileSync(process.env.CREDENTIAL_FILE, "utf8"));
rmSync(process.env.CREDENTIAL_FILE);
const origin = new URL(bootstrapUrl).origin;
const browser = await chromium.launch({ headless: true, executablePath: browserExecutable() });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(bootstrapUrl);
  if (new URL(page.url()).pathname === "/__desktop/bootstrap") throw new Error("bootstrap did not redirect");
  await page.goto(`${origin}/builds`);
  await page.getByRole("heading", { name: "Builds", exact: true }).waitFor();
  // Replace these steps with the interaction your change needs. Opening the dialog saves nothing.
  await page.getByRole("button", { name: "New Build" }).first().click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "Create" }).waitFor();
  await page.screenshot({ path: `${process.env.EVIDENCE_DIR}/rust-new-build-dialog.png` });
  console.log(`captured ${new URL(page.url()).pathname}`);
} finally {
  await browser.close();
}
```

Open the screenshot and confirm it shows the Builds page and the dialog, not a loading screen. Keep the credential and its URL out of command lines, logs, traces, screenshots, and PR text; the program prints only the path it captured. Capture only after the redirect has left the bootstrap page. The gateway rejects a request whose `Origin` header differs from its own origin and requires `Origin` on writes. The browser sets it for you; a script that calls the gateway with another HTTP client must send `Origin` equal to `PP_SERVER_URL` and the session cookie. A bare `401` does not prove an operation works.

[The launch test](../../scripts/verify-rust-gateway/test_launch.py) holds the bootstrap and shutdown contract.

#### Replacement UI on a Rust-only core

`scripts/evidence/run.sh` runs every step below in one command and cleans up after itself; see [the harness README](../../scripts/evidence/README.md). The manual steps follow.

`verify.py` builds `pp-server` without the `desktop_rebuild` feature, so its own build always runs the Node-backed runtime. For the replacement UI (`web/apps/ui`), build the server with the feature, then start it Rust-only on a copy of a synthetic dataset:

```sh
cargo build --locked --manifest-path rust/Cargo.toml -p pp-server --features desktop_rebuild
(cd web && npm run fixture:synthetic -- --seed 1138 "$SEED_DIR")
PP_DESKTOP_REBUILD=1 python3 scripts/verify-rust-gateway/verify.py launch \
  --binary rust/target/debug/pp-server --seed "$SEED_DIR" \
  --credential-file "$PRIVATE_DIR/credential.json" >"$LOG" 2>&1 &
```

`--binary` needs the `rust/bundle-manifest.json` and web build that the binary was built against; see `build()` in `verify.py`. `--seed` refuses a directory without `SYNTHETIC-DATASET.json`, copies it into the launcher's private temporary data directory, and, when `PP_DESKTOP_REBUILD` is `1` or `true`, writes the Rust [ownership marker](../storage.md#ownership-marker-format) into that copy in the compact form. Rust-only startup refuses a non-empty directory without one. That start also needs a `desktop_rebuild` binary, and the helper's own build has no such feature, so `--seed` with `PP_DESKTOP_REBUILD` set requires `--binary` built with `--features desktop_rebuild`. The seed is never changed, and the copy is removed at shutdown.

The gateway serves the old `apps/web` build, not `apps/ui`. Serve the built replacement UI with Vite's preview server, which forwards `/api`, `/ws` and `/health` to the core:

```sh
npm --prefix web run build --workspace @print-partner/ui
(cd web/apps/ui && PP_UI_CORE_URL="$PP_SERVER_URL" npx vite preview)   # http://127.0.0.1:4174
```

The Rust routes need a Rust session as well as the desktop cookie. After the bootstrap redirect, the capture program registers the single account on the disposable data with a `POST /auth/register` from `page.request`, sending `Origin` equal to `PP_SERVER_URL` and a JSON body with an email and a random password it never prints. Cookies are scoped to the host, not the port, so the same browser context then reads `http://127.0.0.1:4174/builds` with both cookies.

### Desktop app

Desktop evidence comes from the native app built at the PR head, on a real display. Follow [Desktop and tray capture](../contributing/gate-evidence.md#desktop-and-tray-capture) for the lifecycle checks: close to tray, restore the same window, and quit with cleanup. Any native recorder works, such as macOS Screenshot (Shift-Command-5) or `screencapture`, Windows Snipping Tool, OBS, or the Homelab `evidence` CLI on Linux. The recorder changes nothing else: on Linux, disclose D-Bus tray activations as that section says.

A browser screenshot is never native evidence. With no native display, report desktop acceptance as BLOCKED; a missing display is not an exemption.

### Check and attach evidence

1. Open every file. Confirm it shows the change itself, not just a page loading, and only the app: no terminals, other windows, tokens, or customer data. The repository is public.
2. Drag the files into the PR description on GitHub, or publish them with `python3 scripts/evidence/publisher.py <pr-number> <files...>`, which runs the installed `evidence publish` with isolated git and a checked cache.
3. Under `## Evidence`, give the head SHA and one line per file saying what it shows. When a UI path changed with no visible effect, such as a type-only refactor, give the reason on the `Exemption:` line. [Gate evidence](../contributing/gate-evidence.md) has the lint rules.

### Report and clean up

Record the journeys you ran and what you asserted, the runtime owner, flag mode, exact HEAD, duration, log location, evidence files, and cleanup outcome. For persistence changes, include restart, recovery, and stable IDs. For ownership changes, show that the non-owner refused and that no second mutation happened. Report a missing browser, display, or native tool as BLOCKED, never as a pass.

Stop only the processes you started, and remove the temporary folders you created. Keep non-secret logs and evidence outside the disposable data.
