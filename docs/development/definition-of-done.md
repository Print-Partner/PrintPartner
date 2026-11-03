# Definition of done

Every pull request meets this bar before it merges. The [pull request template](../../.github/pull_request_template.md) repeats it as a checklist.

## The bar

1. **One job per PR.** Keep the diff under about 1,500 lines, or about 600 for a Mission PR. If you find work that belongs to another ticket, note it on that ticket and leave it out of this PR.
2. **Unslopped.** Run the unslop pass on code, comments and docs before review. Remove filler comments, dead code and speculative abstractions.
3. **Reviewed.** Factory Droid's automatic review runs on the exact head commit, every review thread is resolved, and required CI is green on that commit. The coordinator's scorer run passes the [merge gate](#merge-gate).
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
2. **Required checks** are green.
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

`.github/workflows/droid-review.yml` runs Factory Droid code and security review on every ready PR and on each new push. Its `droid-review` job is a required check, and the main ruleset requires review threads to be resolved, so an open finding blocks the merge. Fix the finding, or reply with the reason it does not apply and resolve the thread. Critical and high security findings also request changes. The job fails when the `FACTORY_API_KEY` secret is missing. On a PR that adds or changes `droid-review.yml` itself, droid-action skips the review and the job passes, so ask for `@droid review` after merging such a change.

Mention `@droid` in a PR or issue comment to ask Droid a question or request another review (`.github/workflows/droid.yml`).

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
