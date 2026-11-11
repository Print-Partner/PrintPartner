# pp-perf

Performance measurement records, Linux process-tree sampling, and the 10% regression comparison. It has no `pp-*` dependencies. It is not product telemetry: nothing here sends data anywhere.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns the `Measurement` record (schema `SCHEMA_VERSION` = 1): every raw sample plus the `Machine`, `Artifact` and `Toolchain` that produced it.
- Compares a candidate against a baseline. `compare` applies `REGRESSION_GATE_PCT` (10%) to medians only when both sides come from the same setup, stay within their noise tolerance, and have at least `MIN_SAMPLES` (5) samples; otherwise it lists every `Mismatch`. It never reports an improvement.
- Samples Linux process-tree PSS and CPU ticks from `/proc`. Linux PSS and macOS footprint/RSS are separate `Metric` values and are never compared with each other.

## Not responsible for

- Launching the app under test and the capture CLI: `pp_core::perf_launch` and the `pp-perf-capture` binary in `pp-core`.
- Remote export, analytics or Sentry: none exist, and none belong here.
- Absolute performance budgets: set after baseline measurement and owner review.

## Interface

| Item | What it does |
| --- | --- |
| `Measurement`, `Metric`, `Machine`, `Artifact`, `Toolchain` | Serde records written by captures. `Metric::required_os` names the only OS a memory metric may come from. |
| `compare(baseline, candidate) -> Comparison` | `Comparison` holds a `Verdict` or the `Mismatch` list; `Side` names which side a mismatch is on. |
| `sample_process_tree`, `sample_process_tree_at` | Sum PSS over a root PID and its descendants. `ProcessTreePss`, `PssError`. |
| `tree_cpu_ticks`, `tree_cpu_ticks_at`, `clock_ticks_per_second` | Process-tree CPU ticks, for idle CPU. |
| `this_machine`, `package_bytes`, `utc_timestamp` | Capture metadata. |

The `_at` variants read a procfs-shaped directory, so tests can use a fake `/proc`. No binaries.

## Effects and flags

- Reads files: `/proc` (Linux only; other OSes return `PssError::Unsupported`), `/proc/cpuinfo`, `/proc/sys/kernel/osrelease`, and the path given to `package_bytes`.
- Calls `libc::sysconf(_SC_CLK_TCK)` on Unix.
- No writes, network, SQLite, env vars, threads or child processes. Cargo features: none.

## Tests

- `tests/perf_record_gate.rs`: record round trip, `compare` verdicts and mismatches, and PSS sums over a fake procfs in a `tempfile` directory.
- `tests/perf_capture.rs`: CPU ticks over a fake procfs, `package_bytes` (symlinks not followed) and `utc_timestamp`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-perf
```
