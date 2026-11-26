# Node compatibility bridge

Frozen, retiring at whole-runtime cutover. Rust owns Node supervision and the private channel; Node owns compatibility domain behaviour.

When evaluating removal of the bridge, read [Whole-runtime cutover](../../../docs/adr/0010-whole-runtime-cutover.md).

Run from the repository root:

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-compat
```

- Do not grow the compatibility bridge or add new domain operations.
- Do not retire Node before replacement acceptance and cutover approval.
