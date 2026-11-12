# Drain test scratch isolation

`scratch_root_is_empty_after_drain_tests` re-executes the drain test binary
with itself skipped and `--test-threads=1`. The shared helper in
`support/scratch.rs` gives that subprocess a private temporary root through
`TMPDIR`, `TMP`, `TEMP`, and `CARGO_TARGET_TMPDIR`. It checks subprocess success,
rejects a run with no tests, and asserts that the root is empty afterward.

The check covers only files under that root. It cannot detect writes to
hardcoded `/tmp` paths or explicit paths outside the root. An empty root does
not prove process cleanup. The drain tests retain their separate process and
deadline assertions.

Nextest reserves all configured test slots for this exact leak detector through
the default profile override. `pp-local-linux` inherits that override. The nested
run executes one test at a time, so it does not multiply concurrent process tests.
The reservation applies to one nextest invocation. It is not a machine-wide lock.

For `cargo test`, outer libtest parallelism also requires `--test-threads=1`.
Run these commands from `rust`.

```sh
cargo test -p pp-core --test process_group_drain -- --test-threads=1
cargo nextest run -p pp-core --test process_group_drain
cargo nextest run -P pp-local-linux -p pp-core --test process_group_drain
```
