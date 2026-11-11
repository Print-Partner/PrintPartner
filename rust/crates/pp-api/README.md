# pp-api

Axum routers and the MCP server that put authenticated HTTP in front of `pp-storage` clients. `pp-core` composes these routers; this crate binds no product listener.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Owns request admission for its routes: loopback Host/Origin checks, credential extraction (session cookie or API key), body limits, rate limits, and bounded concurrency.
- Owns the auth HTTP surface: login, registration, sessions, API keys, password reset mail, and GitHub/Discord OAuth.
- Under `desktop_rebuild`, adapts Builds, drafts, Sources (catalog), Accepted Plan Checkoff, admin/legal reads, jobs, and MCP onto storage clients, matching the Node responses. It also serves Build history, a Rust-only route with no Node counterpart ([Build history](../../../docs/build-history.md)).
- Owns the route list and DTO schemas that `web/packages/rust-client` is generated from.
- Depends on `pp-storage`; `pp-services` only with `desktop_rebuild`.

## Not responsible for

- SQL, transactions, and credential/session state: `pp-storage` (for example `pp_storage::auth`).
- Listeners, bootstrap/session admission at the socket, and static assets: `pp-gateway`.
- Composing routers, starting workers, and startup/shutdown: `pp-core`.
- MCP tool policy and the product catalog: `pp-services::mcp_domain`.
- Printer protocols and outbound target policy: `pp-drivers`, `pp-net`.

## Interface

