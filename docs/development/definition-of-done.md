# Definition of done

Every pull request meets this bar before it merges. The [pull request template](../../.github/pull_request_template.md) repeats it as a checklist.

Work comes from the Rust cutover project, one ticket per fresh session, through `/implement`.

## The bar

1. **One job per PR.** Aim for 300 to 600 changed lines. About 1,500 is the hard ceiling, or about 600 for a cross-cutting PR. Split larger work into sequential PRs: each one starts from `main` after the previous one merges, never stacked on an open PR's branch. If you find work that belongs to another ticket, note it on that ticket and leave it out of this PR.
2. **Unslopped.** Run the unslop pass on code, comments and docs before review. Remove filler comments, dead code and speculative abstractions.
3. **Reviewed.** Required CI is green on the head, and every review thread is resolved. A second review is required only for a [data-loss change](#who-merges). Chad merges.
4. **Docs in the same PR.** When the PR changes behavior, setup or an interface, update the matching page under `docs/` in the same PR.
5. **Rust test renames.** Record every renamed file in its crate's page under [Test file renames](../records/test-renames.md). Rename PRs add rows only to their crate's file; preserve existing rows verbatim. The shared index is navigation.

## Who merges

Chad merges. An agent opens the PR and stops.

A second review, by someone other than the author, is required only when the change can lose or corrupt data:

- import
- schema
- backup and restore
- Checkoff
- send
- Accept and publication (`plan_publication`, `plates/initialize.rs`, `working_drafts/save.rs`; owner decision, 2026-11-12)

Say that in the PR body. Other changes need the required checks green and the review threads resolved. Chad still merges those.

## Fixes only

Every PR starts its description with `Fixes GRE-xxx` and closes its ticket when it merges. Don't write `Refs GRE-xxx`.

A ticket's work is finished when its child tickets are closed too. Count open children when you report progress on a ticket or a plan.

When some acceptance items stay unmet, split them out before the merge: create a child ticket of the PR's ticket whose description quotes each unmet item verbatim, then merge with `Fixes` on the parent. Create other tickets only for bugs on main, or splits the parent ticket already calls for.

## Ticket states

`.github/workflows/linear-pr-sync.yml` runs `scripts/linear/pr_status_sync.py` and owns every ticket state that follows a PR: In Review when a PR opens, Done when a `Fixes` PR merges, and back to In Progress when a PR closes unmerged. Don't set those states by hand or from an agent. The sync skips an owner-signed list of protected tickets, listed in `PROTECTED` in that script, and logs each skipped write; the owner moves those tickets.

## Parity scope

- Match Node on the data a real library holds and on everything Node's own code writes: the import must carry it into Rust intact ([ADR 0016](../adr/0016-rust-owns-its-data-formats.md)). Verify with a copy of real data or with synthetic data, never with the live library or its database.
- Build behaviour to the product contract in `docs/`. Node's error strings, type coercions and ordering are not requirements, so new code, tests and goldens leave them out. Where the contract is silent and Node's behaviour was an accident of JavaScript, choose the simpler correct behaviour and name the difference in the PR.
- Divergence on inputs the app or a user would not produce (exotic Unicode, V8 or js-yaml quirks, generated differential cases) is listed in `docs/desktop-rebuild.md` as a known difference, not fixed.
- Resource limits are simple caps: file size, depth, item count, a timeout. No per-allocation budget accounting.
- Print Partner is single-user. Do no tenant-isolation or multi-user contract work.

## One ticket at a time

Until the owner has walked [the daily loop](../product/daily-loop.md) on Rust, take only a ticket labelled `daily-loop`. Two exceptions: a PR already in the merge queue or one push from it, and a fix for a red `main`. With no such ticket, stop. New tickets are for a bug on `main` or a split a `daily-loop` ticket calls for.

One ticket per fresh session. Fold a small fix into the next PR that touches the same code. Aim for 300 to 600 changed lines, with about 1,500 as the hard ceiling. Cross-cutting PRs stay under about 600.
