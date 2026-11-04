# pp-domain

Bounded wire values and pure rules shared by the Rust crates. It has no `pp-*` dependencies.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns bounded IDs and values that reject out-of-range JSON at deserialize time: integers, code-point-bounded text, and 64-character lowercase hex digests.
- Owns pure rules: G-code object name parsing and matching to STL filenames, Product Module feature resolution, and Build name validation.
- Gives `pp-api-types` and the crates above it one definition of each value.

## Not responsible for

- Wire DTOs, request validation, and JSON Schema documents: `pp-api-types`.
- Operation contracts and parity tooling: `pp-contracts`.
- Storing feature choices or any SQL: `pp-storage`.
- I/O, Tokio, HTTP, or Tauri of any kind.

## Interface

| Item | What it does |
| --- | --- |
| `WireInteger<MIN, MAX>` | `u64` in `MIN..=MAX`; `new`, `get`. Accepts whole-number floats like JS. |
| `PositiveId`, `Version`, `Quantity` | Aliases: `1..=MAX_SAFE_INTEGER`, `0..=MAX_SAFE_INTEGER`, `1..=10_000`. |
| `WireText<MIN, MAX>` | String bounded by code-point count; `new`, `as_str`. |
| `Digest` | 64 lowercase hex characters; `new`, `as_str`. |
| `DraftState` | `Open`, `Abandoned`, `Consumed`, serialized lowercase. |
| `gcode_objects` | `group_objects_by_part`, `match_objects_to_filenames`, `interpret_sliced_object_name`. Port of the Node G-code object parser. |
| `features` (`desktop_rebuild`) | `Feature` registry (`Farm`, `Orders`, `Bambu`, `Spoolman`), `Toggles`, and `resolve(user, plan) -> Effective`. A Plan choice beats the user choice, which beats the default. |
| `build_identity` (`desktop_rebuild`) | `BuildName::parse` and `trim_js`, which trims whitespace the way JS `String.prototype.trim` does. |

Constructors return `&'static str` error codes such as `integer_bounds` and `digest_pattern`.

## Effects and flags

None. The crate is pure: no files, network, env vars, threads, or Tokio.

Cargo features:

- `desktop_rebuild` (default off): compiles the `features` and `build_identity` modules.
- `codegen`: adds `schemars` and `JsonSchema` impls (private `schema` module) for the wire values and `DraftState`. Used by the generators in `pp-api-types`.

## Tests

- `tests/wire_props.rs`: `proptest` properties for the bounded values.
- `tests/gcode_objects_parity.rs`: replays the TypeScript goldens in `fixtures/parity/gcode-objects` at the repo root. Regenerate them with `node fixtures/parity/gcode-objects/generate.mjs`.
- `src/features/tests.rs`: unit tests for feature resolution; they run only with `desktop_rebuild`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-domain
cargo nextest run --manifest-path rust/Cargo.toml -p pp-domain --features desktop_rebuild
```
