# Node compatibility bridge

Frozen, retiring at whole-runtime cutover. Rust owns Node supervision and the private channel; Node owns compatibility domain behaviour.

Read [the authority page](../../../docs/architecture.md) before changing behaviour.
Follow [agent workflow](../../../docs/development/agent-workflow.md) for checks, evidence and handoffs.

Run from the repository root:

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-compat
cargo nextest run --manifest-path rust/Cargo.toml -p pp-compat --features desktop_rebuild
```

- Do not grow the compatibility bridge or add new domain operations.
- Do not retire Node before replacement acceptance and cutover approval.
