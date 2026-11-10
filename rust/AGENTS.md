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

- Do not enable `desktop_rebuild` or `PP_DESKTOP_REBUILD` by default.
- Do not add mutation fallback to Node or write SQL outside `pp-storage`.
- Do not send real prints during tests.
