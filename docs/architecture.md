# Architecture

Owner: Chad. Target decisions recorded 2026-11-02, against `main` at `a8d6323a`.
This is a design contract, not a claim that the target ships today. Linear owns
execution state. [ADR 0008](adr/0008-replace-the-old-screens.md) records screen
replacement; [ADR 0009](adr/0009-keep-crates-and-deepen-modules.md) records module
ownership; [ADR 0010](adr/0010-whole-runtime-cutover.md) records cutover.
The owner-delegated research choice on 2026-11-03 keeps that design and adds
[ADR 0015](adr/0015-shared-filesystem-portability.md) for narrow filesystem
sharing. The amendment is chosen design, not implemented portability.

## Product and system shape

One Rust core serves one replacement React UI over HTTP to a Tauri 2 window,
Docker browsers, and explicitly admitted LAN devices. npm and Vite build assets;
Node is not part of the target runtime. Docker is the only headless deployment,
not launchd, systemd, or Windows Service.

```text
pp-desktop / pp-server
          |
       pp-core                 composition, startup, workers, shutdown
       /       \
      /         \
pp-gateway     pp-api           composed sibling routers
listener,      HTTP, MCP,
admission,     event adapters
assets            |
                  v
       typed operation-family interfaces
          | pp-storage          transactions, jobs, backups
          | pp-services         policy and native slicer operations
          | pp-source           immutable filesystem snapshots
          ` pp-drivers -> pp-net checked external effects
