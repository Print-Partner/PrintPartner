# HTTP API

Print Partner serves JSON APIs, browser routes, downloads, and WebSockets from the same Fastify server. This page explains the stable entry points and conventions. The generated OpenAPI document is the source of truth for individual request and response schemas.

## Entry points

| URL | Purpose |
|-----|---------|
| `GET /health` | Health, version, database state, and release identity |
| `GET /api/v1` | API discovery |
| `GET /api/v1/openapi.json` | OpenAPI 3.1 document |
| `GET /api/v1/docs` | Swagger UI outside production, or when `OPENAPI_UI=1` |
| `POST /api/v1/mcp` | Streamable HTTP MCP |
| `GET /metrics` | Prometheus metrics |

Example:

```bash
curl http://127.0.0.1:8080/health
curl http://127.0.0.1:8080/api/v1
```

## Authentication

Open self-host leaves `MULTI_USER` and `SINGLE_USER_AUTH` unset. That process uses the `default` tenant. Set `PRINT_PARTNER_API_KEY` before exposing `/api/v1` or `/api/v1/mcp` beyond the host. `PRINT_PARTNER_API_KEY` is a process secret. A key created in Settings is a different secret, stored for one tenant.

Send the deployment key with either header:

```http
X-Print-Partner-Api-Key: <key>
```

```http
Authorization: Bearer <key>
```

`SINGLE_USER_AUTH=1` requires the administrator session for the API. The same mode accepts either the deployment key or a Settings API key on the exact `/api/v1/mcp` route without a session. Registration closes after the first account.

`MULTI_USER=1` requires a login session for `/api/v1`, for unprefixed application routes, and for `/api/v1/mcp` on self-host. A request that sends only a bearer key gets `401` and this body:

```json
{"detail":"Authentication required"}
```

The server does not choose a tenant from that key. The first registered user is the administrator and owns the `default` tenant. Later accounts each own a separate tenant and are not administrators. Only the administrator can create a Settings API key, and the server stores it for the administrator tenant. A request with that tenant's session can send the matching Settings key. The same session with a key stored for another tenant gets `401` and `Valid API key required`. Hosted planning (`DEPLOY_MODE=saas`) disables the Settings API-key routes. HTTP MCP on the exact `/api/v1/mcp` route is off there too; bearer or session requests get `403` and `This feature is off on the hosted planning site.`, not the `401` body above.

## API families

Print Partner contains three route families:

| Family | Role |
|--------|------|
| Unprefixed routes | Browser-facing application operations and downloads |
| `/api/v1` | Integration API, MCP, job discovery, and legacy Plan summary shapes |
| `/api/v2` | Current typed Plan summary contract |

Some operations are available both unprefixed and under `/api/v1` because the browser application predates the versioned API. Use the typed endpoint definitions in `web/apps/web/src/api/endpoints` when adding browser calls. External integrations should prefer `/api/v1` or `/api/v2` and consult OpenAPI.

## Main resources

### Sources

Source routes register, update, sync, inspect, and search Git repositories, local directories, and uploaded archives.

Common paths include:

```text
GET    /sources
POST   /sources
GET    /sources/:id
PATCH  /sources/:id
DELETE /sources/:id
GET    /sources/:id/stl-tree
GET    /sources/stl-search
GET    /sources/:id/docs
PUT    /sources/:id/import-rules
POST   /jobs/sync
```

Sync and scanning operations use background jobs when the work may outlive one request.

`local_path` is an operator-facing capability for trusted, single-tenant
self-host deployments. SaaS and multi-user deployments reject it on Source
create and update requests, confine content to the Source's managed workspace,
and return `local_path: null`. Clients must use the `content_available` boolean
to decide whether Source files and documents are available.

### Builds and Plans

The API retains the historical `plans` resource name for Builds.

```text
GET    /plans
POST   /plans
GET    /plans/:id
PATCH  /plans/:id
DELETE /plans/:id
POST   /plans/:id/duplicate
POST   /plans/:id/archive
GET    /plans/:id/drafts
POST   /plans/:id/drafts/recompute
POST   /plans/:id/drafts/:draftId/apply
```

Applying a draft creates the accepted Plan revision used by Checkoff and Production. Callers should not update accepted state by patching part rows directly.

Production Setup is a Build-owned resource:

```text
GET   /plans/:id/production-setup
PATCH /plans/:id/production-setup
```

