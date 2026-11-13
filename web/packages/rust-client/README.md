# @print-partner/rust-client

Typed requests, responses and Zod runtime schemas for the Rust core's HTTP routes, generated from Rust. The replacement UI imports this package instead of `@print-partner/contracts`.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns the generated TypeScript view of the complete contract written by `pp-api`'s `export-rust-client` binary: one Zod schema per DTO and one entry per route.
- `web/apps/ui` imports it for its core reads and printer status descriptors; no other app does.

## Not responsible for

- Rust DTOs and route registration: `pp-api-types` and `pp-api`. Change those and regenerate.
- The hand-maintained Node API types: `@print-partner/contracts` ([README](../contracts/README.md)).
- `/api/v1` mounts and routes absent from the exported contract.

## Interface

One entry point, `.` (`src/index.ts`). `src/` holds only generated files. Do not edit them; change the Rust types or the metadata consumed by `export-rust-client` and regenerate.

The exporter composes the base `CLIENT_ROUTES` and `document()` in
`rust/crates/pp-api/src/client_contract.rs` with two module extensions: the
send queue's `CLIENT_ROUTES` and `client_document()` in
`rust/crates/pp-api/src/send_queue.rs`, and the first-run probe's
`CLIENT_ROUTE` and `client_document()` in
`rust/crates/pp-api/src/printer_host/probe.rs`. Each module adds its DTO
schemas and Rust-serialized fixtures. `document()` and `document_bytes()` return
the base contract only; use the export binary for the complete generator input.

| File | Contents |
| --- | --- |
| `src/schemas/*.generated.ts` | One module per named Rust schema, importing only its dependencies; DTO modules also export their output types |
| `src/schemas.generated.ts` | DTO schema/type exports and `dtoSchemas` keyed by Rust contract name |
| `src/descriptors.generated.ts` | Constant tables from the contract's `descriptors` list. Today only `printerDisplayDescriptors`: one row per printer display state with its label, Lucide icon name and `--status-*` colour token |
| `src/routes/*.generated.ts` | One route definition per module, importing its request and response schemas |
| `src/routes.generated.ts` | `routes` (method, path, path params, text params, required request headers such as `reconcileDraft`'s `Idempotency-Key`, auth, request and response schema), `RouteRequest`, `RouteResponse`, `routePath` (positive integer IDs, or a percent-encoded non-empty string for a text param such as `undoFarmLogEntry`'s `id`) |
| `src/index.ts` | Re-exports the three indexes and records the generated file inventory |

Each file starts with a `Content SHA256` header over the rest of the file. Generation removes obsolete modules. Both checks verify the recursive file inventory; missing modules, stray files and hand edits fail. Adding a route changes its own modules and the indexes, without rewriting unrelated route or schema modules.

Transition for lanes: after merging main, rerun the rust-client generator. Do not hand-merge generated files.

### Coverage

| Route | Request | Response |
| --- | --- | --- |
| `GET /api/v2/plans` | | `ProfileList` |
| `GET /api/v2/plans/{id}` | | `ProfileSummary` |
| `GET /api/v2/plans/{id}/layers` | | `ProfileLayers` |
| `GET /api/v2/plans/{id}/drafts` | | `DraftList` |
| `GET /api/v2/plans/{id}/drafts/{draft_id}` | | `DraftWorkspace` |
| `GET /api/v2/plans/{id}/drafts/{draft_id}/accept-preview` | | `DraftAcceptPreview` |
| `PATCH /api/v2/plans/{id}/drafts/{draft_id}/parts` | `EditDraftPartsRequest` | `DraftWorkspace` |
| `POST /api/v2/plans/{id}/drafts/{draft_id}/abandon` | `AbandonDraftRequest` | `DraftIdentity` |
| `GET /api/v2/plans/{id}/filament-shortfalls` | | `FilamentShortfalls` |
| `GET /api/v2/plans/{id}/build-planning` | | `BuildPlanningResponse` |
| `GET /api/v2/plans/{id}/decisions` | | `PlanDecisionList` |
| `GET /api/v2/printers` | | `FarmPrinterList` |
| `POST /api/v2/printers/probe` | `PrinterProbeRequest` | `PrinterProbeResult` |
| `GET /api/v2/printer-checkoff` | | `CheckoffResultList` |
| `GET /api/v2/printer-checkoff/unattributed` | | `UnmatchedPrintList` |
| `GET /api/v2/printer-checkoff/unattributed/suggestions` | | `MatchSuggestionList` |
| `POST /api/v2/printer-checkoff/verify-batch` | `VerifyBatchRequest` | `VerifyBatchReceipt` |
| `POST /api/v2/printer-checkoff/verify-batch/undo` | `UndoBatchRequest` | `UndoBatchReceipt` |
| `POST /api/v2/printer-checkoff/unattributed/{id}/claim` | `ClaimUnattributedRequest` | `ClaimUnattributedReceipt` |
| `POST /api/v2/printer-checkoff/unattributed/{id}/dismiss` | `DismissUnattributedRequest` | `DismissUnattributedReceipt` |
| `GET /api/v2/printer-send-queue` | | `SendQueue` |
| `POST /api/v2/printer-send-queue` (`multipart/form-data`) | | `QueuedSendReceipt` |
| `POST /api/v2/printer-send-queue/{id}/dispatch` | | `SendNowReceipt` |
| `POST /api/v2/printer-send-queue/{id}/reconcile` | `ReconcileSendRequest` | `QueuedSendReceipt` |
| `DELETE /api/v2/printer-send-queue/{id}` | | `QueuedSendReceipt` |
| `GET /api/v2/farm-log` (query `cursor`, `limit`) | | `FarmLogPage` |
| `POST /api/v2/farm-log/{id}/undo` | | `FarmLogEntry` |
| `POST /api/v2/integrations` | `CreateIntegrationRequest` | `IntegrationView` |
| `DELETE /api/v2/integrations/{id}` | | `IntegrationView` |
| `GET /api/v2/integrations` | | `IntegrationList` |
| `GET /api/v1/integrations/{id}/tls-pin` | | `TlsPin` (`sha256_prefix` and `pinned_at` only when pinned) |
| `POST /api/v1/integrations/{id}/tls-pin/reset` | | `TlsPinReset` |
| `POST /api/v2/printers` | `CreatePrinterRequest` | `PrinterConfig` |
| `GET /api/v2/printers/{id}` | | `PrinterConfig` |
| `PATCH /api/v2/printers/{id}` | `UpdatePrinterRequest` | `PrinterConfig` |
| `DELETE /api/v2/printers/{id}` | | `PrinterConfig` |
| `GET /health` | | `Health` (`modules` is always present in Rust-only mode; other Rust auth routers and Node omit it) |
| `GET /auth/status` | | `AuthStatus` (`setup_required` only) |
| `GET /auth/me` | | `CurrentUser` |
| `POST /auth/login` | `SignInRequest` | `SignedIn` |
| `POST /auth/register` | `RegisterRequest` | `SignedIn` |
| `POST /auth/logout` | | `SignedOut` |

