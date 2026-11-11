# Contributing

This page describes how work moves from a ticket to a merged pull request. [Building](building.md) covers the local toolchain.

## Sources of truth

- **Goals and decisions.** The Notion page "Goals and decisions (2026-10-15)" under Print Partner Roadmap holds the product goals. Where an older document disagrees, that page wins.
- **Plan and later decisions.** The plan is the Notion page "Print Partner: execution plan v17.1" (Chad, 2026-11-04). Chad's decisions since then are dated, newest first, on the Notion page [Agent handoff](https://app.notion.com/p/4116dd30b698814c9107f5a22b58e6b5). Where two pages disagree, the newer dated decision wins. If you can't tell which is newer, ask the owner. [Handoffs](agent-workflow.md#handoffs) says where live status and work in progress live.
- **Execution.** The Linear project "Print Partner Desktop (Rust core)". Milestones 1 to 9 are the delivery chunks. The desktop rebuild slices (D-slices) are sub-issues of GRE-595.
- **Code and evidence.** The `main` branch of `Print-Partner/PrintPartner`.

Without Notion or Linear access, work from a task brief the task owner exports for you. [Agent and contributor workflow](agent-workflow.md#start-from-a-task-brief) lists its fields and when to stop and ask. The owner moves the ticket through its states.

## Pick a ticket

1. Take the highest-priority unblocked ticket in the lowest-numbered milestone.
2. Check the ticket's blocking relations in Linear before you start.
3. Move the ticket to In Progress.

Open one PR per ticket. If you find work that belongs to another ticket, add a comment to that ticket instead of changing it in your PR.

## Keep rebuild code behind the flag

New Rust rebuild code compiles only with the `desktop_rebuild` Cargo feature, which is off by default. Code that a user can see also checks the `PP_DESKTOP_REBUILD` environment variable. Do not turn `desktop_rebuild` on by default; that is the maintainer's decision. [Desktop rebuild flag](../desktop-rebuild.md) describes the flag and the slice plan.

## TypeScript naming

The web ESLint config enforces `camelCase` for ordinary identifiers, `PascalCase` for types, and `UPPER_CASE` for constants. Property names, imports, and destructured names are exempt so contract, JSON, SQLite, and third-party field names stay unchanged. Generated `*.generated.ts` files are exempt; fix their names in the generator instead. `web/eslint-suppressions.json` tracks existing violations in the legacy Node server. Remove entries as those names are corrected; do not increase the baseline to hide new violations.

## Rust test renames

[Test file renames](../records/test-renames.md) is the crate index and naming guide.
Record old paths, new paths and tickets in `docs/records/test-renames/<crate>.md`.
Rename PRs add rows only to their crate's file, so unrelated crates do not edit a
shared table. Keep existing rows verbatim and follow the index's rules for binary
names, module paths and command references.

## Complexity limits

The web ESLint config limits `complexity` to 20, nesting depth to 5 and parameters to 5 in every TypeScript and JavaScript file. Existing violations are recorded in `web/eslint-suppressions.json` with the naming violations, as a count per file and rule. A new violation, or one more in a file that already has some, fails `npm run lint`. When you simplify code, run `npx eslint . --prune-suppressions` from `web/` and commit the lower counts. Function and file length limits come after the Prettier rollout, because reformatting changes line counts.

## Open the pull request

1. Meet the [definition of done](definition-of-done.md).
2. Before you push, run `just ci --fast` for a normal PR, or the full `just ci` for a sensitive PR as the [merge gate](definition-of-done.md#merge-gate) defines it. Fix every failure, and name the command under `## Evidence`. [Gate evidence](../contributing/gate-evidence.md#run-ci-locally-before-you-push) describes both.
3. Fill in every section of the [pull request template](../../.github/pull_request_template.md). Put verification output under `## Evidence`.
4. Start the PR description with `Fixes GRE-xxx`. Split unmet acceptance items into a child ticket that quotes them verbatim, as [Fixes only](definition-of-done.md#fixes-only) describes.
5. Don't move the ticket yourself. `linear-pr-sync.yml` sets In Review, Done, and In Progress from PR events. If the PR is blocked or escalated, add a comment to the ticket.

## After the merge

- Add a comment to the ticket that says what landed and what remains.
- Do not mark a parent ticket Done because one child merged.

## Changes that need the maintainer

Prepare the files and ask the maintainer for changes to:

- GitHub repository settings and branch protection.
- Releases and deployments.
- The production instance.