Each `PATCH` body is one typed command: `set_preferred_slicer_instance`,
`set_selection`, `replace_printer_assignments`, `set_route`, or
`replace_rules`. Read the returned resource after a `409` conflict before you
retry. The former full-record `PUT` operation was removed; clients that used it
must send the corresponding field commands instead.

### Checkoff

```text
GET   /plans/:id/checkoff
PATCH /parts/:id/progress
GET   /parts/:id/assembled
PATCH /parts/:id/assembled
POST  /plans/:id/progress/import
```

Printer completion does not mark units complete without review. The printer checkoff routes record the host result, then accept a confirm or reject decision.

### Filament shortfalls (Rust-only)

```text
GET /plans/:id/filament-shortfalls     (also /api/v1 and /api/v2)
```

Served only by the Rust core with both rebuild gates on, behind a session or API key. The body is one of:

- `module_off` with `module: "spoolman"` and the same `reason` that `/health` lists. No Spoolman request is made.
- `no_accepted_plan`.
- `ready` with one row per filament that has remaining units: the exact `filament_color_id`, Spoolman's `label`, `grams_needed`, `grams_available`, `status` (`sufficient`, `short` with `short_grams`, or `unknown`), and each Part's need.

`grams_needed` is `measured` or `estimated` (each with `grams` and the `job_count` used), or `unknown` with a reason. It comes from past prints: each completed job's `filament_consumed_g` divided by the units that job printed, averaged over the Part's five most recent such jobs, times the remaining units, rounded up. The reason is `grams_not_recorded` when no print job of the Build has measured grams, and `part_not_printed` when other Parts do but this one does not. Nothing in Node or Rust writes `filament_consumed_g` today: Node's printer checkoff records jobs as `sent` with no grams. So on real data every `grams_needed` is `unknown` with `grams_not_recorded`, and `status` is `unknown`, until a writer records grams. The owner chose the writer's design (GRE-1165): record the slicer's estimate when a job is sent, then replace it with the measured value when the print finishes. `estimated` is the basis while any Part's grams come from an estimate; nothing returns it until GRE-1165 ships. `estimated` and `measured` both decide `status`. `grams_available` is `spoolman` (whole grams over unarchived spools, rounded down) or `unknown` with a reason (`not_spoolman`, `integration_missing`, `integration_disabled` (the integration is `enabled: false` and is not contacted), `unreachable`, `filament_missing`, `spool_weight_missing`). Unknown never reads as zero, and an unknown on either side gives `status: unknown`, never `short`. The read never reserves or deducts stock. Errors are [problem responses](problem-responses.md).

### History

```text
GET   /plans/:id/history?limit=50&cursor=...
```

The Rust API serves this read only with `desktop_rebuild` on. It returns a
Build's accepted revisions, snapshots, checkoff bursts, sends and assistant
decisions, newest first, one page at a time. [Build history](build-history.md)
describes the entries and paging.

### Plates and exports

```text
GET   /plans/:id/plates
POST  /plans/:id/plates/initialize
POST  /plans/:id/plates/arrange
POST  /jobs/export-accepted-plate-3mf
POST  /jobs/export-direct-3mf
POST  /jobs/export-stl-pack
GET   /exports/*
```

Export jobs return a job id. Completed jobs provide an artifact URL under `/exports/`.

### Printers and integrations

```text
GET    /printers
POST   /printers
PUT    /printers/:id/details
DELETE /printers/:id
GET    /integrations
POST   /integrations
PATCH  /integrations/:id
POST   /integrations/:id/test
GET    /integrations/:id/status
POST   /printer-send-queue
```

Printer fleet entries describe planning geometry. Integration records hold the host connection and capability configuration. Link the two instead of placing secrets on a fleet entry.

The Review sheet and part-color dialog send `profile_id` on filament catalog
and Spoolman inventory reads. Their query keys separate Plans and Spoolman access
states. When the engine advertises `tenant_feature_toggles`, these views mask
Spoolman enrichment during access refresh or denial and refetch when access
returns. An open 3D preview uses the same masked part data. Without that
capability, health failures do not hide Spoolman, queries retain the default
30-second freshness window, and the part-color dialog keeps the saved display
label when the live catalog has no match. Settings retains account scope by
omitting `profile_id`.

See [Printer setup](integrations/PRINTER_SETUP.md).

