# @print-partner/rust-client

Typed requests, responses and Zod runtime schemas for the Rust core's HTTP routes, generated from Rust. The replacement UI imports this package instead of `@print-partner/contracts`.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns the generated TypeScript view of the Rust routes listed in `CLIENT_ROUTES` in `rust/crates/pp-api/src/client_contract.rs`: one Zod schema per DTO and one entry per route.
- Nothing in `web/apps` imports it yet.

## Not responsible for

- Rust DTOs and route registration: `pp-api-types` and `pp-api`. Change those and regenerate.
- The hand-maintained Node API types: `@print-partner/contracts` ([README](../contracts/README.md)).
- Error bodies, `/api/v1` mounts, and routes outside `CLIENT_ROUTES`.

## Interface

One entry point, `.` (`src/index.ts`). `src/` holds only generated files. Do not edit them; change the Rust types or `pp_api::client_contract` and regenerate.

| File | Contents |
| --- | --- |
| `src/schemas.generated.ts` | One Zod schema and output type per DTO, plus `dtoSchemas` keyed by Rust contract name |
| `src/descriptors.generated.ts` | Constant tables from the contract's `descriptors` list. Today only `printerDisplayDescriptors`: one row per printer display state with its label, Lucide icon name and `--status-*` colour token |
| `src/routes.generated.ts` | `routes` (method, path, path params, auth, request and response schema), `RouteRequest`, `RouteResponse`, `routePath` |
| `src/index.ts` | Re-exports both files |

Each file starts with a `Content SHA256` header over the rest of the file.

### Coverage

| Route | Request | Response |
| --- | --- | --- |
| `GET /api/v2/plans` | | `ProfileList` |
| `GET /api/v2/plans/{id}` | | `ProfileSummary` |
| `GET /api/v2/plans/{id}/layers` | | `ProfileLayers` |
| `GET /api/v2/plans/{id}/drafts` | | `DraftList` |
| `GET /api/v2/plans/{id}/drafts/{draft_id}` | | `DraftWorkspace` |
| `PATCH /api/v2/plans/{id}/drafts/{draft_id}/parts` | `EditDraftPartsRequest` | `DraftWorkspace` |
| `POST /api/v2/plans/{id}/drafts/{draft_id}/abandon` | `AbandonDraftRequest` | `DraftIdentity` |
| `POST /api/v2/installation/backup` | | `InstallationBackup` |
| `POST /api/v2/installation/restore/stage` | gzip archive bytes, not JSON | `RestorePreview` |
| `POST /api/v2/installation/restore` | `RestoreRequest` | `RestoreOutcome` |
| `GET /health` | | `Health` |

`stageInstallationRestore` has `request: null` because its body is the archive itself; send the bytes with `Content-Type: application/gzip`. `GET /api/v2/installation/backup/archive` returns the archive bytes and is not in the table.

`PrinterState` (execution, bed-clear and display state as separate fields) and `PrinterDisplayDescriptor` are DTOs without a route yet.

Every route except `/health` needs a session cookie or API key. Only 200 bodies are typed; error bodies are not yet. The `/api/v1` mounts answer with the legacy Plan summary and are not covered.

`pp-core`'s `client_routes_match_registrations` test sends each route to the composed routers. It checks that the route is registered, refuses anonymous requests when it needs a credential, and reaches its handler.

## Effects and flags

- Runtime: none. The generated code is pure; its only dependency is `zod`.
- `generate` and `check` run `cargo run -p pp-api --features codegen --bin export-rust-client`, which writes schemas, routes and Rust-serialized fixtures to a temporary file. `generate` rewrites `src/`; `check` writes nothing in the repo.
- No Cargo feature or runtime flag gates the package. The `codegen` feature only adds schema derives to the Rust export.

## Tests

`check` compares every file in `src/` byte for byte, fails on extra files, then runs the round-trip tests in `web/scripts/rust-contracts/client.test.mjs`. `test` (`client.mjs --verify`) checks header hashes and the file set without cargo. `npm test` in `web/` runs `test`; `npm run contracts:verify` runs `check`.

Run from the repository root:

```sh
npm --prefix web run generate --workspace @print-partner/rust-client   # export from Rust and write src/
npm --prefix web run check --workspace @print-partner/rust-client      # exit 1 on drift; writes nothing in the repo
npm --prefix web run test --workspace @print-partner/rust-client       # header hashes and file set, no cargo
npm --prefix web run typecheck --workspace @print-partner/rust-client
```

## Wire rules

- **Unknown keys.** Request DTOs use `#[serde(deny_unknown_fields)]`, so their schemas are `z.strictObject` and reject unknown keys. Response DTOs use `z.object`, which strips unknown keys.
- **Omitted versus null.** Every covered field is required. A Rust `Option` field that is always serialized is `.nullable()`, not `.optional()`, through `schemars(with = "RequiredNullable<T>")`.
- **Integers.** Bounded Rust wire integers keep their bounds. `i64`, `u64` and `usize` are limited to the JavaScript safe range, so a larger value fails to parse instead of losing precision.
- **Tagged unions.** Internally tagged enums become `z.discriminatedUnion` on their tag (`kind` or `status`).
- **Not expressed.** JSON Schema cannot state the one-edit-per-Part-field rule for `EditDraftPartsRequest`; the server refuses such a request with 400.

The emitter (`web/scripts/rust-contracts/client.mjs`, `rust-client-zod/1`) refuses any schema keyword, format or shape it does not handle.

## Add a route

1. Make sure the request and response types derive `schemars::JsonSchema` under the `codegen` feature, and that the handler serializes that type rather than a `json!` literal.
2. Add the route to `CLIENT_ROUTES` and its schema and fixtures to `document()` in `rust/crates/pp-api/src/client_contract.rs`.
3. Run `generate`, then `check`.

## Add a descriptor table

1. Add an entry (`name`, `dto`, `key`, `values`) to the `descriptors` list in `document()` in `client_contract.rs`, with the rows from a typed helper. The key field must be a Rust string enum.
2. Run `generate`. The generator refuses rows that do not list every key value once, in Rust order, and the output's `satisfies Record<...>` makes TypeScript refuse a missing key.