| Module | Entry points |
| --- | --- |
| `auth` | `auth_router(AuthHttpConfig, impl Into<AuthBackend>, ProviderClient, ResetMailer)`. `AuthBackend` is `Pool(AuthClient)` or, with `desktop_rebuild`, `Rebuild(RebuildAuthClient)`. `ResetMailer::disabled` or `ResetMailer::smtp(SmtpConfig)`; `OAuthCredentials`, `CookieTransport`, `Delivery`. |
| `builds` | `build_router(BuildHttpConfig, BuildGraphClient)`: Plan list/detail/create/delete, layers, kit manifest. |
| `drafts` | `draft_router(DraftHttpConfig, WorkingDraftClient, RebuildAuthClient)` and `admit_draft_routes`. |
| `catalog` | `admit_catalog_routes(CatalogHttpConfig, Arc<Mutex<WriterOwner>>)`: Sources, import rules, STL naming. |
| `printer_status` | `printer_status_router(AdminHttpConfig, ReadClient, Arc<dyn PrinterStatusPort>)`: `GET`/`HEAD /api/v1/integrations/{id}/status` from the watchers' cached statuses after authentication; 404 for a non-Moonraker id, 503 while monitoring is unavailable. The caller supplies the port. |
| `accepted` | `accepted_router(AcceptedHttpConfig, ReadClient)`: Checkoff and Part assembled units. `AcceptedHttpConfig::with_stock(Arc<dyn InventoryReader>)` adds `/plans/{id}/filament-shortfalls`; the caller supplies the Spoolman reader. |
| `printer_host` | `printer_host_router(AdminHttpConfig, ReadClient, Arc<dyn PrinterHostPort>, Arc<SnapshotStore>)`: `GET /printers/{id}/capabilities`, `/files`, `/cameras`, the streamed `/files/content` (capturing a Build's snapshot into the store) and `/cameras/view` at the root and under `/api/v1`, and `POST /api/v1/integrations/{id}/test` with Node's errors and per-route budgets ([Printer-host reads](../../../docs/printer-host-reads.md)). The caller supplies the port. |
| `events` | `events_router(AcceptedHttpConfig, DomainEventsClient, StreamLimits)`: the domain event stream at `/api/v1/events` as Server-Sent Events ([Domain events](../../../docs/domain-events.md#http-stream)). |
| `farm` | `farm_router(AcceptedHttpConfig, FarmClient, Arc<dyn FarmObservations>)`: read-only `/api/v2/printers`, `/api/v2/printer-checkoff`, `/api/v2/printer-checkoff/unattributed`, `/api/v2/printer-checkoff/unattributed/suggestions` and `/api/v2/printer-send-queue` ([Farm reads](../../../docs/farm-reads.md)). The caller supplies the observations. |
| `file_assignments` | `file_assignments_router(AcceptedHttpConfig, ProgressClient, Arc<SnapshotStore>, Arc<dyn PrinterHostPort>)`: `POST /printer-checkoff/file-assignments`, `/preview` and `/upload` at `/` and `/api/v1`, redeeming files/content snapshots and held uploads, and reading a print file off its printer ([Print-file assignment routes](../../../docs/records/d19-publication-client.md#print-file-assignment-routes-gre-1220-gre-1255-gre-1256)). |
| `history` | `history_router(AcceptedHttpConfig, HistoryClient)`: paged Build history at `/plans/{id}/history` and `/api/v2/plans/{id}/history`. |
| `planning` | `planning_router(AcceptedHttpConfig, PlanningClient)`: `/plans/{id}/build-planning` and `/plans/{id}/decisions` at `/`, `/api/v1` and `/api/v2`, with Node's data ([Build planning and decisions](../../../docs/build-planning.md)). |
| `workflow` | `workflow_router(AcceptedHttpConfig, WorkflowClient)`: `/plans/{id}/workflow` at `/`, `/api/v1`, `/api/v2`, and the task feed at `/api/v1/tasks` and `/api/v2/tasks` ([Build workflow](../../../docs/build-workflow.md)). |
| `admin` | `admin_router(AdminHttpConfig, ReadClient, AdminFiles)`: legal pages and custom filaments. |
| `jobs` | `jobs_router(JobsHttpConfig, AtomicJobClient)`: job list/read and `/ws/jobs/{id}`. |
| `mcp` | `mcp_router(McpHttpConfig, RebuildAuthClient, McpClients)`, where `McpClients` holds the `BuildGraphClient`, `WorkflowClient`, `WorkingDraftClient`, `PlanningClient`, `ProgressClient` and accepted `ReadClient`: Streamable HTTP MCP at `/mcp` and `/api/v1/mcp`. Every request needs an API key; a session cookie alone is refused. `get_build_workflow` reads the `WorkflowClient` with that key. `list_plans` reads the `BuildGraphClient`, and `get_plan_decisions` and `list_plan_snapshots` read the `PlanningClient`, `get_remaining` reads the accepted `ReadClient`, and `get_printer_checkoff` reads links and unattributed prints through the `ProgressClient`, each with that key. `propose_verify_printer_checkoff` reads the link, and its `confirm_apply` runs a one-bed Checkoff burst through the `ProgressClient` under the key `mcp:<action_id>` ([MCP](../../../docs/mcp.md)). |
| `client_contract` | `CLIENT_ROUTES`, `ClientRoute`, `ClientAuth`. With `codegen`: `document`, `document_bytes`. |
| `problem` | With `desktop_rebuild` only (feature off, just an identity `without_envelope`): `Problem`, `FieldError`, `ProblemType` (stable types, `title`, `classify`) and their bounds, plus the one builder for error responses: `response`, `coded`, `typed`, `overloaded`, `build`, the `layer` backstop each router mounts, `parse_body` for field errors, and `without_envelope` for Node comparisons. See [Problem responses](../../../docs/problem-responses.md). |
| `fixtures` | Node/Rust parity runner: `Sandbox`, `Served`, `Exchange`, `Ledger`, `StateGraph`, `Labels`. |

Every module except `auth` needs `desktop_rebuild`. Each `*HttpConfig::from_startup(origin, api_key_tenant, flag)` takes a loopback origin; `CatalogHttpConfig::from_startup` also takes an `AuthPolicy` before `flag`; the router mounts nothing unless `flag` is `1` or `true`. `pp-core` passes `Some("1")` from its gated composition. The private `loopback` module holds the shared Host/Origin and credential checks.

Binaries:

- `export-rust-client PATH` (needs `--features codegen`): writes the generator input (DTO schemas, routes, Rust-serialized fixtures) for `web/packages/rust-client`. `npm --prefix web/packages/rust-client run generate` and `run check` call it through `web/scripts/rust-contracts/client.mjs`.
- `auth-http-fixture DISPOSABLE_DIRECTORY [PORT] [single|open|closed] [dev-reset|production] [CONFIG_JSON]`: serves only `auth_router` over a store in that directory on `127.0.0.1` for manual auth checks. Use a disposable directory.

## Effects and flags

- Network: inbound HTTP via routers mounted by the caller. Outbound HTTPS to the GitHub and Discord OAuth endpoints (`reqwest`, rustls, no redirects or proxy, 15 s timeout) and SMTP for reset mail (`lettre`) when configured.
- Tokio: handlers are async; reset mail is sent on spawned tasks; `tokio::sync::Semaphore` bounds admission.
- Files: `fixtures::Sandbox` creates and removes a `0700` directory; `StateGraph` hashes files and opens SQLite read-only. `auth-http-fixture` opens a `WriterOwner` in its directory; `export-rust-client` writes `PATH`.
- Env: `BuildHttpConfig::new`, `CatalogHttpConfig::new`, `JobsHttpConfig::new`, and `DraftHttpConfig::new` read `PP_DESKTOP_REBUILD`. `from_startup` takes the value as an argument instead.

Cargo features:

- `desktop_rebuild` (default off): compiles every module except `auth`, switches `auth` errors to problem responses, adds `pp-services`, `rmcp`, `rusqlite`, `http-body`, `ryu-js`, `serde_path_to_error`, and forwards to `pp-storage/desktop_rebuild`.
- `codegen`: implies `desktop_rebuild`, adds `schemars`, forwards to `pp-storage/codegen`, and enables `export-rust-client`.

## Tests

- `tests/auth_*_parity.rs`: replay Node exchanges from `fixtures/parity/auth-*` at the repo root. `auth_authorization_start`, `auth_key_regenerate`, `auth_oauth`, `auth_password_change`, and `auth_session` need `desktop_rebuild`.
- `tests/auth_http.rs`: auth router behavior.
- `src/problem/tests.rs` and, with `desktop_rebuild`, `src/problem/tests/canary.rs` (`-E 'test(problem_response)'`): problem envelope, extractor rejections, field-error bounds, and the leak canary.
- `tests/builds_http.rs`, `catalog_http.rs`, `jobs_http.rs`, `manifest_options_http.rs`: mostly `desktop_rebuild`-gated tests against `fixtures/parity/*-http` and related directories.
- `tests/node_fixture_replay.rs`, `accepted_plan_checkoff_http.rs`, `admin_http.rs`: gated parity suites.
- Unit tests: `src/auth/tests/` (provider and SMTP, with `tests/fixtures/loopback-*.der`), `src/drafts/http/tests.rs`, `src/mcp/tests.rs`. Lib tests share scratch helpers from `pp-storage/tests/common`.
- Dev-dependencies: `tokio-rustls`, `rusqlite`, `tempfile`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-api
cargo nextest run --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild
```
