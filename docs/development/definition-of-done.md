# Definition of done

Every pull request meets this bar before it merges. The [pull request template](../../.github/pull_request_template.md) repeats it as a checklist.

## The bar

1. **One job per PR.** Aim for 300 to 600 changed lines. About 1,500 is the hard ceiling, or about 600 for a Mission PR. Split larger work into sequential PRs: each one starts from `main` after the previous one merges, never stacked on an open PR's branch. If you find work that belongs to another ticket, note it on that ticket and leave it out of this PR.
2. **Unslopped.** Run the unslop pass on code, comments and docs before review. Remove filler comments, dead code and speculative abstractions.
3. **Reviewed.** The required `review-gate` check passes on the exact head commit (see [Code review](#code-review)), every review thread is resolved, and required CI is green on that commit. A sensitive PR, or a head whose `review-gate` passed only through the coordinator record, also passes the coordinator's scorer run under the [merge gate](#merge-gate).
4. **Docs in the same PR.** When the PR changes behavior, setup or an interface, update the matching page under `docs/` in the same PR.

## Review and merge policy

- Review depth (re-scope, 2026-11-08). A normal PR needs a passing `review-gate` (Droid, the Claude code and security reviews, or the coordinator record) and green required checks. The second-vendor scorer applies to sensitive PRs and to heads whose `review-gate` passed only through the coordinator record; see [Merge gate](#merge-gate).
- P0 and P1 findings block the merge. Fix them in the PR.
- A P2 that affects real user data, security, or causes a crash becomes a bug ticket in its chunk.
- Every other P2 and all P3s go on the post-parity polish list (GRE-974) or in a PR comment. Do not create a ticket for them.
- After the first review, a PR gets at most two fix rounds. If new P0/P1 findings still appear, stop. Post an escalation comment on the ticket with the findings and two or three options (split the PR, replace custom code with a maintained library or standard tool, or defer), then pick other work.
- Prefer maintained libraries and standard platform mechanisms over custom hardening. Examples are OS file locks, CODEOWNERS, actionlint and zizmor.

## Design calls

Design decisions that stay inside the approved plan, the AGENTS.md rules, and the data invariants (SQLite formats and history, immutable Plan, Plate and Required-unit IDs, autosave, restart recovery, and one runtime per operation) are pre-approved. The coordinator makes them and logs each one on the ticket with its reason; lanes ask the coordinator rather than wait for the owner.

Every other decision goes to the owner in one daily list: scope, cutover, defaults, data formats, and security posture. When in doubt, it goes on the list. Work that depends on an open item waits for the owner's answer.

## Fixes only

Every PR starts its description with `Fixes GRE-xxx` and closes its ticket when it merges. Don't write `Refs GRE-xxx`.

When some acceptance items stay unmet, split them out before the merge: create a child ticket of the PR's ticket whose description quotes each unmet item verbatim, then merge with `Fixes` on the parent. Whoever merges, lane or coordinator, checks that the quotes match the parent's unmet items before merging; when the scorer runs on that head, it checks them too. Create other tickets only for bugs on main, P0/P1 findings, or splits the parent ticket already calls for.

## Ticket states

`.github/workflows/linear-pr-sync.yml` runs `scripts/linear/pr_status_sync.py` and owns every ticket state that follows a PR: In Review when a PR opens, Done when a `Fixes` PR merges, and back to In Progress when a PR closes unmerged. Don't set those states by hand or from an agent. The sync skips an owner-signed list of protected tickets, listed in `PROTECTED` in that script, and logs each skipped write; the owner moves those tickets.

## Merge gate

A lane merges its own normal PR. The coordinator merges sensitive PRs (item 6) and marker-only heads, from its own worktree (see [Coordinator worktree](agent-workflow.md#coordinator-worktree)). The owner merges changes to the safety guard, its hook adapters, and CI controls. Every merge needs all of these on the same head commit:

1. **Exact head.** Every check, review, and score below is on the PR's current head SHA.
2. **Required checks** are green, including `review-gate`.
3. **Every other reported check** passes or is skipped. Only these checks may fail: `rust (macos)` and `rust (windows)`, which are advisory, and `droid-review`, `claude-code-review`, and `claude-security-review`, only when `review-gate` passed on the same head through another path (for example while Droid or the Claude subscription is out of usage). The owner extends that list. Windows work is deferred until after cutover (re-scope, 2026-11-08), and the owner removed `rust (windows)` from the required checks on 2026-11-08.
4. **All review threads** are resolved.
5. **Scorer, sensitive PRs and marker-only heads.** For a PR that item 6 names, and for any head whose `review-gate` passed only through the coordinator record, the coordinator runs its own scorer on that head from a [scorer seat](#scorer-seats) whose vendor differs from the PR's author. A marker-only head passes with a score strictly above 8; a sensitive PR needs item 6. Other PRs, whose gate passed on a Claude or Droid review, need no scorer run (re-scope, 2026-11-08).
6. **Sensitive PRs** that touch schema or migrations, auth and sessions, backup and restore, Accept and publication, secret handling, or the protected-files paths (CI workflows, `.github/zizmor.yml`, the safety guard, `.factory/` and `.claude/`; see [Code review](#code-review)) need a score of 9 or more from each of two [scorer seats](#scorer-seats).
7. **Command.** `gh pr merge <number> --squash --match-head-commit <sha>`, with the full 40-character head SHA, run on its own. Never `--admin`, `--auto`, or `--delete-branch`. The safety guard allows only this form; see [Merges](agent-workflow.md#merges).

### Lane merges

A lane may merge its own PR only when, on the exact head:

- the PR is not sensitive: it touches none of the item 6 areas or paths. The lane checks this against the PR's file list (`gh pr diff <number> --name-only`) and its own diff; when unsure, the PR is sensitive;
- `review-gate` passed through a Droid or Claude review of that head (path 1 or 2 under [Code review](#code-review)), not only through the coordinator record;
- every required check passes, nothing else is pending, and every other check passes or is on the item 3 list;
- every review thread is resolved;
- the PR's base is `main`.

Otherwise the lane reports readiness to the coordinator and stops.

Outside the coordinator worktree, the safety guard enforces part of this list before it allows the merge: it refuses a lane merge whose base is not `main`, whose head is not the given SHA, whose file list or diff is incomplete, that renames or copies a file, that changes a lane-protected path (CI and agent configs, every script directory and file a workflow runs, and every schema file), or whose diff adds or removes a schema line in Rust or SQL. A failed or slow lookup refuses too. [Merges](agent-workflow.md#merges) lists exactly what it checks and what it cannot see. The other item 6 areas (auth and sessions, backup and restore, Accept and publication, secret handling) have no fixed paths, so they rely on the lane's judgement and the coordinator's audit.

The coordinator audits merges at least daily. It lists every PR merged since the start of its last audit, on any base, and checks each one's base, files, and merger against this list and item 6:

```sh
gh pr list -R Print-Partner/PrintPartner --state merged --search "merged:>=<last audit date>" --limit 200 --json number,mergedAt,baseRefName,files,mergedBy
```

When the list returns 200 PRs, it narrows the date range and lists again until a range returns fewer. For any merge that broke a rule, it opens a revert PR and notes it on the ticket.

### Scorer seats

The owner's choice (2026-11-08). A seat counts only when its model's vendor differs from the PR author's.

1. OpenCode `opencode/crouton-alpha-free`, variant `max` (OpenCode free).
2. OpenCode `opencode/deepseek-v4.1-flash-free`, variant `max` (OpenCode free).
3. Fallback: Cursor `grok-4.7`, when the OpenCode free quota runs out or a seat's vendor matches the author's.

A sensitive PR needs two seats; a marker-only head needs one. Take them in this order.

`post-score.sh` posts a score as a PR comment. That comment is a record only. A posted score from a lane or an earlier head does not count; for a sensitive PR or a marker-only head, the gate is the coordinator's own scorer run on the exact head.

## Code review

The required `review-gate` check (`.github/workflows/review-gate.yml`) passes on a PR head when any of these holds for that exact head SHA:

1. **Factory Droid.** The `droid-review` job of `.github/workflows/droid-review.yml` succeeded, and its log shows that droid-action's review step ran.
2. **Claude.** The `claude-code-review` job of `.github/workflows/claude-review.yml` succeeded, and `claude-security-review` succeeded or was neutral.
3. **Coordinator record.** A comment by the repository owner account, `poitee`, carries a `pp-review-gate` marker for that head. See [Coordinator review record](#coordinator-review-record).

Every push or edit of the PR starts a new `review-gate` run. It waits up to 30 minutes for the head's Claude jobs, then fails unless one of these holds for the head. `.github/workflows/review-gate-rerun.yml` reruns the gate when a Droid or Claude review run finishes and when the owner creates, edits, or deletes a marker comment. A rerun repeats the default branch's gate, so a fork workflow that borrows a review workflow's name can trigger a rerun but cannot make it pass. If the check stays red after a review finished, rerun the `review-gate` job from the PR's Checks tab.

On a PR, the gate runs on `pull_request_target`, so GitHub runs the default branch's copy of `review-gate.yml`, never the PR's. On a merge group it only confirms that every PR in the group passed this PR check on its current head ([Merge queue](ci.md#merge-queue)). The job reads the GitHub API only, with read permissions and no secrets, and never checks out or runs PR code. It applies these checks before any path counts:

- **Result origin.** A `droid-review` or Claude result counts only when its check suite comes from the GitHub Actions app, from the expected workflow path, on `pull_request`, and GitHub ran the default branch's copy of that file. The workflow run must also belong to this PR: this PR targets the default branch, the run's `pull_requests` list names this PR, and every PR it names targets the default branch. A run from another PR with the same head SHA but another base, which runs that base's copy of the workflow, never counts, and neither does a run with an empty list. That holds when the file's blob at the head equals the default branch's blob, or the PR does not change the file, so the merge commit carries the base's copy.
- **Same-named checks.** The gate fails when any check run on the head named `review-gate`, `droid-review`, `claude-code-review`, or `claude-security-review` comes from another app, another workflow file, or another event.
- **Complete file list.** The gate fails when GitHub's file list for the PR is incomplete: the files API stops at 3000 files, so a PR with 3000 or more changed files, or a list shorter than the PR's `changed_files`, fails. Split such a PR.
- **Protected files.** When a PR author other than the owner account changes a file under `.github/workflows/`, `.github/zizmor.yml`, `scripts/agent-guard.py`, `.factory/`, or `.claude/` (the hook configs that load the guard), including deleting such a file or renaming it into or out of those paths, the PR needs both the coordinator record (the owner's marker for that exact head) and a passing Droid or Claude review. Neither alone passes it, so a marker produced without a review cannot certify a change to the guard or the workflows. Dependabot's weekly GitHub Actions bumps fall under this rule: the Claude jobs review them, and the owner or the coordinator adds the marker after checking that head.

Every reviewer posts findings as review threads, and the ruleset requires every thread to be resolved, whichever path passed the gate. Fix the finding, or reply with the reason it does not apply and resolve the thread.

Remaining risk: a PR can still add a `pull_request` workflow with a job named `review-gate`. The real gate detects that check and fails, but GitHub may still show the spoofed check as passing next to it, and the ruleset matches required checks by name and app. Lanes also push with the owner's account, so the protected-files rule does not stop a lane from editing a workflow. Whoever merges, lane or coordinator, therefore reads the `review-gate` run and the head's other check runs before every merge, and the coordinator audits lane merges under [Lane merges](#lane-merges). A PR that edits a workflow or the guard counts as sensitive, so a lane never merges it, and its head is scored by two seats under the [merge gate](#merge-gate). The ruleset does not require code-owner review, so CODEOWNERS only requests the owner's review. A future hardening step is a dedicated GitHub App that posts `review-gate` as its own check run, with the ruleset pinned to that app's `integration_id`, so no workflow job can produce a matching check.

### Factory Droid

**Suspended (GRE-1222).** The Factory quota is out, so `droid-review` runs only when the repository variable `PP_DROID_REVIEW` is `on`; it is unset, so the job reports `skipped`. The gate then passes through the Claude reviews or the coordinator marker. Droid stays off until a provider-neutral gate replaces it or the quota returns; to restore it, set `PP_DROID_REVIEW` to `on`. The rest of this section describes the job when it runs.

Droid is optional. `.github/workflows/droid-review.yml` runs Droid code and security review on every ready PR and on each new push. When Droid's weekly usage runs out, the job fails and one of the other paths carries the PR; when Droid does review, its threads still have to be resolved. Critical and high security findings also request changes. The job fails when the `FACTORY_API_KEY` secret is missing. On a PR that adds or changes `droid-review.yml` itself, droid-action skips the review and the job still passes, so ask for `@droid review` after merging such a change. Because the job conclusion does not show a skip, the gate reads the job log and requires exactly one whole runner line for droid-action's review step, `##[end-action id=__Factory-AI_droid-action.droid;outcome=success;conclusion=...;duration_ms=...]`, after the timestamp. Text that only contains that string, or a second line for the step, fails. That line belongs to the pinned droid-action version; if an update renames the step, the Droid path stops counting until the gate is updated, which fails closed.

Mention `@droid` in a PR or issue comment to ask Droid a question or request another review (`.github/workflows/droid.yml`).

### Claude

`.github/workflows/claude-review.yml` runs two `anthropics/claude-code-action` jobs with Claude Opus 5.5 (`claude-opus-5-5`) on every ready PR head. `claude-code-review` looks for correctness bugs and breaks of the AGENTS.md rules. `claude-security-review` looks for injection, missing authentication or admission checks, secrets in logs or responses, path traversal, unsafe file operations, unconfirmed printer sends, and unsafe CI changes. Both post findings as inline review comments.

- **Trusted authors only.** The action is not hardened against prompt injection, so both jobs run only on PRs from this repository authored by the owner account or by Dependabot (`allowed_bots: dependabot`). PRs from forks or any other account skip both jobs and need Droid or a coordinator record. Dependabot runs receive only Dependabot secrets, so the owner must also store `CLAUDE_CODE_OAUTH_TOKEN` under **Settings > Secrets and variables > Dependabot** for its PRs to be reviewed.
- **No shell, no gh, no PR code.** A workflow step writes the PR diff to `.pp-review/pr.diff`. Claude's only allowed tools are `Read(./**)`, which keeps reads inside the checked-out workspace, and the action's inline-comment tool. An explicit `--disallowedTools` list blocks Bash, edits, web access, subagents, `.git/`, and the runner paths that hold credentials (`/proc`, `/tmp`, home dotfiles such as `~/.git-credentials` and `~/.config`, `~/work/_temp`). Keep that list in step with the secrets the jobs expose: a new secret env var or credential file in `claude-review.yml` needs its path denied too. Because `Read` follows symlinks and deny rules match the link's own path, a step before Claude fails the job when the checkout contains any symlink at all, including one into `.git/`. The repository tracks none (`git ls-files -s` lists no mode `120000`), and `scripts/test_review_gate.py` checks that. Checkout uses `persist-credentials: false`; the action still writes its own `GITHUB_TOKEN` into `.git/config`, which is why `.git/` is denied. The inline-comment server receives only `GITHUB_TOKEN` and repo details, never the Claude credential, and `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` scrubs secrets from any subprocess. Claude never sees PR comments, so another account's comment cannot inject instructions. The action replaces `.claude/`, `.mcp.json`, `CLAUDE.md`, and the other files Claude Code reads at startup with the base branch's copies.
- **Subscription, not per-call billing.** The jobs sign in with the owner's Claude subscription through the `CLAUDE_CODE_OAUTH_TOKEN` repository secret, which the owner creates with `claude setup-token`. If only `ANTHROPIC_API_KEY` is set, the jobs use it instead, and that key bills per call. With neither secret, both jobs fail with an error naming them.
- **Usage limits.** Each push to a ready PR runs both jobs, and each review draws on the subscription's usage limits. When the limit runs out the jobs fail, which is why `review-gate` keeps the Droid and coordinator-record paths. Drafts skip both jobs, so keep a PR in draft while pushing work in progress.

### Coordinator review record

When no automatic review passes the gate, for example while Droid is out of usage on a PR the Claude jobs skip, the coordinator records its own review:

1. Check out the PR head at its exact SHA in a separate worktree, for example `git fetch origin pull/<n>/head` and then `git worktree add --detach <path> <sha>`.
2. Run an independent model review on that checkout. The reviewer comes from a different vendor than the PR's author; use the [scorer seat](#scorer-seats) order. Post its findings as PR review threads.
3. Wait until every finding is fixed or answered and its thread resolved. A fix is a new push, so start again at step 1 for the new head.
4. From the coordinator worktree, post the marker as a PR comment:

   ```sh
   gh pr comment <n> --body '<!-- pp-review-gate head=<40-character sha> reviewer=<model id> findings=<count> -->'
   ```

The gate counts a marker only when the comment's author is `poitee` (the workflows pin that login instead of reading `github.repository_owner`, so moving the repository to an organization changes nothing) and one line of the comment is exactly that text: single spaces, the fields in that order, `head=` equal to the PR's current head SHA in full lowercase, `reviewer=` made of letters, digits and `._:/@+-`, and `findings=` a whole number. A marker for an older head, or in any other spelling or encoding, never counts.

Outside the [coordinator worktree](agent-workflow.md#coordinator-worktree), the safety guard refuses any command line that names `pp-review-gate` in any case, raw or after decoding JSON and GraphQL escapes, URL encoding, or HTML entities, wherever it appears: in a `gh` comment, a `bash -c` or `python3 -c` script, or an `echo` into a file for later posting. It classifies the `-c` scripts of `sh`, `bash`, `zsh`, and `dash` like top-level commands, and refuses `eval` or `source` of anything that runs `git` or `gh`. It also refuses a comment body it cannot read literally: a variable (`$VAR`, `${...}`), a command substitution (`$(...)` or backticks), an ANSI-C `$'...'` string, stdin, a missing file, a REST `--input` body that is not JSON, or a body file in a command line with other commands.

Remaining risk: the guard cannot see a marker that a program builds at run time, for example from base64 or character codes. The gate accepts only the exact raw marker from the owner account, so such a marker passes only if it is built exactly. A lane's marker and the coordinator's both come from `poitee`, so the author of a marker proves nothing. That is why a head whose `review-gate` passed only through the coordinator record merges only from the coordinator worktree, after the coordinator's own scorer run on that head (item 5 of the [merge gate](#merge-gate)). A lane never merges a marker-only head.

### Copilot

The owner can also turn on **Automatically request Copilot code review** in the main ruleset. Copilot's comments are review threads that must be resolved like any other, but Copilot does not satisfy `review-gate`.

## Parity scope

- Match Node on the data a real library holds and on everything Node's own code writes. Verify with a copy of real data or with synthetic data, never with the live library or its database.
- Divergence on inputs the app or a user would not produce (exotic Unicode, V8 or js-yaml quirks, generated differential cases) is listed in `docs/desktop-rebuild.md` as a known difference, not fixed.
- Resource limits are simple caps: file size, depth, item count, a timeout. No per-allocation budget accounting.
- Print Partner is single-user. Do no tenant-isolation or multi-user contract work.

## Work in progress

- At most 2 tickets in flight per agent. If about 10 are already In Review across the project, help land those before opening new PRs.
- Fold small fixes into the next PR that touches the same code. Aim for 300 to 600 changed lines, with about 1,500 as the hard ceiling. Mission PRs stay under about 600.

## Evidence

Put verification output under `## Evidence` in the PR body. The `Lint Evidence block` CI check fails when that section is empty and the `Exemption:` line gives no reason. A PR that changes the web UI or desktop app also needs a screenshot or video under Evidence. [Gate evidence](../contributing/gate-evidence.md) describes the check and the desktop capture steps.
