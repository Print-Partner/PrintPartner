# pp-contracts

Request and outcome types for the existing draft, reconciliation, publication, and Build operations, plus the autosave parity CLI against the Node contracts.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Re-exports the `pp-api-types` autosave contract under `pp_contracts::autosave` (and at the root), so consumers have one facade.
- Owns the operation contracts used by `pp-storage` under `desktop_rebuild`: working-draft requests, reconciliation decisions and views, publication apply outcomes, and Build/profile summaries.
- Owns `contract-parity`, which replays the Node autosave corpus through the Rust parsers.
- Depends only on `pp-api-types`.

## Not responsible for

- Bounded values and pure rules: `pp-domain`.
- DTO schema validation and the typify-generated DTOs: `pp-api-types`.
- Executing these operations or writing SQL: `pp-storage`.
- HTTP routes: `pp-api`.

## Interface

| Item | What it does |
| --- | --- |
| `autosave` | `pub use pp_api_types::autosave::*`: `parse_request`, `parse_receipt`, `parse_identity`, the DTOs, and `BoundaryError`. |
| `working_drafts` (`desktop_rebuild`) | `Request` (tagged by `kind`: `edit`, `rebase`, `recompute`, `select`, `transition`, and others), `RebaseRequest`, `RecomputeOptions`, `EditDecisions`, `IdempotencyKey`, `HexColor`. |
| `reconciliation` (`desktop_rebuild`) | `ReconciliationRequest`, `Decision`, `ReconciliationView`, `Workspace`, `Diff`, `Conflict`, `Outcome`, `RebaseConflict`. |
| `publication` (`desktop_rebuild`) | `ApplyRequest` and `Outcome` for applying a draft, with `ExecutionConflict` and `UnmappableLink`. |
| `build_identity` (`desktop_rebuild`) | `CreateBuildRequest`, `ProfileSummary`, `PlanFreshness`, `ProfileLayers`; re-exports `BuildName` from `pp-domain`. |

Request types use `deny_unknown_fields` and validating constructors such as `DraftPartIds::new` and `EditDecisions::new`.

Binary `contract-parity NODE-CASES.json OUTPUT.json`: parses each case in `cases` and `supplemental`, and each `route_receipts` entry, compares with the Node outcome, writes a JSON report, and exits 1 on any mismatch. `web/scripts/desktop-contracts/verify.mjs` runs it (`npm --prefix web run contracts:verify`).

## Effects and flags

The library is pure. The `contract-parity` binary reads the input file, writes the output file, and prints a one-line summary. No env vars.

Cargo features:

- `desktop_rebuild` (default off): compiles `working_drafts`, `reconciliation`, `publication`, and `build_identity`; forwards to `pp-api-types/desktop_rebuild`.
- `codegen`: adds `schemars` derives and forwards to `pp-api-types/codegen`.

## Tests

- `tests/consumer.rs`: reads typed requests and builds receipts from `web/packages/contracts/test-fixtures/desktop/autosave-v1.json`.
- `tests/rebuild.rs`: the gated contracts; compiled only with `desktop_rebuild`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-contracts
cargo nextest run --manifest-path rust/Cargo.toml -p pp-contracts --features desktop_rebuild
```
