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

`POST /jobs/sync` with `{ "project_ids": [<id>, ...] }` enqueues a sync of those
GitHub Sources. The Rust runtime answers `202` with `{ "job_id" }`. The
replacement UI calls this `startSourceSync`, and reads branches with
`listGithubBranches` (`GET /api/v1/sources/github-branches?url=`). A missing
or empty `project_ids` is a 400 problem. The sync worker publishes each
Source's revision. Sync and scanning operations use background jobs when the
work may outlive one request.

While files download, `GET /jobs/{id}` and `/ws/jobs/{id}` show `progress`
between 0 and 100. A failed sync's `error` is the reason: the repository is
gone, GitHub truncated the tree, a download failed, or the sync would leave
less free space than the import reserve. The Source keeps its previous
revision. An id that is unknown or not a GitHub Source is listed in
`result.failures`; the other Sources still sync and the job ends `done`.
Stopping the server while a sync is `running` leaves that job. The next start
runs the same `job_id` again, and a following sync of the Source is not refused
as busy. Shutdown stops the sync worker inside the mutation drain (5 seconds).

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
POST   /plans/:id/drafts/:draftId/refresh
POST   /plans/:id/drafts/:draftId/apply
```

Applying a draft creates the accepted Plan revision used by Checkoff and Production. Callers should not update accepted state by patching part rows directly.

### Refresh an open Plan draft

Rust's `POST /api/v2/plans/{id}/drafts/{draft_id}/refresh` takes an
`Idempotency-Key` and `{ expected_draft, expected_header_version }`. Use the
open draft identity from the draft list or workspace, including its base,
lifecycle version and snapshot digest. Use the saved header's version, or
`null` when there is no draft header.

The signed-in draft author can refresh the Build's sole open draft against
changed, revision-tracked Source inputs. The Accepted base must still match
the observed draft's base. Refresh carries the header and maps saved
inclusion and quantity choices to Parts from the same Source. The writer
checks the observed draft, header, Accepted base, prepared Source fingerprint
and preparation settings, including Kit options and filament defaults, before
atomically abandoning that draft and inserting its successor. Accepted plans,
Required units and Build History stay intact.

The operation refuses competing drafts, stale observations and untracked
inputs. It also refuses saved manual reconciliation decisions and Part
overrides beyond inclusion and quantity that differ from Accepted.
For Parts with no Accepted predecessor, refresh treats their saved inclusion
as a decision and checks that preparation reproduces any stored filament assignment.
`merge_conflicts` names missing, ambiguous or conflicting Part targets.
A same-Source Part key and a tracked artifact digest that point to different
Parts cause a refusal.
Refusals leave the existing workspace available.

The 200 response is a durable receipt with `source_draft_id` and
`successor_draft_id`. Refresh performs no Required-unit selection. Read the
exact successor by ID and confirm its state before changing workspaces.
After an unconfirmed response, explicitly retry the original URL, body and
key as the same actor. Replay returns the original receipt even if the
successor has since closed. Reusing that key for a different refresh request
on the same Build returns `idempotency_conflict`.

### Production Setup

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

`grams_needed` is `measured` or `estimated` (each with `grams` and the `job_count` used), or `unknown` with a reason. It comes from past sends: each send's grams (the Printer Checkoff link's row in `print_link_grams`) divided by the units that send prints, averaged over the Part's five most recent sends with a checked-off unit for it, times the remaining units, rounded up. A Part with any measured send uses only measured sends. The reason is `grams_not_recorded` when no checked-off unit of the Build belongs to a send with grams, and `part_not_printed` when other Parts have such sends but this one does not. The owner chose the writer's design (GRE-1165): the send queue records the slicer's estimate when a job is uploaded, and the status watcher replaces it with the length the printer measured, converted with the send's grams per millimetre, when the print finishes. `estimated` is the basis while any Part's grams come only from estimates. It carries `reason: "measured_length_not_convertible"` when the printer measured a length but the sent file stated no length to convert it with; otherwise `reason` is `null`. `estimated` and `measured` both decide `status`. `grams_available` is `spoolman` (whole grams over unarchived spools, rounded down) or `unknown` with a reason (`not_spoolman`, `integration_missing`, `integration_disabled` (the integration is `enabled: false` and is not contacted), `unreachable`, `filament_missing`, `spool_weight_missing`). Unknown never reads as zero, and an unknown on either side gives `status: unknown`, never `short`. The read never reserves or deducts stock. Errors are [problem responses](problem-responses.md).

### Spoolman deductions (Rust-only)

```text
GET  /api/v2/spoolman/deductions?state=&link_id=
GET  /api/v2/spoolman/deductions/:id
POST /api/v2/spoolman/deductions/:id/reconcile
```

Served only by the Rust core with both rebuild gates on, behind a session or API key. The list answers `{ "deductions": [...] }`, newest first, at most 100. Each row has `id`, `state` (`pending`, `sending`, `applied`, `unknown`, `failed`, `skipped` or `dismissed`), `reason` (set only on `skipped`), `link_id`, `profile_id` (the Build), `units`, and `integration_id`, `spool_id`, `grams` (whole grams, rounded up) and `grams_basis` (`estimated` or `measured`), which are null on `skipped` rows. `failure` is set only on `failed`, `unknown` and `dismissed`. `failure_status` is set only when `failure` is `refused`. `state` must be one of those states and `link_id` 1 to 200 characters; any other query is refused with 400 `invalid_deduction_filter`. Checkoff writes the rows and returns without waiting for Spoolman. A background worker sends each `pending` row once after the burst commits; see [Spoolman](integrations/SPOOLMAN.md#on-the-rust-core).

`GET /api/v2/spoolman/deductions/:id` answers `{ "deduction": <row>, "grams_before": ..., "grams_now": ... }`. Each gram report is `{ "kind": "known", "grams": N }` or `{ "kind": "unknown" }`. `grams` is whole grams, rounded down. `grams_before` is what Spoolman reported when the row was claimed. `grams_now` is what that spool reports at the read. A skipped row, or a missing, disabled or unreadable integration, is `unknown` and is not contacted. The Spoolman Module being off does not block the read. A missing id is 404 `deduction_not_found`.

`POST /api/v2/spoolman/deductions/:id/reconcile` takes `{ "answer": "deducted" | "retry" | "dismiss" }` and answers the row. `deducted` moves `unknown` to `applied` and does not call Spoolman. `retry` moves `unknown` or `failed` to `pending`, clears the failure, and the worker then makes one more send. `dismiss` moves `unknown` or `failed` to `dismissed`, keeps the failure, and does not call Spoolman. Any other state is 409 `deduction_state` and changes nothing. A body that is not that shape is 400. Errors are [problem responses](problem-responses.md).

### History

```text
GET   /plans/:id/history?limit=50&cursor=...
```

The Rust API serves this read only with `desktop_rebuild` on. It returns a
Build's accepted revisions, snapshots, checkoff bursts, sends and assistant
decisions, newest first, one page at a time. [Build history](build-history.md)
describes the entries and paging.

### Build planning and decisions

```text
GET   /plans/:id/build-planning
GET   /plans/:id/decisions
```

With `desktop_rebuild` on, the Rust API serves both reads with Node's data:
the hydrated planning brief with its phase and readiness, and the latest 200
assistant decisions. [Build planning and decisions](build-planning.md)
describes the fields and the differences from Node.

### Workflow and tasks

```text
GET   /plans/:id/workflow
GET   /api/v1/tasks
```

The Rust API serves these reads only with `desktop_rebuild` on. The workflow
returns a Build's stages and next action with its reason and target; the task
feed returns every open Build's next action, most urgent first, and lists
Builds it could not read under `unavailable`. MCP
`get_build_workflow` returns the same workflow body.
[Build workflow and task feed](build-workflow.md) describes the rules.

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

The Rust core answers exports on request instead, with no job and no stored file:

```text
GET   /api/v2/plans/:id/exports/checklist
GET   /api/v2/plans/:id/exports/stl-pack[?scope=remaining]
GET   /api/v2/plans/:id/exports/managed-3mf?plate_revision_id=:revision
GET   /api/v2/plans/:id/exports/direct-3mf?tokens=:comma_separated_required_unit_tokens
```

It returns one self-contained HTML page as an attachment named `<build-slug>-v<plan version>-checklist.html`. The page shows the Build name, order number, Plan version and generated time, then every included Part grouped by Source and folder: file name, role, colour swatch, quantity, printed units out of quantity, and notes. Each included tracked Part with a renderable accepted artifact includes a PNG thumbnail derived from the artifact pinned by the accepted Plan, never the current Source revision; legacy, missing, unsafe, oversized or unrenderable artifacts omit the image without omitting the Part. Rendering reads at most 64 MiB per artifact and accepts at most 1,000,000 triangles. The export embeds at most 8 MiB of PNG bytes; if an image would exceed the remaining budget, it and subsequent thumbnails are omitted while all Part rows remain. Quantities and printed counts are those of `GET /api/v2/plans/:id/checkoff/units` at the same moment. The page has no script, embeds images as data URLs, and its Content Security Policy allows only its inline style sheet and data images, so opening it from disk makes no request; all Plan text is escaped. Without a valid session or API key it answers 401, for an unknown Build 404, and for a Build with no accepted Plan 409 with code `no_accepted_plan`. An accepted Plan that needs compatibility repair or was never initialized answers 409 with code `accepted_plan_unavailable`, and busy or stopped storage answers 503. The response also sends the page's Content Security Policy as a header and `Cache-Control: no-store`.

The STL pack returns a ZIP attachment named `<build-slug>-v<plan version>-stl.zip`. It holds one stored (uncompressed) entry per Required unit of each included Part, named `<role>/<folder>/<file stem>_<NN>.stl`: `<folder>` is the folder of the Part's Source-relative path, `_root` at the top level, and `<NN>` the unit's number from 01. Each segment keeps ASCII letters, digits, `_`, `-` and `.` and turns any other run of characters, `/` and `\` among them, into one `_`, so no entry name leaves the folder it is extracted into. A name already used, compared without regard to case so no entry overwrites another when the archive is extracted on a case-insensitive file system, gains `_2`, `_3` and so on. Each entry is byte-identical to its Source file. `scope=remaining` leaves out units already checked off; `scope=all` is the default, and any other value answers 400 `invalid_scope`.

Before the first byte, every Source file the pack needs is opened without following links and checked against its accepted SHA-256. If any is missing or altered, the whole pack is refused: 409 `source_files_unavailable` with a `files` list of `relative_path`, `source` and `reason` for each such file. `scope=remaining` with nothing left answers 409 `nothing_to_pack`, and a pack over 65,535 entries or 4 GiB, the limits of a plain ZIP, answers 422 `pack_too_large`. Credentials, unknown Builds and Builds without an accepted Plan answer as for the checklist.

The archive streams as it is written, so the response is chunked and has no Content-Length. Each file is hashed again as it streams; if one changed after its check, the transfer stops, the client keeps a broken archive rather than wrong bytes, and the server logs a warning naming the Source file.

The managed Plate export downloads a ZIP containing one 3MF model per Plate and
`manifest.json`. Use the current `plate_revision_id` from the Plate workspace.
The manifest records the accepted Plan and Plate revisions, printer references,
Required-unit tokens, filament colors from the Plate's accepted Plan revision,
and the verified Source revision and SHA-256 for each model object. Geometry uses
the saved placement in millimeters; the models do not contain sliced toolpaths.
A missing or stale revision, an
explicitly unplaced unit, an invalid layout, or missing or changed Source
geometry refuses the whole download. A valid session or API key is required.
The Rust-only runtime generates the archive on a blocking worker and stores
nothing. One export runs at a time, with caps of 10,000 objects, 5 million
triangles, 64 MiB per Source file, 256 MiB of Source reads, and 128 MiB each
for model XML and the ZIP. Repeated objects count toward those limits.

#### Direct 3MF download

Direct 3MF downloads one `.3mf` attachment (`model/3mf`) from the current
accepted Plan. It takes 1–256 distinct Required-unit tokens, includes only
those units, and verifies each Source SHA-256 before rendering. It packs the
objects in token order, with a 10 mm gap and a 250 mm row target. Larger
objects start their own row. It does not use managed placements, select
a printer, or send to a slicer. It reads the Plan's current filament colors.
For a Spoolman filament, it uses the color saved with the matching role
assignment, without contacting Spoolman.
The managed and direct downloads share the Source, triangle, and output caps
above. Invalid or duplicate tokens return `400 invalid_direct_selection`;
tokens not required by the accepted Plan return `409 invalid_direct_selection`.
An object or layout outside the coordinate range returns
`409 direct_packing_conflict`. Neither download writes a job or a file.

The legacy direct job aliases (`POST /jobs/export-direct-3mf` and
`POST /api/v1/jobs/export-direct-3mf`) are refused with
`410 direct_3mf_job_retired` in the Rust runtime. On the compatibility gateway,
they remain with Node until the existing `export.direct-3mf` operation cutover
record takes ownership of both aliases and fences the Node worker together.
After cutover the gateway refuses both aliases; it never sends them to Node
or starts a Rust replacement job. The managed Plate job and slicer-handoff
aliases are a separate operation and do not switch here.

#### Local slicer handoff

```text
GET  /api/v2/slicer-instances
POST /api/v2/plans/:id/exports/slicer-handoff
```

These routes require a signed-in session on the Rust-only loopback desktop.
They are not mounted in Node, on the headless server, or on a shared listener.
The picker returns `instances` (each with `id`, `name` and `available`) and
`default_instance_id` (null when the configured default is unavailable).
Configure them locally in `<data>/slicer-handoff/instances.json`, for example:

```json
{"default":"orca","instances":[{"id":"orca","name":"OrcaSlicer"}]}
```

Create one inbox directory next to the config for each instance, named by its
ID. On Unix it must be owned by the desktop user and have mode 0700. Missing
inboxes show as unavailable. The config is read on each request: instance IDs
use lowercase ASCII letters, digits and hyphens, start with a letter and are at
most 32 characters. The config may list up to 32 instances. An absent or invalid
config yields an empty picker. There is no remote config endpoint.

POST with `plate_revision_id`, the one-based `plate_ordinal`, and optionally
`instance_id`. Omitting the instance uses only the configured default; it never
chooses another instance. The request verifies the accepted revision and Source
bytes as for the managed export, then publishes one standalone 3MF. The result
names the `instance_id`, `filename` in that inbox, and whether it was
`already_delivered`. Open or copy that file in the slicer; leave the original
in place so a retry, including after restart, does not publish it again. The
filename includes the 3MF's SHA-256, so reusing a revision ID after restore
cannot overwrite a different Plate. A different file at the same name is
refused. This handoff does not launch applications, slice, or send a print.

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
POST   /printer-send-queue/checkoff-preview
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

The Rust-only runtime, with `PP_DESKTOP_REBUILD` exactly `1` or `true`, adds
two routes that Node does not serve. `POST /api/v2/installation/backup`
creates one whole-installation archive under the data directory's `backups`
folder and answers its file name, time, app version, size and SHA-256.
`GET /api/v2/installation/backup/archive` streams that archive. Free disk
bounds the size. The routes are absent when the flag is off. See
[Whole-installation backup export](storage.md#whole-installation-backup-export).
`POST /api/v2/installation/restore/stage` takes a backup archive as the
request body and answers a `restore_id` and the confirmation text.
`POST /api/v2/installation/restore` with that id and the exact text replaces
the database and files. See [Stage and restore routes](storage.md#stage-and-restore-routes).

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

Multipart uploads have their own limits: Source uploads are bounded by free disk and the import reserve (`/settings/import-free-space-reserve`, default 5 GiB), kit bundles and print files 64 MiB, rendered Part thumbnails 1 MiB, and backups 20 GiB. Source uploads stream to disk. The reserve is checked before any byte is staged.

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
