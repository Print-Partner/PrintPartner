# Contributing

This page describes how work moves from a ticket to a merged pull request. [Building](building.md) covers the local toolchain.

## Sources of truth

- **Goals and decisions.** The Notion page "Goals and decisions (2026-10-15)" under Print Partner Roadmap holds the product goals. Where an older document disagrees, that page wins.
- **Later decisions.** Chad records later decisions on the Notion page [Agent handoff](https://app.notion.com/p/4116dd30b698814c9107f5a22b58e6b5). Where two pages disagree, the newer dated decision wins. If you can't tell which is newer, ask the owner.
- **Code and evidence.** The `main` branch of `Print-Partner/PrintPartner`.

Without Notion or Linear access, work from a task brief the task owner exports for you. [Agent and contributor workflow](agent-workflow.md#start-from-a-task-brief) lists its fields and when to stop and ask. The owner moves the ticket through its states.

## Pick a ticket

Use the [ordered Rust parity plan](https://app.notion.com/p/4246dd30b69881d6842ce4a163bda3ad)
for the sequence. Use the [Rust cutover project](https://linear.app/greetz/project/rust-cutover-d28381ab0885)
in Linear for ticket status and
blocking relations. Check the next plan step's blockers in Linear before
starting it through `/implement`, one ticket per fresh session.

Open one PR per ticket. If you find work that belongs to another ticket, add a comment to that ticket instead of changing it in your PR.

## Rebuild feature default

GRE-1646 (M0) turns the `desktop_rebuild` Cargo feature on by default for the crates the Rust-only image builds. `pp-desktop` and `pp-drivers` stay off unless you name the feature. Code that a user can see also checks `PP_DESKTOP_REBUILD`, which still defaults off outside the Rust-only image. `--no-default-features` builds the old path. [Desktop rebuild flag](../desktop-rebuild.md) describes the flag and the slice plan.

## TypeScript naming

The web ESLint config enforces `camelCase` for ordinary identifiers, `PascalCase` for types, and `UPPER_CASE` for constants. Property names, imports, and destructured names are exempt so contract, JSON, SQLite, and third-party field names stay unchanged. Generated `*.generated.ts` files are exempt; fix their names in the generator instead. `web/eslint-suppressions.json` tracks existing violations in the legacy Node server. Remove entries as those names are corrected; do not increase the baseline to hide new violations.

## Rust test renames

[Test file renames](../records/test-renames.md) is the crate index and naming guide.
Record old paths, new paths and tickets in `docs/records/test-renames/<crate>.md`.
Rename PRs add rows only to their crate's file, so unrelated crates do not edit a
shared table. Keep existing rows verbatim and follow the index's rules for binary
names, module paths and command references.

## Complexity limits

The web ESLint config limits `complexity` to 20, nesting depth to 5 and parameters to 5 in every TypeScript and JavaScript file. Existing violations are recorded in `web/eslint-suppressions.json` with the naming violations, as a count per file and rule. A new violation, or one more in a file that already has some, fails `npm run lint`. When you simplify code, run `npx eslint . --prune-suppressions` from `web/` and commit the lower counts. The replacement UI's layer rule (`print-partner/ui-boundary`, [UI layers](../../web/apps/ui/README.md#layers-inside-src-gre-1537)) records its 16 existing cross-layer imports there the same way; a new one fails lint, and when you remove one you must prune its count the same way. Function and file length limits come after the Prettier rollout, because reformatting changes line counts.

## Open the pull request

1. Meet the [definition of done](definition-of-done.md).
2. Before your first push, run `just ci --fast` for a normal PR, or the full `just ci` for a data-loss change as [Who merges](definition-of-done.md#who-merges) defines it. Fix every failure, and name the command under `## Evidence`. A later push, for a merge of `main` or a review fix, runs the short checks in [Set up and run checks](agent-workflow.md#set-up-and-run-checks) and relies on hosted CI. [Gate evidence](../contributing/gate-evidence.md#run-ci-locally-before-you-push) describes both.
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