### Backups and storage

Backup routes are administrative, unprefixed browser API operations:

```text
GET    /backups
POST   /backups
GET    /backups/storage
GET    /backups/:name
GET    /backups/:name/preflight
POST   /backups/validate
POST   /backups/restore
DELETE /backups/:name
```

Use `GET /backups/storage` to read the logical-byte inventory and estimated
backup contents. Use the preflight endpoint before restoring a stored archive.
Uploaded archive validation returns the same restore preflight with its backup
metadata. Restore repeats archive and capacity checks immediately before it
extracts data. It returns `507` without closing SQLite when the data filesystem
does not have the required free space.

Backup uploads accept at most 20 GiB of compressed data. Archive validation
also limits expanded content to 20 GiB, one entry to 8 GiB, the archive to
100,000 entries, and the expansion ratio to 200:1. `POST /backups` validates the
completed archive against the same restore policy before it publishes the file.

Format v2 metadata declares either `scope.kind: "database-only"` with no
included roots or `scope.kind: "full"` with all 11 durable roots. The full scope
includes Source revisions, Working Source files, generated media and exports,
Assistant knowledge, manifest and kit configuration, custom filaments, and
`path-hints.yaml`. A full restore treats an absent scoped path as absent state;
a database-only restore leaves those paths unchanged. Format v1 archives keep
their compatibility behavior and replace only legacy roots that are present.

See [Operations](../OPERATIONS.md) for the restore procedure and concurrency
limits.

## Background jobs

A background operation returns:

```json
{
  "job_id": "<uuid>"
}
```

Read its state with:

```text
GET /jobs/:id
GET /api/v1/jobs
```

The browser subscribes to `GET /ws/jobs/:jobId` for progress. Clients that do not use WebSockets may poll the job route.

Do not retry a mutating job blindly after a timeout. Read the job or remote printer state first so a retry does not create a duplicate export or print start.

## Errors

JSON errors include a human-readable `detail` field:

```json
{
  "detail": "Plan not found"
}
```

Some routes also include `title` or a machine-readable `code`. Treat unknown fields as additive.

Typical status codes:

| Status | Meaning |
|--------|---------|
| `400` | Invalid request or state transition |
| `401` | Missing or invalid credentials |
| `403` | Authenticated but not allowed |
| `404` | Resource not found |
| `409` | Revision or state conflict |
| `413` | Request body or upload exceeds the configured limit |
| `507` | Data filesystem has insufficient space for restore |
| `429` | Rate limit exceeded |
| `500` | Unexpected server error |

## Request size limits

JSON request bodies are limited to 1 MiB. Routes that take bulk Plan edits or manifests accept up to 8 MiB: `POST /plans/{id}/save`, `PATCH /plans/{id}/drafts/{draftId}/parts`, `PUT /plans/{id}/drafts/{draftId}/reconciliation`, `POST /plans/{id}/progress/import`, `POST /plans/{id}/plates/initialize`, `PATCH /plans/{id}/production-setup`, `PUT /plans/{id}/kit-manifest`, and `PUT /sources/{id}/repo-manifest`. Reference-sharing routes accept 4 MiB. Assistant action apply and dismiss, and HTTP MCP, accept 96 MiB so a 64 MiB inline base64 file fits.

Multipart uploads have their own limits: Source uploads 256 MiB, kit bundles and print files 64 MiB, rendered Part thumbnails 1 MiB, and backups 20 GiB. Source uploads stream to disk while the limit is enforced.

## MCP

MCP uses the same domain operations as the application API. Read tools return product state. Mutating tools create a proposal that must be applied with `confirm_apply`.

Use streamable HTTP MCP on the running server:

```text
http://127.0.0.1:8080/api/v1/mcp
```

Inline file content in MCP tools, such as `propose_import_source_files` and `propose_import_3mf_checkoff`, is limited to 64 MiB of decoded bytes. Upload larger Sources through `POST /sources/{id}/upload-zip` or `POST /sources/{id}/upload-files`.

See [MCP setup](assistant-mcp.md) for client configuration and the confirmation flow.

## Browser route fallback

In the single-port Docker build, browser navigation to paths such as `/plan` or `/progress` returns the SPA document. JSON requests to API paths keep their JSON response. Reverse proxies must preserve browser navigation headers, including `Sec-Fetch-Mode` or an `Accept` header containing `text/html`.