`PrinterState` (execution, bed-clear and display state as separate fields) is each `FarmPrinter`'s `state`. `PrinterDisplayDescriptor` has no route; its table is `printerDisplayDescriptors`. The farm routes are documented in [Farm reads](../../../docs/farm-reads.md). `addQueuedSend` lists no request schema because its body is a form: post a `FormData` and parse the answer with the route's response schema. The send routes are documented in [Send queue storage](../../../docs/send-queue.md#routes-gre-365).

The Checkoff burst types (`pp_storage::checkoff_progress::wire`) are the subset the replacement UI sends and reads. The routes parse a JSON value and accept more (imported-extra `additional_decisions`, JavaScript-style coercion), and a receipt carries more fields, such as each bed's stored link, which the generated schema strips. A `UnitDecision` with `result: "rejected"` needs `reason` (one of the ten `RejectReason` values) or the route answers 400; `reason` and `note` may be omitted on a confirmed unit. `checkoff_bursts_http` parses the served receipts into these types and records a rejection with its reason through the mounted route.

`Problem` is the body of every error any covered route returns, served as `application/problem+json`. Its `type` is a `z.enum` of the stable problem URNs and `errors` holds at most 16 field errors. Route-specific extension members, such as a draft conflict's `workspace`, are stripped by `z.object`. See [Problem responses](../../../docs/problem-responses.md).

Every route needs a session cookie or API key except `/health` and the account routes a signed-out owner uses: `/auth/status`, `/auth/login`, `/auth/register` and `/auth/logout`. The login and register handlers read their bodies field by field; `SignInRequest` and `RegisterRequest` describe the bodies the UI sends. 200 bodies are typed per route; every error body is `Problem`. The `/api/v1` mounts answer with the legacy Plan summary and are not covered.

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
- **Omitted versus null.** Every covered field is required, except `Problem`'s `detail`, `code` and `errors` (omitted when empty, as RFC 9457 allows) and `Health`'s `modules` and the six members Node's `/health` does not send (`degraded_detail`, `single_user_auth`, `single_user_setup_required`, `owner_mapping_required`, `github_oauth_configured`, `discord_oauth_configured`), so a client decodes both bodies. `RegisterRequest`'s `display_name` may be omitted. `Omittable<T>` marks such a member. A Rust `Option` field that is always serialized is `.nullable()`, not `.optional()`, using `schemars(with = "RequiredNullable<T>")` or an explicit required key in the exported nullable schema.
- **Integers.** Bounded Rust wire integers keep their bounds. `i64`, `u64` and `usize` are limited to the JavaScript safe range, so a larger value fails to parse instead of losing precision. The generator supports integers only: it has no fractional number type, so every numeric field in the contract is an integer. `PrinterConfig`'s lengths are whole millimetres for that reason.
- **Maps and free JSON.** A Rust map (`BTreeMap<String, T>`) becomes `z.record(z.string(), T)`, and `serde_json::Value` inside a map becomes `z.json()`. Decision `params` and `result` are the only free JSON. The round-trip tests skip the unknown-key and omitted-field checks for record entries and every check under `z.json()`.
- **Tagged unions.** Internally tagged enums become `z.discriminatedUnion` on their tag (`kind` or `status`).
- **Not expressed.** JSON Schema cannot state the one-edit-per-Part-field rule for `EditDraftPartsRequest`; the server refuses such a request with 400.

The emitter (`web/scripts/rust-contracts/client.mjs`, `rust-client-zod/1`) refuses any schema keyword, format or shape it does not handle.

## Add a route

1. Make sure the request and response types derive `schemars::JsonSchema` under the `codegen` feature, and that the handler serializes that type rather than a `json!` literal.
2. Register the route metadata, schemas and serialized fixtures in the contract written by `export-rust-client`. The base uses `CLIENT_ROUTES` and `document()` in `rust/crates/pp-api/src/client_contract.rs`; the probe and send queue modules supply extensions composed by the binary. Include module metadata in `pp-core`'s mounted-route contract test as well.
3. Run `generate`, then `check`.

## Add a descriptor table

1. Add an entry (`name`, `dto`, `key`, `values`) to the `descriptors` list in `document()` in `client_contract.rs`, with the rows from a typed helper. The key field must be a Rust string enum.
2. Run `generate`. The generator refuses rows that do not list every key value once, in Rust order, and the output's `satisfies Record<...>` makes TypeScript refuse a missing key.
