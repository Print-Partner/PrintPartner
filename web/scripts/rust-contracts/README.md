# Rust-to-Zod parity proof

This lane exports draft-7 schemas from native Rust wire DTOs and constrained
scalar types, then generates TypeScript/Zod parsers. The three proven production
parsers, `parseSavePlanChoicesRequest`, `parsePlanDraftIdentity`, and
`parseApplyPlanDraftReceipt`, execute that generated module. Handwritten Zod
remains the typify export. Route aliases, workers, and transactions stay on
Node. GRE-445 remains open until its dependency gates and complete operation
switch pass.

Run from `web/`:

```sh
npm run contracts:reverse:generate
npm run contracts:reverse:verify
```

The existing `contracts:verify` also runs the reverse verification. Cargo uses
the pinned lockfile. `schemars =0.8.22` derives the shapes; the repository-owned
`rust-contracts-zod/1` emitter supports a restricted draft-7 subset and rejects
unsupported keywords, formats, references, overlapping unions and refinement
tags. The generated header pins hashes of the emitter and exported Rust schema.
Both Rust schema and generated TypeScript are checked byte-for-byte for drift.

## Ownership and module boundaries

- `pp-domain/src/schema.rs` implements schema generation for the actual
  `WireInteger`, `WireText` and `Digest` types. Their generic bounds remain the
  source of numeric and text constraints.
- `pp-api-types/src/autosave.rs` derives wire schemas from actual deserialization
  types. Validated public types delegate schemas to their `TryFrom` wire types.
  `RequiredNullable<T>` describes required fields that accept null; plain
  `Option<T>` would incorrectly describe omitted input as valid.
- `pp-api-types/src/contracts.rs` exports named roots and reviewed refinement
  tags. Its native parser calls the actual DTO deserializers and `TryFrom`
  validators without the existing Zod-exported schema/typify adapter. The
  semantic fixture is a conformance probe, not a production route.
- `generate.mjs` emits Zod schemas plus inferred input/output TypeScript types.
  Reviewed tag adapters encode basis consistency, observed-draft linkage,
  duplicate target fields and the probe's positive-write rule. Structural
  JSON Schema alone cannot express these rules or apply omitted defaults.

Native DTO derives were selected over a separate Rust contract algebra. An
algebra could generate both languages, but retaining it beside handwritten
native DTOs would create another source of truth; replacing those DTOs expands
this unit into a migration. The derive approach keeps the actual native types
and validates their semantics independently.

## Semantic and negative controls

The reverse lane reuses all 34 T9 semantic cases, 41 frozen autosave cases and
50 supplemental cases. It compares acceptance and exact accepted JSON values.
`raw_input` is preserved for integral float lexemes and UTF-16 surrogate cases.
Additional raw wire cases verify that repeated JSON object keys keep the last
value before DTO validation, matching Node JSON.parse. Invalid final values
still fail, and duplicate target-field decisions remain invalid.
The frozen supplemental file is compared with the original oracle on every
verification run, so narrowing or changing that corpus fails the check.

Required nullable fields stay required. Optional text rejects null and remains
omitted in output. Defaults apply only to omitted input. Probe objects strip
unknown fields; autosave objects reject them. Integers accept integral numeric
lexemes without string/boolean coercion. Text uses Unicode code-point bounds,
rejects unpaired surrogates and preserves scalar values unchanged. Calendar
dates are validated only in the probe; receipt `applied_at` remains bounded
text. Native parsing and generated Zod must agree with the unchanged oracle.

Tests plant incorrect null presence, defaults, integer bounds, unknown handling
and refinements, and require each change to disagree with the full corpus.
Unsupported schema/refinement declarations fail generation. A modified generated
file must fail the actual CLI drift checker. Verification saves commands, exit
codes and logs in a fresh receipt directory; a second run checks restart-safe
reproducibility without rewriting artifacts.
Standalone receipts hash the imported oracle scripts, transitive contract
sources, native crate sources and build manifests. Drift controls modify
isolated copies of executable inputs and require freshness verification to fail.

This proof does not establish route/worker cutover, failure/restart recovery of
an operation, WS/MCP protocol parity or dependency completion. Node retains
route, worker, and transaction authority. The generated module is authoritative
for the three proven production parsers.

Generated integer parsers normalize negative zero to zero after validation, matching native unsigned serialization. Raw-input controls cover zero-allowed root and nested DTO fields and reject negative zero for positive receipt and array decision quantities. A planted emitter regression must fail exact-output parity.

## Generated client package

`client.mjs` generates `web/packages/rust-client/src` from
`pp_api::client_contract` with its own emitter, `rust-client-zod/1`. It leaves
`generate.mjs` and `rust-wire.generated.ts` unchanged. `client.test.mjs` runs
the round-trip tests. See the
[package README](../../packages/rust-client/README.md).
