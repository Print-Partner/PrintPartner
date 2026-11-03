# Consolidate to a small set of deep crates

Status: accepted on 2026-11-02. Supersedes [ADR 0007](0007-rust-crate-layout.md), whose open questions this record answers.

The Rust workspace consolidates from 15 crates to seven product crates plus one local-platform crate: `pp-model`, `pp-engine`, `pp-connect`, `pp-api`, `pp-app`, `pp-desktop`, and `pp-os`, with `pp-perf` moving to `rust/tools` as a dev tool. `pp-contracts`, `pp-services`, `pp-source` and `pp-api-types` disappear as separate crates; their code moves into the crates named above. `pp-compat` and `pp-gateway` are deleted with Node, after the listener, asset serving and rate limiting move into `pp-api`.

This answers ADR 0007's first open question by deleting the facade rather than choosing between the two names. `pp-contracts` fails the deletion test: it is 970 lines that mostly re-export `pp-api-types`, and its `autosave.rs` is a single line. Delete it and nothing reappears in callers except import paths. The dependency direction in `pp-contracts/Cargo.toml` is `pp-contracts` to `pp-api-types`, so any plan that keeps the facade and absorbs the types is backwards.

The second open question, where `pp-source` belongs, is answered by folding snapshot materialization into `pp-engine`. It writes into the directory the engine owns, and keeping it separate forces the callers to coordinate two owners of the same files.

The shape is driven by what must not break. Every module in the target has an interface a caller can state in a few lines, and the crates carry rules rather than just grouping:

- Only `pp-engine` may link SQLite, so no other crate can open the database or write SQL.
- `pp-api` may not depend on `pp-connect`, so no HTTP handler can reach a printer except by submitting an engine command.
- Only `pp-os` may name `libc` or `windows-sys`, so every platform-specific call sits in one reviewable place.
- A printer send is a command only an interactive user actor can construct, so an assistant or a background job cannot send a print.
- A Plan change is a command that produces a proposal, and only a user command accepts it, so the Accept gate is one implementation instead of one per transport or screen.

Those rules become compile errors through `rust/scripts/check-boundaries.py`, which already asserts that the workspace package set equals its `ALLOWED` map. That assertion is also why crate moves are never invisible: creating, renaming or deleting a crate requires editing the checker in the same PR. The checker currently enforces only workspace edges; the per-crate third-party deny lists above are a real addition.

The alternative shapes were considered and rejected. One crate per operation family gives the best locality, but it opens with roughly 50,000 lines of relocation before anything ships, turns every cross-family transaction into an interface negotiation, and does not split cleanly along transactions: publication reads Source revisions and allocates Required units in one write. Keeping today's 14-crate graph has the least migration risk, but it leaves `pp-storage` at 51,824 non-test lines across 85 files as both the compile unit and the navigation unit, with the fix deferred until after Node retires, which risks never happening.

What this record does not decide is timing. The consolidation happens while the runtime is being built, not in front of it, so that no domain slice is blocked by a crate move. That sequencing is set in [the architecture page](../architecture.md#migration-sequence), not here.
