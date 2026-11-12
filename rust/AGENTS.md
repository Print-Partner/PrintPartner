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
- **Transport shapes live in `pp-api`.** Storage and services return domain results and typed errors. `pp-api` turns them into status codes, response bodies and MCP results, once, for HTTP and MCP alike.
- **Product contract.** Behaviour comes from the pages under `docs/`. Where Node did something by accident of JavaScript, choose the simpler correct behaviour and say so in the PR ([ADR 0016](../docs/adr/0016-rust-owns-its-data-formats.md)).
- **Sized for one user.** Print Partner runs for one person on one machine. Use simple caps and standard platform mechanisms for limits, locks and recovery.

## Guardrails

- Do not enable `desktop_rebuild` or `PP_DESKTOP_REBUILD` by default.
- Do not add mutation fallback to Node or write SQL outside `pp-storage`.
- Do not send real prints during tests.
