---
name: pp-lane
description: Standard Print Partner lane procedure, from a GRE ticket to a PR and a readiness report. Use when a brief names a GRE ticket for a lane to implement.
---

# Print Partner lane

A lane takes one GRE ticket from `main` to an open PR and reports it ready. Lanes never merge; the coordinator merges every PR under the [merge gate](../../../docs/development/definition-of-done.md#merge-gate). The brief names the ticket and anything special; everything else is here. The [agent guide](../../../AGENTS.md) rules apply throughout, and the brief wins where it says otherwise.

## Start

1. Fetch and branch from current `main`: `git fetch origin` then `git checkout -B <branch> origin/main`. Worktrees often start on an old commit. Write `git` and `gh` arguments literally; the [safety guard](../../../docs/development/agent-workflow.md#safety-guard) refuses shell expansions in them.
2. Read the ticket's acceptance items and its newest **Handoff** comment, if any ([Pick up a ticket](../../../docs/development/agent-workflow.md#pick-up-a-ticket)). Comment "Claimed by <lane>" on the ticket.
3. Read the [definition of done](../../../docs/development/definition-of-done.md), [Review and merge](../../../docs/development/agent-workflow.md#review-and-merge), and the docs for the code you touch ([Architecture](../../../docs/architecture.md), [Desktop rebuild flag](../../../docs/desktop-rebuild.md), and any page the ticket links).
4. Check overlap: list open PRs (`gh pr list --state open`) and skip or coordinate with any that touch the same files.
5. Decide whether the PR is sensitive (below). Sensitive PRs run the full local set.

## Build

- Keep rebuild work behind the default-off `desktop_rebuild` feature and `PP_DESKTOP_REBUILD`. No Node fallback for mutations, no dual writes, and no real printer sends.
- Design calls inside the approved plan, the AGENTS.md rules, and the data invariants go to the coordinator; scope, cutover, defaults, data formats, and security posture go to the owner. See [Design calls](../../../docs/development/definition-of-done.md#design-calls).
- Aim for 300 to 600 changed lines; about 1,500 is the hard ceiling. Split larger work into sequential PRs.
- **No stacking.** Never branch from an open PR. Dependent work starts from `main` after the parent merges.
- Update the matching docs in the same PR. Run generators; never hand-edit generated files.

## Check

**Normal PRs run `just ci --fast`**: every local CI step except `nextest-off`, `nextest-on`, `doc-off`, and `doc-on`, plus `nextest-affected`, which runs nextest with `desktop_rebuild` on for the crates the branch changes and their reverse dependents. Run `git fetch origin` first so the merge base is current. [Run the required checks with one command](../../../docs/development/ci.md#run-the-required-checks-with-one-command) has the details. **Sensitive PRs run the full `just ci`.** Rust builds share one sccache through `RUSTC_WRAPPER`; leave it set. A SKIP is not a pass: list each one under Evidence.

## Sensitive PRs

A PR is sensitive when it touches schema or migrations, auth and sessions, backup and restore, Accept and publication, secret handling, or the protected-files paths: `.github/workflows/`, `.github/zizmor.yml`, `scripts/agent-guard.py`, `.factory/`, and `.claude/`. When unsure, it is sensitive. Say so in the PR body and in your readiness report. The coordinator scores its exact head with two of the [scorer seats](../../../docs/development/definition-of-done.md#scorer-seats) and needs 9 or more from each. The owner merges changes to the safety guard, its hook adapters, and CI controls.

## Review

Before pushing, have a fresh-context reviewer read the diff: correctness and AGENTS.md rules always, and security for anything near auth, files, printers, secrets, or CI. Fix real findings. On the PR, fix each review thread's finding or reply with why it doesn't apply, then resolve the thread. P0 and P1 findings block. A P2 that affects real user data or security, or causes a crash, becomes a bug ticket (see the [review policy](../../../docs/development/definition-of-done.md#review-and-merge-policy)); only other P2s and P3s go on GRE-974. At most two fix rounds after the first review; then escalate on the ticket as the [review policy](../../../docs/development/definition-of-done.md#review-and-merge-policy) says.

## Push and open the PR

1. Push once: `git push --no-follow-tags origin HEAD:<branch>`. Never force-push.
2. Open the PR against `main`. The body starts with `Fixes GRE-<n>`, says what changed and why, says whether the PR is sensitive, and has `## Evidence` with each command and its result. Unmet acceptance items go to a child ticket that quotes them verbatim, never `Refs`.
3. UI or desktop changes also need screenshots or video under Evidence; see [Gate evidence](../../../docs/contributing/gate-evidence.md). For the replacement UI on a Rust-only core, use the harness in two steps. First capture with `scripts/evidence/run.sh --shot <path>` (add `--step` or `--module` as needed); it never publishes. Then open each PNG it lists and check that it shows the change and nothing private. Only then publish with `python3 scripts/evidence/publisher.py <pr-number> <files...>`, the only publish path. The [harness README](../../../scripts/evidence/README.md) has the options.
4. Don't edit the PR body after CI starts; every edit reruns the gate.
5. Delete the worktree's `rust/target` to free disk.

## While CI runs

- **Standing rebase rule** (done by merging, not rebasing). When `main` moves and the PR conflicts, merge `origin/main` into the branch, regenerate generated files, rerun the affected checks, and push, without asking. These pushes don't count against "push once". Never rebase a pushed branch or force-push.
- No CI reruns, except once per job for a flake that has a ticket or an external 429 or 5xx.
- `rust (windows)` is advisory; don't chase a red Windows job unless the ticket is Windows work.

## Report readiness

Never merge. The safety guard refuses `gh pr merge` and `gh api` merges outside the coordinator worktree. When the PR is done, report readiness to the coordinator:

- the PR URL and the full head SHA;
- the state of checks and reviews on that head: required checks, `review-gate`, any pending or failing check, and open review threads;
- whether the PR is sensitive, and why;
- any acceptance item moved to a child ticket.

If you stop with work in flight, post a [Handoff](../../../docs/development/agent-workflow.md#post-a-handoff). Finish with `PR <full URL>, head <SHA>, checks and reviews <state>`.
