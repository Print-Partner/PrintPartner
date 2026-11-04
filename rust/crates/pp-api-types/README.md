# pp-api-types

Wire DTOs for the Plan autosave contract and printer status, with the parsers that validate them at the JSON boundary.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns the autosave DTOs (`SavePlanChoicesRequest`, `ApplyPlanDraftReceipt`, `PlanDraftIdentity`, `PlanDraftBasis`), their constructors, validation, and serialization.
- Checks each payload against the checked-in JSON Schemas in `web/scripts/desktop-contracts/schemas` and against typify-generated structural DTOs, so Rust and the TypeScript contracts agree.
- Owns the `PrinterHostStatus` JSON shape. Depends only on `pp-domain`.

## Not responsible for

- Bounded values (`PositiveId`, `Digest`, `WireText`): `pp-domain`. This crate re-exports them.
- Operation contracts and the parity CLI: `pp-contracts`.
- Persisting drafts or receipts: `pp-storage`.
- HTTP routes and the generated TypeScript client: `pp-api`.

## Interface

| Item | What it does |
| --- | --- |
| `parse_request`, `parse_receipt`, `parse_identity` | Parse and validate autosave JSON text; return `BoundaryError` on rejection. Start here. |
| `autosave` (re-exported at the root) | The DTOs above, `PlanChoice`, `FileTarget`, `ReceiptFields`, `BoundaryError`, and `pub use pp_domain::*`. |
| `printer_status` | `PrinterHostState` and `PrinterHostStatus` (`offline`, `unknown` constructors). Absent optional fields stay absent. |
| `semantics` | `parse_semantic_fixture`: a conformance probe with no route. |
| `contracts` | `parse_native` runs a named native parser; `SemanticFixture`, `CalendarDate`, `Mode`. With `codegen`: `export`, `schema_bytes`, `check_schema_file`. |
| `generated` (private) | typify output in `src/generated.rs`. Do not edit by hand. |

Binaries (both need `--features codegen`):

- `generate-wire-dtos --write|--check`: regenerates or checks `src/generated.rs` from the schema directory. Runs `rustfmt`. Called by `npm --prefix web run contracts:generate`.
- `export-rust-contracts --write|--check PATH`: writes or checks the reverse-generated schema. Called by `npm --prefix web run contracts:reverse:generate` and `contracts:reverse:verify`.

## Effects and flags

The library is pure apart from compile-time `include_str!` of the schema files. The binaries read the schema directory, write `src/generated.rs` or the given `PATH`, and spawn `rustfmt`. No env vars at runtime.

Cargo features:

- `desktop_rebuild` (default off): forwards to `pp-domain/desktop_rebuild`.
- `codegen`: adds `typify`, `schemars`, `syn`, and `prettyplease`; enables the binaries, `JsonSchema` derives, and the schema export functions.

## Tests

- `tests/autosave_snapshots.rs`: `insta` snapshots in `tests/snapshots` over `web/packages/contracts/test-fixtures/desktop/autosave-v1.json`.
- `tests/ticket_t_9_semantics.rs`, `tests/ticket_t_63_contracts.rs`: shared accept/reject corpora under `web/packages/contracts/test-fixtures` and `web/scripts/rust-contracts`. The schema drift test against `web/scripts/rust-contracts/schemas.json` runs only with `--features codegen`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-api-types
cargo nextest run --manifest-path rust/Cargo.toml -p pp-api-types --features desktop_rebuild
```
