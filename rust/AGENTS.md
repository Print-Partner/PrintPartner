# Rust core

Active. Rust owns replacement behaviour; `pp-core` composes operation owners. Node still owns live compatibility operations.

Read [the authority page](../docs/architecture.md) before changing behaviour.
Follow [agent workflow](../docs/development/agent-workflow.md) for checks, evidence and handoffs.

Run from the repository root:

```sh
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
python3 rust/scripts/check-boundaries.py
just ci --fast
```

## New code

These apply to new operations and new modules. Code that predates them changes when a ticket already rewrites it, or under [Simplification after cutover](../docs/architecture.md#simplification-after-cutover).

- **Typed data.** Rows and stored JSON deserialize into named structs and enums where storage reads them. Keep `serde_json::Value` for documents Rust stores without reading.
- **Typed errors.** An operation returns its family's error enum, and callers branch on its variants.
  - The enum names every failure a caller can act on: not found, conflict, busy, refused, changed, no space.
  - Storage failures no caller handles go in `WriterFailure::Internal`, which the family's enum wraps. It is the one public variant that carries an opaque `anyhow::Error`. A crate-private error type may carry one too.
  - A family's error enum never implements `From<anyhow::Error>`: with it, any `?` falls into the opaque variant unnoticed. `WriterFailure` has the one sanctioned conversion, and it sorts an error by type into `Auth`, `Busy` or `Internal`. Where a family calls an older helper that returns `anyhow`, convert at that call with `WriterFailure::from`. One family enum written before this rule still has the impl and loses it as its code is next changed: `CreateBuildError`. Do not copy it.
  - Never match on an error's text.
  - A handle constructor that only validates startup configuration may return `anyhow::Result`.
- **One way in to storage.** A write runs through the crate-private `write` helper (`crates/pp-storage/src/write.rs`), on the writer thread, in one transaction. A read runs through the crate-private `read` helper (`crates/pp-storage/src/read.rs`), on the reader pool, in one read transaction. Both run the caller's closure inside that transaction and report failure as `WriterFailure`. The closure checks the credential first, with `authorize`, so the check and the work see the same data. The older read families that run on the writer thread are not a model for new reads.
- **Transport shapes live in `pp-api`.** Storage and services return domain results and typed errors. `pp-api` turns them into status codes, response bodies and MCP results, once, for HTTP and MCP alike. A new family implements `ToProblem` for its error enum and sets its `code` with a `ProblemCode` ([Map a new family's error](../docs/problem-responses.md#map-a-new-familys-error)).
- **Product contract.** Behaviour comes from the pages under `docs/`. Where Node did something by accident of JavaScript, choose the simpler correct behaviour and say so in the PR ([ADR 0016](../docs/adr/0016-rust-owns-its-data-formats.md)).
- **Sized for one user.** Print Partner runs for one person on one machine. Use simple caps and standard platform mechanisms for limits, locks and recovery.

## Guardrails

- Do not enable `desktop_rebuild` or `PP_DESKTOP_REBUILD` by default.
- Do not add mutation fallback to Node or write SQL outside `pp-storage`.
- Do not send real prints during tests.
