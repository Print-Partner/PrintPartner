# Definition of done

Every pull request meets this bar before it merges. The [pull request template](../../.github/pull_request_template.md) repeats it as a checklist.

## The bar

1. **One job per PR.** Keep the diff under about 1,500 lines, or about 600 for a Mission PR. If you find work that belongs to another ticket, note it on that ticket and leave it out of this PR.
2. **Unslopped.** Run the unslop pass on code, comments and docs before review. Remove filler comments, dead code and speculative abstractions.
3. **Reviewed.** The required `review-gate` check passes on the exact head commit (see [Code review](#code-review)), every review thread is resolved, and required CI is green on that commit. The coordinator's scorer run passes the [merge gate](#merge-gate).
4. **Docs in the same PR.** When the PR changes behavior, setup or an interface, update the matching page under `docs/` in the same PR.

## Review and merge policy

- P0 and P1 findings block the merge. Fix them in the PR.
- A P2 that affects real user data, security, or causes a crash becomes a bug ticket in its chunk.
- Every other P2 and all P3s go on the post-parity polish list (GRE-974) or in a PR comment. Do not create a ticket for them.
- After the first review, a PR gets at most two fix rounds. If new P0/P1 findings still appear, stop. Post an escalation comment on the ticket with the findings and two or three options (split the PR, replace custom code with a maintained library or standard tool, or defer), then pick other work.
- Prefer maintained libraries and standard platform mechanisms over custom hardening. Examples are OS file locks, CODEOWNERS, actionlint and zizmor.

## Fixes only

Every PR starts its description with `Fixes GRE-xxx` and closes its ticket when it merges. Don't write `Refs GRE-xxx`.

When some acceptance items stay unmet, split them out before the merge: create a child ticket of the PR's ticket whose description quotes each unmet item verbatim, then merge with `Fixes` on the parent. The scorer checks that the quotes match the parent's unmet items. Create other tickets only for bugs on main, P0/P1 findings, or splits the parent ticket already calls for.

## Ticket states

`.github/workflows/linear-pr-sync.yml` runs `scripts/linear/pr_status_sync.py` and owns every ticket state that follows a PR: In Review when a PR opens, Done when a `Fixes` PR merges, and back to In Progress when a PR closes unmerged. Don't set those states by hand or from an agent. The sync skips an owner-signed list of protected tickets, listed in `PROTECTED` in that script, and logs each skipped write; the owner moves those tickets.

## Merge gate

The coordinator merges, from its own worktree (see [Safety guard](agent-workflow.md#safety-guard)). Lanes and other agents report readiness and never merge. The owner merges changes to the safety guard, its hook adapters, and CI controls.

The coordinator merges a PR only when all of these hold on the same head commit:

1. **Exact head.** Every check, review, and score below is on the PR's current head SHA.
2. **Required checks** are green, including `review-gate`.
3. **Every other reported check** passes or is skipped. Only these advisory checks may fail: `rust (macos)`. The owner extends that list.
4. **All review threads** are resolved.
5. **Scorer.** The coordinator runs its own scorer on that head. The seat is from a different vendor than the PR's author, and the score is strictly above 8.
6. **Sensitive PRs** that touch schema or migrations, auth, backup, Accept, or secrets need a score of 9 or more from each of two seats.
7. **Command.** `gh pr merge <number> --squash --match-head-commit <sha>`, with the full 40-character head SHA. Never `--admin` or `--auto`.

Scorer seats, in order. Use the first available seat whose vendor differs from the author's:

1. OpenCode `opencode/pulse-alpha-free`, falling back to `opencode/deepseek-v4.1-flash-free`.
2. Cursor `grok-4.7`.
3. Pi `halo/qwen38-flash`.

`post-score.sh` posts a score as a PR comment. That comment is a record only. A posted score from a lane or an earlier head does not count; the gate is the coordinator's own scorer run on the exact head.

## Code review

The required `review-gate` check (`.github/workflows/review-gate.yml`) passes on a PR head when any of these holds for that exact head SHA:

1. **Factory Droid.** The `droid-review` job of `.github/workflows/droid-review.yml` succeeded.
2. **Claude.** The `claude-code-review` job of `.github/workflows/claude-review.yml` succeeded, and `claude-security-review` succeeded or was neutral.
3. **Coordinator record.** A comment by the repository owner account, `poitee`, carries a `pp-review-gate` marker for that head. See [Coordinator review record](#coordinator-review-record).

Every push starts a new `review-gate` run, which fails until one of these holds for the new head. `.github/workflows/review-gate-rerun.yml` reruns the gate when a Droid or Claude review run finishes and when the owner creates, edits, or deletes a marker comment. A rerun repeats the default branch's gate, so a fork workflow that borrows a review workflow's name can trigger a rerun but cannot make it pass. If the check stays red after a review finished, rerun the `review-gate` job from the PR's Checks tab.

The gate runs on `pull_request_target`, so GitHub runs the default branch's copy of `review-gate.yml`, never the PR's. The job reads the GitHub API only, with read permissions and no secrets, and never checks out or runs PR code. It applies these checks before any path counts:

- **Result origin.** A `droid-review` or Claude result counts only when its check suite comes from the GitHub Actions app, from the expected workflow path, on `pull_request`, and GitHub ran the default branch's copy of that file. That holds when the file's blob at the head equals the default branch's blob, or the PR does not change the file, so the merge commit carries the base's copy.
- **Same-named checks.** The gate fails when any check run on the head named `review-gate`, `droid-review`, `claude-code-review`, or `claude-security-review` comes from another app, another workflow file, or another event.
- **Protected files.** The gate fails when a PR author other than the owner account changes a file under `.github/workflows/`, `.github/zizmor.yml`, or `scripts/agent-guard.py`.

Every reviewer posts findings as review threads, and the ruleset requires every thread to be resolved, whichever path passed the gate. Fix the finding, or reply with the reason it does not apply and resolve the thread.

Remaining risk: a PR can still add a `pull_request` workflow with a job named `review-gate`. The real gate detects that check and fails, but GitHub may still show the spoofed check as passing next to it, and the ruleset matches required checks by name and app. Lanes also push with the owner's account, so the protected-files rule does not stop a lane from editing a workflow. Merges therefore stay with the owner and the coordinator, who score every head under the [merge gate](#merge-gate) and read the `review-gate` run itself. The ruleset does not require code-owner review, so CODEOWNERS only requests the owner's review. A future hardening step is a dedicated GitHub App that posts `review-gate` as its own check run, with the ruleset pinned to that app's `integration_id`, so no workflow job can produce a matching check.

### Factory Droid

Droid is optional. `.github/workflows/droid-review.yml` runs Droid code and security review on every ready PR and on each new push. When Droid's weekly usage runs out, the job fails and one of the other paths carries the PR; when Droid does review, its threads still have to be resolved. Critical and high security findings also request changes. The job fails when the `FACTORY_API_KEY` secret is missing. On a PR that adds or changes `droid-review.yml` itself, droid-action skips the review and the job passes, so ask for `@droid review` after merging such a change.

Mention `@droid` in a PR or issue comment to ask Droid a question or request another review (`.github/workflows/droid.yml`).

### Claude

`.github/workflows/claude-review.yml` runs two `anthropics/claude-code-action` jobs with Claude Opus 5.5 (`claude-opus-5-5`) on every ready PR head. `claude-code-review` looks for correctness bugs and breaks of the AGENTS.md rules. `claude-security-review` looks for injection, missing authentication or admission checks, secrets in logs or responses, path traversal, unsafe file operations, unconfirmed printer sends, and unsafe CI changes. Both post findings as inline review comments.

- **Trusted authors only.** The action is not hardened against prompt injection, so both jobs run only on PRs from this repository authored by the owner account. PRs from forks, Dependabot, or any other account skip both jobs and need Droid or a coordinator record.
- **No shell, no gh, no PR code.** A workflow step writes the PR diff to `.pp-review/pr.diff`. Claude's only allowed tools are `Read(./**)`, which keeps reads inside the checked-out workspace, and the action's inline-comment tool. An explicit `--disallowedTools` list blocks Bash, edits, web access, subagents, `.git/`, and the runner paths that hold credentials (`/proc`, `/tmp`, home dotfiles, `~/work/_temp`). The inline-comment server receives only `GITHUB_TOKEN` and repo details, never the Claude credential, and `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` scrubs secrets from any subprocess. Claude never sees PR comments, so another account's comment cannot inject instructions. The action replaces `.claude/`, `.mcp.json`, `CLAUDE.md`, and the other files Claude Code reads at startup with the base branch's copies.
- **Subscription, not per-call billing.** The jobs sign in with the owner's Claude subscription through the `CLAUDE_CODE_OAUTH_TOKEN` repository secret, which the owner creates with `claude setup-token`. If only `ANTHROPIC_API_KEY` is set, the jobs use it instead, and that key bills per call. With neither secret, both jobs fail with an error naming them.
- **Usage limits.** Each push to a ready PR runs both jobs, and each review draws on the subscription's usage limits. When the limit runs out the jobs fail, which is why `review-gate` keeps the Droid and coordinator-record paths. Drafts skip both jobs, so keep a PR in draft while pushing work in progress.

### Coordinator review record

When no automatic review passes the gate, for example while Droid is out of usage on a PR the Claude jobs skip, the coordinator records its own review:

1. Check out the PR head at its exact SHA in a separate worktree, for example `git fetch origin pull/<n>/head` and then `git worktree add --detach <path> <sha>`.
2. Run an independent model review on that checkout. The reviewer comes from a different vendor than the PR's author; use the scorer seat order under [Merge gate](#merge-gate). Post its findings as PR review threads.
3. Wait until every finding is fixed or answered and its thread resolved. A fix is a new push, so start again at step 1 for the new head.
4. From the coordinator worktree, post the marker as a PR comment:

   ```sh
   gh pr comment <n> --body '<!-- pp-review-gate head=<40-character sha> reviewer=<model id> findings=<count> -->'
   ```

The gate counts a marker only when the comment's author is the repository owner account, `head=` equals the PR's current head SHA in full lowercase, `reviewer=` is not empty, and `findings=` is a whole number. A marker for an older head never counts. The safety guard refuses `gh pr comment`, `gh issue comment`, and `gh api` comment writes that contain `<!-- pp-review-gate` unless they run from the [coordinator worktree](agent-workflow.md#coordinator-merges), the same rule as merges. Outside that worktree it also refuses a comment body it cannot read literally: a variable, a command substitution, stdin, a missing file, or a body file in a command line with other commands.

### Copilot

The owner can also turn on **Automatically request Copilot code review** in the main ruleset. Copilot's comments are review threads that must be resolved like any other, but Copilot does not satisfy `review-gate`.

## Parity scope

- Match Node on the data a real library holds and on everything Node's own code writes. Verify with a copy of real data or with synthetic data, never with the live library or its database.
- Divergence on inputs the app or a user would not produce (exotic Unicode, V8 or js-yaml quirks, generated differential cases) is listed in `docs/desktop-rebuild.md` as a known difference, not fixed.
- Resource limits are simple caps: file size, depth, item count, a timeout. No per-allocation budget accounting.
- Print Partner is single-user. Do no tenant-isolation or multi-user contract work.

## Work in progress

- At most 2 tickets in flight per agent. If about 10 are already In Review across the project, help land those before opening new PRs.
- Fold small fixes into the next PR that touches the same code. Aim for 300 to 1,500 changed lines. Mission PRs stay under about 600.

## Evidence

Put verification output under `## Evidence` in the PR body. The `Lint Evidence block` CI check fails when that section is empty and the `Exemption:` line gives no reason. A PR that changes the web UI or desktop app also needs a screenshot or video under Evidence. [Gate evidence](../contributing/gate-evidence.md) describes the check and the desktop capture steps.