```

`pp-core` composes the sibling gateway and API routers; `pp-gateway` does not
depend on `pp-api`. This is a call/ownership sketch, not a full Cargo graph.
`pp-core` injects adapters where crates cannot depend on one another.

The [accepted UI target](product/ui-ux-target.md) owns Today, Build-local tabs,
History/Accept, and burst Checkoff. New order fields, transactional duplicate-order
protection, and Spoolman shortfall tasks are in this program. One order has one
authoritative Build. STEP conversion and the Machine tab stay parked and are not
part of the current build order. Re-entry needs Chad's approval. There is no
old-screen visual or route-shape parity requirement.

Preserve single-user SQLite formats and history, immutable Plan/Plate/Required-unit
IDs, draft autosave, and restart recovery. Printer sends require explicit user
action and never auto-retry an uncertain outcome. Sharing defaults off; MCP always
requires authentication, including loopback. No multi-user, SaaS, Postgres, product
analytics, or default-on telemetry. Sentry remains opt-in.

## Out of scope

Order features no longer wait until Rust reaches parity with Node. The
2026-11-02 decisions put new order fields, transactional duplicate-order
protection and Spoolman shortfall tasks in this program. These stay out:

- Multi-user accounts, SaaS, hosted planning and tenant modes, Postgres, and the
  community board. They have no target owner.
- Product analytics and default-on telemetry. Sentry stays opt-in.
- Porting the old screens, and old-screen visual or route-shape parity
  ([ADR 0008](adr/0008-replace-the-old-screens.md)).
- STEP conversion and the Machine tab. They are parked; re-entry needs Chad's
  approval.
- Headless modes other than Docker: launchd, systemd and Windows Service.
- An `/api/v3` namespace, or old route aliases kept only for screen parity.
- Turning on `desktop_rebuild` or `PP_DESKTOP_REBUILD` by default.

## Current state and transition

The shipped gateway still supervises Node through `pp-compat` and a private
channel: a Unix socket on macOS and Linux, and on Windows a named pipe that Rust
creates with a current-user DACL and `PIPE_REJECT_REMOTE_CLIENTS` and Node dials
into. Rust owns the directory lock and process lifetime; Node owns live domain
operations. On Unix, Node runs in its own process group with a parent-death
watcher; on Windows it is created inside a Job Object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the kernel terminates it and its
children when the core exits, however it exits. Rust storage, HTTP routers, and jobs already have substantial tested
implementations behind `desktop_rebuild`. With both gates on, `pp-server` and
`pp-desktop` now start without a Node child and mount the read, draft and jobs
routers in the gateway as a sibling router composed by `pp-core`
([Rust-only product entry point](desktop-rebuild.md#rust-only-product-entry-point)).
That mode is for disposable data until refusal of Node-used directories lands.
A registered router is not a delivered operation.

The baseline gateway inventory has 781 compatibility routes. The operation
manifest has eight Node-owned operations and 82 reviewed resource sets. The
[operation inventory](development/operation-inventory.md) report tracks that
split; it lists 0 of the 781 routes as migrated to Rust. The
per-route cutover machinery works. Whole-runtime cutover makes it unused, not
defective; removal is scheduled retirement work after replacement evidence.

Mounting an authenticated Rust-only product entry point is the prerequisite to
UI work. Keep GRE-621's existing scope and issue untouched. Track any remaining
product-entry-point work in a separate issue, without changing GRE-621 or adding
relations to it. GRE-794 and GRE-800 are existing prerequisites; their issue
content and state also stay unchanged. The new entry point must not wire screens
to Node or introduce a mutation fallback. Maintain the old app for correctness
and data safety until cutover.

Keep the default-off Cargo feature and exact runtime opt-in
`PP_DESKTOP_REBUILD=1` or `true`. Test both gates independently and together.
Neither this design nor cutover approval authorizes changing the default.
[Desktop rebuild](desktop-rebuild.md) remains the implementation/flag reference;
its parity-first sequencing and default-on proposal are superseded by these ADRs.

## Crates and allowed edges

Keep the existing operation crates and deepen their private family modules.
Do not require a blanket split, consolidation or rename, or freeze the crate
count. Keep `pp-services`, `pp-source`, `pp-contracts`, and `pp-perf` initially.
The narrow `pp-os` filesystem crate, specified in ADR 0015, exists with
`pp-storage`, `pp-source` and `pp-compat` as its consumers. This is not a move of all
platform code into one crate.

The following allowed workspace dependencies reflect
[`check-boundaries.py`](../rust/scripts/check-boundaries.py) at the baseline.
An allowed edge does not mean every operation is implemented.
Each linked crate README lists its public interface, effects, flags and tests.

| Crate | Responsibility and interface | Allowed `pp-*` dependencies |
| --- | --- | --- |
| [`pp-domain`](../rust/crates/pp-domain/README.md) | Bounded IDs/values and pure rules; no I/O, Tokio, SQL, HTTP, or Tauri | none |
| [`pp-api-types`](../rust/crates/pp-api-types/README.md) | Wire DTOs, constructors, validation, serialization | `pp-domain` |
| [`pp-contracts`](../rust/crates/pp-contracts/README.md) | Existing operation contracts, compatibility facade and parity tooling | `pp-api-types` |
| [`pp-net`](../rust/crates/pp-net/README.md) | Outbound target policy, checked connections, DNS/redirect restrictions | none |
| [`pp-drivers`](../rust/crates/pp-drivers/README.md) | Printer/provider protocol adapters, not send authority | `pp-net`, `pp-api-types` |
| [`pp-services`](../rust/crates/pp-services/README.md) | Sync policy through `GithubPort`, slicer discovery/profile/handoff modules, operation policy through injected ports | none |
| [`pp-storage`](../rust/crates/pp-storage/README.md) | Typed family handles; SQL, transactions, writer/read pool, migrations, durable jobs, lease, backups | `pp-os`; `pp-domain`, `pp-contracts`, both gated |
| [`pp-source`](../rust/crates/pp-source/README.md) | Immutable Source revision bytes, bounded extraction/materialization; no catalog SQL | `pp-os` |
| [`pp-api`](../rust/crates/pp-api/README.md) | Thin authenticated HTTP/MCP/event adapters over family handles | `pp-storage`; `pp-services` gated |
| [`pp-gateway`](../rust/crates/pp-gateway/README.md) | Listeners, bootstrap/session admission, assets; temporary proxy/ownership code | `pp-compat`, `pp-drivers`, `pp-net`, `pp-os` |
| [`pp-compat`](../rust/crates/pp-compat/README.md) | Temporary Node supervision/private socket, retired with Node | `pp-os` (Windows log entries, Node path spelling) |
| [`pp-core`](../rust/crates/pp-core/README.md) | Compose handles/adapters, lock/startup, workers, bounded shutdown | `pp-gateway`, `pp-compat`, `pp-storage`, `pp-perf`, `pp-os`; `pp-source`, `pp-net`, `pp-api` gated |
| [`pp-desktop`](../rust/crates/pp-desktop/README.md) | Tauri shell, native menus/tray, installed lifecycle | `pp-core` |
| [`pp-server`](../rust/crates/pp-server/README.md) | Docker launcher, signals, headless lifecycle | `pp-core` |
| [`pp-perf`](../rust/crates/pp-perf/README.md) | Measurement records and regression comparison, not product telemetry | none |
| `pp-os` | Directory handles with handle-relative entry access on Unix and Windows (open, inspect, enumerate, create, rename, remove), no-follow opens and inspection, file identity, advisory locks, durable rename, display spelling of canonical paths; no policy | none |
| [`pp-testkit`](../rust/crates/pp-testkit/README.md) | Disposable test installations and the ownership-marker fixture format; only ever a dev-dependency | none (any crate may dev-depend on it) |

The table is the implemented graph, not a manifest for the amendment.
`pp-os` has no `pp-*`, SQL, HTTP, Tauri or Tokio dependencies. `pp-storage`
uses it for the data-directory lease's file mechanics, preflight opens, backup
reads and durable backup publication. `pp-source` uses it for every Source
directory operation: the pinned root walk, staging, enumeration, publication by
no-replace rename and its directory locks. `pp-compat` uses it on Windows for
its log writes relative to the pinned log directory: append opens,
exclusive creates, the archive security open and the replacing rotation
rename. Read-only tail reads still open by path. Its Unix logs still call `openat` and `renameat` through libc.
On every platform it hands Node paths in `pp_os::display_path` spelling, and
`pp-storage` stores, returns and compares Source paths in that spelling, and
`pp-core`'s review observations compare a snapshot root with the repos root
in it.
`pp-core` uses it for its state files (`desktop.toml`, browser preferences and
desktop settings): no-follow reads relative to the data directory handle, identity
rechecks, private creation and durable publication, and for the review thumbnail
open and the perf scratch directory. `pp-os` supplies directory-relative handle access,
identity/no-follow checks, lock primitives and durable file publication. The
crate documentation in `rust/crates/pp-os/src/lib.rs` lists where the Unix and
Windows guarantees differ; on Windows it reports no POSIX owner or mode, so
`pp-storage` skips those checks and relies on inherited ACLs. On Windows a
`Dir` handle shares delete, so the directory can be renamed or removed while it
is open, as on Unix, and entries are opened, inspected, created and deleted
with `NtCreateFile` relative to that handle, the counterpart of the Unix `*at`
calls. NTFS refuses to rename a directory while a handle to any entry below it
is open, so `pp-source` closes every handle below a snapshot candidate or a
retained-capture phase directory before renaming it, and retained captures
prove they still own an entry by its recorded `pp-os` identity, rechecked by
name, instead of by a held handle; Unix also keeps a descriptor per entry so
an inode number cannot be reused. NTFS refuses byte-range locks on a directory, so a directory lock on
Windows is a lock on a regular file inside the directory. Windows locks are
mandatory for the range they cover, so `pp-os` locks only the byte at offset
`i64::MAX` and other handles can still read a held lease file. Storage keeps installation lease
lifetime, migration ordering and SQL transactions. Source keeps snapshot policy.
Native shell behavior and worker/signal orchestration stay with their owners.

Use the baseline graph plus those bounded additions as the initial target
constraint. A concrete family slice may
propose a DTO or pure-domain edge, as ADR 0007 anticipated, but must update the
checker, manifests, and dependency documentation together. Do not assume a new
`pp-services -> pp-storage/net/drivers` edge. `pp-core` composes implementations
through injected ports. Keep transaction orchestration inside `pp-storage`.
Retirement later removes `pp-compat` and its edges in its own reviewed change.

The checker currently enforces workspace edges, not third-party SQL imports.
Removing `pp-api`'s `rusqlite` dependency is a real, separately verified slice:
classify fixture/test uses, move SQL setup behind storage-owned helpers, and
prove HTTP behavior unchanged. Do not describe test imports as production SQL
ownership. No new production SQL belongs outside `pp-storage`.

Under GRE-339, replace the central `Envelope` collision point family by family
with private dispatch modules and typed handles. Keep bounded admission, one
writer, cancellation, deadlines, and reply semantics intact. Exposing unrestricted
connection/write closures to callers would undo this ownership.

## Operation families and write ownership

Each row names the target owner, not current completion. All HTTP and MCP callers
use the same operation interface. A transport must not repeat its policy.

| Family | Owner and collaborators |
| --- | --- |
| Authentication, sessions, API keys, pairing | `pp-api` admission; `pp-storage::auth` owns credential/session transactions; gateway owns listener admission |
| Product Module choices and settings | `pp-storage` feature/settings modules; `pp-api` exposes effective state and off reasons; recheck before worker effects |
| Sources, manifests, imports, scan, docs, sync | `pp-storage` catalog/scan/sync/upload transactions; `pp-source` stages immutable bytes; `pp-services` decides sync policy; core composes checked fetch/watchers |
| Builds, drafts, parts, order/customer/due-date fields | `pp-storage` build/draft modules own lifecycle, autosave receipts and order authority; `pp-services` holds extracted pure policy where useful |
| Accept, proposals, accepted Plans, Required units, Plates | `pp-storage` publication owns one atomic acceptance transaction; UI and MCP share proposal/draft state |
| Checkoff, assembly, bed confirmation, unmatched prints | `pp-storage::checkoff_progress` owns bursts, matching confirmation, receipts, compensating undo and the farm log ([Farm log](farm-log.md)); drivers only supply observations |
| Workflow, readiness, Today tasks, History | `pp-storage::read_model` owns consistent projections; task feed extends the GRE-415 workflow projection, not a second ranking engine (served by `read_model::build_workflow`, [Build workflow](build-workflow.md)); History includes legacy assistant decisions |
| Exports and production setup | Pure encoding/packing modules in `pp-domain` or existing `pp-services`, storage owns revision-bound artifact/job metadata, core runs workers and publishes staged files |
| Printers, queue, discovery, Bambu | Storage owns admitted queue/effect records; `pp-drivers` owns protocols through `pp-net`; core owns watchers; Bambu remains status-only. Farm reads join stored printers, results and queue (`pp-storage::read_model::farm`) with the watchers' observation port, read-only ([Farm reads](farm-reads.md)); `GET /ws/printers` streams the same projection from the watchers' ordered transitions ([Printer stream](printer-stream.md)); Moonraker connection test, capabilities, file browse and camera list are on-demand driver reads through the gateway ([Printer-host reads](printer-host-reads.md)) |
| Stock and Spoolman shortfalls | `pp-drivers` provides Spoolman reads/effects; storage projects readiness/tasks and owns deduction intent/reconciliation. Spoolman alone owns stock counts. Today `pp-storage::read_model::stock` computes shortfalls (the workflow input for Today, Build Overview and the task feed) and `pp-core::spoolman` reads inventory through `pp_net` checked connections, injected into `pp-api` because `pp-core` may not depend on `pp-drivers` |
| Native slicers and profiles | `pp-services` owns discovery/profile/handoff operations; storage binds artifacts to accepted revisions; shell/core invokes native adapters |
| Optional external slicer jobs | Typed HTTP adapter in existing `pp-drivers`, policy in `pp-services`, job/effect state in `pp-storage`, worker lifetime in core |
| Home Assistant, webhooks, Discord | `pp-drivers` checked adapters; storage owns configuration, deduplication and durable delivery state; core runs workers |
| Jobs, cancel, restart recovery | `pp-storage::jobs` claims/fences/recovery; core runs workers; `pp-api` adapts commands and progress |
| Backup, restore, migration/downgrade refusal | `pp-storage` owns snapshot/archive/activation and lock discipline; core quiesces workers/listeners; shell/API requests the operation |
| Admin, legal, logs, diagnostics, health | Storage owns settings; gateway/API exposes bounded safe reads; core owns local logging/liveness; shell owns update lifecycle |
| Reference sharing | Storage owns reference projections, `pp-api` exposes them under [ADR 0004](adr/0004-share-references-without-redistributing-models.md); do not redistribute model bytes or imply LAN admission |
| Assistant and old API aggregators | AI uses authenticated MCP over the same operations; assistant HTTP routes are not a separate policy engine. Rust routers replace v1/v2 aggregators |
| Community board, hosted/tenant/Postgres modes | No target owner; excluded from this single-user product |

One runtime owns the complete operation, including routes, workers, tables, and
files. Within Rust, only storage writes SQL. `pp-source` owns staged immutable
Source bytes, not catalog activation. File-producing workers stage and validate
results before storage admits their publication; startup reconciles interrupted
publication. No transaction remains open across network calls or large exports.

Keep bundled rusqlite, the dedicated bounded writer and read-only pool, WAL,
foreign keys, busy deadlines, and `synchronous=FULL`. Existing defaults are a
32-entry writer queue and four readers, not performance claims. Async handlers
must await bounded admission without blocking Tokio workers. Keep reader
transactions short and observe WAL/checkpoint growth. Require a local filesystem
with tested locking; a Docker mount does not make a network filesystem safe.

## Acceptance, autosave and recovery contracts

Every Plan change, including name/order/customer/due date, first changes a durable
draft. Autosave is not acceptance. Accept checks the preview revision, saves the
Build snapshot, applies the change and records history atomically. Preserve
printed evidence and immutable identities. Restore creates a draft for the gate;
it never rewinds Checkoff or a printer command.

This separation remains implementation work. The baseline
`WorkingDraftClient::save` creates a draft, reconciles it and calls
`publish_in_transaction`; it is not draft-only autosave, and the draft router
does not mount `/save`. Accept is `POST /plans/{id}/drafts/{draft_id}/apply`
on the draft router (GRE-417), the only route that publishes. Deliver durable draft-only saving and an
explicitly authorized Accept operation through the shared family interfaces.
Reuse allocation and reconciliation, but prove accepted-state isolation and
snapshot/history atomicity rather than treating a rename as delivery.

Enforce one authoritative Build per order transactionally. A duplicate is a
detached experiment with lineage; promotion retires the former authority in the
same transaction. Display existing conflicts, never silently merge them.

A Checkoff burst is one transaction with a stable idempotency key, canonical
request hash, authoritative result and undo receipt. Same key/request returns the
original receipt; changed payload conflicts. Check stored receipts before stale
revision rejection on an original retry. Undo is a compensating operation with
conflict rules, not cache rollback. Uncertain responses reconcile by operation ID.
The UI disables offline writes instead of queuing commands silently.

Extend existing durable job claims, attempt fencing, cancellation and effect
records. Persist authorized intent before an external call and observed receipts
afterward. Recovery distinguishes restartable pure/staging work from effects that
may have happened. An unknown printer send becomes reconciliation work, never a
second send. An outbox is suitable for at-least-once notifications, not automatic
printer resend. Spoolman deductions need their own duplicate/reconciliation rules.

Require a send capability constructible only from an authenticated, explicitly
confirmed user action. A freely constructible `Actor::User` enum alone is not
authorization. MCP/background watchers cannot mint the capability. Upload-only
and upload-and-start are separate confirmed effects; upload-only is the default.
Shutdown stops admission, signals cancellation, settles permitted writes and
joins workers before releasing the directory lock.

## HTTP and generated TypeScript contracts

Keep axum family routers over typed operation handles. Parse bounded inputs at
the HTTP seam, enforce state/cross-field rules in the operation, and map safe
typed errors to RFC 9457 problem responses. Preserve required MCP JSON-RPC errors.
Never expose SQL, raw error chains, secrets or arbitrary paths. `pp_api::problem`
is the one builder for those responses; [Problem responses](problem-responses.md)
lists the stable types and the leak canary.

Rust owns new wire contracts. Keep the working Rust schemars-to-JSON-Schema-to-Zod
generation path first, including runtime validation and drift checks. Move
generated-only output into a separate client package before retiring the
hand-maintained contracts mirror. Retire TS-first typify inputs per family, not
by hand-authoring the generated Rust file.

Add method/path/auth/request/response metadata tied to actual Rust registration,
including the existing `/api/v2` draft routes. `web/packages/rust-client` is
that package for Builds list and detail, layers, `/api/v2` drafts and health. Its
route table is `pp_api::client_contract::CLIENT_ROUTES`, which a `pp-core` test
checks against the composed routers. Its
[README](../web/packages/rust-client/README.md) lists the coverage and wire rules. REST shape may change for the new
UI; do not invent `/api/v3` or promise old aliases for screen parity. Preserve
external MCP paths/tool names and verify the actual inventory before retirement.

A bounded comparison may choose ts-rs or generated OpenAPI plus a typed fetch
client. Neither is a prerequisite to the first useful screen. OpenAPI need not
be hand-maintained, and Specta is not limited to Tauri invoke. Choose one
generation authority after wire tests prove tagged unions, omitted/null values,
Unicode, unknown keys, safe integer/ID representation, errors and event versions.
Generation runs without Node server or live data and fails CI on drift.

## Live events and client state

Keep Chad-approved `GET /ws/printers` and existing job WebSockets. For the new
one-way domain feed, prefer authenticated SSE if same-origin session admission
works in all webviews. Bearer-only clients need a reviewed fetch-stream adapter
or WebSocket, not credentials in URLs. Final domain transport remains a bounded
implementation decision; transport does not determine durability.

Commit durable domain-change events with their state transaction. Include schema
version, cursor, aggregate ID/version, operation ID and safe bounded payload.
This is a change feed, not an event-sourced replacement for SQLite state.
Initial state and cursor come from one consistent snapshot; replay-to-live
handoff must close the race. Include a database-generation identifier so backup
restore cannot silently reuse a cursor. Bound replay/storage/queued bytes and
slow-client deadlines; gaps or expired cursors require resnapshot.
The gated storage log, its SSE route `GET /api/v1/events` and their bounds
are described in [Domain events](domain-events.md).

One client event/cache adapter invalidates affected queries and coalesces
refreshes. Apply direct deltas only when versioned and complete. Printer samples
may coalesce; accepted state and Checkoff must not disappear. Last-known data
stays visible with age/stale indicators. Distinguish core offline from printer
unreachable. Browser polling does not return under a different screen name.

## Authentication, LAN and MCP

Use revocable device-scoped sessions with hashed random secrets, HttpOnly cookies,
explicit lifetimes and rotation. Retain compatible credential verification during
upgrade; prefer calibrated Argon2id for new passwords. Apply exact Host/Origin
admission, CSRF protection for cookie mutations, body/content-type limits and
rate limits on every applicable listener/stream. Trust forwarded headers only
from a configured proxy.

Sharing binds a second listener only after opt-in. Pairing needs desktop
approval, expiring single-use challenges, attempt limits and visible revocation.
Secure LAN login/cookies require a tested HTTPS trust path, such as a trusted
reverse proxy or explicit certificate enrollment. A self-signed CA is not
automatically trusted by phones. Any insecure LAN option needs separate informed
owner approval. mDNS is discovery, not authentication.

MCP requires credentials on loopback and LAN. A stdio bridge authenticates to the
existing runtime and never opens another database. Test supported protocol
revisions explicitly. Research identified MCP 2026-07-28 and its changed
Streamable HTTP/session semantics; the pinned rmcp version alone does not prove
conformance. Current-spec OAuth discovery/audience/scope support versus an
explicitly limited API-key compatibility mode remains a release decision.

## Replacement UI and desktop shell

Use a separate Vite workspace app, proposed `web/apps/ui`, and the generated
client `web/packages/rust-client`, so deleting frozen `web/apps/web` pages cannot
delete the replacement. Final package names and router follow the first bounded
integration slice. Enforce an import graph without old pages, Node server,
hand-maintained contracts or legacy domain rules.

The package exists: `@print-partner/ui` at `web/apps/ui` (see its
[README](../web/apps/ui/README.md)). The ESLint rule
`print-partner/ui-boundary` (`web/eslint-rules/ui-boundary.js`) fails any import
that leaves `web/apps/ui` by relative path, and any `@print-partner/*` package
not on its allowlist. The allowlist holds only `@print-partner/rust-client`. The dev server forwards `/api`, `/ws` and `/health` only to
`PP_UI_CORE_URL` and has no default target.

Starting choices, kept until the Builds comparison says otherwise: React Router 7
(`react-router`, the library the old app already uses) and TanStack Query 5 as
the one server-state cache. Both are already in the lockfile, so the package
adds no new runtime library. TanStack Router is compared in the Builds slice.

Use one server-state cache, validated Build URLs, and feature-local presentation
state. Compare TanStack Router with the existing React Router in one Builds
slice; retain Radix/cmdk unless an accessibility test shows a reason to switch.
Bundle IBM Plex, generate exhaustive status descriptors from Rust, and preserve
execution state separately from bed-clear state. `pp_domain::printer_display_state`
defines the printer display enum, its label, icon and colour token in one
wildcard-free match, and `PrinterStateView`, which keeps `execution` and
`bed_clear` as separate fields next to the derived `display` word. The client
generator emits the descriptors as `printerDisplayDescriptors`. Deliver loading, empty, error,
Module-off and offline states. Ship accessible 2D plate views first; lazy 3D
cannot become a requirement for Checkoff, Accept or Production.

Tauri keeps the same authenticated loopback HTTP origin as browsers. Domain work
does not use Tauri invoke. The default webview has no native commands; retain and
test the existing narrow rebuild-only grants separately. Native menu, focus,
tray, notifications and autostart remain Rust-owned. LAN clients gain no native
privileges. Apply CSP to the actual HTTP document responses, not only Tauri config.

Update coordination must quiesce work, verify compatibility and backup before
installation, then recheck schema admission at startup. Signed update artifacts
do not provide database rollback. Sign final assembled bytes, with OS signing
and updater signing as separate trust layers. Keep automation drivers test-only
and prove they are absent from public binaries.

## Docker and optional slicing

Build assets in a Node stage and `pp-server` in a pinned Rust stage. The runtime
contains the Rust binary, assets, required system libraries/certificates and
notices, not Node, npm, Python, a GUI or bundled slicers. Start non-root with an
explicit volume-ownership migration procedure. Prefer a maintained slim runtime
until actual linkage tests justify another base. Keep build secrets out of image
layers. Base-image pinning and SBOM/provenance artifacts follow existing release
policy; this design does not add new acceptance gates for them.

Use `/data` as the writable installation volume, read-only rootfs where supported,
bounded temporary space, dropped capabilities and no-new-privileges. Liveness is
local and bounded; readiness stays false during migration/admission failure.
Health checks never create jobs or contact printers. Test amd64 and arm64 images
independently rather than treating a multi-arch manifest as execution evidence.

Optional Docker slicing runs in a separate container behind an authenticated,
bounded HTTP start/status/cancel/result adapter with request IDs and restart
reconciliation. The core never gets the Docker socket or privileged mode.
Absent/unreachable slicers are honest optional states, not bootstrap failures.
Keep legacy `slicer-sidecar` maintained until the external contract is delivered;
do not delete it merely to remove Node.

## Whole-runtime cutover and rollback

This is the chosen model, not permission for production execution. Chad separately
approves production cutover after a timed, versioned runbook rehearsal.

1. Inventory supported input schemas, database, managed files, secrets, pending
   jobs and external-effect evidence. Pin old/new artifacts and acceptance checks.
2. Rehearse on disposable representative copies with protocol simulators.
   Compare immutable IDs, accepted history, drafts, Checkoff, exports and recovery.
3. Stop/drain Node and exclude writes. Take a consistent whole-installation
   backup using the supported SQLite/archive procedure, not a live `.db` copy.
   Verify digests and restore it into a separate disposable directory.
4. Migrate a separate Rust-owned copy. Node and Rust never share a live data
   directory. Retain the pre-cutover copy and matching old artifact.
5. Validate readiness, authentication, persistence and restart before normal
   mutations or effects. Chad records the go/no-go and authority switch.
6. Observe through an owner-set acceptance window, including an evening Checkoff
   session and restart. Retain recovery copies for a separately declared period.

Rollback stops Rust, preserves its state/evidence, and restores the verified
pre-cutover database **and matching files** for the pinned old runtime. It loses
post-cutover changes. Prefer fix-forward after Rust writes; data-losing restore
needs informed owner approval. A restore cannot undo a printer action or
Spoolman effect. Unknown sends still require reconciliation.

The baseline Rust schema ceilings are 35 without the Cargo feature and 42 with
it. In a rebuild build, the Rust storage writer opens only with the runtime
switch on, and only a directory that carries the durable ownership marker
`.pp-rust-owned.json` or is empty, in which case it writes the marker before
any schema. It refuses a non-empty unmarked directory unchanged. The Node-backed
runtime refuses any marked directory. Rust-only startup makes this decision
before release verification, cutover recovery and source-sync admission, so a
refused directory sees none of them. See [Storage](storage.md#ownership-marker-decision).
Opening a marked directory with the rebuild build can migrate data even though
the runtime switch is process-local. Turning the switch off is not a downgrade. Preflight
must reject unsupported/newer schemas without modifying the original. Prove the
actual Node input versions and older-binary refusal on copies.

Retire Node, the old screens, proxy/ownership machinery and unused contract
mirrors only after replacement acceptance and retention gates. Preserve useful
reviewed resource sets/history before deleting execution code. Flag retirement
needs its own approval/change; never solve it by enabling the default.

## Performance and platform evidence

Extend `pp-perf`, do not invent Tauri memory/startup claims. Capture installed
empty-shell, current compatibility and Rust-only builds on the same declared
machine/OS/webview and disposable scenarios. Measure core-ready, first paint
and first usable screen separately; cold/migration/warm launches are distinct.
Record raw samples, distributions and artifact identity.

Include webview/helper processes. Label Linux PSS, macOS footprint/RSS, and
Windows private bytes/working set separately; their absolute values are not
interchangeable. Measure visible/hidden idle CPU, burst latency, queue waits,
WAL growth, list sizes and asset import closures. Keep the existing 10% comparison
only for comparable low-noise scenarios. Set absolute budgets after baseline
measurement and owner review, not from arena estimates. Local safe diagnostics
are not product analytics; no default remote exporter or Sentry initialization.

| Target | Delivery and required evidence | Baseline gap / release decision |
| --- | --- | --- |
| macOS first | Build and sign the app/DMG for the owner-approved architecture set; notarize/staple; test installed WKWebView and restart/update on each supported architecture | Declared 13.5 minimum needs oldest-OS proof; architecture coverage, public identity and key custody remain open |
| Windows before 1.0 | Per-user NSIS, Evergreen WebView2, Authenticode; normal-user install/update and native smoke | One crate (`pp-desktop`) is excluded from baseline Windows CI. Share filesystem mechanics through narrow `pp-os`, port consumers incrementally, and keep signals with their runtime owner; choose signing provider and OS/architecture minima |
| Linux before 1.0 | AppImage plus selected deb/rpm, WebKitGTK 4.1, no-tray recovery, package/update trust appropriate to format | Declare oldest glibc/distro matrix and hardware GPU evidence. CI software rendering is not hardware certification |
| Docker | Non-root amd64/arm64, SQLite/volume migration, health, TERM/restart and optional slicer simulator | Runtime base, volume permissions and architecture execution require proof |
| LAN browsers | Shared UI, authenticated pairing, HTTPS/cookies, revocation and reconnect, mobile accessibility | LAN admission is not delivered by loopback-only routing; certificate trust must work on actual devices |

Set `WEBKIT_DISABLE_DMABUF_RENDERER=1` for WebKitGTK, matching the accepted UI
target and the repository's existing workaround. Keep a non-WebGL plate preview
fallback. Do not add other global GPU workarounds without a reproduced
platform-specific failure. Start filesystem work in the existing P-WINDOWS track,
in parallel with the gated product entry point, not after domain completion.
Extract existing Unix behavior without changing formats, prove Windows behavior
using real handles, and migrate storage, source and core one consumer at a time.
Preserve no-follow/reparse refusal, identity, locking and durable publication
under races. Capability libraries are candidates, not proof of those guarantees.
If the shared interface loses a guarantee, stop extraction and keep the local
adapter until corrected. This adds no separate assurance program beyond the
existing invariants and platform evidence.

Use real disposable on-disk SQLite, recorded protocol adapters and crash injection.
Proposed invariant tests must cover imported ID-set equality, autosave receipt
replay and stale-write refusal, snapshot-before-Accept atomicity, duplicate-order
promotion, burst replay/undo conflicts, restore preserving printed evidence,
newer-schema refusal without modification, and no resend after uncertain effects.
These are required proofs to implement, not tests claimed passing here.
Browser component/E2E checks complement native WKWebView/WebView2/WebKitGTK and
signed-installer acceptance. A screenshot or successful build alone is insufficient.

## Arena choices and research adjustments

Candidate 2 is the starting design because its family delivery order avoids a
large reorganization prerequisite. Later Chad decisions override its crate renames,
sidecar deletion, and assumed generator switch. Owner-delegated comparison of
the two design proposals retains operation crates and permits only the bounded
filesystem amendment. ADR 0015 links the source traces and primary documentation.
No compile-time, runtime or memory improvement has been measured.

| Proposal/source | Disposition and reason |
| --- | --- |
| Candidate 2, incremental family slices and early Windows proof | Keep; independently reviewable work without a crate-move prerequisite |
| Candidate 1, explicit scope dispositions and named invariant proofs | Graft; retain data/protocol/export compatibility, not old-screen shape. Reconcile existing issues before any tracker action |
| Candidate 3, explicit user send authority | Graft with private capability constructors; a public actor tag is insufficient |
| Candidate 3, legacy name mapping | Graft here, not implementation detail in the glossary: `build_profiles`/legacy `profile_id` refer to Plan identity, `projects` to Sources, `print_jobs` to printer history, `durable_jobs` to app Jobs, `tenant_id=default` remains a format detail |
| Candidate 3, capability filesystem and reviewed ownership history | Evaluate `cap-std`/`cap-fs-ext` for narrow `pp-os` with real Windows race tests; preserve reviewed resource sets at retirement |
| Candidate 1, crate Interface READMEs and boundary orchestration | Keep short interface docs; reuse `just ci`/existing checker before adding an xtask wrapper |
| Candidate 1 family split; candidate 3 six-crate engine; candidate 2 broad crate moves and facade deletion | Reject wholesale reorganization; current crates retain transaction locality. Permit only narrow filesystem sharing under ADR 0015; deeper private family modules address operation ownership |
| Candidate 1 generic public SQL closures and manual generated DTO rewrite | Reject; leaks transaction policy and replaces working generation without product gain |
| Candidate 3 prerequisite schemars 1.x upgrade | Reject as prerequisite; prove one contract family before a generator/dialect migration |
| Unified replacement for `/ws/printers` | Reject; preserve Chad-approved GRE-1014 path |
| Tonight with Rewind order authority and Free burst controls | Keep accepted Today/History/Accept/burst UX; manual fallback is mandatory, and claimed step savings still need fixture validation |
| Rebuild task engine or omit old decisions/planning phase | Reject; port workflow/build-planning projections and give assistant decisions a History home |
| Drop Docker slicing or bundle it into the image | Reject; optional external HTTP container is the chosen model |
| Research, automatic ts-rs versus OpenAPI choice | Defer choice to one wire comparison; retain working schema/Zod route meanwhile |
| Research, fixed performance/image budgets and replay windows | Not adopted as facts or gates; measure and approve first |

## Remaining owner decisions

- Release signing identity/provider, publisher name, key custodians and recovery.
  Evaluate Microsoft Artifact Signing and SignPath eligibility rather than
  assuming EV signing removes SmartScreen warnings.
- Acceptance/rollback window length, backup retention/free-space requirements,
  supported input schemas, timed abort/recovery budgets, and explicit production
  go/no-go. A window never promises lossless rollback.
- Flag retirement conditions/date and responsible issue; `desktop_rebuild` stays
  default-off until separately approved removal.
- Final UI/client package names and router; contract exporter after one-family
  comparison; REST version policy, replay/idempotency retention and undo conflicts.
- LAN TLS/trust UX, device/session lifetimes, supported MCP revisions and whether
  current-spec OAuth or documented limited compatibility ships first.
- Platform minima, signing/update channels and release cadence; Linux package
  set/GPU matrix; reference performance hardware and measured budgets.

The glossary remains [CONTEXT.md](../CONTEXT.md); no term changes are needed.
Keep this existing URL canonical, as the planning research recommends when an
existing explanation can remain in place. It avoids a second competing target
page and preserves root guidance links. [Storage](storage.md),
[Docker](docker.md), [Desktop](desktop.md), [Network sharing](network-sharing.md),
and [Release](release.md) retain their operational/current-state roles.
