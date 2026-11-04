# pp-testkit

Disposable Print Partner data directories for tests, and the format of the Rust ownership marker file. Crates use it only as a dev-dependency.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Creates a fresh temporary data directory per test (`Installation`) and deletes it on drop, so tests never touch a real data directory.
- Defines the ownership marker `.pp-rust-owned.json` (`Marker`) and the corrupt forms tests must refuse (`Corruption`).
- Builds a Node schema 34 database from the pinned `pp-storage/data/historical/schema-34.ts`.

## Not responsible for

- The real lease, migrations, and SQL: `pp-storage`. No product code reads the marker yet; only this crate defines it.
- Release builds. `rust/scripts/check-boundaries.py` refuses `pp-testkit` as a normal or build dependency.

## Interface

| Item | What it does |
| --- | --- |
| `Installation::create(spec)`, `create_in(parent, spec)` | New directory under the system temp directory or under `parent`. `path`, `marker_path`, `database_path`, `entries`, `write_marker`, `write_corrupt_marker`, `remove_marker`. |
| `Fixture` | Named cases: `Empty`, `EmptyWithLeaseLeftovers`, `NodeSchema34`, `RustMarked`, `MarkedCorrupt`. Converts into a `Spec`. |
| `Spec`, `LeaseForm`, `MarkerState` | Custom contents: lease leftovers (`RustFile` or `NodeDirectory`), a Node database, and a marker state. |
| `Marker` | `new`, `to_bytes`, `parse` (refuses unknown keys, versions, owners, and timestamp shapes; errors are `MarkerError`). |
| `allowed_in_empty`, `allowed_in_node_lease` | Entry names that still count as an empty data directory. |
| `MARKER_FILE`, `MARKER_VERSION`, `MARKER_OWNER`, `FIXTURE_CREATED_AT`, `DATABASE_FILE` | Format constants. |
| `node_schema` | `write_schema_34_database`, `schema_34_statements`, `parse_schema_migrations`, `execute`, `stamp_schema_version`, `SCHEMA_34_COMMIT`, `SCHEMA_34_SHA256`. |

No binaries.

## Effects and flags

- Writes only inside the directory it creates: marker files (mode 0600 on Unix), lease leftovers such as `printpartner.lock` and `.desktop-lease`, and `print-partner.db` (WAL, foreign keys on).
- No env vars, network, threads, or child processes.
- No Cargo features.

## Tests

Unit tests are in `src/tests.rs`; `src/lib.rs` has one doc test. `pp-storage` uses the crate in `rust/crates/pp-storage/tests/testkit_installations.rs`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-testkit
cargo test --manifest-path rust/Cargo.toml -p pp-testkit --doc
```
